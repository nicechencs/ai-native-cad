/**
 * One session door for the desktop app and the future web app.
 *
 * The workbench asks for a tree page, a search, an HLOD chunk, an ROI digest,
 * or a graph patch. It does not care whether the bytes come from Tauri or HTTP.
 * Pass `fetchImpl` or `invokeImpl` in tests so Node never loads the Tauri runtime.
 *
 * Web routes (no trailing slash on the base URL):
 * - GET  /api/v1/sessions/:id/tree?offset&limit
 * - GET  /api/v1/sessions/:id/tree/search?query&limit
 * - GET  /api/v1/sessions/:id/hlod-chunks/:chunkId
 *   Optional header `Range: bytes=start-end` (both ends inclusive).
 * - POST /api/v1/sessions/:id/interface-digest
 * - POST /api/project/apply-patch, through `applyAicadGraphPatch`
 *
 * Desktop Tauri commands (`invoke(cmd, { request })`):
 * - fetch_tree_slice
 * - search_assembly_tree
 * - fetch_hlod_chunk
 * - fetch_interface_digest
 * - apply_aicad_patch
 *
 * HLOD chunk ids point at `.cadpack` sections SEC_MESH_HLOD0/1/2.
 * One chunk is at most 16 MiB, matching the streaming container plan.
 */
import { applyAicadGraphPatch, type ProjectPatchResult } from "./projectClient.ts";
import type { GraphPatch } from "../types/graphIr.ts";
import { TRACE_HEADER, createTraceId, readTraceId, withTraceHeader } from "../utils/trace.ts";

export type { GraphPatch, ProjectPatchResult };

/** Mesh detail stored in a `.cadpack` HLOD section. `hlod2` is the coarse first paint. */
export type HlodLevel = "hlod0" | "hlod1" | "hlod2";

export const HLOD_LEVELS: readonly HlodLevel[] = ["hlod0", "hlod1", "hlod2"];

/** Section directory names from the `.cadpack` layout. */
export const HLOD_SECTION_KIND: Readonly<Record<HlodLevel, string>> = {
  hlod0: "SEC_MESH_HLOD0",
  hlod1: "SEC_MESH_HLOD1",
  hlod2: "SEC_MESH_HLOD2",
};

export function isHlodLevel(value: string): value is HlodLevel {
  return HLOD_LEVELS.some((level) => level === value);
}

/** One row of the virtual assembly tree. The full tree stays on the host. */
export interface VirtualTreeNode {
  readonly instance_id: number;
  readonly parent_id: number | null;
  readonly prototype_id: number;
  readonly name: string;
  readonly part_number: string;
  readonly bbox_min_mm: readonly [number, number, number];
  readonly bbox_max_mm: readonly [number, number, number];
  readonly frozen_lightweight: boolean;
}

/** A window of tree rows. `total_count` is the whole tree, not this page. */
export interface TreePageSlice {
  readonly offset: number;
  readonly limit: number;
  readonly total_count: number;
  readonly nodes: readonly VirtualTreeNode[];
}

/**
 * A few-kilobyte description of the parts inside a box.
 * `summary_json` is the component-interface digest text. The client does not expand it.
 */
export interface RoiInterfaceDigest {
  readonly roi_min_mm: readonly [number, number, number];
  readonly roi_max_mm: readonly [number, number, number];
  readonly component_count: number;
  readonly summary_json: string;
}

export interface FetchTreeSliceRequest {
  readonly session_id: string;
  readonly offset: number;
  readonly limit: number;
}

export interface SearchAssemblyTreeRequest {
  readonly session_id: string;
  readonly query: string;
  /** `null` lets the host pick the page size. */
  readonly limit: number | null;
}

export interface FetchHlodChunkRequest {
  readonly session_id: string;
  readonly chunk_id: number;
  /** Inclusive `[start, end]`. `null` reads the whole chunk. */
  readonly byte_range: readonly [number, number] | null;
}

export interface FetchInterfaceDigestRequest {
  readonly session_id: string;
  readonly roi_min_mm: readonly [number, number, number];
  readonly roi_max_mm: readonly [number, number, number];
}

/** Same field names as the Tauri `apply_aicad_patch` request struct. */
export interface ApplyAicadPatchRequest {
  readonly project_path: string;
  readonly patch: GraphPatch;
}

export const SESSION_IPC_COMMANDS = {
  fetchTreeSlice: "fetch_tree_slice",
  searchAssemblyTree: "search_assembly_tree",
  fetchHlodChunk: "fetch_hlod_chunk",
  fetchInterfaceDigest: "fetch_interface_digest",
  applyProjectPatch: "apply_aicad_patch",
} as const;

/** Tauri v2 `invoke(cmd, args)`. The desktop shell passes `{ request }`. */
export type TauriInvoke = (cmd: string, args?: object) => Promise<unknown>;

export interface WebHttpTransportOptions {
  /** Origin of the studio server, without a trailing slash. Empty means this page's origin. */
  readonly baseUrl?: string;
  /** Replacement for global `fetch`. Tests pass a mock here. */
  readonly fetchImpl?: typeof fetch;
}

export interface DesktopTauriTransportOptions {
  /** Replacement for Tauri `invoke`. Tests pass a mock here. */
  readonly invokeImpl?: TauriInvoke;
}

export interface SessionTransportOptions extends WebHttpTransportOptions, DesktopTauriTransportOptions {
  /** Omit to pick desktop when Tauri is present, and web otherwise. */
  readonly mode?: "desktop-tauri" | "web-http";
}

export interface CadSessionTransport {
  readonly mode: "desktop-tauri" | "web-http";
  fetchTreeSlice(sessionId: string, offset: number, limit: number): Promise<TreePageSlice>;
  searchAssemblyTree(sessionId: string, query: string, limit?: number): Promise<readonly number[]>;
  fetchHlodChunk(sessionId: string, chunkId: number, byteRange?: readonly [number, number]): Promise<Uint8Array>;
  fetchInterfaceDigest(
    sessionId: string,
    roiMin: readonly [number, number, number],
    roiMax: readonly [number, number, number],
  ): Promise<RoiInterfaceDigest>;
  applyProjectPatch(relativePath: string, patch: GraphPatch): Promise<ProjectPatchResult>;
}

/** Failure from a session transport call. `status` is the HTTP status, or `0` when no response arrived. */
export class CadSessionTransportError extends Error {
  readonly status: number;
  readonly payload: unknown;
  readonly traceId?: string;

  constructor(message: string, status: number, payload: unknown, traceId?: string) {
    super(message);
    this.name = "CadSessionTransportError";
    this.status = status;
    this.payload = payload;
    this.traceId = traceId ?? readTraceId(undefined, payload, "");
    if (!this.traceId) this.traceId = undefined;
  }
}

const SESSION_HTTP_PREFIX = "/api/v1/sessions";
/** `.cadpack` mesh chunks are capped at 16 MiB. A Range request cannot ask for more. */
const MAX_HLOD_CHUNK_BYTES = 16 * 1024 * 1024;

export class WebHttpSessionTransport implements CadSessionTransport {
  readonly mode = "web-http" as const;
  private readonly baseUrl: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: WebHttpTransportOptions = {}) {
    this.baseUrl = trimBaseUrl(options.baseUrl);
    this.fetchImpl = options.fetchImpl ?? ((input, init) => globalThis.fetch(input, init));
  }

  async fetchTreeSlice(sessionId: string, offset: number, limit: number): Promise<TreePageSlice> {
    const request: FetchTreeSliceRequest = {
      session_id: requireSessionId(sessionId),
      offset: requireOffset(offset),
      limit: requireLimit(limit),
    };
    const params = new URLSearchParams({
      offset: String(request.offset),
      limit: String(request.limit),
    });
    const payload = await this.getJson(this.sessionPath(request.session_id, `tree?${params.toString()}`));
    return this.parseTraced(payload.traceId, payload.body, () => parseTreePageSlice(payload.body, payload.status));
  }

  async searchAssemblyTree(sessionId: string, query: string, limit?: number): Promise<readonly number[]> {
    const request: SearchAssemblyTreeRequest = {
      session_id: requireSessionId(sessionId),
      query: requireQuery(query),
      limit: requireOptionalLimit(limit),
    };
    const params = new URLSearchParams({ query: request.query });
    if (request.limit !== null) params.set("limit", String(request.limit));
    const payload = await this.getJson(this.sessionPath(request.session_id, `tree/search?${params.toString()}`));
    return this.parseTraced(payload.traceId, payload.body, () => parseInstanceIds(payload.body, payload.status));
  }

  async fetchHlodChunk(
    sessionId: string,
    chunkId: number,
    byteRange?: readonly [number, number],
  ): Promise<Uint8Array> {
    const request: FetchHlodChunkRequest = {
      session_id: requireSessionId(sessionId),
      chunk_id: requireChunkId(chunkId),
      byte_range: requireByteRange(byteRange),
    };
    const headers: Record<string, string> = {
      Accept: "application/octet-stream",
      // Range counts raw chunk bytes. Identity keeps a gzip body from changing that length.
      "Accept-Encoding": "identity",
    };
    if (request.byte_range) {
      const [start, end] = request.byte_range;
      headers.Range = `bytes=${start}-${end}`;
    }
    const fetched = await this.request(this.sessionPath(request.session_id, `hlod-chunks/${request.chunk_id}`), {
      method: "GET",
      headers,
    });
    const response = fetched.response;
    if (!response.ok) {
      const payload = await readPayload(response);
      throw new CadSessionTransportError(
        messageFromPayload(payload, response.status, "HLOD chunk request failed"),
        response.status,
        payload,
        readTraceId(response.headers.get(TRACE_HEADER), payload, fetched.traceId),
      );
    }
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (request.byte_range) {
      const expected = request.byte_range[1] - request.byte_range[0] + 1;
      if (bytes.byteLength !== expected) {
        throw new CadSessionTransportError(
          `HLOD range response was ${bytes.byteLength} bytes, expected ${expected}`,
          response.status,
          null,
          fetched.traceId,
        );
      }
    }
    return bytes;
  }

  async fetchInterfaceDigest(
    sessionId: string,
    roiMin: readonly [number, number, number],
    roiMax: readonly [number, number, number],
  ): Promise<RoiInterfaceDigest> {
    const request: FetchInterfaceDigestRequest = {
      session_id: requireSessionId(sessionId),
      roi_min_mm: requireVec3(roiMin, "roiMin"),
      roi_max_mm: requireVec3(roiMax, "roiMax"),
    };
    const payload = await this.postJson(this.sessionPath(request.session_id, "interface-digest"), {
      roi_min_mm: request.roi_min_mm,
      roi_max_mm: request.roi_max_mm,
    });
    return this.parseTraced(payload.traceId, payload.body, () => parseInterfaceDigest(payload.body, payload.status));
  }

  applyProjectPatch(relativePath: string, patch: GraphPatch): Promise<ProjectPatchResult> {
    requireRelativePath(relativePath);
    requirePatch(patch);
    return applyAicadGraphPatch(relativePath, patch, {
      baseUrl: this.baseUrl,
      fetchImpl: this.fetchImpl,
    });
  }

  private sessionPath(sessionId: string, suffix: string): string {
    return `${this.baseUrl}${SESSION_HTTP_PREFIX}/${encodeURIComponent(sessionId)}/${suffix}`;
  }

  private getJson(url: string): Promise<{ readonly status: number; readonly body: unknown; readonly traceId: string }> {
    return this.sendJson(url, { method: "GET", headers: { Accept: "application/json" }, cache: "no-store" });
  }

  private postJson(url: string, body: object): Promise<{ readonly status: number; readonly body: unknown; readonly traceId: string }> {
    return this.sendJson(url, {
      method: "POST",
      headers: { Accept: "application/json", "Content-Type": "application/json" },
      cache: "no-store",
      body: JSON.stringify(body),
    });
  }

  private async sendJson(url: string, init: RequestInit): Promise<{ readonly status: number; readonly body: unknown; readonly traceId: string }> {
    const fetched = await this.request(url, init);
    const body = await readPayload(fetched.response);
    const traceId = readTraceId(fetched.response.headers.get(TRACE_HEADER), body, fetched.traceId);
    if (!fetched.response.ok) {
      throw new CadSessionTransportError(messageFromPayload(body, fetched.response.status, "Session request failed"), fetched.response.status, body, traceId);
    }
    return { status: fetched.response.status, body, traceId };
  }

  private async request(url: string, init: RequestInit): Promise<{ readonly response: Response; readonly traceId: string }> {
    const traceId = createTraceId();
    try {
      const response = await this.fetchImpl(url, withTraceHeader(init, traceId));
      return { response, traceId: readTraceId(response.headers.get(TRACE_HEADER), undefined, traceId) };
    } catch (err) {
      if (err instanceof CadSessionTransportError) throw err;
      throw new CadSessionTransportError(errorMessage(err, "Session request failed"), 0, null, traceId);
    }
  }

  private parseTraced<T>(traceId: string, body: unknown, parse: () => T): T {
    try {
      return parse();
    } catch (err) {
      if (err instanceof CadSessionTransportError) {
        throw new CadSessionTransportError(err.message, err.status, err.payload, err.traceId ?? readTraceId(undefined, body, traceId));
      }
      throw err;
    }
  }
}

export class DesktopTauriSessionTransport implements CadSessionTransport {
  readonly mode = "desktop-tauri" as const;
  private readonly invokeImpl: TauriInvoke;

  constructor(options: DesktopTauriTransportOptions = {}) {
    this.invokeImpl = resolveInvoke(options);
  }

  async fetchTreeSlice(sessionId: string, offset: number, limit: number): Promise<TreePageSlice> {
    const request: FetchTreeSliceRequest = {
      session_id: requireSessionId(sessionId),
      offset: requireOffset(offset),
      limit: requireLimit(limit),
    };
    return this.invokeParsed(SESSION_IPC_COMMANDS.fetchTreeSlice, request, (payload) => parseTreePageSlice(payload, 0));
  }

  async searchAssemblyTree(sessionId: string, query: string, limit?: number): Promise<readonly number[]> {
    const request: SearchAssemblyTreeRequest = {
      session_id: requireSessionId(sessionId),
      query: requireQuery(query),
      limit: requireOptionalLimit(limit),
    };
    return this.invokeParsed(SESSION_IPC_COMMANDS.searchAssemblyTree, request, (payload) => parseInstanceIds(payload, 0));
  }

  async fetchHlodChunk(
    sessionId: string,
    chunkId: number,
    byteRange?: readonly [number, number],
  ): Promise<Uint8Array> {
    const request: FetchHlodChunkRequest = {
      session_id: requireSessionId(sessionId),
      chunk_id: requireChunkId(chunkId),
      byte_range: requireByteRange(byteRange),
    };
    return this.invokeParsed(SESSION_IPC_COMMANDS.fetchHlodChunk, request, (payload) => bytesFromPayload(payload, 0));
  }

  async fetchInterfaceDigest(
    sessionId: string,
    roiMin: readonly [number, number, number],
    roiMax: readonly [number, number, number],
  ): Promise<RoiInterfaceDigest> {
    const request: FetchInterfaceDigestRequest = {
      session_id: requireSessionId(sessionId),
      roi_min_mm: requireVec3(roiMin, "roiMin"),
      roi_max_mm: requireVec3(roiMax, "roiMax"),
    };
    return this.invokeParsed(SESSION_IPC_COMMANDS.fetchInterfaceDigest, request, (payload) => parseInterfaceDigest(payload, 0));
  }

  async applyProjectPatch(relativePath: string, patch: GraphPatch): Promise<ProjectPatchResult> {
    const request: ApplyAicadPatchRequest = {
      project_path: requireRelativePath(relativePath),
      patch: requirePatch(patch),
    };
    return this.invokeParsed(SESSION_IPC_COMMANDS.applyProjectPatch, request, (payload) => parsePatchResult(payload, 0));
  }

  private async invokeParsed<T>(cmd: string, request: object, parse: (payload: unknown) => T): Promise<T> {
    const traced = await this.invoke(cmd, request);
    return this.parseTraced(traced.traceId, traced.payload, () => parse(traced.payload));
  }

  private async invoke(cmd: string, request: object): Promise<{ readonly payload: unknown; readonly traceId: string }> {
    const traceId = createTraceId();
    const tracedRequest = { ...request, trace_id: traceId };
    try {
      const payload = await this.invokeImpl(cmd, { request: tracedRequest });
      return { payload, traceId: readTraceId(undefined, payload, traceId) };
    } catch (err) {
      if (err instanceof CadSessionTransportError) throw err;
      throw new CadSessionTransportError(errorMessage(err, "Desktop session command failed"), 0, err, readTraceId(undefined, err, traceId));
    }
  }

  private parseTraced<T>(traceId: string, body: unknown, parse: () => T): T {
    try {
      return parse();
    } catch (err) {
      if (err instanceof CadSessionTransportError) {
        throw new CadSessionTransportError(err.message, err.status, err.payload, err.traceId ?? readTraceId(undefined, body, traceId));
      }
      throw err;
    }
  }
}

/**
 * Pick a transport.
 * An explicit `mode` wins. Otherwise desktop is used when `invokeImpl` or
 * `window.__TAURI_INTERNALS__.invoke` is present, and web is used in a browser tab.
 */
export function createSessionTransport(options: SessionTransportOptions = {}): CadSessionTransport {
  const mode = resolveMode(options);
  if (mode === "desktop-tauri") return new DesktopTauriSessionTransport(options);
  return new WebHttpSessionTransport(options);
}

export function detectSessionTransportMode(options: SessionTransportOptions = {}): "desktop-tauri" | "web-http" {
  return resolveMode(options);
}

function resolveMode(options: SessionTransportOptions): "desktop-tauri" | "web-http" {
  if (options.mode !== undefined && options.mode !== "desktop-tauri" && options.mode !== "web-http") {
    throw new CadSessionTransportError("Session transport mode must be desktop-tauri or web-http", 0, options.mode);
  }
  if (options.mode) return options.mode;
  if (options.invokeImpl || readGlobalTauriInvoke()) return "desktop-tauri";
  return "web-http";
}

function resolveInvoke(options: DesktopTauriTransportOptions): TauriInvoke {
  if (options.invokeImpl) return options.invokeImpl;
  const found = readGlobalTauriInvoke();
  if (found) return found;
  throw new CadSessionTransportError(
    "Desktop Tauri invoke is not available. Pass invokeImpl or run inside the Tauri v2 runtime.",
    0,
    null,
  );
}

interface TauriHost {
  readonly __TAURI_INTERNALS__?: {
    readonly invoke?: TauriInvoke;
  };
}

function readGlobalTauriInvoke(): TauriInvoke | null {
  const host = (globalThis as { window?: TauriHost }).window;
  const internals = host?.__TAURI_INTERNALS__;
  const invoke = internals?.invoke;
  if (!internals || typeof invoke !== "function") return null;
  return (cmd, args) => invoke.call(internals, cmd, args);
}

function trimBaseUrl(baseUrl: string | undefined): string {
  return (baseUrl ?? "").replace(/\/+$/, "");
}

function requireSessionId(sessionId: string): string {
  if (typeof sessionId !== "string" || sessionId.length === 0 || sessionId.length > 256) {
    throw new CadSessionTransportError("sessionId must be a non-empty string", 0, sessionId);
  }
  if (/[\u0000-\u001f\u007f/\\?#]/.test(sessionId) || sessionId.includes("..")) {
    throw new CadSessionTransportError("sessionId contains unsupported characters", 0, sessionId);
  }
  return sessionId;
}

function requireOffset(offset: number): number {
  if (!Number.isSafeInteger(offset) || offset < 0) {
    throw new CadSessionTransportError("offset must be a non-negative integer", 0, offset);
  }
  return offset;
}

function requireLimit(limit: number): number {
  if (!Number.isSafeInteger(limit) || limit < 1) {
    throw new CadSessionTransportError("limit must be a positive integer", 0, limit);
  }
  return limit;
}

function requireOptionalLimit(limit: number | undefined): number | null {
  if (limit === undefined) return null;
  return requireLimit(limit);
}

function requireQuery(query: string): string {
  if (typeof query !== "string" || query.trim() === "") {
    throw new CadSessionTransportError("query must be a non-empty string", 0, query);
  }
  return query.trim();
}

function requireChunkId(chunkId: number): number {
  if (!Number.isSafeInteger(chunkId) || chunkId < 0) {
    throw new CadSessionTransportError("chunkId must be a non-negative integer", 0, chunkId);
  }
  return chunkId;
}

function requireByteRange(byteRange: readonly [number, number] | undefined): readonly [number, number] | null {
  if (byteRange === undefined) return null;
  if (!Array.isArray(byteRange) || byteRange.length !== 2) {
    throw new CadSessionTransportError("byteRange must be [start, end]", 0, byteRange);
  }
  const start = byteRange[0];
  const end = byteRange[1];
  if (!Number.isSafeInteger(start) || start < 0 || !Number.isSafeInteger(end) || end < 0) {
    throw new CadSessionTransportError("byteRange ends must be non-negative integers", 0, byteRange);
  }
  if (end < start) {
    throw new CadSessionTransportError("byteRange end must be greater than or equal to start", 0, byteRange);
  }
  const length = end - start + 1;
  if (length > MAX_HLOD_CHUNK_BYTES) {
    throw new CadSessionTransportError(`byteRange is larger than one HLOD chunk (${MAX_HLOD_CHUNK_BYTES} bytes)`, 0, byteRange);
  }
  return [start, end];
}

function requireVec3(value: readonly [number, number, number], label: string): readonly [number, number, number] {
  if (!Array.isArray(value) || value.length !== 3 || !value.every(isFiniteNumber)) {
    throw new CadSessionTransportError(`${label} must be three finite numbers`, 0, value);
  }
  return [value[0], value[1], value[2]];
}

function requireRelativePath(relativePath: string): string {
  if (typeof relativePath !== "string" || relativePath.trim() === "") {
    throw new CadSessionTransportError("relativePath must be a non-empty string", 0, relativePath);
  }
  return relativePath;
}

function requirePatch(patch: GraphPatch): GraphPatch {
  if (!isRecord(patch) || typeof patch.patch_id !== "string" || !Array.isArray(patch.ops)) {
    throw new CadSessionTransportError("patch must be a GraphPatch", 0, patch);
  }
  return patch;
}

function parseTreePageSlice(payload: unknown, status: number): TreePageSlice {
  const record = requireRecord(payload, "Tree page", status);
  const nodes = record.nodes;
  if (!Array.isArray(nodes)) {
    throw new CadSessionTransportError("Tree page is missing nodes", status, payload);
  }
  return {
    offset: requireCount(record, "offset", status, payload),
    limit: requireCount(record, "limit", status, payload),
    total_count: requireCount(record, "total_count", status, payload),
    nodes: nodes.map((node) => parseTreeNode(node, status, payload)),
  };
}

function parseTreeNode(payload: unknown, status: number, page: unknown): VirtualTreeNode {
  const record = requireRecord(payload, "Virtual tree node", status);
  return {
    instance_id: requireId(record, "instance_id", status, page),
    parent_id: parseParentId(record.parent_id, status, page),
    prototype_id: requireId(record, "prototype_id", status, page),
    name: requireString(record, "name", status, page),
    part_number: requireString(record, "part_number", status, page),
    bbox_min_mm: parseVec3(record.bbox_min_mm, "bbox_min_mm", status, page),
    bbox_max_mm: parseVec3(record.bbox_max_mm, "bbox_max_mm", status, page),
    frozen_lightweight: requireBoolean(record, "frozen_lightweight", status, page),
  };
}

function parseParentId(value: unknown, status: number, payload: unknown): number | null {
  if (value === null) return null;
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return value;
  throw new CadSessionTransportError("Virtual tree node is missing parent_id", status, payload);
}

function parseInstanceIds(payload: unknown, status: number): readonly number[] {
  if (!Array.isArray(payload)) {
    throw new CadSessionTransportError("Assembly search response must be an array of instance ids", status, payload);
  }
  return payload.map((value, index) => {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
      throw new CadSessionTransportError(`Assembly search response has an invalid id at ${index}`, status, payload);
    }
    return value;
  });
}

function parseInterfaceDigest(payload: unknown, status: number): RoiInterfaceDigest {
  const record = requireRecord(payload, "Interface digest", status);
  return {
    roi_min_mm: parseVec3(record.roi_min_mm, "roi_min_mm", status, payload),
    roi_max_mm: parseVec3(record.roi_max_mm, "roi_max_mm", status, payload),
    component_count: requireCount(record, "component_count", status, payload),
    summary_json: requireString(record, "summary_json", status, payload),
  };
}

function parsePatchResult(payload: unknown, status: number): ProjectPatchResult {
  const record = requireRecord(payload, "Project patch", status);
  if (record.status !== "ok") {
    throw new CadSessionTransportError("Project patch response was not successful", status, payload);
  }
  return {
    status: "ok",
    patch_id: requireString(record, "patch_id", status, payload),
    path: requireString(record, "path", status, payload),
  };
}

function bytesFromPayload(payload: unknown, status: number): Uint8Array {
  if (payload instanceof Uint8Array) return payload;
  if (payload instanceof ArrayBuffer) return new Uint8Array(payload);
  if (ArrayBuffer.isView(payload)) {
    return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  }
  if (Array.isArray(payload)) {
    const bytes = new Uint8Array(payload.length);
    for (let index = 0; index < payload.length; index += 1) {
      const value = payload[index];
      if (typeof value !== "number" || !Number.isInteger(value) || value < 0 || value > 255) {
        throw new CadSessionTransportError("HLOD chunk bytes must be integers in 0..255", status, payload);
      }
      bytes[index] = value;
    }
    return bytes;
  }
  throw new CadSessionTransportError("HLOD chunk response was not bytes", status, payload);
}

async function readPayload(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text.trim()) return null;
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

function messageFromPayload(payload: unknown, status: number, fallback: string): string {
  if (isRecord(payload)) {
    if (typeof payload.message === "string" && payload.message.trim()) return payload.message;
    if (typeof payload.error === "string" && payload.error.trim()) return payload.error;
  }
  if (typeof payload === "string" && payload.trim()) return payload.trim().slice(0, 500);
  return `${fallback} (HTTP ${status})`;
}

function requireRecord(payload: unknown, label: string, status: number): Record<string, unknown> {
  if (!isRecord(payload)) {
    throw new CadSessionTransportError(`${label} response was not an object`, status, payload);
  }
  return payload;
}

function requireString(record: Record<string, unknown>, key: string, status: number, payload: unknown): string {
  const value = record[key];
  if (typeof value !== "string") {
    throw new CadSessionTransportError(`Session response is missing ${key}`, status, payload);
  }
  return value;
}

function requireCount(record: Record<string, unknown>, key: string, status: number, payload: unknown): number {
  const value = record[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new CadSessionTransportError(`Session response is missing ${key}`, status, payload);
  }
  return value;
}

function requireId(record: Record<string, unknown>, key: string, status: number, payload: unknown): number {
  return requireCount(record, key, status, payload);
}

function requireBoolean(record: Record<string, unknown>, key: string, status: number, payload: unknown): boolean {
  const value = record[key];
  if (typeof value !== "boolean") {
    throw new CadSessionTransportError(`Session response is missing ${key}`, status, payload);
  }
  return value;
}

function parseVec3(
  value: unknown,
  key: string,
  status: number,
  payload: unknown,
): readonly [number, number, number] {
  if (!Array.isArray(value) || value.length !== 3 || !value.every(isFiniteNumber)) {
    throw new CadSessionTransportError(`Session response is missing ${key}`, status, payload);
  }
  return [value[0], value[1], value[2]];
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function errorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error && err.message.trim()) return err.message;
  if (typeof err === "string" && err.trim()) return err.trim();
  return fallback;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
