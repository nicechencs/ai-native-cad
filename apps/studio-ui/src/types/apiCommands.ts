/**
 * Strongly typed `cad-api::SemanticCommand` wire contracts.
 *
 * Mirrors `crates/cad-api/src/command.rs` and its referenced `crates/cad-ir`
 * types 1:1 (adjacently tagged `{"command":"...","args":{...}}` with snake_case
 * variants and `deny_unknown_fields`).
 *
 * These commands are the authoritative write boundary for Live-Attach and
 * `cad-cli api apply`. UI/session-only actions live in
 * `WorkbenchSessionCommand` (`src/core/session/engineeringSession.ts`).
 */

import type { AngleUnit, IrType, IrValue, LengthUnit, Quantity } from "./graphIr.ts";

export type Vec2 = readonly [number, number];
export type Vec3 = readonly [number, number, number];

export interface ApiDesignContract {
  readonly part_name: string;
  readonly summary?: string | null;
  readonly material?: string | null;
  readonly manufacturing_method?: string | null;
  readonly attributes?: Readonly<Record<string, string>>;
}

export type FastenerClearance = "close" | "normal" | "loose";

export type FastenerHoleStandard =
  | "iso4762_counterbore"
  | "iso10642_countersink"
  | "metric_tap_drill"
  | "metric_clearance";

export type DimInput =
  | (Quantity & { readonly source: "literal" })
  | { readonly source: "param"; readonly param_id: string }
  | { readonly source: "node_output"; readonly node_id: string; readonly port?: string };

export type IntInput =
  | { readonly source: "literal"; readonly value: number }
  | { readonly source: "param"; readonly param_id: string }
  | { readonly source: "node_output"; readonly node_id: string; readonly port?: string };

export type FeatureBodyOp = "new_body" | "join" | "cut" | "intersect";

export type HoleDepthSpec =
  | { readonly kind: "through_all" }
  | { readonly kind: "blind"; readonly depth: DimInput };

export type HoleProfileSpec =
  | { readonly kind: "simple" }
  | { readonly kind: "counterbore"; readonly bore_diameter: DimInput; readonly bore_depth: DimInput }
  | { readonly kind: "countersink"; readonly sink_diameter: DimInput; readonly sink_angle: DimInput };

export type HoleDefinition =
  | {
      readonly hole: "generic";
      readonly diameter: DimInput;
      readonly depth: HoleDepthSpec;
      readonly profile: HoleProfileSpec;
    }
  | {
      readonly hole: "standard";
      readonly standard: FastenerHoleStandard;
      readonly designation: string;
      readonly clearance_class?: FastenerClearance | null;
      readonly depth: HoleDepthSpec;
      readonly thread_depth?: DimInput | null;
    };

export interface LinearPatternAxisSpec {
  readonly direction: Vec3;
  readonly count: IntInput;
  readonly spacing: DimInput;
}

export type PatternDefinition =
  | {
      readonly pattern: "linear";
      readonly direction: Vec3;
      readonly count: IntInput;
      readonly spacing: DimInput;
      readonly second_direction?: LinearPatternAxisSpec | null;
    }
  | {
      readonly pattern: "polar";
      readonly axis_origin_mm?: Vec3;
      readonly axis_direction?: Vec3;
      readonly count: IntInput;
      readonly total_angle: DimInput;
    };

export type SweepPathInput =
  | { readonly path: "sketch"; readonly node_id: string }
  | { readonly path: "polyline"; readonly points_mm: readonly Vec3[] };

export type BooleanOpKind = "union" | "subtract" | "intersect";

export type DirectFaceEditOp =
  | { readonly op: "offset"; readonly distance: DimInput }
  | { readonly op: "move"; readonly delta_mm: Vec3 }
  | { readonly op: "delete_and_heal" };

export type ComponentSourceSpec =
  | { readonly kind: "local_node"; readonly node_id: string }
  | { readonly kind: "external_ref"; readonly ref_id: string };

export type MateKindSpec =
  | { readonly kind: "coincident" }
  | { readonly kind: "concentric" }
  | { readonly kind: "distance"; readonly value: DimInput }
  | { readonly kind: "angle"; readonly value: DimInput }
  | { readonly kind: "parallel" }
  | { readonly kind: "perpendicular" }
  | { readonly kind: "gear"; readonly ratio: number };

export type DesignAssertionKind =
  | {
      readonly rule: "volume_bounds";
      readonly min_mm3?: number | null;
      readonly max_mm3?: number | null;
    }
  | { readonly rule: "min_wall_thickness"; readonly min: DimInput }
  | { readonly rule: "boolean_condition"; readonly condition: string }
  | {
      readonly rule: "custom";
      readonly expression: string;
      readonly params?: Readonly<Record<string, string>>;
    };

export type DatumPlane = "xy" | "xz" | "yz";

export type SketchDimValue =
  | (Quantity & { readonly binding: "literal" })
  | { readonly binding: "param"; readonly param_id: string };

export type SketchPlaneSpec =
  | { readonly kind: "datum"; readonly plane: DatumPlane; readonly offset?: SketchDimValue | null }
  | { readonly kind: "face_ref"; readonly face: string; readonly offset?: SketchDimValue | null }
  | {
      readonly kind: "custom";
      readonly origin_mm: Vec3;
      readonly normal: Vec3;
      readonly x_axis?: Vec3 | null;
    };

export type SketchEntity2D =
  | {
      readonly kind: "point";
      readonly id: string;
      readonly position_mm: Vec2;
      readonly fixed?: boolean;
      readonly construction?: boolean;
    }
  | {
      readonly kind: "line";
      readonly id: string;
      readonly start_mm: Vec2;
      readonly end_mm: Vec2;
      readonly construction?: boolean;
    }
  | {
      readonly kind: "arc";
      readonly id: string;
      readonly center_mm: Vec2;
      readonly start_mm: Vec2;
      readonly end_mm: Vec2;
      readonly ccw?: boolean;
      readonly construction?: boolean;
    }
  | {
      readonly kind: "circle";
      readonly id: string;
      readonly center_mm: Vec2;
      readonly radius_mm: number;
      readonly construction?: boolean;
    }
  | {
      readonly kind: "rectangle";
      readonly id: string;
      readonly origin_mm: Vec2;
      readonly width_mm: number;
      readonly height_mm: number;
      readonly centered?: boolean;
      readonly construction?: boolean;
    }
  | {
      readonly kind: "spline_blob";
      readonly id: string;
      readonly blob: string;
      readonly closed?: boolean;
      readonly construction?: boolean;
    };

export interface SketchProfileLoop {
  readonly id: string;
  readonly entity_ids: readonly string[];
  readonly is_outer?: boolean;
}

export type SketchAnchor = "point" | "start" | "end" | "center" | "midpoint";

export interface SketchPointRef {
  readonly entity_id: string;
  readonly anchor?: SketchAnchor;
}

export type SketchConstraint2D =
  | {
      readonly kind: "coincident";
      readonly name?: string | null;
      readonly a: SketchPointRef;
      readonly b: SketchPointRef;
    }
  | {
      readonly kind: "point_on_line";
      readonly name?: string | null;
      readonly point: SketchPointRef;
      readonly line: string;
    }
  | { readonly kind: "horizontal"; readonly name?: string | null; readonly entity: string }
  | { readonly kind: "vertical"; readonly name?: string | null; readonly entity: string }
  | {
      readonly kind: "parallel";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
    }
  | {
      readonly kind: "perpendicular";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
    }
  | {
      readonly kind: "tangent";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
    }
  | {
      readonly kind: "concentric";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
    }
  | {
      readonly kind: "equal";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
    }
  | {
      readonly kind: "symmetric";
      readonly name?: string | null;
      readonly a: SketchPointRef;
      readonly b: SketchPointRef;
      readonly axis_line: string;
    }
  | {
      readonly kind: "distance";
      readonly name?: string | null;
      readonly a: SketchPointRef;
      readonly b: SketchPointRef;
      readonly value: SketchDimValue;
    }
  | {
      readonly kind: "radius";
      readonly name?: string | null;
      readonly entity: string;
      readonly value: SketchDimValue;
    }
  | {
      readonly kind: "diameter";
      readonly name?: string | null;
      readonly entity: string;
      readonly value: SketchDimValue;
    }
  | {
      readonly kind: "angle";
      readonly name?: string | null;
      readonly a: string;
      readonly b: string;
      readonly value: SketchDimValue;
    };

export interface UpsertParameterCommandArgs {
  readonly name: string;
  readonly ir_type: IrType;
  readonly value: IrValue;
  readonly formula?: string | null;
}

export interface SetFeatureInputCommandArgs {
  readonly node_id: string;
  readonly input: string;
  readonly length_mm?: number;
  readonly vector_mm?: Vec3;
}

export interface RemoveParameterCommandArgs {
  readonly name: string;
}

export interface SetDesignContractCommandArgs {
  readonly contract: ApiDesignContract;
}

export interface CreateSketchCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly plane: SketchPlaneSpec;
  readonly entities?: readonly SketchEntity2D[];
  readonly profiles?: readonly SketchProfileLoop[];
  readonly constraints?: readonly SketchConstraint2D[];
}

export interface UpsertSketchEntitiesCommandArgs {
  readonly node_id: string;
  readonly entities: readonly SketchEntity2D[];
  readonly replace?: boolean;
}

export interface UpsertSketchConstraintsCommandArgs {
  readonly node_id: string;
  readonly constraints: readonly SketchConstraint2D[];
  readonly replace?: boolean;
}

export interface CreatePrimitiveBoxCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly width: DimInput;
  readonly depth: DimInput;
  readonly height: DimInput;
  readonly origin_mm?: Vec3;
}

export interface CreatePrimitiveCylinderCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly radius: DimInput;
  readonly height: DimInput;
  readonly origin_mm?: Vec3;
  readonly axis?: Vec3;
}

export interface ExtrudeSketchCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly sketch: string;
  readonly distance: DimInput;
  readonly direction?: Vec3 | null;
  readonly symmetric?: boolean;
  readonly operation?: FeatureBodyOp;
  readonly target?: string | null;
}

export interface RevolveSketchCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly sketch: string;
  readonly axis_origin_mm?: Vec3;
  readonly axis_direction?: Vec3;
  readonly angle: DimInput;
  readonly operation?: FeatureBodyOp;
  readonly target?: string | null;
}

export interface SweepProfileCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly profile: string;
  readonly path: SweepPathInput;
  readonly keep_normal?: boolean;
}

export interface LoftSectionsCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly sections: readonly string[];
  readonly guides?: readonly string[];
  readonly closed?: boolean;
  readonly as_surface?: boolean;
}

export interface CreateHoleCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly target_face?: string | null;
  readonly origin_mm?: Vec3;
  readonly axis?: Vec3;
  readonly definition: HoleDefinition;
}

export interface FilletEdgesCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly edges?: readonly string[];
  readonly radius: DimInput;
}

export interface ChamferEdgesCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly edges?: readonly string[];
  readonly distance: DimInput;
  readonly distance2?: DimInput | null;
  readonly angle?: DimInput | null;
}

export interface ShellSolidCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly open_faces?: readonly string[];
  readonly thickness: DimInput;
  readonly inward?: boolean;
}

export interface DraftFacesCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly faces: readonly string[];
  readonly pull_direction: Vec3;
  readonly neutral_plane: SketchPlaneSpec;
  readonly angle: DimInput;
}

export interface PatternFeatureCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly definition: PatternDefinition;
}

export interface MirrorFeatureCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly plane: SketchPlaneSpec;
  readonly keep_original?: boolean;
}

export interface BooleanSolidsCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly op: BooleanOpKind;
  readonly target: string;
  readonly tools: readonly string[];
}

export interface DirectFaceEditCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly face: string;
  readonly edit: DirectFaceEditOp;
}

export interface InsertComponentCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly source: ComponentSourceSpec;
  readonly translation_mm?: Vec3;
  readonly rotation_deg?: Vec3;
  readonly grounded?: boolean;
}

export interface AddMateCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly left: string;
  readonly right: string;
  readonly left_ref: string;
  readonly right_ref: string;
  readonly mate_kind: MateKindSpec;
  readonly flip?: boolean;
}

export interface RenameNodeCommandArgs {
  readonly node_id: string;
  readonly name: string;
}

export interface SetNodeSuppressedCommandArgs {
  readonly node_id: string;
  readonly suppressed: boolean;
}

export interface DeleteNodeCommandArgs {
  readonly node_id: string;
}

export interface ReorderFeatureCommandArgs {
  readonly node_id: string;
  readonly before?: string | null;
}

export interface TagTopologyCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly solid: string;
  readonly tag: string;
  readonly targets?: readonly string[];
}

export interface AddDesignAssertionCommandArgs {
  readonly node_id?: string | null;
  readonly name: string;
  readonly target?: string | null;
  readonly assertion_id: string;
  readonly kind: DesignAssertionKind;
}

/**
 * 1:1 TypeScript union matching `crates/cad-api/src/command.rs::SemanticCommand`.
 */
export type CadApiSemanticCommand =
  | { readonly command: "upsert_parameter"; readonly args: UpsertParameterCommandArgs }
  | { readonly command: "set_feature_input"; readonly args: SetFeatureInputCommandArgs }
  | { readonly command: "remove_parameter"; readonly args: RemoveParameterCommandArgs }
  | { readonly command: "set_design_contract"; readonly args: SetDesignContractCommandArgs }
  | { readonly command: "create_sketch"; readonly args: CreateSketchCommandArgs }
  | { readonly command: "upsert_sketch_entities"; readonly args: UpsertSketchEntitiesCommandArgs }
  | {
      readonly command: "upsert_sketch_constraints";
      readonly args: UpsertSketchConstraintsCommandArgs;
    }
  | { readonly command: "create_primitive_box"; readonly args: CreatePrimitiveBoxCommandArgs }
  | {
      readonly command: "create_primitive_cylinder";
      readonly args: CreatePrimitiveCylinderCommandArgs;
    }
  | { readonly command: "extrude_sketch"; readonly args: ExtrudeSketchCommandArgs }
  | { readonly command: "revolve_sketch"; readonly args: RevolveSketchCommandArgs }
  | { readonly command: "sweep_profile"; readonly args: SweepProfileCommandArgs }
  | { readonly command: "loft_sections"; readonly args: LoftSectionsCommandArgs }
  | { readonly command: "create_hole"; readonly args: CreateHoleCommandArgs }
  | { readonly command: "fillet_edges"; readonly args: FilletEdgesCommandArgs }
  | { readonly command: "chamfer_edges"; readonly args: ChamferEdgesCommandArgs }
  | { readonly command: "shell_solid"; readonly args: ShellSolidCommandArgs }
  | { readonly command: "draft_faces"; readonly args: DraftFacesCommandArgs }
  | { readonly command: "pattern_feature"; readonly args: PatternFeatureCommandArgs }
  | { readonly command: "mirror_feature"; readonly args: MirrorFeatureCommandArgs }
  | { readonly command: "boolean_solids"; readonly args: BooleanSolidsCommandArgs }
  | { readonly command: "direct_face_edit"; readonly args: DirectFaceEditCommandArgs }
  | { readonly command: "insert_component"; readonly args: InsertComponentCommandArgs }
  | { readonly command: "add_mate"; readonly args: AddMateCommandArgs }
  | { readonly command: "rename_node"; readonly args: RenameNodeCommandArgs }
  | { readonly command: "set_node_suppressed"; readonly args: SetNodeSuppressedCommandArgs }
  | { readonly command: "delete_node"; readonly args: DeleteNodeCommandArgs }
  | { readonly command: "reorder_feature"; readonly args: ReorderFeatureCommandArgs }
  | { readonly command: "tag_topology"; readonly args: TagTopologyCommandArgs }
  | { readonly command: "add_design_assertion"; readonly args: AddDesignAssertionCommandArgs };

export type CadApiCommandName = CadApiSemanticCommand["command"];

export const CAD_API_COMMAND_NAMES = [
  "upsert_parameter",
  "set_feature_input",
  "remove_parameter",
  "set_design_contract",
  "create_sketch",
  "upsert_sketch_entities",
  "upsert_sketch_constraints",
  "create_primitive_box",
  "create_primitive_cylinder",
  "extrude_sketch",
  "revolve_sketch",
  "sweep_profile",
  "loft_sections",
  "create_hole",
  "fillet_edges",
  "chamfer_edges",
  "shell_solid",
  "draft_faces",
  "pattern_feature",
  "mirror_feature",
  "boolean_solids",
  "direct_face_edit",
  "insert_component",
  "add_mate",
  "rename_node",
  "set_node_suppressed",
  "delete_node",
  "reorder_feature",
  "tag_topology",
  "add_design_assertion",
] as const satisfies readonly CadApiCommandName[];

/** Convenience helper matching `DimInput::mm` in Rust. */
export function dimMm(value: number): DimInput {
  return { source: "literal", dimension: "length", value, unit: "mm" };
}

/** Convenience helper matching `DimInput::deg` in Rust. */
export function dimDeg(value: number): DimInput {
  return { source: "literal", dimension: "angle", value, unit: "deg" };
}

/** Convenience helper matching `DimInput::Literal(Quantity)`. */
export function dimLength(value: number, unit: LengthUnit = "mm"): DimInput {
  return { source: "literal", dimension: "length", value, unit };
}

/** Convenience helper matching `DimInput::Literal(Quantity)` for angles. */
export function dimAngle(value: number, unit: AngleUnit = "deg"): DimInput {
  return { source: "literal", dimension: "angle", value, unit };
}

/** Convenience helper matching `DimInput::param` in Rust. */
export function dimParam(paramId: string): DimInput {
  return { source: "param", param_id: paramId };
}
