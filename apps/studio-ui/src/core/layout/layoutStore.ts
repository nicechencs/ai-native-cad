export type ThemeMode = "dark" | "light";
export type LayoutPreset = "standard" | "code" | "ai" | "zen";

export interface RolePresetMeta {
  readonly id: LayoutPreset;
  readonly label: string;
  readonly description: string;
  readonly shortcut?: string;
}

/** NX-inspired Role presets controlling panel visibility, widths, and viewport ratio. */
export const ROLE_PRESETS: readonly RolePresetMeta[] = [
  {
    id: "standard",
    label: "机械设计 (NX/Creo)",
    description: "左侧导航树 + 右侧按需工具轨，最大化 3D 视口面积 (≥78%)",
  },
  {
    id: "code",
    label: "代码协同 (KCL)",
    description: "加宽左侧 KCL 源码与特征树双轨编辑，收起右侧抽屉",
  },
  {
    id: "ai",
    label: "AI 创作 (Copilot)",
    description: "展开右侧 AI 副驾与候选分支预览，收起左侧导航释放画布",
  },
  {
    id: "zen",
    label: "纯净审阅 (Zen)",
    description: "折叠双侧停靠栏，全屏沉浸式 3D 模型与工程图审阅",
    shortcut: "Ctrl+\\",
  },
] as const;

export interface WorkbenchLayoutPrefs {
  readonly theme: ThemeMode;
  readonly leftOpen: boolean;
  readonly leftPinned: boolean;
  readonly leftWidth: number;
  readonly rightOpen: boolean;
  readonly rightWidth: number;
  readonly activeRole: LayoutPreset;
}

const STORAGE_KEY = "ai_native_cad_studio_layout_v3";

/**
 * NX/Creo viewport-first default:
 * - Left Resource Navigator docked open (280px)
 * - Right AI / Inspector / Export docked as a slim 38px on-demand rail (rightOpen: false)
 * -> Keeps >= 78% viewport width on 1440px+ screens.
 */
export const DEFAULT_LAYOUT_PREFS: WorkbenchLayoutPrefs = {
  theme: "dark",
  leftOpen: true,
  leftPinned: true,
  leftWidth: 280,
  rightOpen: false,
  rightWidth: 340,
  activeRole: "standard",
};

export const NARROW_VIEWPORT_BREAKPOINT_PX = 1280;

export function clampLeftWidth(width: number): number {
  return Math.max(240, Math.min(540, Math.round(width)));
}

export function clampRightWidth(width: number): number {
  return Math.max(260, Math.min(540, Math.round(width)));
}

export function loadLayoutPrefs(): WorkbenchLayoutPrefs {
  if (typeof window === "undefined") {
    return DEFAULT_LAYOUT_PREFS;
  }
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_LAYOUT_PREFS;
    const parsed = JSON.parse(raw) as Partial<WorkbenchLayoutPrefs>;
    const activeRole: LayoutPreset =
      parsed.activeRole === "standard" ||
      parsed.activeRole === "code" ||
      parsed.activeRole === "ai" ||
      parsed.activeRole === "zen"
        ? parsed.activeRole
        : DEFAULT_LAYOUT_PREFS.activeRole;
    return {
      theme: parsed.theme === "light" ? "light" : "dark",
      leftOpen: typeof parsed.leftOpen === "boolean" ? parsed.leftOpen : DEFAULT_LAYOUT_PREFS.leftOpen,
      leftPinned: typeof parsed.leftPinned === "boolean" ? parsed.leftPinned : DEFAULT_LAYOUT_PREFS.leftPinned,
      leftWidth: typeof parsed.leftWidth === "number" ? clampLeftWidth(parsed.leftWidth) : DEFAULT_LAYOUT_PREFS.leftWidth,
      rightOpen: typeof parsed.rightOpen === "boolean" ? parsed.rightOpen : DEFAULT_LAYOUT_PREFS.rightOpen,
      rightWidth: typeof parsed.rightWidth === "number" ? clampRightWidth(parsed.rightWidth) : DEFAULT_LAYOUT_PREFS.rightWidth,
      activeRole,
    };
  } catch {
    return DEFAULT_LAYOUT_PREFS;
  }
}

export function saveLayoutPrefs(prefs: WorkbenchLayoutPrefs): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Ignore storage quota or private-mode errors.
  }
}

export function applyPresetToPrefs(
  current: WorkbenchLayoutPrefs,
  preset: LayoutPreset,
): WorkbenchLayoutPrefs {
  switch (preset) {
    case "standard":
      return {
        ...current,
        leftOpen: true,
        leftPinned: true,
        leftWidth: 280,
        rightOpen: false,
        rightWidth: 340,
        activeRole: "standard",
      };
    case "code":
      return {
        ...current,
        leftOpen: true,
        leftPinned: true,
        leftWidth: 440,
        rightOpen: false,
        activeRole: "code",
      };
    case "ai":
      return {
        ...current,
        leftOpen: false,
        rightOpen: true,
        rightWidth: 380,
        activeRole: "ai",
      };
    case "zen":
      return {
        ...current,
        leftOpen: false,
        rightOpen: false,
        activeRole: "zen",
      };
  }
}

/**
 * Prevent both docks from crushing the 3D viewport on narrow screens (< 1280px).
 */
export function enforceResponsiveDockBudget(
  prefs: WorkbenchLayoutPrefs,
  viewportWidth: number,
  justOpened: "left" | "right",
): WorkbenchLayoutPrefs {
  if (viewportWidth >= NARROW_VIEWPORT_BREAKPOINT_PX) return prefs;
  if (justOpened === "right" && prefs.rightOpen && prefs.leftOpen) {
    return { ...prefs, leftOpen: false };
  }
  if (justOpened === "left" && prefs.leftOpen && prefs.rightOpen) {
    return { ...prefs, rightOpen: false };
  }
  return prefs;
}
