/**
 * L5 Frontend <-> L2 Compiler/Kernel Data Contracts
 * Strictly aligned with `schemas/scene-mesh.schema.json`,
 * `schemas/diagnostics.schema.json`, and `schemas/design-contract.schema.json`.
 */

import type { WorkspaceAsset as DesktopWorkspaceAsset } from "../../../desktop/src/ipcBridge.ts";

export interface WorkspaceAsset extends DesktopWorkspaceAsset {
  /** Open-shell preview expression (`allow_open_shell = true`) for reference mesh preview. */
  readonly previewExpression?: string;
  /** Standalone KCL wrapper using `previewExpression` for reference mesh preview. */
  readonly previewSource?: string;
}

export interface AssetInspectionSummary {
  readonly status?: string;
  readonly file?: string;
  readonly format?: string;
  readonly watertight?: boolean;
  readonly volume_mm3?: number;
  readonly [key: string]: unknown;
}

/** 1-based source location span matching Rust `cad-syntax` & `diagnostics.schema.json`. */
export interface SourceSpan {
  readonly start_line: number;
  readonly start_col: number;
  readonly end_line: number;
  readonly end_col: number;
  readonly byte_start?: number;
  readonly byte_end?: number;
}

export type SurfaceType =
  | "plane"
  | "cylinder"
  | "cone"
  | "sphere"
  | "torus"
  | "bspline"
  | "mesh";

export type CurveType =
  | "line"
  | "circle"
  | "arc"
  | "ellipse"
  | "bspline"
  | "polyline";

export type Vec3 = readonly [number, number, number];

export type ModelKind = "part" | "assembly";

export interface AssemblyComponentInfo {
  readonly component_id: string;
  readonly part_name: string;
  readonly instance_name?: string;
  readonly origin_mm?: Vec3;
  readonly rotation_deg?: Vec3;
  readonly volume_mm3?: number;
  readonly bbox_mm?: BoundingBox3D;
  readonly face_count?: number;
  readonly material?: string;
}

export interface FaceMesh {
  readonly face_id: string;
  readonly surface_type: SurfaceType | (string & {});
  /** Flattened XYZ vertex coordinates: [x0, y0, z0, x1, y1, z1, ...]. */
  readonly vertices: readonly number[] | Float64Array;
  /** Flattened XYZ vertex normals aligned 1:1 with `vertices`. */
  readonly normals: readonly number[] | Float64Array;
  /** Triangle vertex index buffer (3 indices per triangle). */
  readonly indices: readonly number[] | Uint32Array;
  /** Semantic provenance tags inherited across boolean and split ops. */
  readonly provenance_tags: readonly string[];
  readonly exact_interface?: boolean;
  readonly component_id?: string;
  readonly part_name?: string;
  readonly ast_span: SourceSpan;
}

export interface TopoEdge {
  readonly edge_id: string;
  readonly curve_type: CurveType | (string & {});
  /** Sampled 3D polyline points as flat [x0, y0, z0, ...] or [[x, y, z], ...]. */
  readonly polyline: readonly number[] | Float64Array | readonly Vec3[];
  readonly provenance_tags: readonly string[];
  readonly component_id?: string;
  readonly ast_span: SourceSpan;
}

/**
 * Bidirectional View-Update Synchronization Tiers:
 * - Tier 1: Direct literal / top-level parameter (solid blue handle, lossless in-place rewrite)
 * - Tier 2: Single-variable affine offset (dual-ring handle, choose offset constant vs upstream param)
 * - Tier 3: Coupled multi-variable expression (locked grey handle, Extract Parameter refactor)
 */
export type EditHandleTier = 1 | 2 | 3;

export interface Tier2AffineBinding {
  /** Arithmetic operator in `upstream (+ | -) offset`. */
  readonly operator: "+" | "-";
  readonly upstream_param: string;
  readonly upstream_value: number;
  readonly upstream_unit: string;
  /** Span covering the upstream parameter's literal value in source. */
  readonly upstream_span: SourceSpan;
  readonly offset_value: number;
  readonly offset_unit: string;
  /** Span covering the offset literal in the affine expression. */
  readonly offset_span: SourceSpan;
}

export interface Tier3CoupledBinding {
  /** Raw RHS expression text that cannot be blindly inverted. */
  readonly expression_text: string;
  /** Referenced upstream symbols inside the coupled expression. */
  readonly dependencies: readonly string[];
  /** Suggested top-level / local parameter identifier for Extract Parameter refactor. */
  readonly suggested_param_name: string;
  /** Span covering the full statement or RHS expression to be refactored. */
  readonly statement_span: SourceSpan;
  /** Span covering specifically the RHS expression to replace with the extracted identifier. */
  readonly rhs_span: SourceSpan;
}

export interface EditHandle {
  readonly handle_id: string;
  readonly param_name: string;
  readonly tier: EditHandleTier;
  readonly current_value: number;
  readonly unit: string;
  readonly ast_span: SourceSpan;
  /** Optional schema & UI metadata for 3D gizmo placement and bounds. */
  readonly label?: string;
  readonly min_value?: number;
  readonly max_value?: number;
  readonly anchor_point?: Vec3;
  readonly drag_axis?: Vec3;
  readonly upstream_param?: string;
  readonly offset_value?: number;
  readonly expression?: string;
  readonly component_id?: string;
  readonly tier2_binding?: Tier2AffineBinding;
  readonly tier3_binding?: Tier3CoupledBinding;
}

export interface HoleOffsetHandle {
  readonly face_id: string;
  readonly radius_mm: number;
  /** Selector suffix to append to the binding for the current solid. */
  readonly selector: string;
  /** UTF-8 byte offsets from the start of `return` to the exclusive end of its expression. */
  readonly return_start: number;
  readonly expression_end: number;
}

export interface SceneMeshPayload {
  readonly format_version: "0.1";
  readonly part_name: string;
  readonly kind?: ModelKind;
  readonly units: "mm" | "m" | "in";
  readonly components?: readonly AssemblyComponentInfo[];
  readonly faces: readonly FaceMesh[];
  readonly edges: readonly TopoEdge[];
  readonly edit_handles: readonly EditHandle[];
  readonly hole_offsets?: readonly HoleOffsetHandle[];
}

export type DiagnosticSeverity = "error" | "warning" | "info";

export interface CompilerDiagnostic {
  readonly code: string;
  readonly severity: DiagnosticSeverity;
  readonly message: string;
  readonly hint?: string;
  readonly rule_layer?: "L0" | "L1" | "L2" | "L3" | "L4";
  readonly span: SourceSpan;
}

export interface BoundingBox3D {
  readonly min: Vec3;
  readonly max: Vec3;
}

export interface GeometryMetrics {
  readonly watertight: boolean;
  readonly volume_mm3: number;
  readonly surface_area_mm2?: number;
  readonly bbox_mm: BoundingBox3D;
  readonly face_count: number;
  readonly edge_count: number;
  readonly open_edge_count?: number;
  readonly compile_time_ms?: number;
  readonly kernel_backend?: string;
  readonly kernel_authoritative?: boolean;
  readonly kernel_fallback_reason?: string;
}

export interface DiagnosticsReport {
  readonly status: "ok" | "error";
  readonly file: string;
  readonly part_name?: string;
  readonly kind?: ModelKind;
  readonly components?: readonly AssemblyComponentInfo[];
  readonly diagnostics: readonly CompilerDiagnostic[];
  readonly metrics: GeometryMetrics;
}

export interface KeepOutZone {
  readonly zone_id: string;
  readonly kind: "box" | "cylinder" | "sphere";
  readonly origin_mm?: Vec3;
  readonly diameter_mm?: number;
  readonly height_mm?: number;
  readonly size_mm?: Vec3;
  readonly description?: string;
}

export interface DesignInterfaceSpec {
  readonly interface_id: string;
  readonly kind:
    | "bore"
    | "shaft_seat"
    | "mounting_face"
    | "bolt_pattern"
    | "counterbore_pattern"
    | "slot"
    | "datum";
  readonly diameter_mm?: number;
  readonly tolerance?: string;
  readonly roughness?: string;
  readonly preserve_exact_brep?: boolean;
  readonly notes?: string;
}

export interface DesignLoadSpec {
  readonly load_id: string;
  readonly kind: "force" | "torque" | "pressure" | "fixed_support";
  readonly target_interface?: string;
  readonly magnitude?: number;
  readonly unit?: "N" | "kN" | "N*m" | "MPa";
  readonly vector?: Vec3;
  readonly min_safety_factor?: number;
}

export interface DesignContract {
  readonly contract_version?: string;
  readonly part_name: string;
  readonly summary?: string;
  readonly units:
    | "mm"
    | "m"
    | "in"
    | { readonly length: "mm" | "m" | "in"; readonly angle?: "deg" | "rad"; readonly force?: "N" | "kN" };
  readonly material:
    | string
    | {
        readonly name: string;
        readonly yield_strength_mpa?: number;
        readonly elastic_modulus_gpa?: number;
        readonly density_g_cm3?: number;
      };
  readonly manufacturing_method: string;
  readonly keep_in_bbox_mm: BoundingBox3D;
  readonly keep_out_zones: readonly KeepOutZone[];
  readonly interfaces: readonly DesignInterfaceSpec[];
  readonly loads: readonly DesignLoadSpec[];
}

export interface MaterialPreset {
  readonly id: string;
  readonly name: string;
  readonly density_g_cm3: number;
  readonly yield_strength_mpa: number;
  readonly machinability_index: number;
}

export interface LspCompletionItem {
  readonly label: string;
  readonly kind: "operator" | "namespace" | "variable" | "keyword" | "type";
  readonly detail: string;
  readonly insert_text: string;
}

export interface LspHoverInfo {
  readonly symbol: string;
  readonly dimension_type: string;
  readonly summary: string;
  readonly span?: SourceSpan;
}

export interface LspQueryResponse {
  readonly backend: "cad-cli-lsp" | "builtin-analyzer";
  readonly completions: readonly LspCompletionItem[];
  readonly hover: LspHoverInfo | null;
}

export interface SectionCutState {
  readonly enabled: boolean;
  readonly axis: "X" | "Y" | "Z";
  /** Normalized depth ratio in [0, 1] across the active bounding box axis. */
  readonly ratio: number;
  /** Flip which half-space is kept by the clipping plane. */
  readonly flip?: boolean;
}

export type SelectionTarget =
  | { readonly kind: "face"; readonly face: FaceMesh }
  | { readonly kind: "edge"; readonly edge: TopoEdge }
  | null;

export type AiProvider = "auto" | "template" | "openai" | "gemini";

export type AiPresetId =
  | "openai"
  | "gemini"
  | "deepseek"
  | "openrouter"
  | "ollama"
  | "custom"
  | "template";

export type AiGenerateMode = "generate" | "edit";

export type AiLlmConfig = {
  provider: AiProvider;
  model?: string;
  base_url?: string;
  api_key?: string;
  readonly profile_id?: string;
  readonly enable_vlm?: boolean;
  readonly few_shot_k?: number;
};

export type AiRequestOptions = {
  mode: AiGenerateMode;
  llm: AiLlmConfig;
  readonly selectedTopoRefs?: readonly string[];
  readonly selectedNodeIds?: readonly string[];
};

export type AiConfigStatus = {
  default_provider: string;
  has_openai_key: boolean;
  has_gemini_key: boolean;
  openai_base_url?: string;
  openai_model?: string;
  gemini_model?: string;
};

export interface AiProfile {
  readonly id: string;
  readonly name: string;
  readonly preset: AiPresetId;
  readonly provider: AiProvider;
  readonly model: string;
  readonly baseUrl: string;
  readonly hasApiKey: boolean;
  readonly apiKeyMask: string;
  /** Used only in browser-local fallback mode when no backend profile store is reachable. */
  readonly localApiKey?: string;
  readonly enableVlm: boolean;
  readonly fewShotK: number;
  readonly updatedAt?: string;
}

export interface AiProfileDraftInput {
  readonly id: string;
  readonly name: string;
  readonly preset: AiPresetId;
  readonly provider: AiProvider;
  readonly model: string;
  readonly baseUrl: string;
  readonly apiKey?: string;
  readonly clearApiKey?: boolean;
  readonly enableVlm: boolean;
  readonly fewShotK: number;
}

export interface AiProfilesResponse {
  readonly storage: "server-file" | "browser-local";
  readonly activeProfileId: string | null;
  readonly profiles: readonly AiProfile[];
  readonly envStatus?: AiConfigStatus | null;
}

export interface AiTestConnectionPayload {
  readonly profileId?: string;
  readonly provider: AiProvider;
  readonly model?: string;
  readonly baseUrl?: string;
  readonly apiKey?: string;
  readonly clearApiKey?: boolean;
}

export interface AiTestConnectionResult {
  readonly ok: boolean;
  readonly provider: string;
  readonly model?: string;
  readonly status?: number;
  readonly latencyMs?: number;
  readonly message: string;
}

export interface AiProgressStep {
  readonly stage: string;
  readonly message: string;
  readonly iteration?: number;
  readonly max_iters?: number;
  readonly passed?: boolean;
  readonly timestamp?: number;
}

export interface AiRunFeedback {
  readonly status: "success" | "error";
  readonly message: string;
  readonly detail?: string;
  readonly vlmEnabled?: boolean;
  readonly retrievedExamples?: readonly string[];
  readonly contractRepairAttempts?: number;
  readonly durationMs?: number;
  readonly beforeSource?: string;
  readonly afterSource?: string;
  readonly beforeMetrics?: GeometryMetrics;
  readonly afterMetrics?: GeometryMetrics;
}
