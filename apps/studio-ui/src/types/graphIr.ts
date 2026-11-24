/**
 * Canonical Graph IR contracts for the `.aicad` workbench.
 * Wire shape matches `schemas/graph-ir.schema.json` and `crates/cad-ir`
 * (serde externally adjacent `GraphPatch` ops, internally tagged node state).
 */

/** Workbench view of `cad-container` cache drift. Rust also stores kernel fingerprints on drift; the panel only needs the discriminant. */
export type CacheStatus =
  | "fresh_exact_match"
  | "kernel_version_drift"
  | "graph_modified"
  | "missing";

export type ProjectStorageKind =
  | "directory"
  | "sharded_directory"
  | "sqlite";

export type KclProjectionPolicy =
  | "on_demand_dynamic"
  | "materialized_synced";

export type PayloadPolicy = "eager" | "deferred_payload";

export const IR_SCHEMA_VERSION = 1;

export type IrType =
  | "scalar"
  | "int"
  | "bool"
  | "string"
  | "length"
  | "angle"
  | "force"
  | "pressure"
  | "mass"
  | "density"
  | "vec2_length"
  | "vec3_length"
  | "vec3_dir"
  | "solid"
  | "surface"
  | "subd"
  | "scalar_field"
  | "vector_field"
  | "sketch"
  | "assembly"
  | "component"
  | "topo_ref"
  | "blob_ref"
  | "external_ref";

export type TpmsFamily = "gyroid" | "schwarz_p" | "diamond" | "lidinoid";

export type LengthUnit = "mm" | "m" | "in" | "um" | "cm";
export type AngleUnit = "deg" | "rad";
export type ForceUnit = "N" | "kN";
export type PressureUnit = "MPa" | "Pa";
export type MassUnit = "g" | "kg";

export interface UnitSystem {
  readonly length: LengthUnit;
  readonly angle: AngleUnit;
  readonly force: ForceUnit;
  readonly pressure: PressureUnit;
  readonly mass: MassUnit;
}

export type Quantity =
  | { readonly dimension: "length"; readonly value: number; readonly unit: LengthUnit }
  | { readonly dimension: "angle"; readonly value: number; readonly unit: AngleUnit }
  | { readonly dimension: "force"; readonly value: number; readonly unit: ForceUnit }
  | { readonly dimension: "pressure"; readonly value: number; readonly unit: PressureUnit }
  | { readonly dimension: "mass"; readonly value: number; readonly unit: MassUnit }
  | { readonly dimension: "density"; readonly value: number };

/** v1 nodes expose a single output port named `out`. */
export interface NodeRef {
  readonly node_id: string;
  readonly port: "out";
}

export type IrValue =
  | { readonly type: "quantity"; readonly value: Quantity }
  | { readonly type: "scalar"; readonly value: number }
  | { readonly type: "int"; readonly value: number }
  | { readonly type: "bool"; readonly value: boolean }
  | { readonly type: "string"; readonly value: string }
  | { readonly type: "vec3"; readonly value: readonly [number, number, number] }
  | { readonly type: "node_ref"; readonly value: NodeRef }
  | { readonly type: "blob_ref"; readonly value: string }
  | { readonly type: "external_ref"; readonly value: string }
  | { readonly type: "topo_ref_expr"; readonly value: string }
  | { readonly type: "list"; readonly value: readonly IrValue[] }
  | { readonly type: "map"; readonly value: Readonly<Record<string, IrValue>> };

export interface ParameterDef {
  readonly name: string;
  readonly ir_type: IrType;
  readonly value: IrValue;
  /** Absent or null means the parameter is a literal with no source formula. */
  readonly formula?: string | null;
}

export type BlobKind = "spline" | "subd" | "field" | "import";

export interface BlobManifestEntry {
  readonly handle: string;
  readonly kind: BlobKind;
  readonly sha256: string;
  readonly byte_len: number;
  readonly relative_path: string;
}

export interface ExternalRefDef {
  readonly ref_id: string;
  readonly container_path: string;
  readonly expected_sha256?: string | null;
  readonly target_graph_id?: string | null;
  readonly payload_policy?: PayloadPolicy;
  readonly overrides?: Readonly<Record<string, IrValue>>;
}

export type UnitNodeKind =
  | "sketch_2d"
  | "primitive_box"
  | "primitive_cylinder"
  | "extrude"
  | "revolve"
  | "sweep"
  | "loft_guided"
  | "shell"
  | "fillet"
  | "chamfer"
  | "draft"
  | "mirror"
  | "pattern_linear"
  | "pattern_polar"
  | "boolean_union"
  | "boolean_subtract"
  | "boolean_intersect"
  | "hole"
  | "direct_face_edit"
  | "surface_boundary"
  | "surface_trim"
  | "surface_knit_to_solid"
  | "subd_from_blob"
  | "subd_to_nurbs"
  | "field_sdf"
  | "field_ramp"
  | "field_from_fea_blob"
  | "implicit_smooth_blend"
  | "implicit_lattice_infill"
  | "std_lib_call"
  | "import_asset"
  | "assembly_component"
  | "assembly_mate"
  | "tag_faces"
  | "assert_gate";

export type NodeKind =
  | { readonly kind: "parameter_ref"; readonly param_id: string }
  | { readonly kind: "field_tpms"; readonly family: TpmsFamily }
  | { readonly kind: UnitNodeKind };

/**
 * Evaluator state for one feature node.
 * `failed_with_rollback` keeps the upstream solid on screen and lists the edges or faces to mark in red.
 * Unsuppress writes `stale`, not `valid`: the node must be recomputed.
 */
export type NodeState =
  | { readonly status: "valid" }
  | {
      readonly status: "failed_with_rollback";
      readonly reason: string;
      readonly failed_topo_refs: readonly string[];
      /** Optional repair hint. Metadata `recovery_hint` is the fallback when this is absent. */
      readonly recovery_hint?: string | null;
    }
  | { readonly status: "suppressed" }
  | { readonly status: "stale" };

export interface IrNode {
  readonly id: string;
  readonly name: string;
  readonly kind: NodeKind;
  readonly inputs: Readonly<Record<string, IrValue>>;
  readonly output_type: IrType;
  readonly state: NodeState;
  readonly kernel_op_version?: string | null;
  readonly metadata: Readonly<Record<string, string>>;
}

export interface CanonicalGraph {
  readonly ir_schema_version?: number;
  readonly graph_id: string;
  readonly units: UnitSystem;
  readonly parameters: Readonly<Record<string, ParameterDef>>;
  readonly blobs: Readonly<Record<string, BlobManifestEntry>>;
  readonly external_refs?: Readonly<Record<string, ExternalRefDef>>;
  readonly nodes: Readonly<Record<string, IrNode>>;
  readonly outputs: readonly string[];
}

/**
 * One atomic edit. `op` + `value` matches `cad-ir` `PatchOp` (adjacently tagged serde).
 * `update_node` uses null for fields that stay unchanged.
 */
export type PatchOp =
  | { readonly op: "upsert_parameter"; readonly value: ParameterDef }
  | { readonly op: "remove_parameter"; readonly value: { readonly name: string } }
  | { readonly op: "upsert_blob"; readonly value: BlobManifestEntry }
  | { readonly op: "remove_blob"; readonly value: { readonly handle: string } }
  | { readonly op: "upsert_external_ref"; readonly value: ExternalRefDef }
  | { readonly op: "remove_external_ref"; readonly value: { readonly ref_id: string } }
  | { readonly op: "add_node"; readonly value: IrNode }
  | {
      readonly op: "update_node";
      readonly value: {
        readonly id: string;
        readonly name: string | null;
        readonly inputs: Readonly<Record<string, IrValue>> | null;
        readonly kind: NodeKind | null;
      };
    }
  | { readonly op: "remove_node"; readonly value: { readonly id: string } }
  | { readonly op: "set_node_state"; readonly value: { readonly id: string; readonly state: NodeState } }
  | { readonly op: "set_outputs"; readonly value: readonly string[] };

export interface GraphPatch {
  readonly patch_id: string;
  readonly description: string;
  readonly ops: readonly PatchOp[];
}
