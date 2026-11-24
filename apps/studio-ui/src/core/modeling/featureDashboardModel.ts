/**
 * Contextual Feature Dashboard state machine and Semantic CAD API / KCL / GraphPatch
 * execution builders (Creo Dashboard + NX Apply/OK/Preview hybrid paradigm).
 */
import {
  dimDeg,
  dimMm,
  type BooleanOpKind,
  type CadApiSemanticCommand,
  type DatumPlane,
  type DirectFaceEditOp,
  type FastenerClearance,
  type FastenerHoleStandard,
  type FeatureBodyOp,
  type HoleDefinition,
  type MateKindSpec,
  type SketchConstraint2D,
  type SketchEntity2D,
} from "../../types/apiCommands.ts";
import type { CanonicalGraph, GraphPatch, IrNode, IrValue, UnitNodeKind } from "../../types/graphIr.ts";
import { applyFaceQuickFeatureToKcl } from "../../utils/studioHelpers.ts";

export type FeatureDashboardKind =
  | "hole"
  | "fillet"
  | "chamfer"
  | "primitive_box"
  | "primitive_cylinder"
  | "extrude"
  | "revolve"
  | "shell"
  | "draft"
  | "pattern"
  | "mirror"
  | "boolean"
  | "direct_edit"
  | "mate"
  | "tag_topology"
  | "design_assertion"
  | "sketch";

export interface FeatureDashboardMeta {
  readonly kind: FeatureDashboardKind;
  readonly title: string;
  readonly collectorLabel: string;
  readonly collectorTarget: "face" | "edge" | "body" | "any" | "none";
  readonly allowsMultipleRefs: boolean;
  readonly supportsApplyContinuous: boolean;
  readonly cueEmpty: string;
  readonly cueReady: string;
}

export const FEATURE_DASHBOARD_META: Readonly<Record<FeatureDashboardKind, FeatureDashboardMeta>> = {
  hole: {
    kind: "hole",
    title: "孔特征 (Hole)",
    collectorLabel: "放置参考面",
    collectorTarget: "face",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "孔 [步骤 1/2]：请在 3D 视口中选择放置平面，或直接使用默认顶面 · 中键确认",
    cueReady: "孔 [步骤 2/2]：调整孔径、孔型或 ISO 标准规格 · 单击中键确认 / 点击应用连续打孔",
  },
  fillet: {
    kind: "fillet",
    title: "边缘圆角 (Fillet)",
    collectorLabel: "圆角边集合",
    collectorTarget: "edge",
    allowsMultipleRefs: true,
    supportsApplyContinuous: true,
    cueEmpty: "圆角 [步骤 1/2]：选择一条或多条拓扑边（按住 Ctrl 多选 / 切换切线链意图）",
    cueReady: "圆角 [步骤 2/2]：设定圆角半径 R，实时预览后单击中键确认",
  },
  chamfer: {
    kind: "chamfer",
    title: "边缘倒角 (Chamfer)",
    collectorLabel: "倒角边集合",
    collectorTarget: "edge",
    allowsMultipleRefs: true,
    supportsApplyContinuous: true,
    cueEmpty: "倒角 [步骤 1/2]：选择一条或多条拓扑边（支持 Ctrl 多选）",
    cueReady: "倒角 [步骤 2/2]：设定倒角距离 C（支持对称或双距模式），单击中键确认",
  },
  primitive_box: {
    kind: "primitive_box",
    title: "长方体基体 (Box)",
    collectorLabel: "放置基准",
    collectorTarget: "none",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "长方体：输入长 × 宽 × 高尺寸与原点坐标 · 单击中键创建",
    cueReady: "长方体：确认参数后单击中键创建基体实体",
  },
  primitive_cylinder: {
    kind: "primitive_cylinder",
    title: "圆柱体基体 (Cylinder)",
    collectorLabel: "放置基准",
    collectorTarget: "none",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "圆柱体：输入直径与高度参数 · 单击中键创建",
    cueReady: "圆柱体：确认参数后单击中键创建圆柱实体",
  },
  extrude: {
    kind: "extrude",
    title: "拉伸成形 (Extrude)",
    collectorLabel: "草图轮廓 / 参考面",
    collectorTarget: "face",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "拉伸 [步骤 1/2]：选择草图或平面作为拉伸轮廓",
    cueReady: "拉伸 [步骤 2/2]：设定拉伸距离、对称与布尔方式（新建/合并/切除）· 中键确认",
  },
  revolve: {
    kind: "revolve",
    title: "旋转成形 (Revolve)",
    collectorLabel: "旋转轮廓",
    collectorTarget: "face",
    allowsMultipleRefs: false,
    supportsApplyContinuous: false,
    cueEmpty: "旋转 [步骤 1/2]：选择草图轮廓并设定旋转轴",
    cueReady: "旋转 [步骤 2/2]：设定旋转角度与布尔方式 · 中键确认",
  },
  shell: {
    kind: "shell",
    title: "抽壳薄壁 (Shell)",
    collectorLabel: "移除开放面",
    collectorTarget: "face",
    allowsMultipleRefs: true,
    supportsApplyContinuous: false,
    cueEmpty: "抽壳 [步骤 1/2]：选择一个或多个要移除的开放面（Ctrl 多选）",
    cueReady: "抽壳 [步骤 2/2]：设定均匀壁厚与向内/向外抽壳方向 · 中键确认",
  },
  draft: {
    kind: "draft",
    title: "拔模斜度 (Draft)",
    collectorLabel: "拔模面集合",
    collectorTarget: "face",
    allowsMultipleRefs: true,
    supportsApplyContinuous: false,
    cueEmpty: "拔模 [步骤 1/2]：选择需要添加拔模斜度的侧面",
    cueReady: "拔模 [步骤 2/2]：设定拔模角度与中性基准面 · 中键确认",
  },
  pattern: {
    kind: "pattern",
    title: "特征阵列 (Pattern)",
    collectorLabel: "阵列目标特征",
    collectorTarget: "any",
    allowsMultipleRefs: false,
    supportsApplyContinuous: false,
    cueEmpty: "阵列 [步骤 1/2]：选择阵列类型（线性 / 环形）并设定数量与间距",
    cueReady: "阵列 [步骤 2/2]：核对实例数量与阵列轴向 · 中键确认",
  },
  mirror: {
    kind: "mirror",
    title: "对称镜像 (Mirror)",
    collectorLabel: "镜像参考实体",
    collectorTarget: "any",
    allowsMultipleRefs: false,
    supportsApplyContinuous: false,
    cueEmpty: "镜像 [步骤 1/2]：选择对称基准平面（XY / XZ / YZ）",
    cueReady: "镜像 [步骤 2/2]：确认对称平面与是否保留原体 · 中键确认",
  },
  boolean: {
    kind: "boolean",
    title: "布尔运算 (Boolean)",
    collectorLabel: "工具体 / 目标面",
    collectorTarget: "body",
    allowsMultipleRefs: true,
    supportsApplyContinuous: false,
    cueEmpty: "布尔 [步骤 1/2]：选择合并 / 切除 / 求交方式与目标体",
    cueReady: "布尔 [步骤 2/2]：核对目标实体与工具实体 · 中键确认",
  },
  direct_edit: {
    kind: "direct_edit",
    title: "同步直接编辑 (Direct Edit)",
    collectorLabel: "目标编辑面",
    collectorTarget: "face",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "同步建模 [步骤 1/2]：选择要偏移、移动或删除修补的实体面",
    cueReady: "同步建模 [步骤 2/2]：输入偏移距离或位移向量 · 中键确认",
  },
  mate: {
    kind: "mate",
    title: "装配配合 (Assembly Mate)",
    collectorLabel: "配合参考面/轴",
    collectorTarget: "any",
    allowsMultipleRefs: true,
    supportsApplyContinuous: true,
    cueEmpty: "装配配合 [步骤 1/2]：依次选择两个组件的配合面或轴线",
    cueReady: "装配配合 [步骤 2/2]：选择配合类型（重合/同心/距离/角度等）· 中键确认",
  },
  tag_topology: {
    kind: "tag_topology",
    title: "语义拓扑打标 (Tag Topology)",
    collectorLabel: "待标注面 / 边",
    collectorTarget: "any",
    allowsMultipleRefs: true,
    supportsApplyContinuous: true,
    cueEmpty: "语义打标 [步骤 1/2]：选择需要绑定工程语义标签的面或边",
    cueReady: "语义打标 [步骤 2/2]：输入工程接口标签（如 bearing_seat_wall）· 中键确认",
  },
  design_assertion: {
    kind: "design_assertion",
    title: "设计断言门禁 (Assert Gate)",
    collectorLabel: "断言作用域",
    collectorTarget: "none",
    allowsMultipleRefs: false,
    supportsApplyContinuous: true,
    cueEmpty: "设计断言：配置 L2/L3 工程校验断言（最小壁厚 / 体积区间 / 表达式）",
    cueReady: "设计断言：确认规则阈值后单击中键写入工程验证树",
  },
  sketch: {
    kind: "sketch",
    title: "2D 参数化草图 (Sketch)",
    collectorLabel: "草图放置平面",
    collectorTarget: "face",
    allowsMultipleRefs: false,
    supportsApplyContinuous: false,
    cueEmpty: "草图环境：选择基准面（XY/XZ/YZ）或平面，添加图元与 cad-gcs 约束，点击「完成草图」提交",
    cueReady: "草图环境：绘制 2D 图元并添加几何/尺寸约束 · 单击「完成草图」退出并保存",
  },
};

export const ISO_METRIC_DESIGNATIONS = ["M3", "M4", "M5", "M6", "M8", "M10", "M12", "M16"] as const;

export const ISO_METRIC_CLEARANCE_MM: Readonly<Record<string, { readonly through: number; readonly cbore: number; readonly cboreDepth: number }>> = {
  M3: { through: 3.4, cbore: 6.0, cboreDepth: 3.2 },
  M4: { through: 4.5, cbore: 7.6, cboreDepth: 4.2 },
  M5: { through: 5.5, cbore: 9.2, cboreDepth: 5.2 },
  M6: { through: 6.6, cbore: 11.0, cboreDepth: 6.2 },
  M8: { through: 9.0, cbore: 14.5, cboreDepth: 8.2 },
  M10: { through: 11.0, cbore: 17.5, cboreDepth: 10.2 },
  M12: { through: 13.5, cbore: 20.0, cboreDepth: 12.2 },
  M16: { through: 17.5, cbore: 26.0, cboreDepth: 16.2 },
};

export interface FeatureDashboardDraft {
  readonly kind: FeatureDashboardKind;
  readonly featureName: string;
  readonly references: readonly string[];
  readonly previewEnabled: boolean;
  readonly advancedOpen: boolean;
  // Hole fields
  readonly holeMode: "simple" | "counterbore" | "countersink" | "standard";
  readonly holeStandard: FastenerHoleStandard;
  readonly holeDesignation: string;
  readonly holeClearance: FastenerClearance;
  readonly holeDepthKind: "through_all" | "blind";
  readonly diameterMm: number;
  readonly depthMm: number;
  readonly cboreDiameterMm: number;
  readonly cboreDepthMm: number;
  // Fillet / Chamfer
  readonly radiusMm: number;
  readonly chamferDistanceMm: number;
  readonly chamferDistance2Mm: number;
  readonly chamferSymmetric: boolean;
  // Box / Cylinder / Extrude / Revolve
  readonly widthMm: number;
  readonly lengthMm: number;
  readonly heightMm: number;
  readonly angleDeg: number;
  readonly symmetric: boolean;
  readonly bodyOp: FeatureBodyOp;
  // Shell / Draft
  readonly thicknessMm: number;
  readonly inward: boolean;
  readonly datumPlane: DatumPlane;
  // Pattern
  readonly patternType: "linear" | "polar";
  readonly patternCount: number;
  readonly patternSpacingMm: number;
  // Mirror
  readonly keepOriginal: boolean;
  // Boolean
  readonly booleanOp: BooleanOpKind;
  // Direct face edit
  readonly directEditOp: "offset" | "move" | "delete_and_heal";
  readonly offsetMm: number;
  // Mate
  readonly mateKind: MateKindSpec["kind"];
  readonly mateValue: number;
  readonly mateFlip: boolean;
  // Tag & Assertion
  readonly semanticTag: string;
  readonly assertionRule: "min_wall_thickness" | "volume_bounds" | "boolean_condition";
  readonly assertionMin: number;
  readonly assertionMax: number;
  readonly assertionExpr: string;
  // Sketch context
  readonly sketchEntities: readonly SketchEntity2D[];
  readonly sketchConstraints: readonly SketchConstraint2D[];
}

export function createFeatureDashboardDraft(
  kind: FeatureDashboardKind,
  initialRefs: readonly string[] = [],
  options?: {
    readonly holeMode?: FeatureDashboardDraft["holeMode"];
    readonly sizeMm?: number;
    readonly datumPlane?: DatumPlane;
  },
): FeatureDashboardDraft {
  const size = options?.sizeMm && Number.isFinite(options.sizeMm) && options.sizeMm > 0
    ? options.sizeMm
    : undefined;
  return {
    kind,
    featureName: defaultFeatureName(kind, options?.holeMode),
    references: [...new Set(initialRefs.filter(Boolean))],
    previewEnabled: true,
    advancedOpen: false,
    holeMode: options?.holeMode ?? "simple",
    holeStandard: "iso4762_counterbore",
    holeDesignation: "M6",
    holeClearance: "normal",
    holeDepthKind: "through_all",
    diameterMm: size ?? 6.6,
    depthMm: 15.0,
    cboreDiameterMm: size ? Number((size * 1.65).toFixed(1)) : 11.0,
    cboreDepthMm: size ? Number(Math.max(2.5, size * 0.92).toFixed(1)) : 6.2,
    radiusMm: size ?? 2.0,
    chamferDistanceMm: size ?? 1.5,
    chamferDistance2Mm: size ?? 1.5,
    chamferSymmetric: true,
    widthMm: 80,
    lengthMm: 60,
    heightMm: size ?? 20,
    angleDeg: kind === "draft" ? 2.5 : 360,
    symmetric: false,
    bodyOp: "join",
    thicknessMm: size ?? 2.5,
    inward: true,
    datumPlane: options?.datumPlane ?? "xy",
    patternType: "polar",
    patternCount: 4,
    patternSpacingMm: 24,
    keepOriginal: true,
    booleanOp: "union",
    directEditOp: "offset",
    offsetMm: size ?? 2.0,
    mateKind: "coincident",
    mateValue: 0,
    mateFlip: false,
    semanticTag: "mounting_interface",
    assertionRule: "min_wall_thickness",
    assertionMin: 2.0,
    assertionMax: 500000,
    assertionExpr: "wall_thickness >= 2.0mm",
    sketchEntities: kind === "sketch"
      ? [{ kind: "rectangle", id: "rect_1", origin_mm: [0, 0], width_mm: 60, height_mm: 40, centered: true }]
      : [],
    sketchConstraints: [],
  };
}

function defaultFeatureName(kind: FeatureDashboardKind, holeMode?: FeatureDashboardDraft["holeMode"]): string {
  switch (kind) {
    case "hole":
      return holeMode === "counterbore" ? "沉头孔" : holeMode === "standard" ? "ISO 标准孔" : "通孔";
    case "fillet":
      return "边缘圆角";
    case "chamfer":
      return "边缘倒角";
    case "primitive_box":
      return "长方体基体";
    case "primitive_cylinder":
      return "圆柱基体";
    case "extrude":
      return "拉伸特征";
    case "revolve":
      return "旋转特征";
    case "shell":
      return "薄壁抽壳";
    case "draft":
      return "拔模特征";
    case "pattern":
      return "特征阵列";
    case "mirror":
      return "对称镜像";
    case "boolean":
      return "布尔运算";
    case "direct_edit":
      return "同步面编辑";
    case "mate":
      return "装配配合";
    case "tag_topology":
      return "语义拓扑标签";
    case "design_assertion":
      return "工程断言门禁";
    case "sketch":
      return "草图_1";
  }
}

/** Build live NX-style Cue Line guidance text for the active Feature Dashboard. */
export function buildFeatureCueText(draft: FeatureDashboardDraft): string {
  const meta = FEATURE_DASHBOARD_META[draft.kind];
  if (draft.kind === "sketch") {
    const dof = estimateSketchDof(draft.sketchEntities, draft.sketchConstraints);
    const planeLabel = draft.references[0] ? `面 ${draft.references[0]}` : `${draft.datumPlane.toUpperCase()} 基准面`;
    return `草图 (${planeLabel}) · ${draft.sketchEntities.length} 图元 · ${draft.sketchConstraints.length} 约束 · ${dof.label}`;
  }
  if (meta.collectorTarget === "none") {
    return meta.cueReady;
  }
  if (draft.references.length === 0) {
    return meta.cueEmpty;
  }
  const refSummary = draft.references.length === 1
    ? draft.references[0]
    : `${draft.references[0]} 等 ${draft.references.length} 项`;
  return `${meta.title} · 已收集 ${refSummary} · ${meta.cueReady}`;
}

export interface SketchDofSummary {
  readonly degreesOfFreedom: number;
  readonly status: "under" | "fully" | "over";
  readonly label: string;
}

export function estimateSketchDof(
  entities: readonly SketchEntity2D[],
  constraints: readonly SketchConstraint2D[],
): SketchDofSummary {
  let rawDof = 0;
  for (const entity of entities) {
    switch (entity.kind) {
      case "point":
        rawDof += entity.fixed ? 0 : 2;
        break;
      case "line":
        rawDof += 4;
        break;
      case "circle":
        rawDof += 3;
        break;
      case "arc":
        rawDof += 5;
        break;
      case "rectangle":
        rawDof += 4;
        break;
      case "spline_blob":
        rawDof += 4;
        break;
    }
  }
  const constrained = constraints.reduce((sum, item) => {
    if (item.kind === "coincident" || item.kind === "symmetric") return sum + 2;
    return sum + 1;
  }, 0);
  const dof = rawDof - constrained;
  if (entities.length === 0) {
    return { degreesOfFreedom: 0, status: "under", label: "空草图（请添加图元）" };
  }
  if (dof > 0) {
    return { degreesOfFreedom: dof, status: "under", label: `欠约束 (DOF=${dof})` };
  }
  if (dof === 0) {
    return { degreesOfFreedom: 0, status: "fully", label: "完全约束 (DOF=0)" };
  }
  return { degreesOfFreedom: dof, status: "over", label: `过约束 (DOF=${dof})` };
}

export interface FeatureExecutionPlan {
  readonly summary: string;
  readonly semanticCommand: CadApiSemanticCommand;
  readonly graphPatch: GraphPatch;
  /** Non-null when this feature also projects cleanly onto the live KCL source view. */
  readonly nextKclSource: string | null;
}

function activeSolidNodeId(graph: CanonicalGraph | null): string {
  if (!graph) return "body_1";
  const lastOut = graph.outputs[graph.outputs.length - 1];
  if (lastOut && graph.nodes[lastOut]) return lastOut;
  const ids = Object.keys(graph.nodes);
  return ids[ids.length - 1] ?? "body_1";
}

function uniqueNodeId(graph: CanonicalGraph | null, prefix: string): string {
  const existing = new Set(graph ? Object.keys(graph.nodes) : []);
  let idx = existing.size + 1;
  while (existing.has(`${prefix}_${idx}`)) idx += 1;
  return `${prefix}_${idx}`;
}

function nodeRefValue(nodeId: string): IrValue {
  return { type: "node_ref", value: { node_id: nodeId, port: "out" } };
}

function quantityMm(value: number): IrValue {
  return { type: "quantity", value: { dimension: "length", value, unit: "mm" } };
}

function quantityDeg(value: number): IrValue {
  return { type: "quantity", value: { dimension: "angle", value, unit: "deg" } };
}

function insertKclSnippetBeforeReturn(kclSource: string, snippetLines: (indent: string, targetVar: string) => readonly string[]): string {
  const newline = kclSource.includes("\r\n") ? "\r\n" : "\n";
  const lines = kclSource.split(/\r?\n/);
  let targetLineIdx = -1;
  let targetIndent = "    ";
  let targetVar = "";
  for (let i = 0; i < lines.length; i += 1) {
    const m = /^(\s*)return\s+([A-Za-z_]\w*)\s*(?:\/\/.*)?$/.exec(lines[i]);
    if (m) {
      targetLineIdx = i;
      targetIndent = m[1] || "    ";
      targetVar = m[2];
    }
  }
  if (targetLineIdx < 0 || !targetVar) return kclSource;
  const inserted = snippetLines(targetIndent, targetVar);
  return [...lines.slice(0, targetLineIdx), ...inserted, ...lines.slice(targetLineIdx)].join(newline);
}

/**
 * Compile a FeatureDashboardDraft into a strongly typed `CadApiSemanticCommand`,
 * a local `GraphPatch` for immediate parametric tree update, and an optional KCL projection.
 */
export function buildFeatureExecutionPlan(
  draft: FeatureDashboardDraft,
  kclSource: string,
  graphIr: CanonicalGraph | null,
): FeatureExecutionPlan {
  const solidId = activeSolidNodeId(graphIr);
  const primaryRef = draft.references[0] ?? "face:top";
  const nodeId = uniqueNodeId(graphIr, draft.kind);
  const name = draft.featureName.trim() || defaultFeatureName(draft.kind, draft.holeMode);

  const makeAddPatch = (
    kind: UnitNodeKind,
    inputs: Readonly<Record<string, IrValue>>,
    summary: string,
    outputType: IrNode["output_type"] = "solid",
  ): GraphPatch => ({
    patch_id: `dashboard:${nodeId}`,
    description: summary,
    ops: [
      {
        op: "add_node",
        value: {
          id: nodeId,
          name,
          kind: { kind },
          inputs,
          output_type: outputType,
          state: { status: "valid" },
          metadata: { source: "feature_dashboard" },
        },
      },
      ...(outputType === "solid" ? [{ op: "set_outputs" as const, value: [nodeId] }] : []),
    ],
  });

  switch (draft.kind) {
    case "hole": {
      const isoSpec = ISO_METRIC_CLEARANCE_MM[draft.holeDesignation] ?? ISO_METRIC_CLEARANCE_MM.M6;
      const effectiveDiam = draft.holeMode === "standard" ? isoSpec.through : draft.diameterMm;
      const depthSpec = draft.holeDepthKind === "through_all"
        ? ({ kind: "through_all" } as const)
        : ({ kind: "blind", depth: dimMm(draft.depthMm) } as const);
      const definition: HoleDefinition = draft.holeMode === "standard"
        ? {
            hole: "standard",
            standard: draft.holeStandard,
            designation: draft.holeDesignation,
            clearance_class: draft.holeClearance,
            depth: depthSpec,
          }
        : {
            hole: "generic",
            diameter: dimMm(draft.diameterMm),
            depth: depthSpec,
            profile: draft.holeMode === "counterbore"
              ? {
                  kind: "counterbore",
                  bore_diameter: dimMm(draft.cboreDiameterMm),
                  bore_depth: dimMm(draft.cboreDepthMm),
                }
              : draft.holeMode === "countersink"
                ? {
                    kind: "countersink",
                    sink_diameter: dimMm(draft.cboreDiameterMm),
                    sink_angle: dimDeg(90),
                  }
                : { kind: "simple" },
          };

      const kclKind = draft.holeMode === "counterbore" || (draft.holeMode === "standard" && draft.holeStandard === "iso4762_counterbore")
        ? "counterbore_hole"
        : "through_hole";
      const kclProjected = applyFaceQuickFeatureToKcl(kclSource, primaryRef, kclKind, effectiveDiam);
      const summary = draft.holeMode === "standard"
        ? `创建标准孔 ${draft.holeDesignation} (${draft.holeStandard}) 于 ${primaryRef}`
        : kclProjected.summary;

      return {
        summary,
        semanticCommand: {
          command: "create_hole",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            target_face: primaryRef,
            definition,
          },
        },
        graphPatch: makeAddPatch("hole", {
          solid: nodeRefValue(solidId),
          target_face: { type: "topo_ref_expr", value: primaryRef },
          diameter: quantityMm(effectiveDiam),
        }, summary),
        nextKclSource: kclProjected.nextSource,
      };
    }

    case "fillet": {
      const kclProjected = applyFaceQuickFeatureToKcl(kclSource, primaryRef, "fillet_edges", draft.radiusMm);
      return {
        summary: kclProjected.summary,
        semanticCommand: {
          command: "fillet_edges",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            edges: draft.references,
            radius: dimMm(draft.radiusMm),
          },
        },
        graphPatch: makeAddPatch("fillet", {
          solid: nodeRefValue(solidId),
          radius: quantityMm(draft.radiusMm),
          edges: {
            type: "list",
            value: draft.references.map((ref) => ({ type: "topo_ref_expr" as const, value: ref })),
          },
        }, kclProjected.summary),
        nextKclSource: kclProjected.nextSource,
      };
    }

    case "chamfer": {
      const kclProjected = applyFaceQuickFeatureToKcl(kclSource, primaryRef, "chamfer_edges", draft.chamferDistanceMm);
      return {
        summary: kclProjected.summary,
        semanticCommand: {
          command: "chamfer_edges",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            edges: draft.references,
            distance: dimMm(draft.chamferDistanceMm),
            ...(draft.chamferSymmetric ? {} : { distance2: dimMm(draft.chamferDistance2Mm) }),
          },
        },
        graphPatch: makeAddPatch("chamfer", {
          solid: nodeRefValue(solidId),
          distance: quantityMm(draft.chamferDistanceMm),
          edges: {
            type: "list",
            value: draft.references.map((ref) => ({ type: "topo_ref_expr" as const, value: ref })),
          },
        }, kclProjected.summary),
        nextKclSource: kclProjected.nextSource,
      };
    }

    case "shell": {
      const t = Math.max(0.4, draft.thicknessMm);
      const summary = `创建抽壳特征 shell(thickness = ${t.toFixed(1)}mm)`;
      const nextKcl = insertKclSnippetBeforeReturn(kclSource, (indent, targetVar) => [
        `${indent}let ${targetVar} = ${targetVar} |> shell(thickness = ${t.toFixed(1)}mm)`,
      ]);
      return {
        summary,
        semanticCommand: {
          command: "shell_solid",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            open_faces: draft.references,
            thickness: dimMm(t),
            inward: draft.inward,
          },
        },
        graphPatch: makeAddPatch("shell", {
          solid: nodeRefValue(solidId),
          thickness: quantityMm(t),
        }, summary),
        nextKclSource: nextKcl !== kclSource ? nextKcl : null,
      };
    }

    case "mirror": {
      const planeConst = draft.datumPlane === "xy" ? "XY_PLANE" : draft.datumPlane === "xz" ? "XZ_PLANE" : "YZ_PLANE";
      const summary = `创建对称镜像 mirror(plane = ${planeConst})`;
      const nextKcl = insertKclSnippetBeforeReturn(kclSource, (indent, targetVar) => [
        `${indent}let ${targetVar} = ${targetVar} |> mirror(plane = ${planeConst})`,
      ]);
      return {
        summary,
        semanticCommand: {
          command: "mirror_feature",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            plane: { kind: "datum", plane: draft.datumPlane },
            keep_original: draft.keepOriginal,
          },
        },
        graphPatch: makeAddPatch("mirror", {
          solid: nodeRefValue(solidId),
          plane: { type: "string", value: planeConst },
        }, summary),
        nextKclSource: nextKcl !== kclSource ? nextKcl : null,
      };
    }

    case "primitive_box": {
      const summary = `创建长方体基体 ${draft.widthMm}×${draft.lengthMm}×${draft.heightMm}mm`;
      return {
        summary,
        semanticCommand: {
          command: "create_primitive_box",
          args: {
            node_id: nodeId,
            name,
            width: dimMm(draft.widthMm),
            depth: dimMm(draft.lengthMm),
            height: dimMm(draft.heightMm),
          },
        },
        graphPatch: makeAddPatch("primitive_box", {
          width: quantityMm(draft.widthMm),
          depth: quantityMm(draft.lengthMm),
          height: quantityMm(draft.heightMm),
        }, summary),
        nextKclSource: null,
      };
    }

    case "primitive_cylinder": {
      const radius = Math.max(0.5, draft.diameterMm / 2);
      const summary = `创建圆柱体基体 Ø${draft.diameterMm}×${draft.heightMm}mm`;
      return {
        summary,
        semanticCommand: {
          command: "create_primitive_cylinder",
          args: {
            node_id: nodeId,
            name,
            radius: dimMm(radius),
            height: dimMm(draft.heightMm),
          },
        },
        graphPatch: makeAddPatch("primitive_cylinder", {
          radius: quantityMm(radius),
          height: quantityMm(draft.heightMm),
        }, summary),
        nextKclSource: null,
      };
    }

    case "extrude": {
      const summary = `拉伸成形 ${draft.heightMm}mm (${draft.bodyOp})`;
      return {
        summary,
        semanticCommand: {
          command: "extrude_sketch",
          args: {
            node_id: nodeId,
            name,
            sketch: primaryRef,
            distance: dimMm(draft.heightMm),
            symmetric: draft.symmetric,
            operation: draft.bodyOp,
            target: solidId,
          },
        },
        graphPatch: makeAddPatch("extrude", {
          target: nodeRefValue(solidId),
          distance: quantityMm(draft.heightMm),
          operation: { type: "string", value: draft.bodyOp },
        }, summary),
        nextKclSource: null,
      };
    }

    case "revolve": {
      const summary = `旋转成形 ${draft.angleDeg}° (${draft.bodyOp})`;
      return {
        summary,
        semanticCommand: {
          command: "revolve_sketch",
          args: {
            node_id: nodeId,
            name,
            sketch: primaryRef,
            angle: dimDeg(draft.angleDeg),
            operation: draft.bodyOp,
            target: solidId,
          },
        },
        graphPatch: makeAddPatch("revolve", {
          target: nodeRefValue(solidId),
          angle: quantityDeg(draft.angleDeg),
        }, summary),
        nextKclSource: null,
      };
    }

    case "draft": {
      const summary = `拔模斜度 ${draft.angleDeg}° (${draft.datumPlane.toUpperCase()})`;
      return {
        summary,
        semanticCommand: {
          command: "draft_faces",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            faces: draft.references.length > 0 ? draft.references : [primaryRef],
            pull_direction: [0, 0, 1],
            neutral_plane: { kind: "datum", plane: draft.datumPlane },
            angle: dimDeg(draft.angleDeg),
          },
        },
        graphPatch: makeAddPatch("draft", {
          solid: nodeRefValue(solidId),
          angle: quantityDeg(draft.angleDeg),
        }, summary),
        nextKclSource: null,
      };
    }

    case "pattern": {
      const isPolar = draft.patternType === "polar";
      const summary = isPolar
        ? `环形阵列 ${draft.patternCount} 实例 (${draft.angleDeg}°)`
        : `线性阵列 ${draft.patternCount} 实例 · 间距 ${draft.patternSpacingMm}mm`;
      return {
        summary,
        semanticCommand: {
          command: "pattern_feature",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            definition: isPolar
              ? {
                  pattern: "polar",
                  count: { source: "literal", value: draft.patternCount },
                  total_angle: dimDeg(draft.angleDeg),
                }
              : {
                  pattern: "linear",
                  direction: [1, 0, 0],
                  count: { source: "literal", value: draft.patternCount },
                  spacing: dimMm(draft.patternSpacingMm),
                },
          },
        },
        graphPatch: makeAddPatch(isPolar ? "pattern_polar" : "pattern_linear", {
          solid: nodeRefValue(solidId),
          count: { type: "int", value: draft.patternCount },
        }, summary),
        nextKclSource: null,
      };
    }

    case "boolean": {
      const summary = `布尔运算 ${draft.booleanOp}`;
      const opKind: UnitNodeKind =
        draft.booleanOp === "union"
          ? "boolean_union"
          : draft.booleanOp === "subtract"
            ? "boolean_subtract"
            : "boolean_intersect";
      return {
        summary,
        semanticCommand: {
          command: "boolean_solids",
          args: {
            node_id: nodeId,
            name,
            op: draft.booleanOp,
            target: solidId,
            tools: draft.references.length > 0 ? draft.references : [solidId],
          },
        },
        graphPatch: makeAddPatch(opKind, {
          target: nodeRefValue(solidId),
        }, summary),
        nextKclSource: null,
      };
    }

    case "direct_edit": {
      const edit: DirectFaceEditOp = draft.directEditOp === "offset"
        ? { op: "offset", distance: dimMm(draft.offsetMm) }
        : draft.directEditOp === "move"
          ? { op: "move", delta_mm: [0, 0, draft.offsetMm] }
          : { op: "delete_and_heal" };
      const summary = draft.directEditOp === "delete_and_heal"
        ? `同步删除并修补面 ${primaryRef}`
        : `同步面${draft.directEditOp === "offset" ? "偏移" : "移动"} ${draft.offsetMm}mm (${primaryRef})`;
      return {
        summary,
        semanticCommand: {
          command: "direct_face_edit",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            face: primaryRef,
            edit,
          },
        },
        graphPatch: makeAddPatch("direct_face_edit", {
          solid: nodeRefValue(solidId),
          face: { type: "topo_ref_expr", value: primaryRef },
          offset: quantityMm(draft.offsetMm),
        }, summary),
        nextKclSource: null,
      };
    }

    case "mate": {
      const mateSpec: MateKindSpec =
        draft.mateKind === "distance"
          ? { kind: "distance", value: dimMm(draft.mateValue) }
          : draft.mateKind === "angle"
            ? { kind: "angle", value: dimDeg(draft.mateValue) }
            : draft.mateKind === "gear"
              ? { kind: "gear", ratio: Math.max(0.1, draft.mateValue || 1) }
              : { kind: draft.mateKind };
      const leftRef = draft.references[0] ?? "comp_1:face_top";
      const rightRef = draft.references[1] ?? "comp_2:face_bottom";
      const summary = `装配配合 ${draft.mateKind} (${leftRef} ↔ ${rightRef})`;
      return {
        summary,
        semanticCommand: {
          command: "add_mate",
          args: {
            node_id: nodeId,
            name,
            left: "comp_1",
            right: "comp_2",
            left_ref: leftRef,
            right_ref: rightRef,
            mate_kind: mateSpec,
            flip: draft.mateFlip,
          },
        },
        graphPatch: makeAddPatch("assembly_mate", {
          left_ref: { type: "topo_ref_expr", value: leftRef },
          right_ref: { type: "topo_ref_expr", value: rightRef },
        }, summary, "assembly"),
        nextKclSource: null,
      };
    }

    case "tag_topology": {
      const tag = draft.semanticTag.trim() || "interface_face";
      const summary = `语义拓扑打标 "${tag}" (${draft.references.length || 1} 项)`;
      return {
        summary,
        semanticCommand: {
          command: "tag_topology",
          args: {
            node_id: nodeId,
            name,
            solid: solidId,
            tag,
            targets: draft.references.length > 0 ? draft.references : [primaryRef],
          },
        },
        graphPatch: makeAddPatch("tag_faces", {
          solid: nodeRefValue(solidId),
          tag: { type: "string", value: tag },
        }, summary, "topo_ref"),
        nextKclSource: null,
      };
    }

    case "design_assertion": {
      const kindSpec =
        draft.assertionRule === "min_wall_thickness"
          ? ({ rule: "min_wall_thickness", min: dimMm(draft.assertionMin) } as const)
          : draft.assertionRule === "volume_bounds"
            ? ({ rule: "volume_bounds", min_mm3: draft.assertionMin, max_mm3: draft.assertionMax } as const)
            : ({ rule: "boolean_condition", condition: draft.assertionExpr } as const);
      const summary = `添加工程断言门禁 (${draft.assertionRule})`;
      return {
        summary,
        semanticCommand: {
          command: "add_design_assertion",
          args: {
            node_id: nodeId,
            name,
            target: solidId,
            assertion_id: nodeId,
            kind: kindSpec,
          },
        },
        graphPatch: makeAddPatch("assert_gate", {
          target: nodeRefValue(solidId),
          rule: { type: "string", value: draft.assertionRule },
        }, summary, "bool"),
        nextKclSource: null,
      };
    }

    case "sketch": {
      const planeSpec = draft.references[0]
        ? ({ kind: "face_ref", face: draft.references[0] } as const)
        : ({ kind: "datum", plane: draft.datumPlane } as const);
      const planeDesc = draft.references[0] ? `面 ${draft.references[0]}` : `${draft.datumPlane.toUpperCase()} 平面`;
      const summary = `完成 2D 草图 ${name} (${planeDesc} · ${draft.sketchEntities.length} 图元 / ${draft.sketchConstraints.length} 约束)`;
      return {
        summary,
        semanticCommand: {
          command: "create_sketch",
          args: {
            node_id: nodeId,
            name,
            plane: planeSpec,
            entities: draft.sketchEntities,
            constraints: draft.sketchConstraints,
          },
        },
        graphPatch: makeAddPatch("sketch_2d", {
          plane: { type: "string", value: planeDesc },
          entity_count: { type: "int", value: draft.sketchEntities.length },
          constraint_count: { type: "int", value: draft.sketchConstraints.length },
        }, summary, "sketch"),
        nextKclSource: null,
      };
    }
  }
}
