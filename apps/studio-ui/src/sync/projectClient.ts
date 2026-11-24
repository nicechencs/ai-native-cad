/**
 * Typed calls for the dev-server `.aicad` bridge (`/api/project/*`).
 * Pass `fetchImpl` to test without a server. Set `baseUrl` for an ephemeral server;
 * leave it empty in the browser so requests stay on the current origin.
 */
import type {
  CacheStatus,
  CanonicalGraph,
  GraphPatch,
  ProjectStorageKind,
} from "../types/graphIr.ts";
import { createTraceId, readTraceId, withTraceHeader } from "../utils/trace.ts";

export interface ProjectClientOptions {
  /** Origin of the studio dev server, without a trailing slash. */
  readonly baseUrl?: string;
  /** Replacement for global `fetch`. Tests pass a mock here. */
  readonly fetchImpl?: typeof fetch;
}

export interface ProjectInitOptions extends ProjectClientOptions {
  /** Optional physical container format (`"directory"` default). */
  readonly format?: ProjectStorageKind;
}

export interface ProjectApplyPatchOptions extends ProjectClientOptions {
  /** Optional target branch when patching a SQLite `.aicad` container. */
  readonly branch?: string;
}

export interface ProjectVacuumOptions extends ProjectClientOptions {
  /** Optional number of recent `oplog` entries to retain per branch during GC. */
  readonly keepRecentOplog?: number;
}

export interface ProjectInitResult {
  readonly status: "ok";
  readonly project_name: string;
  readonly path: string;
}

export interface ProjectInspectResult {
  readonly status: "ok";
  readonly project_name: string;
  readonly format_version: number;
  readonly ir_schema_version: number;
  readonly node_count: number;
  readonly parameter_count: number;
  readonly blob_count: number;
  readonly pmi_count: number;
  readonly bom_count: number;
  readonly drawing_count: number;
  readonly cache_status: CacheStatus;
  readonly storage: ProjectStorageKind;
  readonly active_branch?: string;
  readonly external_ref_count?: number;
  readonly node_brep_cache_count?: number;
  readonly bundled_ref_count?: number;
  readonly has_cadpack?: boolean;
  readonly blobs_verified?: boolean;
}

export interface ProjectPatchResult {
  readonly status: "ok";
  readonly patch_id: string;
  readonly path: string;
  readonly branch_name?: string;
  readonly oplog_seq?: number;
  readonly outputs_merkle?: string;
}

export interface ProjectEmitKclResult {
  readonly ok: true;
  readonly kcl: string;
}

export type ProjectBranchPurpose = "main" | "user" | "ai_candidate";
export type ProjectBranchStatus = "active" | "promoted" | "discarded" | "archived";

export interface ProjectBranchInfo {
  readonly branch_name: string;
  readonly head_seq: number;
  readonly base_branch?: string | null;
  readonly base_seq?: number | null;
  readonly purpose: ProjectBranchPurpose;
  readonly status: ProjectBranchStatus;
  readonly gate_score?: number | null;
  readonly is_active: boolean;
  readonly created_at_ms: number;
}

export type ProjectBranchCommand =
  | { readonly action: "list" }
  | {
      readonly action: "create";
      readonly branchName: string;
      readonly fromBranch?: string;
      readonly purpose?: "user" | "ai_candidate";
    }
  | {
      readonly action: "checkout";
      readonly branchName: string;
    }
  | {
      readonly action: "rollback";
      readonly toSeq: number;
    };

export type ProjectBranchResult =
  | {
      readonly status: "ok";
      readonly action: "list";
      readonly active_branch: string;
      readonly branches: readonly ProjectBranchInfo[];
    }
  | {
      readonly status: "ok";
      readonly action: "create";
      readonly branch: ProjectBranchInfo;
      readonly active_branch?: string;
    }
  | {
      readonly status: "ok";
      readonly action: "checkout";
      readonly active_branch: string;
      readonly node_count: number;
    }
  | {
      readonly status: "ok";
      readonly action: "rollback";
      readonly active_branch: string;
      readonly head_seq: number;
      readonly node_count: number;
    };

export interface SliceContextRequest {
  readonly focusNodes: readonly string[];
  readonly upstreamDepth?: number;
  readonly downstreamDepth?: number;
  readonly ensureValidClosure?: boolean;
}

export interface ProjectContextSliceResult {
  readonly status: "ok";
  readonly focus_nodes: readonly string[];
  readonly kept_node_count: number;
  readonly total_node_count: number;
  readonly graph: CanonicalGraph;
  readonly kcl: string;
}

export interface ProjectVacuumResult {
  readonly status: "ok";
  readonly bytes_before: number;
  readonly bytes_after: number;
  readonly reclaimed_bytes: number;
  readonly removed_blobs: number;
  readonly removed_node_caches: number;
  readonly pruned_oplog_entries: number;
}

export interface ProjectPackAndGoResult {
  readonly status: "ok";
  readonly output_path: string;
  readonly bundled_count: number;
  readonly locked_hashes: Readonly<Record<string, string>>;
}

/** HTTP or response-shape failure from a project bridge call. */
export class ProjectClientError extends Error {
  readonly status: number;
  readonly payload: unknown;
  readonly traceId?: string;

  constructor(message: string, status: number, payload: unknown, traceId?: string) {
    super(message);
    this.name = "ProjectClientError";
    this.status = status;
    this.payload = payload;
    const resolved = traceId ?? readTraceId(undefined, payload, "");
    this.traceId = resolved || undefined;
  }
}

export function initAicadProject(
  relativeDir: string,
  name: string,
  options?: ProjectInitOptions,
): Promise<ProjectInitResult>;
export function initAicadProject(
  relativeDir: string,
  name: string,
  format: ProjectStorageKind | undefined,
  options?: ProjectClientOptions,
): Promise<ProjectInitResult>;
export function initAicadProject(
  relativeDir: string,
  name: string,
  formatOrOptions?: ProjectStorageKind | ProjectInitOptions,
  maybeOptions?: ProjectClientOptions,
): Promise<ProjectInitResult> {
  const format = typeof formatOrOptions === "string" ? formatOrOptions : formatOrOptions?.format;
  const options = typeof formatOrOptions === "string" || formatOrOptions === undefined
    ? maybeOptions
    : formatOrOptions;
  const body = format === undefined ? { relativeDir, name } : { relativeDir, name, format };
  return postProject("init", body, parseInitResult, options);
}

export function inspectAicadProject(
  relativePath: string,
  options?: ProjectClientOptions,
): Promise<ProjectInspectResult> {
  return postProject("inspect", { relativePath }, parseInspectResult, options);
}

export function applyAicadGraphPatch(
  relativePath: string,
  patch: GraphPatch,
  options?: ProjectApplyPatchOptions,
): Promise<ProjectPatchResult> {
  const body = options?.branch === undefined
    ? { relativePath, patch }
    : { relativePath, patch, branch: options.branch };
  return postProject("apply-patch", body, parsePatchResult, options);
}

export function emitAicadKcl(
  relativePath: string,
  options?: ProjectClientOptions,
): Promise<ProjectEmitKclResult> {
  return postProject("emit-kcl", { relativePath }, parseEmitResult, options);
}

export function branchAicadProject(
  relativePath: string,
  command: ProjectBranchCommand,
  options?: ProjectClientOptions,
): Promise<ProjectBranchResult> {
  const body = buildBranchBody(relativePath, command);
  return postProject(
    "branch",
    body,
    (payload, status) => parseBranchResult(payload, status, command.action),
    options,
  );
}

export function sliceAicadContext(
  relativePath: string,
  slice: SliceContextRequest,
  options?: ProjectClientOptions,
): Promise<ProjectContextSliceResult> {
  const body = {
    relativePath,
    focusNodes: slice.focusNodes,
    ...(slice.upstreamDepth !== undefined ? { upstreamDepth: slice.upstreamDepth } : {}),
    ...(slice.downstreamDepth !== undefined ? { downstreamDepth: slice.downstreamDepth } : {}),
    ...(slice.ensureValidClosure !== undefined
      ? { ensureValidClosure: slice.ensureValidClosure }
      : {}),
  };
  return postProject("slice-context", body, parseSliceContextResult, options);
}

export function vacuumAicadProject(
  relativePath: string,
  options?: ProjectVacuumOptions,
): Promise<ProjectVacuumResult> {
  const body = options?.keepRecentOplog === undefined
    ? { relativePath }
    : { relativePath, keepRecentOplog: options.keepRecentOplog };
  return postProject("vacuum", body, parseVacuumResult, options);
}

export function packAndGoAicadProject(
  relativePath: string,
  outputRelativePath: string,
  options?: ProjectClientOptions,
): Promise<ProjectPackAndGoResult> {
  return postProject(
    "pack-and-go",
    { relativePath, outputRelativePath },
    parsePackAndGoResult,
    options,
  );
}

function buildBranchBody(
  relativePath: string,
  command: ProjectBranchCommand,
): Record<string, unknown> {
  switch (command.action) {
    case "list":
      return { relativePath, action: "list" };
    case "create":
      return {
        relativePath,
        action: "create",
        branchName: command.branchName,
        ...(command.fromBranch !== undefined ? { fromBranch: command.fromBranch } : {}),
        ...(command.purpose !== undefined ? { purpose: command.purpose } : {}),
      };
    case "checkout":
      return {
        relativePath,
        action: "checkout",
        branchName: command.branchName,
      };
    case "rollback":
      return {
        relativePath,
        action: "rollback",
        toSeq: command.toSeq,
      };
  }
}

async function postProject<T extends object>(
  endpoint: string,
  body: unknown,
  parse: (payload: unknown, status: number) => T,
  options: ProjectClientOptions | undefined,
): Promise<T> {
  const fetchImpl = options?.fetchImpl
    ?? ((input: RequestInfo | URL, init?: RequestInit) => globalThis.fetch(input, init));
  const baseUrl = (options?.baseUrl ?? "").replace(/\/+$/, "");
  const traceId = createTraceId();
  let response: Response;
  try {
    response = await fetchImpl(`${baseUrl}/api/project/${endpoint}`, withTraceHeader({
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    }, traceId));
  } catch (err) {
    const message = err instanceof Error ? err.message : "Project request failed";
    throw new ProjectClientError(message, 0, null, traceId);
  }

  const payload = await readPayload(response);
  const finalTraceId = readTraceId(response.headers.get("x-trace-id"), payload, traceId);
  if (!response.ok) {
    throw new ProjectClientError(
      messageFromPayload(payload, response.status),
      response.status,
      payload,
      finalTraceId,
    );
  }
  try {
    const parsed = parse(payload, response.status);
    return { ...parsed, traceId: finalTraceId } as T;
  } catch (err) {
    if (err instanceof ProjectClientError) {
      throw new ProjectClientError(err.message, err.status, err.payload, err.traceId ?? finalTraceId);
    }
    throw err;
  }
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

function messageFromPayload(payload: unknown, status: number): string {
  if (isRecord(payload)) {
    if (typeof payload.message === "string" && payload.message.trim()) return payload.message;
    if (typeof payload.error === "string" && payload.error.trim()) return payload.error;
  }
  if (typeof payload === "string" && payload.trim()) return payload.trim().slice(0, 500);
  return `Project request failed with HTTP ${status}`;
}

function parseInitResult(payload: unknown, status: number): ProjectInitResult {
  const record = requireOkRecord(payload, "init", status);
  return {
    status: "ok",
    project_name: requireString(record, "project_name", status, payload),
    path: requireString(record, "path", status, payload),
  };
}

function parseInspectResult(payload: unknown, status: number): ProjectInspectResult {
  const record = requireOkRecord(payload, "inspect", status);
  const cacheStatus = requireString(record, "cache_status", status, payload);
  if (!isCacheStatus(cacheStatus)) {
    throw new ProjectClientError(`Unexpected cache_status '${cacheStatus}'`, status, payload);
  }
  const storage = requireString(record, "storage", status, payload);
  if (!isProjectStorageKind(storage)) {
    throw new ProjectClientError(`Unexpected storage '${storage}'`, status, payload);
  }
  const activeBranch = optionalString(record, "active_branch", status, payload);
  const externalRefCount = optionalNumber(record, "external_ref_count", status, payload);
  const nodeBrepCacheCount = optionalNumber(record, "node_brep_cache_count", status, payload);
  const bundledRefCount = optionalNumber(record, "bundled_ref_count", status, payload);
  const hasCadpack = optionalBoolean(record, "has_cadpack", status, payload);
  const blobsVerified = optionalBoolean(record, "blobs_verified", status, payload);

  return {
    status: "ok",
    project_name: requireString(record, "project_name", status, payload),
    format_version: requireNumber(record, "format_version", status, payload),
    ir_schema_version: requireNumber(record, "ir_schema_version", status, payload),
    node_count: requireNumber(record, "node_count", status, payload),
    parameter_count: requireNumber(record, "parameter_count", status, payload),
    blob_count: requireNumber(record, "blob_count", status, payload),
    pmi_count: requireNumber(record, "pmi_count", status, payload),
    bom_count: requireNumber(record, "bom_count", status, payload),
    drawing_count: requireNumber(record, "drawing_count", status, payload),
    cache_status: cacheStatus,
    storage,
    ...(activeBranch !== undefined ? { active_branch: activeBranch } : {}),
    ...(externalRefCount !== undefined ? { external_ref_count: externalRefCount } : {}),
    ...(nodeBrepCacheCount !== undefined ? { node_brep_cache_count: nodeBrepCacheCount } : {}),
    ...(bundledRefCount !== undefined ? { bundled_ref_count: bundledRefCount } : {}),
    ...(hasCadpack !== undefined ? { has_cadpack: hasCadpack } : {}),
    ...(blobsVerified !== undefined ? { blobs_verified: blobsVerified } : {}),
  };
}

function parsePatchResult(payload: unknown, status: number): ProjectPatchResult {
  const record = requireOkRecord(payload, "apply-patch", status);
  const branchName = optionalString(record, "branch_name", status, payload)
    ?? optionalString(record, "branch", status, payload);
  const oplogSeq = optionalNumber(record, "oplog_seq", status, payload)
    ?? optionalNumber(record, "seq", status, payload);
  const outputsMerkle = optionalString(record, "outputs_merkle", status, payload);
  return {
    status: "ok",
    patch_id: requireString(record, "patch_id", status, payload),
    path: requireString(record, "path", status, payload),
    ...(branchName !== undefined ? { branch_name: branchName } : {}),
    ...(oplogSeq !== undefined ? { oplog_seq: oplogSeq } : {}),
    ...(outputsMerkle !== undefined ? { outputs_merkle: outputsMerkle } : {}),
  };
}

function parseEmitResult(payload: unknown, status: number): ProjectEmitKclResult {
  if (!isRecord(payload) || payload.ok !== true || typeof payload.kcl !== "string") {
    throw new ProjectClientError("Project emit-kcl response is missing kcl", status, payload);
  }
  return { ok: true, kcl: payload.kcl };
}

function parseBranchInfo(raw: unknown, status: number, payload: unknown): ProjectBranchInfo {
  if (!isRecord(raw)) {
    throw new ProjectClientError("Project branch entry is not an object", status, payload);
  }
  const purpose = requireString(raw, "purpose", status, payload);
  if (purpose !== "main" && purpose !== "user" && purpose !== "ai_candidate") {
    throw new ProjectClientError(`Unexpected branch purpose '${purpose}'`, status, payload);
  }
  const branchStatus = requireString(raw, "status", status, payload);
  if (
    branchStatus !== "active"
    && branchStatus !== "promoted"
    && branchStatus !== "discarded"
    && branchStatus !== "archived"
  ) {
    throw new ProjectClientError(`Unexpected branch status '${branchStatus}'`, status, payload);
  }
  const isActive = raw.is_active;
  if (typeof isActive !== "boolean") {
    throw new ProjectClientError("Project branch entry is missing is_active", status, payload);
  }
  const baseBranch = raw.base_branch === undefined || raw.base_branch === null
    ? raw.base_branch
    : requireString(raw, "base_branch", status, payload);
  const baseSeq = raw.base_seq === undefined || raw.base_seq === null
    ? raw.base_seq
    : requireNumber(raw, "base_seq", status, payload);
  const gateScore = raw.gate_score === undefined || raw.gate_score === null
    ? raw.gate_score
    : requireNumber(raw, "gate_score", status, payload);

  return {
    branch_name: requireString(raw, "branch_name", status, payload),
    head_seq: requireNumber(raw, "head_seq", status, payload),
    ...(baseBranch !== undefined ? { base_branch: baseBranch } : {}),
    ...(baseSeq !== undefined ? { base_seq: baseSeq } : {}),
    purpose,
    status: branchStatus,
    ...(gateScore !== undefined ? { gate_score: gateScore } : {}),
    is_active: isActive,
    created_at_ms: requireNumber(raw, "created_at_ms", status, payload),
  };
}

function parseBranchResult(
  payload: unknown,
  status: number,
  action: ProjectBranchCommand["action"],
): ProjectBranchResult {
  const record = requireOkRecord(payload, "branch", status);
  switch (action) {
    case "list": {
      if (!Array.isArray(record.branches)) {
        throw new ProjectClientError("Project branch list response is missing branches", status, payload);
      }
      return {
        status: "ok",
        action: "list",
        active_branch: requireString(record, "active_branch", status, payload),
        branches: record.branches.map((item) => parseBranchInfo(item, status, payload)),
      };
    }
    case "create": {
      const activeBranch = optionalString(record, "active_branch", status, payload);
      return {
        status: "ok",
        action: "create",
        branch: parseBranchInfo(record.branch, status, payload),
        ...(activeBranch !== undefined ? { active_branch: activeBranch } : {}),
      };
    }
    case "checkout":
      return {
        status: "ok",
        action: "checkout",
        active_branch: requireString(record, "active_branch", status, payload),
        node_count: requireNumber(record, "node_count", status, payload),
      };
    case "rollback":
      return {
        status: "ok",
        action: "rollback",
        active_branch: requireString(record, "active_branch", status, payload),
        head_seq: requireNumber(record, "head_seq", status, payload),
        node_count: requireNumber(record, "node_count", status, payload),
      };
  }
}

function parseSliceContextResult(payload: unknown, status: number): ProjectContextSliceResult {
  const record = requireOkRecord(payload, "slice-context", status);
  if (!Array.isArray(record.focus_nodes) || !record.focus_nodes.every((item) => typeof item === "string")) {
    throw new ProjectClientError("Project slice-context response is missing focus_nodes", status, payload);
  }
  if (!isRecord(record.graph) || typeof record.graph.graph_id !== "string") {
    throw new ProjectClientError("Project slice-context response is missing graph", status, payload);
  }
  return {
    status: "ok",
    focus_nodes: [...record.focus_nodes],
    kept_node_count: requireNumber(record, "kept_node_count", status, payload),
    total_node_count: requireNumber(record, "total_node_count", status, payload),
    graph: record.graph as unknown as CanonicalGraph,
    kcl: requireString(record, "kcl", status, payload),
  };
}

function parseVacuumResult(payload: unknown, status: number): ProjectVacuumResult {
  if (!isRecord(payload) || (payload.status !== undefined && payload.status !== "ok")) {
    throw new ProjectClientError("Project vacuum response was not successful", status, payload);
  }
  return {
    status: "ok",
    bytes_before: requireNumber(payload, "bytes_before", status, payload),
    bytes_after: requireNumber(payload, "bytes_after", status, payload),
    reclaimed_bytes: requireNumber(payload, "reclaimed_bytes", status, payload),
    removed_blobs: requireNumber(payload, "removed_blobs", status, payload),
    removed_node_caches: requireNumber(payload, "removed_node_caches", status, payload),
    pruned_oplog_entries: requireNumber(payload, "pruned_oplog_entries", status, payload),
  };
}

function parsePackAndGoResult(payload: unknown, status: number): ProjectPackAndGoResult {
  const record = requireOkRecord(payload, "pack-and-go", status);
  if (!isRecord(record.locked_hashes)) {
    throw new ProjectClientError("Project pack-and-go response is missing locked_hashes", status, payload);
  }
  const lockedHashes: Record<string, string> = {};
  for (const [key, val] of Object.entries(record.locked_hashes)) {
    if (typeof val !== "string") {
      throw new ProjectClientError(`Invalid hash for external ref '${key}'`, status, payload);
    }
    lockedHashes[key] = val;
  }
  return {
    status: "ok",
    output_path: requireString(record, "output_path", status, payload),
    bundled_count: requireNumber(record, "bundled_count", status, payload),
    locked_hashes: lockedHashes,
  };
}

function requireOkRecord(
  payload: unknown,
  command: string,
  status: number,
): Record<string, unknown> {
  if (!isRecord(payload) || payload.status !== "ok") {
    throw new ProjectClientError(`Project ${command} response was not successful`, status, payload);
  }
  return payload;
}

function requireString(
  record: Record<string, unknown>,
  key: string,
  status: number,
  payload: unknown,
): string {
  const value = record[key];
  if (typeof value !== "string") {
    throw new ProjectClientError(`Project response is missing ${key}`, status, payload);
  }
  return value;
}

function optionalString(
  record: Record<string, unknown>,
  key: string,
  status: number,
  payload: unknown,
): string | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== "string") {
    throw new ProjectClientError(`Project response field ${key} must be a string`, status, payload);
  }
  return value;
}

function requireNumber(
  record: Record<string, unknown>,
  key: string,
  status: number,
  payload: unknown,
): number {
  const value = record[key];
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new ProjectClientError(`Project response is missing ${key}`, status, payload);
  }
  return value;
}

function optionalNumber(
  record: Record<string, unknown>,
  key: string,
  status: number,
  payload: unknown,
): number | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new ProjectClientError(`Project response field ${key} must be a number`, status, payload);
  }
  return value;
}

function optionalBoolean(
  record: Record<string, unknown>,
  key: string,
  status: number,
  payload: unknown,
): boolean | undefined {
  const value = record[key];
  if (value === undefined) return undefined;
  if (typeof value !== "boolean") {
    throw new ProjectClientError(`Project response field ${key} must be a boolean`, status, payload);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCacheStatus(value: string): value is CacheStatus {
  switch (value) {
    case "fresh_exact_match":
    case "kernel_version_drift":
    case "graph_modified":
    case "missing":
      return true;
    default:
      return false;
  }
}

function isProjectStorageKind(value: string): value is ProjectStorageKind {
  switch (value) {
    case "directory":
    case "sharded_directory":
    case "sqlite":
      return true;
    default:
      return false;
  }
}
