import type {
  AiConfigStatus,
  AiLlmConfig,
  AiProfileDraftInput,
  AiProfilesResponse,
  AiProgressStep,
  AiTestConnectionPayload,
  AiTestConnectionResult,
  DiagnosticsReport,
  SceneMeshPayload,
} from "../../studio-ui/src/types/scene.ts";
import type {
  CacheStatus,
  CanonicalGraph,
  GraphPatch,
  ProjectStorageKind,
} from "../../studio-ui/src/types/graphIr.ts";
import type {
  ProjectBranchInfo,
  ProjectInitResult,
  ProjectPatchResult,
} from "../../studio-ui/src/sync/projectClient.ts";
import {
  compactSceneToSceneMesh,
  decodeCms2Envelope,
  type Cms2ArtifactMetadata,
  type DecodedCompactScene,
} from "../../studio-ui/src/sync/cms2Decoder.ts";

export interface EngineStatus {
  readonly runtime: "tauri-v2" | "browser-standalone";
  readonly engine_version: string;
  readonly occt_available: boolean;
  readonly wgpu_backend: string;
  readonly offline_ready: boolean;
}

export interface CompileKclRequest {
  readonly file_name: string;
  readonly source: string;
  /** Optional identifier for a workspace already authorized by the native host. */
  readonly workspace_root?: string;
}

export interface BuildArtifactResponse {
  readonly diagnostics: DiagnosticsReport;
  readonly scene_mesh: SceneMeshPayload;
  readonly elapsed_ms: number;
}

export interface BuildArtifactCms2Response extends BuildArtifactResponse {
  readonly compact_scene: DecodedCompactScene;
  readonly trace_id?: string;
}

export class Cms2IpcUnavailableError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "Cms2IpcUnavailableError";
  }
}

export interface ExportRequest {
  readonly file_name: string;
  readonly source: string;
  /** Optional identifier for a workspace already authorized by the native host. */
  readonly workspace_root?: string;
  readonly out_path: string;
  readonly include_pmi?: boolean;
  readonly views?: readonly string[];
  /** Workbench trace id. Required for desktop STEP, DXF, drawing SVG, and 3MF cancel to name one publication. */
  readonly trace_id?: string;
}

export interface ExportResult {
  readonly format: "STEP_AP242" | "DXF" | "DRAWING_SVG" | "3MF" | "SCENE_MESH_JSON";
  readonly out_path: string;
  readonly bytes_written: number;
  readonly pmi_entity_count: number;
  readonly message: string;
}

export interface WorkspaceAsset {
  readonly name: string;
  readonly path: string;
  readonly sizeBytes: number;
  readonly format: "step" | "brep" | "iges" | "stl" | "3mf" | "obj" | "gltf";
  readonly expression: string;
  readonly source: string;
}

export interface InitAicadProjectIpcRequest {
  readonly project_path: string;
  readonly project_name: string;
  readonly format?: ProjectStorageKind;
  readonly workspace_root?: string;
}

export interface InspectAicadProjectIpcRequest {
  readonly project_path: string;
  readonly workspace_root?: string;
}

export interface DesktopProjectSummary {
  readonly status: "ok";
  readonly project_name: string;
  readonly format_version: number;
  readonly ir_schema_version: number;
  readonly active_branch: string;
  readonly node_count: number;
  readonly parameter_count: number;
  readonly blob_count: number;
  readonly external_ref_count: number;
  readonly pmi_count: number;
  readonly bom_count: number;
  readonly drawing_count: number;
  readonly node_brep_cache_count: number;
  readonly bundled_ref_count: number;
  readonly cache_status: CacheStatus;
  readonly storage: ProjectStorageKind;
  readonly blobs_verified: boolean;
}

export interface ApplyAicadPatchIpcRequest {
  readonly project_path: string;
  readonly patch: GraphPatch;
  readonly branch?: string;
  readonly workspace_root?: string;
}

export interface EmitAicadKclIpcRequest {
  readonly project_path: string;
  readonly workspace_root?: string;
}

export interface BranchAicadProjectIpcRequest {
  readonly project_path: string;
  readonly action:
    | "list"
    | "create"
    | "checkout"
    | "rollback"
    | "diff"
    | "commit_preview"
    | "discard_preview";
  readonly branch_name?: string;
  readonly from_branch?: string;
  readonly purpose?: "main" | "user" | "ai_candidate";
  readonly to_seq?: number;
  readonly workspace_root?: string;
}

export interface DesktopBranchResponse {
  readonly status: "ok";
  readonly action: string;
  readonly active_branch: string;
  readonly branch?: ProjectBranchInfo;
  readonly branches?: readonly ProjectBranchInfo[];
  readonly head_seq?: number;
  readonly node_count?: number;
  readonly diff?: unknown;
  readonly revision?: number;
}

export interface SliceAicadContextIpcRequest {
  readonly project_path: string;
  readonly focus_nodes: readonly string[];
  readonly upstream_depth?: number;
  readonly downstream_depth?: number;
  readonly ensure_valid_closure?: boolean;
  readonly workspace_root?: string;
}

export interface DesktopContextSliceResponse {
  readonly status: "ok";
  readonly focus_nodes: readonly string[];
  readonly kept_node_count: number;
  readonly total_node_count: number;
  readonly graph: CanonicalGraph;
  readonly kcl: string;
}

export interface VacuumAicadProjectIpcRequest {
  readonly project_path: string;
  readonly keep_recent_oplog?: number;
  readonly workspace_root?: string;
}

export interface DesktopVacuumReport {
  readonly bytes_before: number;
  readonly bytes_after: number;
  readonly reclaimed_bytes: number;
  readonly removed_blobs: number;
  readonly removed_node_caches: number;
  readonly pruned_oplog_entries: number;
}

export interface PackAndGoAicadProjectIpcRequest {
  readonly project_path: string;
  readonly output_path: string;
  readonly workspace_root?: string;
}

export interface DesktopBundledRefEntry {
  readonly ref_id: string;
  readonly original_container_path: string;
  readonly container_sha256: string;
  readonly source_format: string;
  readonly uncompressed_len: number;
  readonly bundled_at_ms: number;
}

export interface DesktopPackAndGoResponse {
  readonly status: "ok";
  readonly output_path: string;
  readonly bundled_count: number;
  readonly locked_hashes: Readonly<Record<string, string>>;
  readonly bundled_refs: readonly DesktopBundledRefEntry[];
}

interface TauriEventEnvelope<T> {
  readonly payload?: T;
}

interface TauriInternals {
  invoke<T>(cmd: string, args?: Readonly<Record<string, unknown>> | ArrayBuffer, options?: { readonly headers: Readonly<Record<string, string>> }): Promise<T>;
  transformCallback?<T>(callback: (response: T) => void): number;
}

interface WindowWithTauri {
  readonly __TAURI_INTERNALS__?: TauriInternals;
}

export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") {
    return false;
  }
  const win = window as unknown as WindowWithTauri;
  return typeof win.__TAURI_INTERNALS__?.invoke === "function";
}

const EXPORT_CLIENT_ABORTED = "CLIENT_ABORTED";

type DiscardErrorCtor = new (message: string) => Error;

export class StepExportDiscardError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "StepExportDiscardError";
  }
}

export class DxfExportDiscardError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DxfExportDiscardError";
  }
}

export class DrawingSvgExportDiscardError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DrawingSvgExportDiscardError";
  }
}

export class ThreeMfExportDiscardError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ThreeMfExportDiscardError";
  }
}

interface ExportPublication {
  readonly command: string;
  readonly cancelCommand: string;
  readonly discardCommand: string;
  readonly formatLabel: string;
  readonly discardError: DiscardErrorCtor;
}

const STEP_PUBLICATION: ExportPublication = {
  command: "export_step_ap242",
  cancelCommand: "cancel_export_step_ap242",
  discardCommand: "discard_export_step_ap242",
  formatLabel: "STEP",
  discardError: StepExportDiscardError,
};

const DXF_PUBLICATION: ExportPublication = {
  command: "export_dxf_drawing",
  cancelCommand: "cancel_export_dxf_drawing",
  discardCommand: "discard_export_dxf_drawing",
  formatLabel: "DXF",
  discardError: DxfExportDiscardError,
};

const DRAWING_SVG_PUBLICATION: ExportPublication = {
  command: "export_drawing_svg",
  cancelCommand: "cancel_export_drawing_svg",
  discardCommand: "discard_export_drawing_svg",
  formatLabel: "Drawing SVG",
  discardError: DrawingSvgExportDiscardError,
};

const THREEMF_PUBLICATION: ExportPublication = {
  command: "export_3mf",
  cancelCommand: "cancel_export_3mf",
  discardCommand: "discard_export_3mf",
  formatLabel: "3MF",
  discardError: ThreeMfExportDiscardError,
};

export const AI_AGENT_PROGRESS_EVENT = "cad-agent://progress";

export interface AiAgentRunRequest {
  readonly trace_id?: string;
  readonly traceId?: string;
  readonly prompt: string;
  readonly material?: string;
  readonly manufacturing_method?: string;
  readonly manufacturingMethod?: string;
  readonly mode?: "generate" | "edit";
  readonly existing_source?: string;
  readonly existingSource?: string;
  readonly workspace_root?: string;
  readonly workspaceRoot?: string;
  readonly project_path?: string;
  readonly projectPath?: string;
  readonly selected_topo_refs?: readonly string[];
  readonly selectedTopoRefs?: readonly string[];
  readonly selected_node_ids?: readonly string[];
  readonly selectedNodeIds?: readonly string[];
  readonly llm?: AiLlmConfig;
}

export interface AiAgentRunResult {
  readonly ok: boolean;
  readonly trace_id: string;
  readonly exit_code: number;
  readonly payload: Readonly<Record<string, unknown>>;
}

export interface AiAgentProgressEvent extends AiProgressStep {
  readonly event: "progress";
  readonly trace_id: string;
}

export interface SaveAiProfilesIpcRequest {
  readonly activeProfileId: string | null;
  readonly profiles: readonly AiProfileDraftInput[];
}

function abortError(): DOMException {
  return new DOMException("The operation was aborted.", "AbortError");
}

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "";
}

function isPublicationAbort(error: unknown): boolean {
  return errorText(error).includes(EXPORT_CLIENT_ABORTED);
}

async function discardPublishedExport(
  invoker: TauriInternals,
  publication: ExportPublication,
  traceId: string,
  outPath: string,
): Promise<void> {
  const published = outPath.trim();
  if (!published) return;
  const DiscardError = publication.discardError;
  if (!traceId) {
    throw new DiscardError(
      `${publication.formatLabel} export was aborted, but ${published} was written without a trace id and was left on disk.`,
    );
  }
  try {
    await invoker.invoke(publication.discardCommand, {
      request: { trace_id: traceId, out_path: published },
    });
  } catch (error) {
    const detail = errorText(error) || "unknown error";
    throw new DiscardError(
      `${publication.formatLabel} export was aborted, but ${published} could not be removed: ${detail}`,
    );
  }
}

// Tauri invoke cannot abort an in-process kernel or drawing render. Honor
// AbortSignal at publication: cancel skips the write, and a write that already
// landed is deleted only while this trace id still owns the path.
async function invokePublishedExport(
  request: ExportRequest,
  signal: AbortSignal | undefined,
  publication: ExportPublication,
  standalone: ExportResult | null,
): Promise<ExportResult> {
  signal?.throwIfAborted();
  const invoker = getTauriInvoker();
  if (!invoker) {
    if (standalone) return standalone;
    throw new Error("3MF 导出只在 Tauri 桌面端可用，浏览器预览不会假装写出 3MF 文件。");
  }
  const traceId = request.trace_id?.trim() ?? "";
  const cancel = () => {
    if (!traceId) return;
    void invoker.invoke(publication.cancelCommand, { request: { trace_id: traceId } }).catch(() => {});
  };
  signal?.addEventListener("abort", cancel, { once: true });
  try {
    signal?.throwIfAborted();
    const result = await invoker.invoke<ExportResult>(publication.command, { request });
    if (signal?.aborted) {
      await discardPublishedExport(invoker, publication, traceId, result.out_path);
      signal.throwIfAborted();
    }
    return result;
  } catch (error) {
    if (error instanceof publication.discardError) throw error;
    if (signal?.aborted || isPublicationAbort(error)) throw abortError();
    throw error;
  } finally {
    signal?.removeEventListener("abort", cancel);
  }
}

function getTauriInvoker(): TauriInternals | null {
  if (!isTauriRuntime()) {
    return null;
  }
  const win = window as unknown as WindowWithTauri;
  return win.__TAURI_INTERNALS__ ?? null;
}

function requireTauri(command: string): TauriInternals {
  const invoker = getTauriInvoker();
  if (!invoker) {
    throw new Error(`${command} IPC is only available inside the Tauri v2 desktop runtime.`);
  }
  return invoker;
}

function newAgentTraceId(): string {
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function agentTraceId(request: AiAgentRunRequest): string {
  const supplied = (request.trace_id ?? request.traceId ?? "").trim();
  return supplied || newAgentTraceId();
}

function agentRequestBody(request: AiAgentRunRequest, traceId: string): AiAgentRunRequest {
  return {
    trace_id: traceId,
    prompt: request.prompt,
    material: request.material,
    manufacturing_method: request.manufacturing_method ?? request.manufacturingMethod,
    mode: request.mode,
    existing_source: request.existing_source ?? request.existingSource,
    workspace_root: request.workspace_root ?? request.workspaceRoot,
    project_path: request.project_path ?? request.projectPath,
    selected_topo_refs: request.selected_topo_refs ?? request.selectedTopoRefs,
    selected_node_ids: request.selected_node_ids ?? request.selectedNodeIds,
    llm: request.llm,
  };
}

async function listenTauriEvent<T>(
  invoker: TauriInternals,
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  if (typeof invoker.transformCallback !== "function") {
    return () => {};
  }
  const callbackId = invoker.transformCallback((response: TauriEventEnvelope<T>) => {
    if (response.payload !== undefined) handler(response.payload);
  });
  const eventId = await invoker.invoke<number>("plugin:event|listen", {
    event,
    target: { kind: "Any" },
    handler: callbackId,
  });
  return () => {
    void invoker.invoke<void>("plugin:event|unlisten", { event, eventId }).catch(() => {});
  };
}

export interface DesktopIpcClient {
  getEngineStatus(): Promise<EngineStatus>;
  compileKcl(request: CompileKclRequest): Promise<DiagnosticsReport>;
  buildCadArtifact(request: CompileKclRequest): Promise<BuildArtifactResponse>;
  buildCadArtifactBinary(request: CompileKclRequest, signal?: AbortSignal): Promise<BuildArtifactResponse>;
  buildCadArtifactCms2?(request: CompileKclRequest, signal?: AbortSignal): Promise<BuildArtifactCms2Response>;
  listWorkspaceAssets(): Promise<readonly WorkspaceAsset[]>;
  uploadWorkspaceAsset(name: string, bytes: ArrayBuffer): Promise<WorkspaceAsset>;
  exportStepAp242(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult>;
  exportDxfDrawing(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult>;
  exportDrawingSvg(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult>;
  exportThreeMf(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult>;
  getAiConfig(): Promise<AiConfigStatus>;
  getAiProfiles(): Promise<AiProfilesResponse>;
  saveAiProfiles(request: SaveAiProfilesIpcRequest): Promise<AiProfilesResponse>;
  testAiConnection(request: AiTestConnectionPayload): Promise<AiTestConnectionResult>;
  runAiAgent(
    request: AiAgentRunRequest,
    signal?: AbortSignal,
    onProgress?: (event: AiAgentProgressEvent) => void,
  ): Promise<AiAgentRunResult>;
  cancelAiAgent(traceId: string): Promise<boolean>;
  initAicadProject(request: InitAicadProjectIpcRequest): Promise<ProjectInitResult>;
  inspectAicadProject(request: InspectAicadProjectIpcRequest): Promise<DesktopProjectSummary>;
  applyAicadPatch(request: ApplyAicadPatchIpcRequest): Promise<ProjectPatchResult>;
  emitAicadKcl(request: EmitAicadKclIpcRequest): Promise<string>;
  branchAicadProject(request: BranchAicadProjectIpcRequest): Promise<DesktopBranchResponse>;
  sliceAicadContext(request: SliceAicadContextIpcRequest): Promise<DesktopContextSliceResponse>;
  vacuumAicadProject(request: VacuumAicadProjectIpcRequest): Promise<DesktopVacuumReport>;
  packAndGoAicadProject(request: PackAndGoAicadProjectIpcRequest): Promise<DesktopPackAndGoResponse>;
}

const nativeLittleEndian = new Uint8Array(Uint32Array.of(1).buffer)[0] === 1;

function decodeBinaryArtifact(payload: ArrayBuffer | number[]): BuildArtifactResponse {
  // Tauri's raw postMessage fallback can deliver an array of bytes.
  const buffer = Array.isArray(payload) ? Uint8Array.from(payload).buffer : payload;
  const view = new DataView(buffer);
  if (view.byteLength < 8 || view.getUint32(0) !== 0x434d5331) { // CMS1
    throw new Error("Invalid binary SceneMesh header");
  }
  const metadataOffset = view.getUint32(4, true);
  if (metadataOffset < 8 || metadataOffset >= view.byteLength) {
    throw new Error("Invalid binary SceneMesh metadata offset");
  }
  const metadata = JSON.parse(
    new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(buffer, metadataOffset)),
  ) as BuildArtifactResponse;
  if (!Array.isArray(metadata?.scene_mesh?.faces) || !Array.isArray(metadata.scene_mesh.edges)) {
    throw new Error("Invalid binary SceneMesh metadata");
  }

  let cursor = 8;
  function readArray(elementBytes: number): { start: number; count: number } {
    if (cursor + 4 > metadataOffset) throw new Error("Truncated binary SceneMesh array header");
    const count = view.getUint32(cursor, true);
    const start = Math.ceil((cursor + 4) / elementBytes) * elementBytes;
    cursor = start + count * elementBytes;
    if (cursor > metadataOffset) throw new Error("Truncated binary SceneMesh array");
    return { start, count };
  }
  function readFloats(): Float64Array {
    const { start, count } = readArray(Float64Array.BYTES_PER_ELEMENT);
    if (nativeLittleEndian) return new Float64Array(buffer, start, count);
    const values = new Float64Array(count);
    for (let i = 0; i < count; i++) values[i] = view.getFloat64(start + i * 8, true);
    return values;
  }
  function readIndices(): Uint32Array {
    const { start, count } = readArray(Uint32Array.BYTES_PER_ELEMENT);
    if (nativeLittleEndian) return new Uint32Array(buffer, start, count);
    const values = new Uint32Array(count);
    for (let i = 0; i < count; i++) values[i] = view.getUint32(start + i * 4, true);
    return values;
  }

  const faces = metadata.scene_mesh.faces.map((face) => ({
    ...face, vertices: readFloats(), normals: readFloats(), indices: readIndices(),
  }));
  const edges = metadata.scene_mesh.edges.map((edge) => ({ ...edge, polyline: readFloats() }));
  if (cursor !== metadataOffset) throw new Error("Unexpected data in binary SceneMesh");
  return { ...metadata, scene_mesh: { ...metadata.scene_mesh, faces, edges } };
}

export function createDesktopIpcClient(): DesktopIpcClient {
  return {
    async getEngineStatus(): Promise<EngineStatus> {
      const invoker = getTauriInvoker();
      if (invoker) {
        return invoker.invoke<EngineStatus>("get_engine_status");
      }
      return {
        runtime: "browser-standalone",
        engine_version: "0.1.0-preview",
        occt_available: false,
        wgpu_backend: "WebGPU / Canvas2D Fallback",
        offline_ready: true,
      };
    },

    async compileKcl(request: CompileKclRequest): Promise<DiagnosticsReport> {
      const invoker = getTauriInvoker();
      if (!invoker) {
        throw new Error(
          "compile_kcl IPC is only available inside Tauri v2 desktop runtime.",
        );
      }
      return invoker.invoke<DiagnosticsReport>("compile_kcl", { request });
    },

    async buildCadArtifact(
      request: CompileKclRequest,
    ): Promise<BuildArtifactResponse> {
      const invoker = getTauriInvoker();
      if (!invoker) {
        throw new Error(
          "build_cad_artifact IPC is only available inside Tauri v2 desktop runtime.",
        );
      }
      return invoker.invoke<BuildArtifactResponse>("build_cad_artifact", {
        request,
      });
    },

    async buildCadArtifactBinary(request, signal) {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("Binary SceneMesh IPC requires the Tauri v2 desktop runtime");
      signal?.throwIfAborted();
      const payload = await invoker.invoke<ArrayBuffer | number[]>("build_cad_artifact_binary", { request });
      signal?.throwIfAborted();
      return decodeBinaryArtifact(payload);
    },

    async buildCadArtifactCms2(request, signal) {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Cms2IpcUnavailableError("CMS2 Scene IPC requires the Tauri v2 desktop runtime");
      signal?.throwIfAborted();
      const payload = await invoker.invoke<ArrayBuffer | number[]>("build_cad_artifact_cms2", { request });
      signal?.throwIfAborted();
      const { metadata, scene } = decodeCms2Envelope<Cms2ArtifactMetadata>(payload);
      const scene_mesh = compactSceneToSceneMesh(scene, {
        diagnostics: metadata.diagnostics,
        edit_handles: metadata.edit_handles,
        hole_offsets: metadata.hole_offsets,
        components: metadata.components,
      });
      return {
        diagnostics: metadata.diagnostics,
        scene_mesh,
        compact_scene: scene,
        elapsed_ms: metadata.elapsed_ms,
        ...(metadata.trace_id ? { trace_id: metadata.trace_id } : {}),
      };
    },

    async listWorkspaceAssets() {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("Native workspace is not available");
      return invoker.invoke<readonly WorkspaceAsset[]>("list_workspace_assets");
    },

    async uploadWorkspaceAsset(name, bytes) {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("Native workspace is not available");
      return invoker.invoke<WorkspaceAsset>("upload_workspace_asset", bytes, { headers: { "X-Asset-Name": name } });
    },

    async exportStepAp242(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult> {
      return invokePublishedExport(request, signal, STEP_PUBLICATION, {
        format: "STEP_AP242",
        out_path: request.out_path,
        bytes_written: 0,
        pmi_entity_count: 2,
        message: "浏览器独立预览模式：已生成 STEP AP242 导出任务描述（需在 Tauri 桌面端调用本地 OCCT 写出物理文件）。",
      });
    },

    async exportDxfDrawing(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult> {
      return invokePublishedExport(request, signal, DXF_PUBLICATION, {
        format: "DXF",
        out_path: request.out_path,
        bytes_written: 0,
        pmi_entity_count: 0,
        message: "浏览器独立预览模式：已生成正交三视图 DXF 导出任务描述（需在 Tauri 桌面端生成）。",
      });
    },

    async exportDrawingSvg(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult> {
      return invokePublishedExport(request, signal, DRAWING_SVG_PUBLICATION, {
        format: "DRAWING_SVG",
        out_path: request.out_path,
        bytes_written: 0,
        pmi_entity_count: 0,
        message: "浏览器独立预览模式：已生成 2D 工程图 SVG 导出任务描述（需在 Tauri 桌面端生成）。",
      });
    },

    async exportThreeMf(request: ExportRequest, signal?: AbortSignal): Promise<ExportResult> {
      return invokePublishedExport(request, signal, THREEMF_PUBLICATION, null);
    },

    async getAiConfig(): Promise<AiConfigStatus> {
      const invoker = requireTauri("get_ai_config");
      return invoker.invoke<AiConfigStatus>("get_ai_config");
    },

    async getAiProfiles(): Promise<AiProfilesResponse> {
      const invoker = requireTauri("get_ai_profiles");
      return invoker.invoke<AiProfilesResponse>("get_ai_profiles");
    },

    async saveAiProfiles(request: SaveAiProfilesIpcRequest): Promise<AiProfilesResponse> {
      const invoker = requireTauri("save_ai_profiles");
      return invoker.invoke<AiProfilesResponse>("save_ai_profiles", { request });
    },

    async testAiConnection(request: AiTestConnectionPayload): Promise<AiTestConnectionResult> {
      const invoker = requireTauri("test_ai_connection");
      return invoker.invoke<AiTestConnectionResult>("test_ai_connection", { request });
    },

    async runAiAgent(request, signal, onProgress): Promise<AiAgentRunResult> {
      const invoker = requireTauri("run_ai_agent");
      signal?.throwIfAborted();
      const traceId = agentTraceId(request);
      const body = agentRequestBody(request, traceId);
      const stop = onProgress
        ? await listenTauriEvent<AiAgentProgressEvent>(invoker, AI_AGENT_PROGRESS_EVENT, (payload) => {
            if (payload.trace_id === traceId) onProgress(payload);
          })
        : () => {};
      const cancel = () => {
        void invoker.invoke<boolean>("cancel_ai_agent", { request: { trace_id: traceId } }).catch(() => {});
      };
      signal?.addEventListener("abort", cancel, { once: true });
      try {
        signal?.throwIfAborted();
        const result = await invoker.invoke<AiAgentRunResult>("run_ai_agent", { request: body });
        if (signal?.aborted) throw abortError();
        return result;
      } catch (error) {
        if (signal?.aborted || isPublicationAbort(error)) throw abortError();
        throw error;
      } finally {
        signal?.removeEventListener("abort", cancel);
        stop();
      }
    },

    async cancelAiAgent(traceId: string): Promise<boolean> {
      const invoker = requireTauri("cancel_ai_agent");
      return invoker.invoke<boolean>("cancel_ai_agent", { request: { trace_id: traceId } });
    },

    async initAicadProject(request: InitAicadProjectIpcRequest): Promise<ProjectInitResult> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("init_aicad_project IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<ProjectInitResult>("init_aicad_project", { request });
    },

    async inspectAicadProject(request: InspectAicadProjectIpcRequest): Promise<DesktopProjectSummary> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("inspect_aicad_project IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<DesktopProjectSummary>("inspect_aicad_project", { request });
    },

    async applyAicadPatch(request: ApplyAicadPatchIpcRequest): Promise<ProjectPatchResult> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("apply_aicad_patch IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<ProjectPatchResult>("apply_aicad_patch", { request });
    },

    async emitAicadKcl(request: EmitAicadKclIpcRequest): Promise<string> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("emit_aicad_kcl IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<string>("emit_aicad_kcl", { request });
    },

    async branchAicadProject(request: BranchAicadProjectIpcRequest): Promise<DesktopBranchResponse> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("branch_aicad_project IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<DesktopBranchResponse>("branch_aicad_project", { request });
    },

    async sliceAicadContext(request: SliceAicadContextIpcRequest): Promise<DesktopContextSliceResponse> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("slice_aicad_context IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<DesktopContextSliceResponse>("slice_aicad_context", { request });
    },

    async vacuumAicadProject(request: VacuumAicadProjectIpcRequest): Promise<DesktopVacuumReport> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("vacuum_aicad_project IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<DesktopVacuumReport>("vacuum_aicad_project", { request });
    },

    async packAndGoAicadProject(request: PackAndGoAicadProjectIpcRequest): Promise<DesktopPackAndGoResponse> {
      const invoker = getTauriInvoker();
      if (!invoker) throw new Error("pack_and_go_aicad_project IPC requires the Tauri v2 desktop runtime.");
      return invoker.invoke<DesktopPackAndGoResponse>("pack_and_go_aicad_project", { request });
    },
  };
}
