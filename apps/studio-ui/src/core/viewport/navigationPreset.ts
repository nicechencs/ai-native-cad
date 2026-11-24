/**
 * Industrial 3D CAD navigation presets, Selection Intent rules, box-selection
 * semantics, and 8-way NX-style Radial Menu gesture geometry.
 */

export type NavigationPresetId = "nx-creo" | "solidworks" | "web-trackpad";

export interface NavigationPresetMeta {
  readonly id: NavigationPresetId;
  readonly label: string;
  readonly shortLabel: string;
  readonly description: string;
  readonly orbitHint: string;
  readonly panHint: string;
  readonly zoomHint: string;
  readonly selectHint: string;
  readonly confirmHint: string;
  readonly idleHint: string;
}

export const NAVIGATION_PRESETS: readonly NavigationPresetMeta[] = [
  {
    id: "nx-creo",
    label: "NX / Creo 工业标准",
    shortLabel: "NX/Creo",
    description: "中键旋转 · Shift/右键+中键平移 · 左键点选与框选 · 中键单击确认 · 中键双击适配",
    orbitHint: "中键拖拽",
    panHint: "Shift+中键 / 右+中键",
    zoomHint: "滚轮 / Ctrl+中键",
    selectHint: "左键点选 / 拖拽框选 / Ctrl多选",
    confirmHint: "空白处单击中键",
    idleHint: "中键旋转 · Shift+中键平移 · 左键点选/框选 · 滚轮缩放 · 右键划动手势",
  },
  {
    id: "solidworks",
    label: "SolidWorks 习惯",
    shortLabel: "SW",
    description: "中键旋转 · Ctrl+中键平移 · Shift+中键缩放 · 左键点选与框选",
    orbitHint: "中键拖拽",
    panHint: "Ctrl+中键",
    zoomHint: "滚轮 / Shift+中键",
    selectHint: "左键点选 / 拖拽框选 / Ctrl多选",
    confirmHint: "Enter / 确认按钮",
    idleHint: "中键旋转 · Ctrl+中键平移 · 左键点选/框选 · 滚轮缩放 · 右键划动手势",
  },
  {
    id: "web-trackpad",
    label: "Web / 触控板兼容",
    shortLabel: "Web/触控板",
    description: "左键旋转 · Shift+左键或中键平移 · Alt+左键框选 · 适合无三键鼠标环境",
    orbitHint: "左键拖拽",
    panHint: "Shift+左键 / 中键",
    zoomHint: "滚轮 / 双指缩放",
    selectHint: "左键点选 / Alt+左键框选 / Ctrl多选",
    confirmHint: "Enter / 确认按钮",
    idleHint: "左键旋转 · 中键/Shift 平移 · Alt+左键框选 · 滚轮缩放 · 双击适配",
  },
] as const;

export function getNavigationPresetMeta(id: NavigationPresetId): NavigationPresetMeta {
  return NAVIGATION_PRESETS.find((item) => item.id === id) ?? NAVIGATION_PRESETS[0];
}

export type PointerDragAction = "orbit" | "pan" | "zoom" | "box-select" | "radial-gesture" | "none";

export interface PointerDownInput {
  readonly button: number;
  readonly buttons?: number;
  readonly shiftKey: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
}

/**
 * Determine which viewport gesture starts on pointer down for the active navigation preset.
 * Note: left-click without moving beyond drag threshold always resolves to geometry pick.
 */
export function resolvePointerDragAction(
  preset: NavigationPresetId,
  input: PointerDownInput,
): PointerDragAction {
  const { button, buttons = 0, shiftKey, ctrlKey, altKey } = input;

  if (button === 2) {
    // NX chord pan: holding RMB then pressing MMB is handled when button === 1;
    // pressing RMB alone can evolve into an 8-way radial gesture if dragged past threshold.
    return "radial-gesture";
  }

  if (preset === "nx-creo") {
    if (button === 1) {
      const rmbHeld = (buttons & 2) !== 0;
      if (shiftKey || rmbHeld) return "pan";
      if (ctrlKey) return "zoom";
      return "orbit";
    }
    if (button === 0) {
      return "box-select";
    }
    return "none";
  }

  if (preset === "solidworks") {
    if (button === 1) {
      if (ctrlKey) return "pan";
      if (shiftKey) return "zoom";
      return "orbit";
    }
    if (button === 0) {
      return "box-select";
    }
    return "none";
  }

  // web-trackpad
  if (button === 1) return "pan";
  if (button === 0) {
    if (shiftKey) return "pan";
    if (altKey) return "box-select";
    return "orbit";
  }
  return "none";
}

export type BoxSelectMode = "window" | "crossing";

/**
 * Industry-standard CAD box selection convention:
 * - Dragging left -> right = "window" (blue solid, entity must be fully inside)
 * - Dragging right -> left = "crossing" (green dashed, any intersecting entity is selected)
 */
export function resolveBoxSelectMode(startX: number, endX: number): BoxSelectMode {
  return endX >= startX ? "window" : "crossing";
}

/** NX Selection Intent rules controlling how a single click expands geometry selection. */
export type SelectionIntentRule = "single" | "tangent" | "feature" | "loop";

export interface SelectionIntentMeta {
  readonly id: SelectionIntentRule;
  readonly label: string;
  readonly shortLabel: string;
  readonly description: string;
}

export const SELECTION_INTENT_OPTIONS: readonly SelectionIntentMeta[] = [
  {
    id: "single",
    label: "单一对象",
    shortLabel: "单选",
    description: "仅选择光标命中的单个面或边",
  },
  {
    id: "tangent",
    label: "切线相连 (Tangent)",
    shortLabel: "切线链",
    description: "自动扩展至相切连续的邻接边或曲面（适合圆角/倒角）",
  },
  {
    id: "feature",
    label: "同特征拓扑 (Feature)",
    shortLabel: "特征组",
    description: "按 AST/语义血缘选中属于同一建模特征的全部面或边",
  },
  {
    id: "loop",
    label: "边界环 (Boundary Loop)",
    shortLabel: "边界环",
    description: "选中所选平面边界或同高度轮廓闭环的拓扑边",
  },
] as const;

export type RadialDirection = "N" | "NE" | "E" | "SE" | "S" | "SW" | "W" | "NW";

export interface RadialMenuSlot {
  readonly direction: RadialDirection;
  readonly angleDeg: number;
  readonly commandId: string;
  readonly label: string;
  readonly icon: string;
  readonly accent?: "ai";
}

/** 8-way NX-style radial gesture menu slots (clockwise from North = -90 deg). */
export const RADIAL_MENU_SLOTS: readonly RadialMenuSlot[] = [
  { direction: "N", angleDeg: -90, commandId: "view.fit", label: "适应窗口", icon: "fitView" },
  { direction: "NE", angleDeg: -45, commandId: "view.preset.iso", label: "等轴测", icon: "viewIso" },
  { direction: "E", angleDeg: 0, commandId: "display.cycleStyle", label: "着色/透视", icon: "translucent" },
  { direction: "SE", angleDeg: 45, commandId: "display.isolate", label: "孤立选中", icon: "isolate" },
  { direction: "S", angleDeg: 90, commandId: "view.toggleSectionCut", label: "实时剖切", icon: "sectionCut" },
  { direction: "SW", angleDeg: 135, commandId: "ai.inlinePrompt", label: "AI 改选区", icon: "sparkles", accent: "ai" },
  { direction: "W", angleDeg: 180, commandId: "view.normalTo", label: "正视选中面", icon: "normalTo" },
  { direction: "NW", angleDeg: -135, commandId: "display.hideSelected", label: "隐藏选中", icon: "eyeOff" },
] as const;

/**
 * Resolve the active 8-way radial menu sector from pointer displacement (dx, dy),
 * returning `null` inside the center deadzone.
 */
export function resolveRadialDirection(
  dx: number,
  dy: number,
  deadzonePx = 22,
): RadialDirection | null {
  const dist = Math.hypot(dx, dy);
  if (!Number.isFinite(dist) || dist < deadzonePx) return null;
  const deg = (Math.atan2(dy, dx) * 180) / Math.PI; // -180..180, E = 0, S = 90, N = -90
  // Normalize so N (-90) is centered at index 0
  const shifted = ((deg + 90 + 22.5) % 360 + 360) % 360;
  const index = Math.floor(shifted / 45) % 8;
  const dirs: readonly RadialDirection[] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
  return dirs[index] ?? null;
}

/**
 * Compute new camera pan so that zooming around cursor (cursorX, cursorY) keeps
 * the projected world point under the cursor stationary on screen.
 */
export function zoomAroundCursor(params: {
  readonly cursorX: number;
  readonly cursorY: number;
  readonly cx: number;
  readonly cy: number;
  readonly panX: number;
  readonly panY: number;
  readonly prevZoom: number;
  readonly nextZoom: number;
}): { readonly panX: number; readonly panY: number; readonly zoom: number } {
  const { cursorX, cursorY, cx, cy, panX, panY, prevZoom, nextZoom } = params;
  if (prevZoom <= 0 || nextZoom <= 0 || Math.abs(nextZoom - prevZoom) < 1e-9) {
    return { panX, panY, zoom: nextZoom };
  }
  const ratio = nextZoom / prevZoom;
  const relX = cursorX - cx - panX;
  const relY = cursorY - cy - panY;
  return {
    panX: cursorX - cx - relX * ratio,
    panY: cursorY - cy - relY * ratio,
    zoom: nextZoom,
  };
}
