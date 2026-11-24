import {
  Cms2IpcUnavailableError,
  StepExportDiscardError,
  ThreeMfExportDiscardError,
  createDesktopIpcClient,
  isTauriRuntime,
  type AiAgentProgressEvent,
  type AiAgentRunRequest,
  type AiAgentRunResult,
  type AppUpdateCheckResult,
  type AppUpdateInstallResult,
  type AppUpdaterInfo,
  type AppUpdaterProgressEvent,
  type DesktopIpcClient,
  type InstallAppUpdateRequest,
} from "../../../desktop/src/ipcBridge.ts";
import {
  compactSceneToSceneMesh,
  decodeCms2Envelope,
  type Cms2ArtifactMetadata,
  type DecodedCompactScene,
} from "./cms2Decoder.ts";

export { StepExportDiscardError, ThreeMfExportDiscardError };

export function classifyNativeAgentError(error: unknown): { readonly error_code: string; readonly error: string } {
  const message = error instanceof Error && error.message.trim() ? error.message : String(error);
  const structured = message.match(/^AICAD_ERROR:([A-Za-z0-9_]+):(.*)$/s);
  if (structured) {
    return { error_code: structured[1], error: structured[2].trim() };
  }
  if (/event\.listen not allowed|allow-listen|core:event:allow-listen/i.test(message)) {
    return {
      error_code: "event_listen_denied",
      error: "桌面应用未授予事件监听权限，无法订阅 AI 进度。请检查应用权限配置后重试。",
    };
  }
  return { error_code: "ipc_error", error: message };
}


import { evaluateDemoKclModel } from "../data/demoWorkbenchState.ts";
import { browserStepBlockedMessage, previewBrowserStep } from "../preview/stepPreview.ts";
import { DIAGNOSTIC_CODES } from "../types/schemaConstants.ts";
import type {
  AiConfigStatus,
  AiProfileDraftInput,
  AiProfilesResponse,
  AiTestConnectionPayload,
  AiTestConnectionResult,
  DiagnosticsReport,
  SceneMeshPayload,
  WorkspaceAsset,
} from "../types/scene.ts";
import { generateFallbackDrawingSvg, generateFallbackDxfFromMesh } from "../utils/studioHelpers.ts";
import {
  TRACE_HEADER,
  TRANSPORT_IPC_FAILED,
  createTraceId,
  normalizeTraceId,
  readTraceId,
  traceIdFrom,
  transportCodeFrom,
  transportMessageFrom,
  withTraceHeader,
  type TransportError,
} from "../utils/trace.ts";

export type {
  AppUpdateCheckResult,
  AppUpdateInstallResult,
  AppUpdaterInfo,
  AppUpdaterProgressEvent,
  InstallAppUpdateRequest,
  WorkspaceAsset,
  TransportError,
};

export interface WorkbenchDocument {
  readonly fileName: string;
  readonly source: string;
}

export interface WorkbenchModel {
  readonly diagnostics: DiagnosticsReport;
  readonly sceneMesh: SceneMeshPayload;
  /** Optional zero-copy decoded CMS2 compact scene when transported via CMS2. */
  readonly compactScene?: DecodedCompactScene;
  readonly origin: "host" | "demo" | "browser-step";
  /** Final id from the response header or body, otherwise the id this call sent. */
  readonly traceId?: string;
  /** Network, IPC, or host crash. Never a source-span diagnostic. */
  readonly transportError?: TransportError;
}

export type ExportKind = "step" | "dxf" | "drawing" | "3mf" | "mesh";
type HostExportKind = Exclude<ExportKind, "mesh">;

export interface WorkbenchExport {
  readonly message: string;
  readonly download?: {
    readonly filename: string;
    readonly content: string | Uint8Array<ArrayBuffer>;
    readonly mimeType: string;
  };
  readonly traceId?: string;
}

export interface PreviewBranchRequest {
  readonly projectPath: string;
  readonly branchName: string;
}

export interface PreviewBranchActionResult {
  readonly ok: boolean;
  readonly action: "diff" | "commit_preview" | "discard_preview";
  readonly branch: string;
  readonly diff?: unknown;
  readonly revision?: number;
  readonly activeBranch?: string;
  readonly kcl?: string;
  readonly traceId?: string;
}

export interface WorkbenchBackend {
  readonly kind: "tauri" | "http";
  compile(document: WorkbenchDocument, signal: AbortSignal): Promise<WorkbenchModel>;
  listAssets(): Promise<readonly WorkspaceAsset[]>;
  uploadAsset(name: string, bytes: ArrayBuffer): Promise<WorkspaceAsset>;
  inspectAsset(asset: WorkspaceAsset): Promise<unknown>;
  exportArtifact(kind: HostExportKind, document: WorkbenchDocument, model: WorkbenchModel, signal: AbortSignal, options?: { allowNonAuthoritativePreview?: boolean }): Promise<WorkbenchExport>;
  getAiConfig(signal: AbortSignal): Promise<AiConfigStatus | null>;
  getAiProfiles?(signal: AbortSignal): Promise<AiProfilesResponse | null>;
  saveAiProfiles?(
    payload: { readonly activeProfileId: string | null; readonly profiles: readonly AiProfileDraftInput[] },
    signal?: AbortSignal,
  ): Promise<AiProfilesResponse | null>;
  testAiConnection?(payload: AiTestConnectionPayload, signal?: AbortSignal): Promise<AiTestConnectionResult>;
  requestAi(body: unknown, signal: AbortSignal): Promise<Response | null>;
  diffPreviewBranch?(request: PreviewBranchRequest, signal?: AbortSignal): Promise<PreviewBranchActionResult>;
  commitPreviewBranch?(request: PreviewBranchRequest, signal?: AbortSignal): Promise<PreviewBranchActionResult>;
  discardPreviewBranch?(request: PreviewBranchRequest, signal?: AbortSignal): Promise<PreviewBranchActionResult>;
  importAicadDocument?(fileName: string, contentsBase64: string): Promise<{ projectPath: string; projectName: string; source: string; history?: { canRollback: boolean; oplogLen: number; headSeq: number | null; previousSeq: number | null } }>;
  saveAicadDocument?(request: { projectPath: string; projectName: string; source: string }): Promise<{ projectPath: string; projectName: string; history?: { canRollback: boolean; oplogLen: number; headSeq: number | null; previousSeq: number | null } }>;
  undoAicadContainer?(request: { projectPath: string }): Promise<{ projectPath: string; projectName: string; source: string; history: { canRollback: boolean; oplogLen: number; headSeq: number | null; previousSeq: number | null } }>;
  getAppUpdaterInfo?(): Promise<AppUpdaterInfo>;
  checkAppUpdate?(): Promise<AppUpdateCheckResult>;
  installAppUpdate?(
    request?: InstallAppUpdateRequest,
    onProgress?: (event: AppUpdaterProgressEvent) => void,
  ): Promise<AppUpdateInstallResult>;
}

export class WorkbenchUnavailableError extends Error {
  readonly traceId: string;

  constructor(message: string, traceId = createTraceId()) {
    super(message);
    this.name = "WorkbenchUnavailableError";
    this.traceId = traceId;
  }
}

/** Host call failed. The UI shows this beside source diagnostics, not as a fake span. */
export class WorkbenchTransportError extends Error {
  readonly code: string;
  readonly traceId: string;
  readonly operation?: string;

  constructor(error: TransportError) {
    super(error.message);
    this.name = "WorkbenchTransportError";
    this.code = error.code || TRANSPORT_IPC_FAILED;
    this.traceId = error.traceId;
    this.operation = error.operation;
  }

  toInfo(): TransportError {
    return {
      code: this.code,
      message: this.message,
      traceId: this.traceId,
      ...(this.operation ? { operation: this.operation } : {}),
    };
  }
}

const OFFLINE_MESSAGE = "工作台服务未连接。";

export const EMPTY_SCENE: SceneMeshPayload = {
  format_version: "0.1", part_name: "", units: "mm", faces: [], edges: [], edit_handles: [],
};

/** Empty report. Source diagnostics come from the compiler, not from a transport failure. */
export function compileErrorReport(file: string): DiagnosticsReport {
  return {
    status: "ok",
    file,
    diagnostics: [],
    metrics: { watertight: false, volume_mm3: 0, bbox_mm: { min: [0, 0, 0], max: [0, 0, 0] }, face_count: 0, edge_count: 0 },
  };
}

export function transportErrorFrom(error: unknown, traceId: string, operation: string): TransportError {
  if (error instanceof WorkbenchTransportError) {
    return {
      code: error.code || TRANSPORT_IPC_FAILED,
      message: error.message,
      traceId: error.traceId || traceId,
      operation: error.operation ?? operation,
    };
  }
  if (error instanceof WorkbenchUnavailableError) {
    return {
      code: TRANSPORT_IPC_FAILED,
      message: error.message,
      traceId: error.traceId || traceId,
      operation,
    };
  }
  const message = error instanceof Error && error.message.trim() ? error.message : "工作台服务请求失败。";
  return {
    code: TRANSPORT_IPC_FAILED,
    message,
    traceId: traceIdFrom(error) ?? traceId,
    operation,
  };
}

export function transportFailureModel(file: string, error: TransportError): WorkbenchModel {
  return {
    origin: "host",
    sceneMesh: EMPTY_SCENE,
    diagnostics: compileErrorReport(file),
    traceId: error.traceId,
    transportError: error,
  };
}

interface HttpBuild {
  readonly ok?: boolean;
  readonly diagnostics?: DiagnosticsReport;
  readonly sceneMesh?: SceneMeshPayload | null;
  readonly stepContent?: string;
  readonly dxfContent?: string;
  readonly dxfIsCutProfile?: boolean;
  readonly drawingSvg?: string;
  readonly threemfBase64?: string;
  readonly trace_id?: string;
  readonly traceId?: string;
}

function isValidSceneMesh(mesh: SceneMeshPayload | null | undefined): mesh is SceneMeshPayload {
  return Boolean(
    mesh &&
    Array.isArray(mesh.faces) &&
    Array.isArray(mesh.edges) &&
    Array.isArray(mesh.edit_handles),
  );
}

function hostModel(diagnostics: DiagnosticsReport, sceneMesh: SceneMeshPayload | null | undefined, traceId: string): WorkbenchModel {
  if (!Array.isArray(diagnostics?.diagnostics) || !diagnostics.metrics?.bbox_mm) {
    throw new WorkbenchTransportError({
      code: TRANSPORT_IPC_FAILED,
      message: "编译服务未返回有效诊断。",
      traceId,
      operation: "compile",
    });
  }
  if (diagnostics.status === "error") {
    // Keep a rolled-back solid when the compiler still sent its mesh. An error
    // with no faces stays an empty viewport instead of a demo stand-in.
    if (isValidSceneMesh(sceneMesh) && sceneMesh.faces.length > 0) {
      return { diagnostics, sceneMesh, origin: "host", traceId };
    }
    return { diagnostics, sceneMesh: EMPTY_SCENE, origin: "host", traceId };
  }
  if (diagnostics.status !== "ok" || !isValidSceneMesh(sceneMesh)) {
    throw new WorkbenchTransportError({
      code: TRANSPORT_IPC_FAILED,
      message: "编译服务未返回有效网格。",
      traceId,
      operation: "compile",
    });
  }
  return { diagnostics, sceneMesh, origin: "host", traceId };
}

/** Bytes for one requested format. A DXF rejection does not discard STEP, 3MF, or SVG that this build already wrote. */
export function selectHostExport(
  kind: HostExportKind,
  build: HttpBuild,
): { content: string; message: string } | { error: string } {
  const failure = build.diagnostics?.diagnostics?.find((item) => item.severity === "error")?.message
    ?? "导出失败。";
  if (kind === "dxf") {
    if (!build.dxfContent) return { error: failure };
    return {
      content: build.dxfContent,
      message: build.dxfIsCutProfile ? "已下载 XY 投影 DXF。" : "已下载网格边线 DXF，仅供预览。",
    };
  }
  if (kind === "3mf") {
    if (!build.threemfBase64) return { error: failure };
    return { content: build.threemfBase64, message: "已下载 3MF 文件。" };
  }
  if (kind === "step") {
    if (!build.stepContent) return { error: failure };
    return { content: build.stepContent, message: "已下载 STEP 文件。" };
  }
  if (!build.drawingSvg) return { error: failure };
  return { content: build.drawingSvg, message: "已下载工程图 SVG。" };
}

function httpModel(build: HttpBuild, traceId: string): WorkbenchModel {
  // `traceId` is already header, then body, then the id this call sent.
  const finalTraceId = normalizeTraceId(traceId) ?? readTraceId(undefined, build, traceId);
  if (!build.diagnostics || (build.ok === false && build.diagnostics.status === "ok")) {
    throw new WorkbenchTransportError({
      code: transportCodeFrom(build) ?? TRANSPORT_IPC_FAILED,
      message: "编译服务未返回有效诊断。",
      traceId: finalTraceId,
      operation: "compile",
    });
  }
  return hostModel(build.diagnostics, build.sceneMesh, finalTraceId);
}

export function aiPreviewFromResponse(backend: WorkbenchBackend, payload: Record<string, unknown>): WorkbenchModel | undefined {
  // The HTTP agent uses the same workspace as HTTP compile; Tauri builds against its own workspace.
  if (backend.kind !== "http") return undefined;
  const traceId = readTraceId(undefined, payload, createTraceId());
  try {
    return httpModel(payload as HttpBuild, traceId);
  } catch (error) {
    if (payload.ok !== false) return undefined;
    const info = error instanceof WorkbenchTransportError
      ? { ...error.toInfo(), message: "AI 服务端编译失败，未返回有效诊断。", operation: "ai-preview" }
      : transportErrorFrom(error, traceId, "ai-preview");
    return transportFailureModel("ai_generated.kcl", {
      ...info,
      message: "AI 服务端编译失败，未返回有效诊断。",
      operation: "ai-preview",
    });
  }
}

export async function compileWorkbenchPreview(backend: WorkbenchBackend, document: WorkbenchDocument, signal: AbortSignal): Promise<WorkbenchModel> {
  signal.throwIfAborted();
  try {
    const model = await backend.compile(document, signal);
    signal.throwIfAborted();
    return model;
  } catch (error) {
    signal.throwIfAborted();
    if (backend.kind === "http" && error instanceof WorkbenchUnavailableError) {
      const blocked = browserStepBlockedMessage(document.source);
      if (blocked) throw new Error(blocked);
      const step = await previewBrowserStep(document.source, signal);
      signal.throwIfAborted();
      if (step) return { ...step, origin: "browser-step", traceId: error.traceId };
      if (/\bimport\s*::\s*(?:mesh|step)\s*\(/.test(document.source)) throw error;
      return { ...evaluateDemoKclModel(document.source), origin: "demo", traceId: error.traceId };
    }
    if (error instanceof WorkbenchTransportError || backend.kind === "tauri") {
      return transportFailureModel(document.fileName, transportErrorFrom(error, traceIdFrom(error) ?? createTraceId(), "compile"));
    }
    throw error;
  }
}

const EXPORT_FILES = {
  step: { suffix: ".step", mimeType: "application/step" },
  dxf: { suffix: ".dxf", mimeType: "application/dxf" },
  drawing: { suffix: "_drawing.svg", mimeType: "image/svg+xml" },
  "3mf": { suffix: ".3mf", mimeType: "model/3mf" },
  mesh: { suffix: ".mesh.json", mimeType: "application/json" },
} as const;

function decodeBase64Bytes(base64: string): Uint8Array<ArrayBuffer> {
  const clean = base64.replace(/\s+/g, "");
  if (typeof atob === "function") {
    const binary = atob(clean);
    const bytes = new Uint8Array(new ArrayBuffer(binary.length));
    for (let i = 0; i < binary.length; i += 1) {
      bytes[i] = binary.charCodeAt(i);
    }
    return bytes;
  }
  const buf = Buffer.from(clean, "base64");
  const bytes = new Uint8Array(new ArrayBuffer(buf.byteLength));
  bytes.set(buf);
  return bytes;
}

function downloadArtifact(
  kind: ExportKind,
  partName: string,
  content: string | Uint8Array<ArrayBuffer>,
  message: string,
  traceId?: string,
): WorkbenchExport {
  const { suffix, mimeType } = EXPORT_FILES[kind];
  return {
    message,
    download: { filename: `${partName}${suffix}`, content, mimeType },
    ...(traceId ? { traceId } : {}),
  };
}

function previewExport(kind: "dxf" | "drawing", { sceneMesh, diagnostics, traceId }: WorkbenchModel): WorkbenchExport {
  const partName = sceneMesh.part_name || "model";
  return kind === "dxf"
    ? downloadArtifact(kind, partName, generateFallbackDxfFromMesh(sceneMesh), "已下载网格边线 DXF，仅供预览。", traceId)
    : downloadArtifact(kind, partName, generateFallbackDrawingSvg(sceneMesh, diagnostics.metrics, {}), "已下载预览工程图 SVG。", traceId);
}

export async function exportWorkbenchModel(backend: WorkbenchBackend, kind: ExportKind, document: WorkbenchDocument, model: WorkbenchModel, signal: AbortSignal, options?: { allowNonAuthoritativePreview?: boolean }): Promise<WorkbenchExport> {
  signal.throwIfAborted();
  const { diagnostics, sceneMesh, origin } = model;
  if (diagnostics.status !== "ok" || model.transportError) throw new Error("请先修复编译错误。");
  if (kind === "mesh") {
    const content = JSON.stringify(sceneMesh, (_key, value) =>
      value instanceof Float64Array || value instanceof Uint32Array ? Array.from(value) : value, 2);
    return downloadArtifact(kind, sceneMesh.part_name || "model", content, "已下载 Scene-Mesh JSON。", model.traceId);
  }
  if (origin === "host") return backend.exportArtifact(kind, document, model, signal, options);
  if (kind === "step") throw new Error("STEP 导出需要已编译模型及支持该格式的工作台服务。");
  if (kind === "3mf") throw new Error("3MF 导出需要已编译模型及支持该格式的工作台服务。");
  return previewExport(kind, model);
}

function jsonPost(body: unknown, signal?: AbortSignal): RequestInit {
  return { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body), signal };
}

function responseWithTrace(response: Response, traceId: string): Response {
  if (normalizeTraceId(response.headers.get(TRACE_HEADER))) return response;
  const headers = new Headers(response.headers);
  headers.set(TRACE_HEADER, traceId);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

export function createHttpWorkbenchBackend(fetcher: typeof fetch = fetch): WorkbenchBackend {
  async function request(path: string, init: RequestInit | undefined, operation: string): Promise<{ response: Response; traceId: string }> {
    const traceId = createTraceId();
    try {
      const response = await fetcher(path, withTraceHeader(init, traceId));
      return { response, traceId: readTraceId(response.headers.get(TRACE_HEADER), undefined, traceId) };
    } catch (error) {
      init?.signal?.throwIfAborted();
      if (error instanceof TypeError) throw new WorkbenchUnavailableError(OFFLINE_MESSAGE, traceId);
      throw new WorkbenchTransportError({
        code: TRANSPORT_IPC_FAILED,
        message: error instanceof Error && error.message.trim() ? error.message : OFFLINE_MESSAGE,
        traceId,
        operation,
      });
    }
  }

  async function json<T>(path: string, init: RequestInit | undefined, operation: string): Promise<{ value: T; traceId: string }> {
    const { response, traceId } = await request(path, init, operation);
    const type = response.headers.get("content-type") ?? "";
    const body = await response.json().catch(() => null);
    const finalTraceId = readTraceId(response.headers.get(TRACE_HEADER), body, traceId);
    init?.signal?.throwIfAborted();
    // Static React hosts can answer with a 404/405 or the SPA HTML instead of an API response.
    if ((!type.includes("json") && [404, 405].includes(response.status)) ||
        (response.ok && type.includes("text/html"))) {
      throw new WorkbenchUnavailableError(OFFLINE_MESSAGE, finalTraceId);
    }
    if (!response.ok || !body) {
      throw new WorkbenchTransportError({
        code: transportCodeFrom(body) ?? TRANSPORT_IPC_FAILED,
        message: transportMessageFrom(body, `工作台服务请求失败 (HTTP ${response.status})`),
        traceId: finalTraceId,
        operation,
      });
    }
    return { value: body as T, traceId: finalTraceId };
  }

  function build(
    document: WorkbenchDocument,
    previewOnly: boolean,
    signal: AbortSignal,
    operation: string,
    exportKind?: HostExportKind,
  ): Promise<{ value: HttpBuild; traceId: string }> {
    const payload = exportKind
      ? { file: document.fileName, source: document.source, previewOnly, exportKind }
      : { file: document.fileName, source: document.source, previewOnly };
    return json<HttpBuild>("/api/compile", jsonPost(payload, signal), operation);
  }

  return {
    kind: "http",
    async compile(document, signal) {
      const built = await build(document, true, signal, "compile");
      return httpModel(built.value, built.traceId);
    },
    async listAssets() {
      return (await json<{ assets: WorkspaceAsset[] }>("/api/assets", undefined, "list-assets")).value.assets;
    },
    async uploadAsset(name, bytes) {
      return (await json<{ asset: WorkspaceAsset }>(`/api/assets?name=${encodeURIComponent(name)}`, {
        method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: bytes,
      }, "upload-asset")).value.asset;
    },
    async inspectAsset(asset) {
      return (await json("/api/inspect-asset", jsonPost({ path: asset.path }), "inspect-asset")).value;
    },
    async exportArtifact(kind, document, model, signal) {
      const built = await build(document, false, signal, "export", kind);
      const traceId = normalizeTraceId(built.traceId) ?? built.traceId;
      const selected = selectHostExport(kind, built.value);
      if ("error" in selected) {
        throw new Error(selected.error);
      }
      const content = kind === "3mf" ? decodeBase64Bytes(selected.content) : selected.content;
      return downloadArtifact(kind, model.sceneMesh.part_name || "model", content, selected.message, traceId);
    },
    async getAiConfig(signal) {
      const { value: config } = await json<Partial<AiConfigStatus>>("/api/ai-config", { signal }, "ai-config");
      return typeof config.default_provider === "string" && typeof config.has_openai_key === "boolean" &&
        typeof config.has_gemini_key === "boolean" ? config as AiConfigStatus : null;
    },
    async getAiProfiles(signal) {
      try {
        const { value } = await json<Partial<AiProfilesResponse>>("/api/ai-profiles", { signal }, "ai-profiles");
        if (!Array.isArray(value?.profiles)) return null;
        return {
          storage: value.storage === "server-file" ? "server-file" : "browser-local",
          activeProfileId: typeof value.activeProfileId === "string" ? value.activeProfileId : null,
          profiles: value.profiles,
          ...(value.envStatus ? { envStatus: value.envStatus } : {}),
        };
      } catch (error) {
        if (error instanceof WorkbenchUnavailableError) return null;
        throw error;
      }
    },
    async saveAiProfiles(payload, signal) {
      try {
        const { value } = await json<Partial<AiProfilesResponse>>(
          "/api/ai-profiles",
          {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(payload),
            signal,
          },
          "ai-profiles-save",
        );
        if (!Array.isArray(value?.profiles)) return null;
        return {
          storage: value.storage === "server-file" ? "server-file" : "browser-local",
          activeProfileId: typeof value.activeProfileId === "string" ? value.activeProfileId : null,
          profiles: value.profiles,
          ...(value.envStatus ? { envStatus: value.envStatus } : {}),
        };
      } catch (error) {
        if (error instanceof WorkbenchUnavailableError) return null;
        throw error;
      }
    },
    async testAiConnection(payload, signal) {
      const { value } = await json<AiTestConnectionResult>("/api/ai-test", jsonPost(payload, signal), "ai-test");
      return value;
    },
    async requestAi(body, signal) {
      const missing = (response: Response) => [404, 405].includes(response.status) ||
        (response.ok && response.headers.get("content-type")?.includes("text/html"));
      const post = async (path: string) => {
        const { response, traceId } = await request(path, jsonPost(body, signal), "ai-request");
        return responseWithTrace(response, traceId);
      };
      try {
        const stream = await post("/api/agent-generate-stream");
        if (!missing(stream)) return stream;
        void stream.body?.cancel().catch(() => {});
        const sync = await post("/api/agent-generate");
        if (!missing(sync)) return sync;
        void sync.body?.cancel().catch(() => {});
        return null;
      } catch (error) {
        if (error instanceof WorkbenchUnavailableError) return null;
        throw error;
      }
    },
    async diffPreviewBranch(req, signal) {
      const { value, traceId } = await json<Record<string, unknown>>(
        "/api/project/preview/diff",
        jsonPost({ relativePath: req.projectPath, branchName: req.branchName }, signal),
        "preview-diff",
      );
      return normalizePreviewBranchResult("diff", req.branchName, value, traceId);
    },
    async commitPreviewBranch(req, signal) {
      const { value, traceId } = await json<Record<string, unknown>>(
        "/api/project/preview/commit",
        jsonPost({ relativePath: req.projectPath, branchName: req.branchName }, signal),
        "preview-commit",
      );
      return normalizePreviewBranchResult("commit_preview", req.branchName, value, traceId);
    },
    async discardPreviewBranch(req, signal) {
      const { value, traceId } = await json<Record<string, unknown>>(
        "/api/project/preview/discard",
        jsonPost({ relativePath: req.projectPath, branchName: req.branchName }, signal),
        "preview-discard",
      );
      return normalizePreviewBranchResult("discard_preview", req.branchName, value, traceId);
    },
  };
}

function normalizePreviewBranchResult(
  action: PreviewBranchActionResult["action"],
  branchName: string,
  value: Record<string, unknown>,
  traceId: string,
): PreviewBranchActionResult {
  const revision = typeof value.revision === "number" && Number.isSafeInteger(value.revision)
    ? value.revision
    : undefined;
  const activeBranch = typeof value.active_branch === "string"
    ? value.active_branch
    : typeof value.activeBranch === "string"
      ? value.activeBranch
      : undefined;
  const kcl = typeof value.kcl === "string" ? value.kcl : undefined;
  return {
    ok: value.ok !== false,
    action,
    branch: typeof value.branch === "string" && value.branch.trim() ? value.branch.trim() : branchName,
    ...(value.diff !== undefined ? { diff: value.diff } : {}),
    ...(revision !== undefined ? { revision } : {}),
    ...(activeBranch !== undefined ? { activeBranch } : {}),
    ...(kcl !== undefined ? { kcl } : {}),
    traceId,
  };
}

function nativeCompileRequest(document: WorkbenchDocument, traceId: string) {
  return { file_name: document.fileName, source: document.source, trace_id: traceId };
}

function rethrowIpc(error: unknown, traceId: string, operation: string): never {
  if (error instanceof WorkbenchTransportError) throw error;
  throw new WorkbenchTransportError(transportErrorFrom(error, traceId, operation));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readStringArray(value: unknown): readonly string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const items = value
    .filter((entry): entry is string => typeof entry === "string" && entry.trim().length > 0)
    .map((entry) => entry.trim());
  return items.length > 0 ? items : undefined;
}

function toNativeAiRunRequest(body: unknown, traceId: string): AiAgentRunRequest {
  const raw = isRecord(body) ? body : {};
  const prompt = typeof raw.prompt === "string" ? raw.prompt : "";
  const mode = raw.mode === "edit" ? "edit" : "generate";
  const manufacturingMethod =
    typeof raw.manufacturing_method === "string"
      ? raw.manufacturing_method
      : typeof raw.manufacturingMethod === "string"
        ? raw.manufacturingMethod
        : undefined;
  const existingSource =
    typeof raw.existing_source === "string"
      ? raw.existing_source
      : typeof raw.existingSource === "string"
        ? raw.existingSource
        : undefined;
  const workspaceRoot =
    typeof raw.workspace_root === "string"
      ? raw.workspace_root
      : typeof raw.workspaceRoot === "string"
        ? raw.workspaceRoot
        : undefined;
  const projectPath =
    typeof raw.project_path === "string"
      ? raw.project_path
      : typeof raw.projectPath === "string"
        ? raw.projectPath
        : undefined;
  const selectedTopoRefs = readStringArray(raw.selected_topo_refs ?? raw.selectedTopoRefs);
  const selectedNodeIds = readStringArray(raw.selected_node_ids ?? raw.selectedNodeIds);
  const llm = isRecord(raw.llm) ? (raw.llm as AiAgentRunRequest["llm"]) : undefined;

  return {
    trace_id: traceId,
    prompt,
    ...(typeof raw.material === "string" ? { material: raw.material } : {}),
    ...(manufacturingMethod ? { manufacturing_method: manufacturingMethod } : {}),
    mode,
    ...(existingSource ? { existing_source: existingSource } : {}),
    ...(workspaceRoot ? { workspace_root: workspaceRoot } : {}),
    ...(projectPath ? { project_path: projectPath } : {}),
    ...(selectedTopoRefs ? { selected_topo_refs: selectedTopoRefs } : {}),
    ...(selectedNodeIds ? { selected_node_ids: selectedNodeIds } : {}),
    ...(llm ? { llm } : {}),
  };
}

function buildNativeChangePreview(
  raw: Record<string, unknown>,
  request: AiAgentRunRequest,
): Record<string, unknown> | null {
  if (isRecord(raw.changePreview)) return raw.changePreview;
  const isV2 =
    raw.runtime_path === "semantic_v2" ||
    typeof raw.preview_branch === "string" ||
    typeof raw.branch === "string" ||
    raw.verification !== undefined ||
    raw.diff !== undefined;
  if (!isV2) return null;
  const previewBranch =
    typeof raw.preview_branch === "string" && raw.preview_branch.trim()
      ? raw.preview_branch.trim()
      : typeof raw.branch === "string" && raw.branch.trim()
        ? raw.branch.trim()
        : null;
  const verdict =
    typeof raw.overall_status === "string"
      ? raw.overall_status
      : typeof raw.verdict === "string"
        ? raw.verdict
        : raw.passed === true
          ? "PASS"
          : "UNKNOWN";
  return {
    runtime_path: typeof raw.runtime_path === "string" ? raw.runtime_path : "semantic_v2",
    mode: typeof raw.mode === "string" ? raw.mode : (request.mode ?? "generate"),
    project_path: request.project_path ?? null,
    preview_branch: previewBranch,
    base_branch: typeof raw.base_branch === "string" && raw.base_branch.trim() ? raw.base_branch.trim() : "main",
    revision: typeof raw.revision === "number" && Number.isSafeInteger(raw.revision) ? raw.revision : null,
    passed: Boolean(raw.passed),
    verdict,
    diff: raw.diff ?? null,
    verification: raw.verification ?? null,
    explanation: raw.explanation ?? null,
    commands: Array.isArray(raw.commands) ? raw.commands : [],
    selected_topo_refs: request.selected_topo_refs ?? [],
    selected_node_ids: request.selected_node_ids ?? [],
  };
}

function buildNativeAgentSummary(raw: Record<string, unknown>, mode: "generate" | "edit"): Record<string, unknown> {
  if (isRecord(raw.agentSummary)) return raw.agentSummary;
  return {
    mode: typeof raw.mode === "string" ? raw.mode : mode,
    passed: typeof raw.passed === "boolean" ? raw.passed : undefined,
    used_llm: raw.used_llm === true,
    ...(typeof raw.llm_provider === "string" ? { llm_provider: raw.llm_provider } : {}),
    ...(typeof raw.llm_model === "string" ? { llm_model: raw.llm_model } : {}),
    ...(typeof raw.vlm_enabled === "boolean" ? { vlm_enabled: raw.vlm_enabled } : {}),
    ...(typeof raw.contract_repair_attempts === "number"
      ? { contract_repair_attempts: raw.contract_repair_attempts }
      : {}),
    ...(Array.isArray(raw.retrieved_examples) ? { retrieved_examples: raw.retrieved_examples } : {}),
  };
}

async function normalizeNativeAiEvent(
  ipc: DesktopIpcClient,
  result: AiAgentRunResult,
  request: AiAgentRunRequest,
  fallbackTraceId: string,
): Promise<{ readonly event: "result" | "error"; readonly data: Record<string, unknown> }> {
  const raw: Record<string, unknown> = isRecord(result.payload) ? result.payload : {};
  const mode = request.mode === "edit" ? "edit" : "generate";
  const traceId = normalizeTraceId(result.trace_id) ?? readTraceId(undefined, raw, fallbackTraceId);
  const verdictStr = String(
    raw.overall_status ??
      raw.verdict ??
      (isRecord(raw.changePreview) ? raw.changePreview.verdict : "") ??
      "",
  ).toUpperCase();
  const explicitError =
    (typeof raw.error === "string" && raw.error.trim().length > 0) ||
    (typeof raw.error_code === "string" && raw.error_code.trim().length > 0);
  const changePreview = buildNativeChangePreview(raw, request);
  const agentSummary = buildNativeAgentSummary(raw, mode);

  if (explicitError) {
    const errorText =
      typeof raw.error === "string" && raw.error.trim()
        ? raw.error.trim()
        : typeof raw.message === "string" && raw.message.trim()
          ? raw.message.trim()
          : "AI Agent 执行失败";
    return {
      event: "error",
      data: {
        ok: false,
        status: 422,
        trace_id: traceId,
        mode,
        error: errorText,
        ...(typeof raw.error_code === "string" ? { error_code: raw.error_code } : {}),
        agentSummary,
        ...(changePreview ? { changePreview } : {}),
      },
    };
  }

  // Distinguish honest UNKNOWN verdict (e.g. non-OCCT L4) from hard FAIL.
  const isHardFail = verdictStr === "FAIL" || (!result.ok && verdictStr !== "UNKNOWN");
  if (isHardFail) {
    const reason =
      typeof raw.explanation === "string" && raw.explanation.trim()
        ? raw.explanation.trim()
        : mode === "edit"
          ? "V2 语义修改未通过几何门禁校验，已保留原分支。"
          : "V2 语义建模未通过几何门禁校验。";
    return {
      event: "error",
      data: {
        ok: false,
        status: 422,
        trace_id: traceId,
        mode,
        error: reason,
        error_code: mode === "edit" ? "edit_validation_failed" : "verification_failed",
        agentSummary,
        ...(changePreview ? { changePreview } : {}),
      },
    };
  }

  let source =
    typeof raw.source === "string" && raw.source.trim()
      ? raw.source
      : typeof raw.kcl_code === "string" && raw.kcl_code.trim()
        ? raw.kcl_code
        : "";
  if (!source && request.project_path) {
    try {
      const emitted = await ipc.emitAicadKcl({ project_path: request.project_path });
      if (typeof emitted === "string" && emitted.trim()) {
        source = emitted;
      }
    } catch {
      // Keep source empty; downstream handles missing KCL cleanly.
    }
  }

  return {
    event: "result",
    data: {
      ok: true,
      trace_id: traceId,
      mode,
      generatorBackend: "tauri-native-v2",
      source,
      agentSummary,
      ...(changePreview ? { changePreview } : {}),
    },
  };
}

/** Decodes a `CM2E` binary CMS2 artifact envelope directly into a `WorkbenchModel`. */
export function decodeCms2WorkbenchModel(
  payload: ArrayBuffer | ArrayBufferView | readonly number[],
  fallbackTraceId = createTraceId(),
): WorkbenchModel {
  const { metadata, scene } = decodeCms2Envelope<Cms2ArtifactMetadata>(payload);
  const sceneMesh = compactSceneToSceneMesh(scene, {
    diagnostics: metadata.diagnostics,
    edit_handles: metadata.edit_handles,
    hole_offsets: metadata.hole_offsets,
    components: metadata.components,
  });
  const traceId = readTraceId(undefined, metadata, fallbackTraceId);
  const model = hostModel(metadata.diagnostics, sceneMesh, traceId);
  return { ...model, compactScene: scene };
}

function shouldFallbackFromCms2(error: unknown): boolean {
  if (error instanceof Cms2IpcUnavailableError) return true;
  const text = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return /build_cad_artifact_cms2/i.test(text) && /not found|unknown command|unregistered/i.test(text);
}

export interface TauriWorkbenchBackendOptions {
  readonly preferCms2?: boolean;
}

export function createTauriWorkbenchBackend(
  ipc: DesktopIpcClient,
  _deprecatedHttpAi?: Pick<WorkbenchBackend, "getAiConfig" | "getAiProfiles" | "saveAiProfiles" | "testAiConnection" | "requestAi">,
  options?: TauriWorkbenchBackendOptions,
): WorkbenchBackend {
  void _deprecatedHttpAi;
  const preferCms2 = options?.preferCms2 !== false;
  return {
    kind: "tauri",
    async compile(document, signal) {
      const traceId = createTraceId();
      const request = nativeCompileRequest(document, traceId);
      try {
        if (preferCms2 && typeof ipc.buildCadArtifactCms2 === "function") {
          try {
            const cms2Artifact = await ipc.buildCadArtifactCms2(request, signal);
            signal.throwIfAborted();
            const model = hostModel(
              cms2Artifact.diagnostics,
              cms2Artifact.scene_mesh,
              readTraceId(undefined, cms2Artifact, traceId),
            );
            return { ...model, compactScene: cms2Artifact.compact_scene };
          } catch (cms2Error) {
            signal.throwIfAborted();
            if (!shouldFallbackFromCms2(cms2Error)) {
              throw cms2Error;
            }
          }
        }
        const artifact = await ipc.buildCadArtifactBinary(request, signal);
        signal.throwIfAborted();
        return hostModel(artifact.diagnostics, artifact.scene_mesh, readTraceId(undefined, artifact, traceId));
      } catch (error) {
        signal.throwIfAborted();
        rethrowIpc(error, traceId, "compile");
      }
    },
    async listAssets() {
      const traceId = createTraceId();
      try {
        return await ipc.listWorkspaceAssets();
      } catch (error) {
        rethrowIpc(error, traceId, "list-assets");
      }
    },
    async uploadAsset(name, bytes) {
      const traceId = createTraceId();
      try {
        return await ipc.uploadWorkspaceAsset(name, bytes);
      } catch (error) {
        rethrowIpc(error, traceId, "upload-asset");
      }
    },
    async inspectAsset(asset) {
      const traceId = createTraceId();
      try {
        const request = { source: asset.source, file_name: `${asset.name}.kcl`, trace_id: traceId };
        const report = await ipc.compileKcl(request);
        return report.status === "ok" ? report.metrics : report.diagnostics;
      } catch (error) {
        rethrowIpc(error, traceId, "inspect-asset");
      }
    },
    async exportArtifact(kind, document, model, signal, options) {
      signal.throwIfAborted();
      // The native DXF/SVG commands return output paths, not bytes. Keep the existing
      // downloadable mesh-based previews without requiring a writable install directory.
      if (kind !== "step" && kind !== "3mf") return previewExport(kind, model);
      const traceId = createTraceId();
      try {
        const partName = model.sceneMesh.part_name || "model";
        const safePart = partName.replace(/[^A-Za-z0-9_-]/g, "_");
        const result = kind === "step"
          ? await ipc.exportStepAp242(
              {
                ...nativeCompileRequest(document, traceId),
                out_path: `out/${safePart}.step`,
                include_pmi: true,
                allow_non_authoritative_preview: options?.allowNonAuthoritativePreview,
              },
              signal,
            )
          : await ipc.exportThreeMf(
              {
                ...nativeCompileRequest(document, traceId),
                out_path: `out/${safePart}.3mf`,
              },
              signal,
            );
        signal.throwIfAborted();
        return { message: `${result.message} → ${result.out_path}`, traceId: readTraceId(undefined, result, traceId) };
      } catch (error) {
        if (error instanceof StepExportDiscardError || error instanceof ThreeMfExportDiscardError) throw error;
        signal.throwIfAborted();
        if (error instanceof Error && error.name === "AbortError") throw error;
        rethrowIpc(error, traceId, "export");
      }
    },
    async getAiConfig(signal) {
      signal.throwIfAborted();
      const traceId = createTraceId();
      try {
        const config = await ipc.getAiConfig();
        signal.throwIfAborted();
        return config;
      } catch (error) {
        signal.throwIfAborted();
        rethrowIpc(error, traceId, "ai-config");
      }
    },
    async getAiProfiles(signal) {
      signal.throwIfAborted();
      const traceId = createTraceId();
      try {
        const profiles = await ipc.getAiProfiles();
        signal.throwIfAborted();
        return profiles;
      } catch (error) {
        signal.throwIfAborted();
        rethrowIpc(error, traceId, "ai-profiles");
      }
    },
    async saveAiProfiles(payload, signal) {
      signal?.throwIfAborted();
      const traceId = createTraceId();
      try {
        const saved = await ipc.saveAiProfiles(payload);
        signal?.throwIfAborted();
        return saved;
      } catch (error) {
        signal?.throwIfAborted();
        rethrowIpc(error, traceId, "ai-profiles-save");
      }
    },
    async testAiConnection(payload, signal) {
      signal?.throwIfAborted();
      const traceId = createTraceId();
      try {
        const result = await ipc.testAiConnection(payload);
        signal?.throwIfAborted();
        return result;
      } catch (error) {
        signal?.throwIfAborted();
        rethrowIpc(error, traceId, "ai-test");
      }
    },
    async requestAi(body, signal) {
      signal.throwIfAborted();
      const traceId = createTraceId();
      const nativeRequest = toNativeAiRunRequest(body, traceId);
      const encoder = new TextEncoder();
      let closed = false;
      let cancelRequested = false;

      const requestNativeCancel = () => {
        if (cancelRequested) return;
        cancelRequested = true;
        void ipc.cancelAiAgent(traceId).catch(() => {});
      };

      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          const enqueueEvent = (eventName: string, data: unknown) => {
            if (closed || signal.aborted) return;
            try {
              controller.enqueue(encoder.encode(`event: ${eventName}\ndata: ${JSON.stringify(data)}\n\n`));
            } catch {
              closed = true;
            }
          };
          const closeStream = () => {
            if (closed) return;
            closed = true;
            try {
              controller.close();
            } catch {
              // Stream may already be closed or cancelled.
            }
          };
          const onAbort = () => {
            requestNativeCancel();
            if (!closed) {
              closed = true;
              try {
                controller.error(new DOMException("The operation was aborted.", "AbortError"));
              } catch {
                // Ignore if already errored/closed.
              }
            }
          };
          signal.addEventListener("abort", onAbort, { once: true });

          void ipc
            .runAiAgent(nativeRequest, signal, (progress: AiAgentProgressEvent) => {
              enqueueEvent("progress", progress);
            })
            .then(async (result) => {
              if (signal.aborted || closed) return;
              const normalized = await normalizeNativeAiEvent(ipc, result, nativeRequest, traceId);
              if (signal.aborted || closed) return;
              enqueueEvent(normalized.event, normalized.data);
              closeStream();
            })
            .catch((error) => {
              if (signal.aborted || closed) return;
              if (error instanceof Error && error.name === "AbortError") {
                onAbort();
                return;
              }
              const classified = classifyNativeAgentError(error);
              enqueueEvent("error", {
                ok: false,
                status: 500,
                trace_id: traceId,
                error: classified.error,
                error_code: classified.error_code,
              });
              closeStream();
            })
            .finally(() => {
              signal.removeEventListener("abort", onAbort);
            });
        },
        cancel() {
          closed = true;
          requestNativeCancel();
        },
      });

      return new Response(stream, {
        status: 200,
        headers: {
          "Content-Type": "text/event-stream; charset=utf-8",
          [TRACE_HEADER]: traceId,
        },
      });
    },
    async diffPreviewBranch(req, signal) {
      signal?.throwIfAborted();
      const traceId = createTraceId();
      const target = req.branchName.trim();
      if (!target) {
        throw new WorkbenchTransportError({
          code: TRANSPORT_IPC_FAILED,
          message: "diff_preview 需要非空的预览分支名。",
          traceId,
          operation: "preview-diff",
        });
      }
      try {
        const diffed = await ipc.branchAicadProject({
          project_path: req.projectPath,
          action: "diff",
          branch_name: target,
        });
        signal?.throwIfAborted();
        return {
          ok: true,
          action: "diff",
          branch: target,
          ...(diffed.diff !== undefined ? { diff: diffed.diff } : {}),
          ...(typeof diffed.revision === "number" ? { revision: diffed.revision } : {}),
          activeBranch: diffed.active_branch,
          traceId,
        };
      } catch (error) {
        signal?.throwIfAborted();
        rethrowIpc(error, traceId, "preview-diff");
      }
    },
    async commitPreviewBranch(req, signal) {
      signal?.throwIfAborted();
      const traceId = createTraceId();
      const branch = req.branchName.trim();
      if (!branch || branch === "main") {
        throw new WorkbenchTransportError({
          code: TRANSPORT_IPC_FAILED,
          message: "不允许在受保护的 main 分支上执行 commit_preview。",
          traceId,
          operation: "preview-commit",
        });
      }
      try {
        const committed = await ipc.branchAicadProject({
          project_path: req.projectPath,
          action: "commit_preview",
          branch_name: branch,
        });
        signal?.throwIfAborted();
        const kcl = await ipc.emitAicadKcl({ project_path: req.projectPath });
        signal?.throwIfAborted();
        return {
          ok: true,
          action: "commit_preview",
          branch,
          activeBranch: committed.active_branch,
          ...(typeof committed.revision === "number" ? { revision: committed.revision } : {}),
          kcl,
          traceId,
        };
      } catch (error) {
        signal?.throwIfAborted();
        const message = error instanceof Error ? error.message : error;
        const conflictCode = DIAGNOSTIC_CODES.E409_PREVIEW_DIVERGED;
        if (typeof message === "string" && message.includes(`${conflictCode}:`)) {
          throw new WorkbenchTransportError({
            code: conflictCode,
            message: "主分支已在预览创建后更新。请基于当前版本重新生成预览。",
            traceId,
            operation: "preview-commit",
          });
        }
        rethrowIpc(error, traceId, "preview-commit");
      }
    },
    async discardPreviewBranch(req, signal) {
      signal?.throwIfAborted();
      const traceId = createTraceId();
      const branch = req.branchName.trim();
      if (!branch || branch === "main") {
        throw new WorkbenchTransportError({
          code: TRANSPORT_IPC_FAILED,
          message: "不允许在受保护的 main 分支上执行 discard_preview。",
          traceId,
          operation: "preview-discard",
        });
      }
      try {
        const discarded = await ipc.branchAicadProject({
          project_path: req.projectPath,
          action: "discard_preview",
          branch_name: branch,
        });
        signal?.throwIfAborted();
        return {
          ok: true,
          action: "discard_preview",
          branch,
          activeBranch: discarded.active_branch,
          ...(typeof discarded.revision === "number" ? { revision: discarded.revision } : {}),
          traceId,
        };
      } catch (error) {
        signal?.throwIfAborted();
        rethrowIpc(error, traceId, "preview-discard");
      }
    },
    async importAicadDocument(fileName, contentsBase64) {
      const traceId = createTraceId();
      try {
        const opened = await ipc.importAicadDocument({
          file_name: fileName,
          contents_base64: contentsBase64,
        });
        return {
          projectPath: opened.project_path,
          projectName: opened.project_name,
          source: opened.source,
          history: {
            canRollback: opened.history?.can_rollback === true,
            oplogLen: opened.history?.oplog_len ?? 0,
            headSeq: opened.history?.head_seq ?? null,
            previousSeq: opened.history?.previous_seq ?? null,
          },
        };
      } catch (error) {
        rethrowIpc(error, traceId, "import-aicad");
      }
    },
    async saveAicadDocument(request) {
      const traceId = createTraceId();
      try {
        const saved = await ipc.saveAicadDocument({
          project_path: request.projectPath,
          project_name: request.projectName,
          source: request.source,
        });
        return {
          projectPath: saved.project_path,
          projectName: saved.project_name,
          history: {
            canRollback: saved.history?.can_rollback === true,
            oplogLen: saved.history?.oplog_len ?? 0,
            headSeq: saved.history?.head_seq ?? null,
            previousSeq: saved.history?.previous_seq ?? null,
          },
        };
      } catch (error) {
        rethrowIpc(error, traceId, "save-aicad");
      }
    },
    async undoAicadContainer(request) {
      const traceId = createTraceId();
      try {
        const undone = await ipc.undoAicadContainer({
          project_path: request.projectPath,
        });
        return {
          projectPath: undone.project_path,
          projectName: undone.project_name,
          source: undone.source,
          history: {
            canRollback: undone.history?.can_rollback === true,
            oplogLen: undone.history?.oplog_len ?? 0,
            headSeq: undone.history?.head_seq ?? null,
            previousSeq: undone.history?.previous_seq ?? null,
          },
        };
      } catch (error) {
        rethrowIpc(error, traceId, "undo-aicad-container");
      }
    },
    async getAppUpdaterInfo() {
      return ipc.getAppUpdaterInfo();
    },
    async checkAppUpdate() {
      return ipc.checkAppUpdate();
    },
    async installAppUpdate(request, onProgress) {
      return ipc.installAppUpdate(request, onProgress);
    },
  };
}

export function createWorkbenchBackend(): WorkbenchBackend {
  return isTauriRuntime() ? createTauriWorkbenchBackend(createDesktopIpcClient()) : createHttpWorkbenchBackend();
}
