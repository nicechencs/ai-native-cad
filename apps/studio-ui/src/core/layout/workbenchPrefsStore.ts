import type { NavigationPresetId, SelectionIntentRule } from "../viewport/navigationPreset.ts";

export type PersistedLeftTab = "model" | "assembly" | "source" | "assets" | "checks";
export type PersistedRightTab = "ai" | "properties" | "export";
export type PersistedRibbonTab =
  | "model"
  | "assembly"
  | "analysis"
  | "annotate"
  | "view"
  | "tools"
  | "ai";
export type PersistedViewportTab = "3d" | "drawing";
export type PersistedSectionAxis = "X" | "Y" | "Z";
export type PersistedAiMode = "generate" | "edit";
export type PersistedModelSubTab = "browser" | "graph";
export type PersistedChecksSubTab = "diagnostics" | "contract";
export type PersistedPropertySubTab = "geometry" | "material";
export type PersistedKclEditMode = "highlighted" | "raw";
export type PersistedAiSubTab = "compose" | "run" | "config";
export type PersistedDiagnosticFilter = "all" | "error" | "warning" | "info";
export type PersistedNotificationFilter = "all" | "info" | "success" | "warning" | "error";

export interface PersistedSectionCut {
  readonly enabled: boolean;
  readonly axis: PersistedSectionAxis;
  readonly ratio: number;
  readonly flip?: boolean;
}

export interface WorkbenchUiPrefs {
  readonly leftTab: PersistedLeftTab;
  readonly rightTab: PersistedRightTab;
  readonly ribbonTab: PersistedRibbonTab;
  readonly ribbonExpanded: boolean;
  readonly viewportTab: PersistedViewportTab;
  readonly showKeepInBox: boolean;
  readonly sectionCut: PersistedSectionCut;
  readonly quickParamInput: string;
  readonly materialId: string;
  readonly aiGenMode: PersistedAiMode;
  readonly aiSubTab: PersistedAiSubTab;
  readonly aiPrompt: string;
  readonly modelSubTab: PersistedModelSubTab;
  readonly checksSubTab: PersistedChecksSubTab;
  readonly diagnosticFilter: PersistedDiagnosticFilter;
  readonly notificationFilter: PersistedNotificationFilter;
  readonly propertySubTab: PersistedPropertySubTab;
  readonly kclEditMode: PersistedKclEditMode;
  readonly showLspDrawer: boolean;
  readonly commandHistory: readonly string[];
  readonly navigationPreset: NavigationPresetId;
  readonly selectionIntent: SelectionIntentRule;
}

const STORAGE_KEY = "ai_native_cad_studio_ui_prefs_v1";

export const DEFAULT_WORKBENCH_UI_PREFS: WorkbenchUiPrefs = {
  leftTab: "model",
  rightTab: "ai",
  ribbonTab: "model",
  ribbonExpanded: true,
  viewportTab: "3d",
  showKeepInBox: true,
  sectionCut: { enabled: false, axis: "Z", ratio: 0.55 },
  quickParamInput: "",
  materialId: "Al-6061-T6",
  aiGenMode: "edit",
  aiSubTab: "compose",
  aiPrompt: "设计一个带 4 个 M4 螺纹孔的铝合金薄壁控制盒",
  modelSubTab: "graph",
  checksSubTab: "diagnostics",
  diagnosticFilter: "all",
  notificationFilter: "all",
  propertySubTab: "geometry",
  kclEditMode: "highlighted",
  showLspDrawer: false,
  commandHistory: [],
  navigationPreset: "nx-creo",
  selectionIntent: "single",
};

function oneOf<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value) ? (value as T) : fallback;
}

function migrateRibbonTab(value: unknown): PersistedRibbonTab {
  if (value === "file") return "model";
  if (value === "inspect") return "analysis";
  if (value === "window") return "view";
  return oneOf(
    value,
    ["model", "assembly", "analysis", "annotate", "view", "tools", "ai"] as const,
    DEFAULT_WORKBENCH_UI_PREFS.ribbonTab,
  );
}

function migrateRightTab(value: unknown): PersistedRightTab {
  if (value === "assembly") return "properties";
  return oneOf(value, ["ai", "properties", "export"] as const, DEFAULT_WORKBENCH_UI_PREFS.rightTab);
}

function normalizeSectionCut(raw: unknown): PersistedSectionCut {
  if (!raw || typeof raw !== "object") return DEFAULT_WORKBENCH_UI_PREFS.sectionCut;
  const obj = raw as Partial<PersistedSectionCut>;
  const ratio = typeof obj.ratio === "number" && Number.isFinite(obj.ratio)
    ? Math.max(0.05, Math.min(0.95, obj.ratio))
    : DEFAULT_WORKBENCH_UI_PREFS.sectionCut.ratio;
  return {
    enabled: typeof obj.enabled === "boolean" ? obj.enabled : DEFAULT_WORKBENCH_UI_PREFS.sectionCut.enabled,
    axis: oneOf(obj.axis, ["X", "Y", "Z"] as const, DEFAULT_WORKBENCH_UI_PREFS.sectionCut.axis),
    ratio,
    ...(typeof obj.flip === "boolean" ? { flip: obj.flip } : {}),
  };
}

function normalizeCommandHistory(raw: unknown): readonly string[] {
  if (!Array.isArray(raw)) return DEFAULT_WORKBENCH_UI_PREFS.commandHistory;
  return raw
    .filter((item): item is string => typeof item === "string" && item.trim().length > 0)
    .slice(0, 30);
}

export function loadWorkbenchUiPrefs(storage?: Pick<Storage, "getItem"> | null): WorkbenchUiPrefs {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return DEFAULT_WORKBENCH_UI_PREFS;
  try {
    const raw = target.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_WORKBENCH_UI_PREFS;
    const parsed = JSON.parse(raw) as Partial<WorkbenchUiPrefs>;
    return {
      leftTab: oneOf(parsed.leftTab, ["model", "assembly", "source", "assets", "checks"] as const, DEFAULT_WORKBENCH_UI_PREFS.leftTab),
      rightTab: migrateRightTab(parsed.rightTab),
      ribbonTab: migrateRibbonTab(parsed.ribbonTab),
      ribbonExpanded: typeof parsed.ribbonExpanded === "boolean" ? parsed.ribbonExpanded : DEFAULT_WORKBENCH_UI_PREFS.ribbonExpanded,
      viewportTab: oneOf(parsed.viewportTab, ["3d", "drawing"] as const, DEFAULT_WORKBENCH_UI_PREFS.viewportTab),
      showKeepInBox: typeof parsed.showKeepInBox === "boolean" ? parsed.showKeepInBox : DEFAULT_WORKBENCH_UI_PREFS.showKeepInBox,
      sectionCut: normalizeSectionCut(parsed.sectionCut),
      quickParamInput: typeof parsed.quickParamInput === "string" ? parsed.quickParamInput : DEFAULT_WORKBENCH_UI_PREFS.quickParamInput,
      materialId: typeof parsed.materialId === "string" && parsed.materialId ? parsed.materialId : DEFAULT_WORKBENCH_UI_PREFS.materialId,
      aiGenMode: oneOf(parsed.aiGenMode, ["generate", "edit"] as const, DEFAULT_WORKBENCH_UI_PREFS.aiGenMode),
      aiSubTab: oneOf(parsed.aiSubTab, ["compose", "run", "config"] as const, DEFAULT_WORKBENCH_UI_PREFS.aiSubTab),
      aiPrompt: typeof parsed.aiPrompt === "string" ? parsed.aiPrompt : DEFAULT_WORKBENCH_UI_PREFS.aiPrompt,
      modelSubTab: oneOf(parsed.modelSubTab, ["browser", "graph"] as const, DEFAULT_WORKBENCH_UI_PREFS.modelSubTab),
      checksSubTab: oneOf(parsed.checksSubTab, ["diagnostics", "contract"] as const, DEFAULT_WORKBENCH_UI_PREFS.checksSubTab),
      diagnosticFilter: oneOf(parsed.diagnosticFilter, ["all", "error", "warning", "info"] as const, DEFAULT_WORKBENCH_UI_PREFS.diagnosticFilter),
      notificationFilter: oneOf(parsed.notificationFilter, ["all", "info", "success", "warning", "error"] as const, DEFAULT_WORKBENCH_UI_PREFS.notificationFilter),
      propertySubTab: oneOf(parsed.propertySubTab, ["geometry", "material"] as const, DEFAULT_WORKBENCH_UI_PREFS.propertySubTab),
      kclEditMode: oneOf(parsed.kclEditMode, ["highlighted", "raw"] as const, DEFAULT_WORKBENCH_UI_PREFS.kclEditMode),
      showLspDrawer: typeof parsed.showLspDrawer === "boolean" ? parsed.showLspDrawer : DEFAULT_WORKBENCH_UI_PREFS.showLspDrawer,
      commandHistory: normalizeCommandHistory(parsed.commandHistory),
      navigationPreset: oneOf(
        parsed.navigationPreset,
        ["nx-creo", "solidworks", "web-trackpad"] as const,
        DEFAULT_WORKBENCH_UI_PREFS.navigationPreset,
      ),
      selectionIntent: oneOf(
        parsed.selectionIntent,
        ["single", "tangent", "feature", "loop"] as const,
        DEFAULT_WORKBENCH_UI_PREFS.selectionIntent,
      ),
    };
  } catch {
    return DEFAULT_WORKBENCH_UI_PREFS;
  }
}

export function saveWorkbenchUiPrefs(
  prefs: WorkbenchUiPrefs,
  storage?: Pick<Storage, "setItem"> | null,
): void {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return;
  try {
    target.setItem(STORAGE_KEY, JSON.stringify(prefs));
  } catch {
    // Ignore storage quota or private-mode errors.
  }
}

export function updateWorkbenchUiPrefs(
  patch: Partial<WorkbenchUiPrefs>,
  storage?: Pick<Storage, "getItem" | "setItem"> | null,
): WorkbenchUiPrefs {
  const next: WorkbenchUiPrefs = { ...loadWorkbenchUiPrefs(storage), ...patch };
  saveWorkbenchUiPrefs(next, storage);
  return next;
}
