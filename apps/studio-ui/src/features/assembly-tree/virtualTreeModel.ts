/**
 * Page state for the virtual assembly tree.
 *
 * The host keeps the full tree. This module only stores one window of rows,
 * so a 10,000-instance assembly never becomes 10,000 React objects.
 * It has no DOM or React imports, and Node can test every transition directly.
 */
import type {
  CadSessionTransport,
  HlodLevel,
  RoiInterfaceDigest,
  VirtualTreeNode,
} from "../../sync/cadSessionTransport.ts";

export type { HlodLevel, RoiInterfaceDigest, VirtualTreeNode };

/** Rows kept in the browser at once. The host still owns the rest of the tree. */
export const DEFAULT_VIRTUAL_TREE_PAGE_SIZE = 50;

/**
 * Search has no page cursor, only a limit. Hold at most this many hit ids and
 * page those ids locally. The assembly itself can be larger than this cap.
 */
const SEARCH_HIT_CAP = 10_000;

/** Browse pages stay small. Resolving a handful of search hits may read a wider slice. */
const RESOLVE_BATCH = 256;

export interface VirtualTreeState {
  readonly sessionId: string;
  readonly offset: number;
  readonly limit: number;
  readonly totalCount: number;
  readonly nodes: readonly VirtualTreeNode[];
  readonly searchQuery: string;
  readonly loading: boolean;
  readonly error: string | null;
  readonly selectedInstanceId: number | null;
  readonly hlodLevel: HlodLevel;
  readonly latestDigest: RoiInterfaceDigest | null;
}

export interface VirtualTreePageBounds {
  readonly pageIndex: number;
  readonly pageCount: number;
  readonly hasPrev: boolean;
  readonly hasNext: boolean;
}

/** `hlod2` is the coarse tier the host paints first for a large assembly. */
export function createInitialVirtualTreeState(sessionId: string, pageSize?: number): VirtualTreeState {
  return {
    sessionId,
    offset: 0,
    limit: normalizePageSize(pageSize),
    totalCount: 0,
    nodes: [],
    searchQuery: "",
    loading: false,
    error: null,
    selectedInstanceId: null,
    hlodLevel: "hlod2",
    latestDigest: null,
  };
}

/**
 * Load one browse page, or the next page of the active search.
 * A transport failure keeps the previous rows. The returned state is always safe to render.
 */
export async function loadVirtualTreePage(
  transport: CadSessionTransport,
  state: VirtualTreeState,
  offset: number,
): Promise<VirtualTreeState> {
  const safeOffset = normalizeOffset(offset);
  if (safeOffset === null) {
    return { ...state, loading: false, error: "offset must be a non-negative integer" };
  }
  const query = state.searchQuery.trim();
  try {
    if (query) return await loadSearchPage(transport, state, query, safeOffset);
    return await loadBrowsePage(transport, state, safeOffset);
  } catch (err) {
    return { ...state, loading: false, error: errorText(err) };
  }
}

/**
 * Search the host index. An empty query leaves search mode and reloads the first browse page.
 * Hit ids come back from the host; this function then resolves only the visible page into rows.
 */
export async function searchVirtualTree(
  transport: CadSessionTransport,
  state: VirtualTreeState,
  query: string,
): Promise<VirtualTreeState> {
  const trimmed = query.trim();
  if (!trimmed) {
    return loadVirtualTreePage(transport, { ...state, searchQuery: "" }, 0);
  }
  try {
    return await loadSearchPage(transport, state, trimmed, 0);
  } catch (err) {
    return { ...state, loading: false, error: errorText(err) };
  }
}

/**
 * Ask the host for the interface digest of the selected row.
 * The ROI is that row's `bbox_min_mm` / `bbox_max_mm`. A missing row or a failed
 * request updates `error` and leaves the current page in place.
 */
export async function inspectSelectionRoiDigest(
  transport: CadSessionTransport,
  state: VirtualTreeState,
  instanceId: number,
): Promise<VirtualTreeState> {
  if (!Number.isSafeInteger(instanceId) || instanceId < 0) {
    return { ...state, loading: false, error: "instanceId must be a non-negative integer" };
  }
  const node = state.nodes.find((item) => item.instance_id === instanceId);
  if (!node) {
    return {
      ...state,
      selectedInstanceId: instanceId,
      loading: false,
      error: `Instance ${instanceId} is not on the current page`,
    };
  }
  const roiMin: readonly [number, number, number] = [node.bbox_min_mm[0], node.bbox_min_mm[1], node.bbox_min_mm[2]];
  const roiMax: readonly [number, number, number] = [node.bbox_max_mm[0], node.bbox_max_mm[1], node.bbox_max_mm[2]];
  try {
    const latestDigest = await transport.fetchInterfaceDigest(state.sessionId, roiMin, roiMax);
    return {
      ...state,
      selectedInstanceId: instanceId,
      latestDigest,
      loading: false,
      error: null,
    };
  } catch (err) {
    return {
      ...state,
      selectedInstanceId: instanceId,
      loading: false,
      error: errorText(err),
    };
  }
}

/** Path from the parent instance to this row. A root has no parent segment. */
export function formatAssemblyBreadcrumb(node: VirtualTreeNode): string {
  const name = node.name.trim();
  const part = node.part_number.trim();
  const label = name || part || `#${node.instance_id}`;
  if (node.parent_id === null) return label;
  return `#${node.parent_id} / ${label}`;
}

/**
 * Page controls for a window that may be empty, partial, or past the last row.
 * Non-integers and negatives collapse to an empty page so the buttons stay deterministic.
 */
export function computePageBounds(offset: number, limit: number, totalCount: number): VirtualTreePageBounds {
  const safeLimit = Number.isSafeInteger(limit) && limit > 0 ? limit : 1;
  const safeTotal = Number.isSafeInteger(totalCount) && totalCount > 0 ? totalCount : 0;
  const safeOffset = Number.isSafeInteger(offset) && offset > 0 ? offset : 0;
  if (safeTotal === 0) {
    return { pageIndex: 0, pageCount: 0, hasPrev: false, hasNext: false };
  }
  const pageCount = Math.ceil(safeTotal / safeLimit);
  const pageIndex = Math.min(Math.floor(safeOffset / safeLimit), pageCount - 1);
  return {
    pageIndex,
    pageCount,
    hasPrev: safeOffset > 0,
    hasNext: safeOffset + safeLimit < safeTotal,
  };
}

async function loadBrowsePage(
  transport: CadSessionTransport,
  state: VirtualTreeState,
  offset: number,
): Promise<VirtualTreeState> {
  const start = clampBrowseOffset(offset, state.limit, state.totalCount);
  const page = await transport.fetchTreeSlice(state.sessionId, start, state.limit);
  return {
    ...state,
    offset: page.offset,
    limit: state.limit,
    totalCount: page.total_count,
    nodes: page.nodes.slice(0, state.limit).map(copyNode),
    searchQuery: "",
    loading: false,
    error: null,
  };
}

async function loadSearchPage(
  transport: CadSessionTransport,
  state: VirtualTreeState,
  query: string,
  offset: number,
): Promise<VirtualTreeState> {
  const ids = uniqueIds(await transport.searchAssemblyTree(state.sessionId, query, SEARCH_HIT_CAP)).slice(0, SEARCH_HIT_CAP);
  const window = sliceHitPage(ids, offset, state.limit);
  const nodes = window.ids.length === 0
    ? []
    : await resolveNodes(transport, state.sessionId, window.ids, state.limit);
  return {
    ...state,
    offset: window.offset,
    limit: state.limit,
    totalCount: ids.length,
    nodes,
    searchQuery: query,
    loading: false,
    error: null,
  };
}

/**
 * The host search returns ids, not rows. Walk browse slices until every id on
 * this page is found. Stop early so a hit on the first screen does not pull the rest of the tree.
 */
async function resolveNodes(
  transport: CadSessionTransport,
  sessionId: string,
  ids: readonly number[],
  pageSize: number,
): Promise<VirtualTreeNode[]> {
  const wanted = new Set(ids);
  const found = new Map<number, VirtualTreeNode>();
  const batch = Math.max(pageSize, RESOLVE_BATCH);
  let offset = 0;
  let total = Number.POSITIVE_INFINITY;
  while (found.size < wanted.size && offset < total) {
    const page = await transport.fetchTreeSlice(sessionId, offset, batch);
    total = page.total_count;
    for (const node of page.nodes) {
      if (wanted.has(node.instance_id) && !found.has(node.instance_id)) {
        found.set(node.instance_id, copyNode(node));
      }
    }
    if (page.nodes.length === 0) break;
    const next = page.offset + page.nodes.length;
    if (next <= offset) break;
    offset = next;
  }
  const ordered: VirtualTreeNode[] = [];
  for (const id of ids) {
    const node = found.get(id);
    if (node) ordered.push(node);
  }
  return ordered;
}

function sliceHitPage(
  ids: readonly number[],
  offset: number,
  limit: number,
): { readonly offset: number; readonly ids: readonly number[] } {
  if (ids.length === 0) return { offset: 0, ids: [] };
  const safeLimit = limit > 0 ? limit : 1;
  const last = Math.floor((ids.length - 1) / safeLimit) * safeLimit;
  const start = offset > last ? last : offset;
  return { offset: start, ids: ids.slice(start, start + safeLimit) };
}

function clampBrowseOffset(offset: number, limit: number, totalCount: number): number {
  if (totalCount <= 0 || offset < totalCount) return offset;
  const safeLimit = limit > 0 ? limit : 1;
  return Math.floor((totalCount - 1) / safeLimit) * safeLimit;
}

function uniqueIds(ids: readonly number[]): number[] {
  const seen = new Set<number>();
  const unique: number[] = [];
  for (const id of ids) {
    if (!Number.isSafeInteger(id) || id < 0 || seen.has(id)) continue;
    seen.add(id);
    unique.push(id);
  }
  return unique;
}

function copyNode(node: VirtualTreeNode): VirtualTreeNode {
  return {
    instance_id: node.instance_id,
    parent_id: node.parent_id,
    prototype_id: node.prototype_id,
    name: node.name,
    part_number: node.part_number,
    bbox_min_mm: [node.bbox_min_mm[0], node.bbox_min_mm[1], node.bbox_min_mm[2]],
    bbox_max_mm: [node.bbox_max_mm[0], node.bbox_max_mm[1], node.bbox_max_mm[2]],
    frozen_lightweight: node.frozen_lightweight,
  };
}

function normalizePageSize(pageSize: number | undefined): number {
  if (pageSize === undefined) return DEFAULT_VIRTUAL_TREE_PAGE_SIZE;
  if (!Number.isSafeInteger(pageSize) || pageSize < 1) return DEFAULT_VIRTUAL_TREE_PAGE_SIZE;
  return pageSize;
}

function normalizeOffset(offset: number): number | null {
  if (!Number.isSafeInteger(offset) || offset < 0) return null;
  return offset;
}

function errorText(err: unknown): string {
  if (err instanceof Error && err.message.trim()) return err.message;
  if (typeof err === "string" && err.trim()) return err.trim();
  return "Assembly tree request failed";
}
