import type { FaceMesh, SceneMeshPayload, TopoEdge } from "../../types/scene.ts";

/** L0: Global rendering style for 3D CAD geometry. */
export type VisualStyle =
  | "shaded-edges"
  | "shaded"
  | "translucent"
  | "wireframe"
  | "hlr";

/** L0: Camera projection mode (aligned with `crates/cad-render/src/camera.rs`). */
export type ProjectionMode = "orthographic" | "perspective";

/** Standard CAD camera view presets. */
export type ViewPreset =
  | "iso"
  | "iso-back"
  | "top"
  | "front"
  | "right"
  | "left"
  | "back"
  | "bottom";

/** 3D viewport geometric selection filter mode. */
export type SelectionFilterMode = "all" | "face" | "edge" | "body";

export interface SelectionFilterMeta {
  readonly id: SelectionFilterMode;
  readonly label: string;
  readonly shortLabel: string;
  readonly description: string;
  readonly shortcut: string;
}

export const SELECTION_FILTER_OPTIONS: readonly SelectionFilterMeta[] = [
  {
    id: "all",
    label: "全部几何",
    shortLabel: "全部",
    description: "智能拾取拓扑边与实体面（边优先）",
    shortcut: "Alt+1",
  },
  {
    id: "face",
    label: "仅选面",
    shortLabel: "面",
    description: "仅拾取平面与曲面，忽略边线干扰",
    shortcut: "Alt+2",
  },
  {
    id: "edge",
    label: "仅选边",
    shortLabel: "边",
    description: "增大拾取容差，仅拾取拓扑棱边与圆弧边",
    shortcut: "Alt+3",
  },
  {
    id: "body",
    label: "仅选实体",
    shortLabel: "体",
    description: "点击任意面时选中整个零件或装配组件",
    shortcut: "Alt+4",
  },
];

export interface ViewPresetMeta {
  readonly id: ViewPreset;
  readonly label: string;
  readonly shortLabel: string;
  readonly shortcut?: string;
}

export const VIEW_PRESET_OPTIONS: readonly ViewPresetMeta[] = [
  { id: "iso", label: "等轴测 (前右)", shortLabel: "等轴测", shortcut: "1" },
  { id: "iso-back", label: "等轴测 (后左)", shortLabel: "后等轴", shortcut: "8" },
  { id: "top", label: "俯视 (+Z)", shortLabel: "俯视", shortcut: "2" },
  { id: "front", label: "主视 (-Y)", shortLabel: "主视", shortcut: "3" },
  { id: "right", label: "右视 (+X)", shortLabel: "右视", shortcut: "4" },
  { id: "left", label: "左视 (-X)", shortLabel: "左视", shortcut: "5" },
  { id: "back", label: "后视 (+Y)", shortLabel: "后视", shortcut: "6" },
  { id: "bottom", label: "仰视 (-Z)", shortLabel: "仰视", shortcut: "7" },
];

/** L1: Global master visibility switches by object category (Fusion / SW pattern). */
export interface DisplayCategoryVisibility {
  readonly faces: boolean;
  readonly edges: boolean;
  readonly grid: boolean;
  readonly axisTriad: boolean;
  readonly keepInBox: boolean;
  readonly editHandles: boolean;
  readonly sectionPlane: boolean;
  readonly semanticHighlights: boolean;
}

export type DisplayCategoryKey = keyof DisplayCategoryVisibility;

/** L3: Per-component or per-face local display overrides (SOLIDWORKS Display Pane pattern). */
export interface EntityDisplayOverride {
  readonly hidden?: boolean;
  readonly opacity?: number;
  readonly visualStyle?: VisualStyle;
  readonly color?: string;
}

/** L2: Reusable display state snapshot (Named Display State). */
export interface DisplayStatePreset {
  readonly id: string;
  readonly name: string;
  readonly visualStyle: VisualStyle;
  readonly projection?: ProjectionMode;
  readonly categories: Partial<DisplayCategoryVisibility>;
}

/** L2: User-saved full display state snapshot (persisted across sessions). */
export interface SavedDisplayState {
  readonly id: string;
  readonly name: string;
  readonly visualStyle: VisualStyle;
  readonly projection: ProjectionMode;
  readonly categories: DisplayCategoryVisibility;
  readonly componentOverrides: Readonly<Record<string, EntityDisplayOverride>>;
  readonly faceOverrides: Readonly<Record<string, EntityDisplayOverride>>;
  readonly isolatedIds: readonly string[] | null;
  readonly activeSemanticFilter: string | null;
}

export interface PersistedDisplayPrefs {
  readonly visualStyle: VisualStyle;
  readonly projection: ProjectionMode;
  readonly categories: DisplayCategoryVisibility;
  readonly savedStates: readonly SavedDisplayState[];
}

/** Complete 4-layer 3D viewport display state, decoupled from any specific renderer. */
export interface ViewportDisplayState {
  readonly visualStyle: VisualStyle;
  readonly projection: ProjectionMode;
  readonly categories: DisplayCategoryVisibility;
  /** Component-level overrides keyed by `component_id`. */
  readonly componentOverrides: Readonly<Record<string, EntityDisplayOverride>>;
  /** Face-level overrides keyed by `face_id`. */
  readonly faceOverrides: Readonly<Record<string, EntityDisplayOverride>>;
  /**
   * Temporary Isolate mode: when non-null, only entities whose `component_id` or `face_id`
   * appears in `isolatedIds` are shown. Exiting restores `componentOverrides` / `faceOverrides` losslessly.
   */
  readonly isolatedIds: readonly string[] | null;
  /** Optional semantic provenance tag filter (e.g. "bearing_seat_wall"). */
  readonly activeSemanticFilter: string | null;
}

export interface ResolvedFaceDisplay {
  readonly visible: boolean;
  readonly drawSurface: boolean;
  readonly drawEdges: boolean;
  readonly opacity: number;
  readonly visualStyle: VisualStyle;
  readonly colorOverride?: string;
  readonly isSemanticHighlighted: boolean;
  readonly isMutedByFilter: boolean;
}

export interface ResolvedEdgeDisplay {
  readonly visible: boolean;
  readonly opacity: number;
  readonly dashed: boolean;
  readonly colorOverride?: string;
}

export const VISUAL_STYLE_OPTIONS: readonly {
  readonly id: VisualStyle;
  readonly label: string;
  readonly shortLabel: string;
  readonly description: string;
}[] = [
  {
    id: "shaded-edges",
    label: "带边线上色",
    shortLabel: "带边着色",
    description: "实体着色并叠加清晰拓扑边线（默认工程视图）",
  },
  {
    id: "shaded",
    label: "无边线上色",
    shortLabel: "纯着色",
    description: "仅显示曲面光照着色，隐藏内部边线以审视外观",
  },
  {
    id: "translucent",
    label: "半透明透视",
    shortLabel: "半透明",
    description: "实体半透明显示，便于检查内部孔道与装配干涉",
  },
  {
    id: "wireframe",
    label: "纯线框",
    shortLabel: "线框",
    description: "隐藏实体面，仅绘制全部拓扑边线",
  },
  {
    id: "hlr",
    label: "隐藏线消除 (HLR)",
    shortLabel: "技术线稿",
    description: "以背景色遮挡背面，呈现干净的工程白模/线稿效果",
  },
];

export const DISPLAY_CATEGORY_META: readonly {
  readonly key: DisplayCategoryKey;
  readonly label: string;
  readonly description: string;
}[] = [
  { key: "faces", label: "实体面", description: "显示或隐藏所有三维实体曲面" },
  { key: "edges", label: "拓扑边线", description: "显示或隐藏 B-Rep 拓扑边界线" },
  { key: "grid", label: "参考网格", description: "显示或隐藏 Z=0 基准参考网格" },
  { key: "axisTriad", label: "坐标轴指示", description: "显示或隐藏左下角 XYZ 坐标轴" },
  { key: "keepInBox", label: "Keep-In 包络盒", description: "显示或隐藏设计契约允许空间包络" },
  { key: "editHandles", label: "3D 参数手柄", description: "显示或隐藏 Tier 1/2/3 交互拖拽圆点" },
  { key: "sectionPlane", label: "剖切面框", description: "剖切开启时显示剖切平面线框" },
  { key: "semanticHighlights", label: "语义界面着色", description: "高亮精密配合面（如轴承座孔壁）" },
];

export const BUILTIN_DISPLAY_PRESETS: readonly DisplayStatePreset[] = [
  {
    id: "engineering",
    name: "标准工程视图",
    visualStyle: "shaded-edges",
    projection: "orthographic",
    categories: {
      faces: true,
      edges: true,
      grid: true,
      axisTriad: true,
      editHandles: true,
      semanticHighlights: true,
    },
  },
  {
    id: "presentation",
    name: "纯净外观审视",
    visualStyle: "shaded",
    projection: "perspective",
    categories: {
      faces: true,
      edges: false,
      grid: false,
      axisTriad: false,
      keepInBox: false,
      editHandles: false,
      semanticHighlights: false,
    },
  },
  {
    id: "xray",
    name: "内部结构透视",
    visualStyle: "translucent",
    projection: "orthographic",
    categories: {
      faces: true,
      edges: true,
      grid: true,
      editHandles: false,
      semanticHighlights: true,
    },
  },
  {
    id: "technical",
    name: "技术线稿 (HLR)",
    visualStyle: "hlr",
    projection: "orthographic",
    categories: {
      faces: true,
      edges: true,
      grid: false,
      keepInBox: false,
      editHandles: false,
      semanticHighlights: false,
    },
  },
];

export const DEFAULT_DISPLAY_CATEGORIES: DisplayCategoryVisibility = {
  faces: true,
  edges: true,
  grid: true,
  axisTriad: true,
  keepInBox: true,
  editHandles: true,
  sectionPlane: true,
  semanticHighlights: true,
};

export const TRANSLUCENT_OPACITY = 0.28;

export function createDefaultDisplayState(keepInBox = true): ViewportDisplayState {
  return {
    visualStyle: "shaded-edges",
    projection: "orthographic",
    categories: { ...DEFAULT_DISPLAY_CATEGORIES, keepInBox },
    componentOverrides: {},
    faceOverrides: {},
    isolatedIds: null,
    activeSemanticFilter: null,
  };
}

export function setVisualStyle(
  state: ViewportDisplayState,
  visualStyle: VisualStyle,
): ViewportDisplayState {
  return state.visualStyle === visualStyle ? state : { ...state, visualStyle };
}

export function setProjectionMode(
  state: ViewportDisplayState,
  projection: ProjectionMode,
): ViewportDisplayState {
  return state.projection === projection ? state : { ...state, projection };
}

export function toggleProjectionMode(state: ViewportDisplayState): ViewportDisplayState {
  return {
    ...state,
    projection: state.projection === "orthographic" ? "perspective" : "orthographic",
  };
}

export function setCategoryVisibility(
  state: ViewportDisplayState,
  key: DisplayCategoryKey,
  visible: boolean,
): ViewportDisplayState {
  if (state.categories[key] === visible) return state;
  return {
    ...state,
    categories: { ...state.categories, [key]: visible },
  };
}

export function toggleCategoryVisibility(
  state: ViewportDisplayState,
  key: DisplayCategoryKey,
): ViewportDisplayState {
  return setCategoryVisibility(state, key, !state.categories[key]);
}

export function resetCategoryVisibility(
  state: ViewportDisplayState,
  keepInBox = state.categories.keepInBox,
): ViewportDisplayState {
  return {
    ...state,
    categories: { ...DEFAULT_DISPLAY_CATEGORIES, keepInBox },
  };
}

export function applyDisplayPreset(
  state: ViewportDisplayState,
  preset: DisplayStatePreset,
): ViewportDisplayState {
  return {
    ...state,
    visualStyle: preset.visualStyle,
    projection: preset.projection ?? state.projection,
    categories: { ...state.categories, ...preset.categories },
  };
}

function normalizeOverride(override: EntityDisplayOverride): EntityDisplayOverride | null {
  const cleaned: {
    hidden?: boolean;
    opacity?: number;
    visualStyle?: VisualStyle;
    color?: string;
  } = {};
  if (override.hidden) cleaned.hidden = true;
  if (override.opacity !== undefined && override.opacity < 0.999) {
    cleaned.opacity = Math.max(0.05, Math.min(1, override.opacity));
  }
  if (override.visualStyle) cleaned.visualStyle = override.visualStyle;
  if (override.color) cleaned.color = override.color;
  return Object.keys(cleaned).length > 0 ? cleaned : null;
}

function updateOverrideMap(
  map: Readonly<Record<string, EntityDisplayOverride>>,
  id: string,
  updater: (current: EntityDisplayOverride) => EntityDisplayOverride,
): Readonly<Record<string, EntityDisplayOverride>> {
  const current = map[id] ?? {};
  const next = normalizeOverride(updater(current));
  const copy: Record<string, EntityDisplayOverride> = { ...map };
  if (next) copy[id] = next;
  else delete copy[id];
  return copy;
}

export function setEntityHidden(
  state: ViewportDisplayState,
  scope: "component" | "face",
  id: string,
  hidden: boolean,
): ViewportDisplayState {
  if (!id) return state;
  if (scope === "component") {
    return {
      ...state,
      componentOverrides: updateOverrideMap(state.componentOverrides, id, (cur) => ({
        ...cur,
        hidden,
      })),
    };
  }
  return {
    ...state,
    faceOverrides: updateOverrideMap(state.faceOverrides, id, (cur) => ({
      ...cur,
      hidden,
    })),
  };
}

export function toggleEntityHidden(
  state: ViewportDisplayState,
  scope: "component" | "face",
  id: string,
): ViewportDisplayState {
  const map = scope === "component" ? state.componentOverrides : state.faceOverrides;
  const currentlyHidden = Boolean(map[id]?.hidden);
  return setEntityHidden(state, scope, id, !currentlyHidden);
}

export function toggleEntityTranslucent(
  state: ViewportDisplayState,
  scope: "component" | "face",
  id: string,
  translucentOpacity = TRANSLUCENT_OPACITY,
): ViewportDisplayState {
  if (!id) return state;
  const map = scope === "component" ? state.componentOverrides : state.faceOverrides;
  const isTranslucent = (map[id]?.opacity ?? 1) < 0.99;
  const nextOpacity = isTranslucent ? 1 : translucentOpacity;
  if (scope === "component") {
    return {
      ...state,
      componentOverrides: updateOverrideMap(state.componentOverrides, id, (cur) => ({
        ...cur,
        opacity: nextOpacity,
      })),
    };
  }
  return {
    ...state,
    faceOverrides: updateOverrideMap(state.faceOverrides, id, (cur) => ({
      ...cur,
      opacity: nextOpacity,
    })),
  };
}

export function setEntityVisualStyleOverride(
  state: ViewportDisplayState,
  scope: "component" | "face",
  id: string,
  visualStyle: VisualStyle | undefined,
): ViewportDisplayState {
  if (!id) return state;
  if (scope === "component") {
    return {
      ...state,
      componentOverrides: updateOverrideMap(state.componentOverrides, id, (cur) => ({
        ...cur,
        visualStyle,
      })),
    };
  }
  return {
    ...state,
    faceOverrides: updateOverrideMap(state.faceOverrides, id, (cur) => ({
      ...cur,
      visualStyle,
    })),
  };
}

export function isolateEntities(
  state: ViewportDisplayState,
  ids: readonly string[],
): ViewportDisplayState {
  const unique = Array.from(new Set(ids.filter((id) => id.trim().length > 0)));
  if (unique.length === 0) return state;
  // Toggling isolate on the exact same single target exits isolate mode.
  if (
    state.isolatedIds &&
    state.isolatedIds.length === unique.length &&
    unique.every((id) => state.isolatedIds?.includes(id))
  ) {
    return exitIsolate(state);
  }
  return { ...state, isolatedIds: unique };
}

export function exitIsolate(state: ViewportDisplayState): ViewportDisplayState {
  return state.isolatedIds === null ? state : { ...state, isolatedIds: null };
}

export function showAllEntities(state: ViewportDisplayState): ViewportDisplayState {
  const clearHidden = (map: Readonly<Record<string, EntityDisplayOverride>>) => {
    const next: Record<string, EntityDisplayOverride> = {};
    for (const [key, val] of Object.entries(map)) {
      const cleaned = normalizeOverride({ ...val, hidden: false });
      if (cleaned) next[key] = cleaned;
    }
    return next;
  };
  return {
    ...state,
    isolatedIds: null,
    componentOverrides: clearHidden(state.componentOverrides),
    faceOverrides: clearHidden(state.faceOverrides),
  };
}

export function clearAllOverrides(state: ViewportDisplayState): ViewportDisplayState {
  return {
    ...state,
    isolatedIds: null,
    activeSemanticFilter: null,
    componentOverrides: {},
    faceOverrides: {},
  };
}

export function setSemanticFilter(
  state: ViewportDisplayState,
  tag: string | null,
): ViewportDisplayState {
  return state.activeSemanticFilter === tag ? state : { ...state, activeSemanticFilter: tag };
}

/**
 * Resolve final face display properties by merging L0 -> L1 -> L2 -> L3 layers.
 */
export function resolveFaceDisplay(
  state: ViewportDisplayState,
  face: Pick<FaceMesh, "face_id" | "component_id" | "provenance_tags">,
): ResolvedFaceDisplay {
  const compOverride = face.component_id ? state.componentOverrides[face.component_id] : undefined;
  const faceOverride = state.faceOverrides[face.face_id];

  // Isolate takes priority as temporary visibility mask.
  if (state.isolatedIds !== null) {
    const matchesIsolate =
      state.isolatedIds.includes(face.face_id) ||
      (face.component_id !== undefined && state.isolatedIds.includes(face.component_id));
    if (!matchesIsolate) {
      return {
        visible: false,
        drawSurface: false,
        drawEdges: false,
        opacity: 0,
        visualStyle: state.visualStyle,
        isSemanticHighlighted: false,
        isMutedByFilter: false,
      };
    }
  } else if (compOverride?.hidden || faceOverride?.hidden) {
    return {
      visible: false,
      drawSurface: false,
      drawEdges: false,
      opacity: 0,
      visualStyle: state.visualStyle,
      isSemanticHighlighted: false,
      isMutedByFilter: false,
    };
  }

  const effectiveStyle = faceOverride?.visualStyle ?? compOverride?.visualStyle ?? state.visualStyle;
  const baseOpacity = effectiveStyle === "translucent" ? TRANSLUCENT_OPACITY : 1;
  const overrideOpacity = faceOverride?.opacity ?? compOverride?.opacity ?? 1;
  const hasSemanticTag = face.provenance_tags.includes("bearing_seat_wall");
  const isSemanticHighlighted = state.categories.semanticHighlights && hasSemanticTag;
  const isMutedByFilter =
    state.activeSemanticFilter !== null &&
    !face.provenance_tags.includes(state.activeSemanticFilter);

  const opacity = Math.min(baseOpacity, overrideOpacity) * (isMutedByFilter ? 0.22 : 1);
  const drawSurface = state.categories.faces && effectiveStyle !== "wireframe";
  const drawEdges = state.categories.edges && effectiveStyle !== "shaded";

  return {
    visible: drawSurface || drawEdges,
    drawSurface,
    drawEdges,
    opacity,
    visualStyle: effectiveStyle,
    colorOverride: faceOverride?.color ?? compOverride?.color,
    isSemanticHighlighted,
    isMutedByFilter,
  };
}

/**
 * Resolve final edge display properties by merging L0 -> L1 -> L2 -> L3 layers.
 */
export function resolveEdgeDisplay(
  state: ViewportDisplayState,
  edge: Pick<TopoEdge, "edge_id" | "component_id" | "provenance_tags">,
): ResolvedEdgeDisplay {
  const compOverride = edge.component_id ? state.componentOverrides[edge.component_id] : undefined;

  if (state.isolatedIds !== null) {
    const matchesIsolate =
      state.isolatedIds.includes(edge.edge_id) ||
      (edge.component_id !== undefined && state.isolatedIds.includes(edge.component_id));
    // If isolating individual faces (not components), keep edges dimmed unless component matches.
    if (!matchesIsolate) {
      return { visible: false, opacity: 0, dashed: false };
    }
  } else if (compOverride?.hidden) {
    return { visible: false, opacity: 0, dashed: false };
  }

  const effectiveStyle = compOverride?.visualStyle ?? state.visualStyle;
  const visible = state.categories.edges && effectiveStyle !== "shaded";
  if (!visible) {
    return { visible: false, opacity: 0, dashed: false };
  }

  const isMutedByFilter =
    state.activeSemanticFilter !== null &&
    !edge.provenance_tags.includes(state.activeSemanticFilter);
  const opacity = isMutedByFilter
    ? 0.22
    : effectiveStyle === "translucent" || (compOverride?.opacity ?? 1) < 0.99
      ? 0.65
      : 1;

  return {
    visible: true,
    opacity,
    dashed: false,
    colorOverride: compOverride?.color,
  };
}

export function countHiddenEntities(state: ViewportDisplayState): number {
  let count = 0;
  for (const val of Object.values(state.componentOverrides)) {
    if (val.hidden) count += 1;
  }
  for (const val of Object.values(state.faceOverrides)) {
    if (val.hidden) count += 1;
  }
  return count;
}

export function countTranslucentEntities(state: ViewportDisplayState): number {
  let count = 0;
  for (const val of Object.values(state.componentOverrides)) {
    if ((val.opacity ?? 1) < 0.99) count += 1;
  }
  for (const val of Object.values(state.faceOverrides)) {
    if ((val.opacity ?? 1) < 0.99) count += 1;
  }
  return count;
}

export function hasActiveDisplayOverrides(state: ViewportDisplayState): boolean {
  return (
    state.isolatedIds !== null ||
    state.activeSemanticFilter !== null ||
    Object.keys(state.componentOverrides).length > 0 ||
    Object.keys(state.faceOverrides).length > 0
  );
}

/**
 * Check whether the fast WebGL2 static assembly renderer can draw this frame directly,
 * or if wireframe / perspective / entity overrides need the full interactive pipeline.
 */
export function isFastAssemblyGpuEligible(state: ViewportDisplayState): boolean {
  return (
    state.visualStyle !== "wireframe" &&
    state.projection === "orthographic" &&
    state.categories.faces &&
    state.isolatedIds === null &&
    state.activeSemanticFilter === null &&
    Object.keys(state.componentOverrides).length === 0 &&
    Object.keys(state.faceOverrides).length === 0
  );
}

/** Collect unique semantic provenance tags across all faces and edges in a scene. */
export function collectSemanticTags(
  scene: Pick<SceneMeshPayload, "faces" | "edges">,
): readonly string[] {
  const tags = new Set<string>();
  for (const face of scene.faces) {
    for (const tag of face.provenance_tags) {
      const trimmed = tag.trim();
      if (trimmed) tags.add(trimmed);
    }
  }
  for (const edge of scene.edges) {
    for (const tag of edge.provenance_tags) {
      const trimmed = tag.trim();
      if (trimmed) tags.add(trimmed);
    }
  }
  return Array.from(tags).sort((a, b) => a.localeCompare(b));
}

/** Capture the current viewport display state into a named snapshot. */
export function captureDisplayState(
  state: ViewportDisplayState,
  name: string,
  id?: string,
): SavedDisplayState {
  const trimmedName = name.trim() || "自定义显示状态";
  return {
    id: id ?? `ds_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`,
    name: trimmedName,
    visualStyle: state.visualStyle,
    projection: state.projection,
    categories: { ...state.categories },
    componentOverrides: { ...state.componentOverrides },
    faceOverrides: { ...state.faceOverrides },
    isolatedIds: state.isolatedIds ? [...state.isolatedIds] : null,
    activeSemanticFilter: state.activeSemanticFilter,
  };
}

/** Restore a saved named display state snapshot. */
export function restoreDisplayState(saved: SavedDisplayState): ViewportDisplayState {
  return {
    visualStyle: saved.visualStyle,
    projection: saved.projection,
    categories: { ...DEFAULT_DISPLAY_CATEGORIES, ...saved.categories },
    componentOverrides: { ...saved.componentOverrides },
    faceOverrides: { ...saved.faceOverrides },
    isolatedIds: saved.isolatedIds ? [...saved.isolatedIds] : null,
    activeSemanticFilter: saved.activeSemanticFilter,
  };
}

const DISPLAY_PREFS_STORAGE_KEY = "ai_native_cad_viewport_display_prefs_v1";
const VALID_VISUAL_STYLES: readonly VisualStyle[] = [
  "shaded-edges",
  "shaded",
  "translucent",
  "wireframe",
  "hlr",
];

function sanitizeCategories(raw: unknown, fallbackKeepInBox: boolean): DisplayCategoryVisibility {
  const base: DisplayCategoryVisibility = { ...DEFAULT_DISPLAY_CATEGORIES, keepInBox: fallbackKeepInBox };
  if (!raw || typeof raw !== "object") return base;
  const obj = raw as Partial<Record<DisplayCategoryKey, unknown>>;
  return {
    faces: typeof obj.faces === "boolean" ? obj.faces : base.faces,
    edges: typeof obj.edges === "boolean" ? obj.edges : base.edges,
    grid: typeof obj.grid === "boolean" ? obj.grid : base.grid,
    axisTriad: typeof obj.axisTriad === "boolean" ? obj.axisTriad : base.axisTriad,
    keepInBox: typeof obj.keepInBox === "boolean" ? obj.keepInBox : base.keepInBox,
    editHandles: typeof obj.editHandles === "boolean" ? obj.editHandles : base.editHandles,
    sectionPlane: typeof obj.sectionPlane === "boolean" ? obj.sectionPlane : base.sectionPlane,
    semanticHighlights:
      typeof obj.semanticHighlights === "boolean"
        ? obj.semanticHighlights
        : base.semanticHighlights,
  };
}

export function loadPersistedDisplayPrefs(
  fallbackKeepInBox = true,
  storage?: Pick<Storage, "getItem"> | null,
): PersistedDisplayPrefs {
  const fallback: PersistedDisplayPrefs = {
    visualStyle: "shaded-edges",
    projection: "orthographic",
    categories: { ...DEFAULT_DISPLAY_CATEGORIES, keepInBox: fallbackKeepInBox },
    savedStates: [],
  };
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return fallback;
  try {
    const raw = target.getItem(DISPLAY_PREFS_STORAGE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<PersistedDisplayPrefs>;
    const visualStyle =
      typeof parsed.visualStyle === "string" &&
      (VALID_VISUAL_STYLES as readonly string[]).includes(parsed.visualStyle)
        ? (parsed.visualStyle as VisualStyle)
        : fallback.visualStyle;
    const projection: ProjectionMode =
      parsed.projection === "perspective" || parsed.projection === "orthographic"
        ? parsed.projection
        : fallback.projection;
    const categories = sanitizeCategories(parsed.categories, fallbackKeepInBox);
    const savedStates: SavedDisplayState[] = Array.isArray(parsed.savedStates)
      ? parsed.savedStates
          .filter(
            (item): item is SavedDisplayState =>
              Boolean(item) &&
              typeof item === "object" &&
              typeof item.id === "string" &&
              typeof item.name === "string",
          )
          .slice(0, 12)
          .map((item) => ({
            id: item.id,
            name: item.name,
            visualStyle: (VALID_VISUAL_STYLES as readonly string[]).includes(item.visualStyle)
              ? item.visualStyle
              : "shaded-edges",
            projection: item.projection === "perspective" ? "perspective" : "orthographic",
            categories: sanitizeCategories(item.categories, fallbackKeepInBox),
            componentOverrides:
              item.componentOverrides && typeof item.componentOverrides === "object"
                ? item.componentOverrides
                : {},
            faceOverrides:
              item.faceOverrides && typeof item.faceOverrides === "object" ? item.faceOverrides : {},
            isolatedIds: Array.isArray(item.isolatedIds)
              ? item.isolatedIds.filter((x): x is string => typeof x === "string")
              : null,
            activeSemanticFilter:
              typeof item.activeSemanticFilter === "string" ? item.activeSemanticFilter : null,
          }))
      : [];
    return { visualStyle, projection, categories, savedStates };
  } catch {
    return fallback;
  }
}

export function savePersistedDisplayPrefs(
  prefs: PersistedDisplayPrefs,
  storage?: Pick<Storage, "setItem"> | null,
): void {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return;
  try {
    target.setItem(DISPLAY_PREFS_STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Ignore storage quota or private-mode errors.
  }
}
