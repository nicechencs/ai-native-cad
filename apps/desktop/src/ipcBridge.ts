import type {
  DiagnosticsReport,
  SceneMeshPayload,
} from "../../studio-ui/src/types/scene.ts";

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
}

export interface BuildArtifactResponse {
  readonly diagnostics: DiagnosticsReport;
  readonly scene_mesh: SceneMeshPayload;
  readonly elapsed_ms: number;
}

export interface ExportRequest {
  readonly file_name: string;
  readonly source: string;
  readonly out_path: string;
  readonly include_pmi?: boolean;
  readonly views?: readonly string[];
}

export interface ExportResult {
  readonly format: "STEP_AP242" | "DXF" | "SCENE_MESH_JSON";
  readonly out_path: string;
  readonly bytes_written: number;
  readonly pmi_entity_count: number;
  readonly message: string;
}

interface TauriInternals {
  invoke<T>(cmd: string, args?: Readonly<Record<string, unknown>>): Promise<T>;
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

function getTauriInvoker(): TauriInternals | null {
  if (!isTauriRuntime()) {
    return null;
  }
  const win = window as unknown as WindowWithTauri;
  return win.__TAURI_INTERNALS__ ?? null;
}

export interface DesktopIpcClient {
  getEngineStatus(): Promise<EngineStatus>;
  compileKcl(request: CompileKclRequest): Promise<DiagnosticsReport>;
  buildCadArtifact(request: CompileKclRequest): Promise<BuildArtifactResponse>;
  exportStepAp242(request: ExportRequest): Promise<ExportResult>;
  exportDxfDrawing(request: ExportRequest): Promise<ExportResult>;
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

    async exportStepAp242(request: ExportRequest): Promise<ExportResult> {
      const invoker = getTauriInvoker();
      if (invoker) {
        return invoker.invoke<ExportResult>("export_step_ap242", { request });
      }
      return {
        format: "STEP_AP242",
        out_path: request.out_path,
        bytes_written: 0,
        pmi_entity_count: 2,
        message: "浏览器独立预览模式：已生成 STEP AP242 导出任务描述（需在 Tauri 桌面端调用本地 OCCT 写出物理文件）。",
      };
    },

    async exportDxfDrawing(request: ExportRequest): Promise<ExportResult> {
      const invoker = getTauriInvoker();
      if (invoker) {
        return invoker.invoke<ExportResult>("export_dxf_drawing", { request });
      }
      return {
        format: "DXF",
        out_path: request.out_path,
        bytes_written: 0,
        pmi_entity_count: 0,
        message: "浏览器独立预览模式：已生成正交三视图 DXF 导出任务描述（需在 Tauri 桌面端生成）。",
      };
    },
  };
}
