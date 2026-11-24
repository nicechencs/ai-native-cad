import { useCallback, useEffect, useMemo, useState, type Dispatch, type MouseEvent as ReactMouseEvent, type SetStateAction } from "react";
import { focusWorkbenchTab } from "../components/WorkbenchTabs.tsx";
import { experimentalTracksEnabled } from "../core/session/experimentalTracks.ts";
import { parseCommandLine } from "../core/commands/commandLineParser.ts";
import { isCommandEnabled, matchesKeyboardEvent, type CommandContext, type CommandDefinition } from "../core/commands/commandRegistry.ts";
import type { LayoutPreset, WorkbenchLayoutPrefs } from "../core/layout/layoutStore.ts";
import type { PersistedLeftTab, PersistedRightTab } from "../core/layout/workbenchPrefsStore.ts";
import type { FeatureDashboardDraft, FeatureDashboardKind } from "../core/modeling/featureDashboardModel.ts";
import { exitIsolate, hasActiveDisplayOverrides, setCategoryVisibility, setProjectionMode } from "../core/viewport/displayState.ts";
import type { DatumPlane } from "../types/apiCommands.ts";
import type { AiGenerateMode, SectionCutState } from "../types/scene.ts";
import type { ContextMenuItem } from "../ui/chrome/StudioOverlays.tsx";
import type { useWorkbenchDocument } from "./useWorkbenchDocument.ts";
import type { useWorkbenchViewport } from "./useWorkbenchViewport.ts";
import type { WorkbenchNotify } from "./workbenchSource.ts";
import { TURN_LEFT_YAW, TURN_RIGHT_YAW } from "../features/viewport/viewOrientation.ts";

function isEditableElement(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}

type CommandDocument = Pick<ReturnType<typeof useWorkbenchDocument>,
  "sourceFileInput" | "handleDownloadSource" | "model" | "isAssembly" | "canExport" | "exporting"
  | "aiGenerating" | "canUndo" | "canRedo" | "handleUndo" | "handleRedo" | "handleExportCommand"
  | "handleApplyQuickFeature" | "handleCancelAi" | "handleAiGenerate"
  | "aicadFileInput" | "handleSaveAicadProject"
>;

type CommandViewport = Pick<ReturnType<typeof useWorkbenchViewport>,
  "selectedTarget" | "selectedTargets" | "setSelectedTarget" | "selectedComponentId" | "selectionFilter" | "setSelectionFilter"
  | "selectionIntent" | "navigationPreset" | "setNavigationPreset"
  | "selectedMaterial" | "viewportTab"
  | "setViewportTab" | "viewportRef" | "measurementMode" | "setMeasurementMode" | "showKeepInBox"
  | "setShowKeepInBox" | "displayState" | "setDisplayState" | "sectionCut" | "setSectionCut"
  | "explodeFactor" | "setExplodeFactor" | "handleFocusAstSpan" | "handleChangeVisualStyle"
  | "handleCycleVisualStyle" | "handleToggleProjection" | "handleToggleEntityHidden"
  | "handleToggleEntityTranslucent" | "handleIsolateEntity" | "handleExitIsolate"
  | "handleShowAllEntities" | "handleClearAllOverrides"
>;

interface WorkbenchCommandOptions {
  readonly documentState: CommandDocument;
  readonly viewport: CommandViewport;
  readonly layoutPrefs: WorkbenchLayoutPrefs;
  readonly setLayoutPrefs: Dispatch<SetStateAction<WorkbenchLayoutPrefs>>;
  readonly openLeftDock: (tab: PersistedLeftTab) => void;
  readonly openRightDock: (tab: PersistedRightTab) => void;
  readonly applyLayoutPreset: (preset: LayoutPreset) => void;
  readonly setAiGenMode: (mode: AiGenerateMode) => void;
  readonly notify: WorkbenchNotify;
  readonly activeDashboard: FeatureDashboardDraft | null;
  readonly hasPreview: boolean;
  readonly onOpenFeatureDashboard: (
    kind: FeatureDashboardKind,
    options?: {
      readonly holeMode?: FeatureDashboardDraft["holeMode"];
      readonly sizeMm?: number;
      readonly datumPlane?: DatumPlane;
    },
  ) => void;
  readonly onConfirmFeatureDashboard: () => void;
  readonly onCancelFeatureDashboard: () => void;
  readonly onSketchQuickAdd: (kind: "rect" | "circle" | "line" | "arc" | "constrain_hv" | "constrain_dim") => void;
  readonly onOpenFileBackstage: () => void;
  readonly onOpenInlineAi: () => void;
  readonly onRegenerateModel: () => void;
}

export function useWorkbenchCommands({
  documentState,
  viewport,
  layoutPrefs,
  setLayoutPrefs,
  openLeftDock,
  openRightDock,
  applyLayoutPreset,
  setAiGenMode,
  notify,
  activeDashboard,
  hasPreview,
  onOpenFeatureDashboard,
  onConfirmFeatureDashboard,
  onCancelFeatureDashboard,
  onSketchQuickAdd,
  onOpenFileBackstage,
  onOpenInlineAi,
  onRegenerateModel,
}: WorkbenchCommandOptions) {
  const {
    sourceFileInput, handleDownloadSource, model, isAssembly, canExport, exporting,
    aiGenerating, canUndo, canRedo, handleUndo, handleRedo, handleExportCommand,
    handleApplyQuickFeature, handleCancelAi, handleAiGenerate,
  } = documentState;
  const {
    selectedTarget, selectedTargets, setSelectedTarget, selectedComponentId, selectionFilter, setSelectionFilter,
    selectionIntent, navigationPreset, setNavigationPreset,
    selectedMaterial, viewportTab,
    setViewportTab, viewportRef, measurementMode, setMeasurementMode, showKeepInBox,
    setShowKeepInBox, displayState, setDisplayState, sectionCut, setSectionCut,
    explodeFactor, setExplodeFactor, handleFocusAstSpan, handleChangeVisualStyle,
    handleCycleVisualStyle, handleToggleProjection, handleToggleEntityHidden, handleToggleEntityTranslucent,
    handleIsolateEntity, handleExitIsolate, handleShowAllEntities, handleClearAllOverrides,
  } = viewport;
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const [resetConfirmOpen, setResetConfirmOpen] = useState(false);
  const [contextMenuPos, setContextMenuPos] = useState<{ readonly x: number; readonly y: number } | null>(null);
  const [aiExternalPrompt, setAiExternalPrompt] = useState<{ readonly text: string; readonly seq: number } | null>(null);

  const commandContext: CommandContext = useMemo(
    () => ({
      hasSelection: selectedTarget !== null,
      selectionKind: selectedTarget?.kind ?? null,
      selectionCount: selectedTargets.length,
      viewportTab,
      isAssembly,
      canExport,
      exporting: exporting !== undefined,
      isHostBackend: model?.origin === "host",
      aiGenerating,
      canUndo,
      canRedo,
      hasMeasurement: measurementMode !== null,
      showKeepInBox: showKeepInBox && displayState.categories.keepInBox,
      sectionCutEnabled: sectionCut.enabled,
      explodeActive: explodeFactor > 0,
      leftOpen: layoutPrefs.leftOpen,
      rightOpen: layoutPrefs.rightOpen,
      zenMode: !layoutPrefs.leftOpen && !layoutPrefs.rightOpen,
      theme: layoutPrefs.theme,
      measurementMode,
      visualStyle: displayState.visualStyle,
      projection: displayState.projection,
      selectionFilter,
      selectionIntent,
      navigationPreset,
      activeDashboardKind: activeDashboard?.kind ?? null,
      sketchActive: activeDashboard?.kind === "sketch",
      hasDisplayOverrides: hasActiveDisplayOverrides(displayState),
      isIsolated: displayState.isolatedIds !== null,
      hasPreview,
    }),
    [
      selectedTarget,
      selectedTargets.length,
      selectionFilter,
      selectionIntent,
      navigationPreset,
      activeDashboard?.kind,
      hasPreview,
      viewportTab,
      isAssembly,
      canExport,
      exporting,
      model?.origin,
      aiGenerating,
      canUndo,
      canRedo,
      measurementMode,
      showKeepInBox,
      displayState,
      sectionCut.enabled,
      explodeFactor,
      layoutPrefs,
    ],
  );

  const experimentalEnabled = experimentalTracksEnabled();
  const commands: readonly CommandDefinition[] = useMemo(
    () => [
      // 文件 (File Backstage)
      {
        id: "file.openBackstage",
        title: "打开文件后台菜单 (File)",
        category: "文件",
        ribbonPath: "文件",
        keywords: ["file", "backstage", "文件"],
        icon: "fileMenu",
        execute: onOpenFileBackstage,
      },
      {
        id: "file.loadSource",
        title: "载入 KCL 源码…",
        description: "将本地 KCL 文件载入当前文档，可撤销。",
        category: "文件",
        ribbonPath: "文件 › 文档与互操作",
        keywords: ["open", "kcl", "源码"],
        icon: "code",
        execute: () => sourceFileInput.current?.click(),
      },
      {
        id: "file.downloadSource",
        title: "保存 / 下载 KCL 源码",
        category: "文件",
        ribbonPath: "文件 › 文档与互操作",
        keywords: ["save", "download", "kcl", "源码"],
        icon: "export",
        shortcut: "Ctrl+S",
        scope: "global",
        execute: handleDownloadSource,
      },
      {
        id: "file.openAicad",
        title: "打开 .aicad 工程…",
        category: "文件",
        keywords: ["open", "aicad", "工程", "project"],
        icon: "code",
        execute: () => documentState.aicadFileInput?.current?.click(),
      },
      {
        id: "file.saveAicad",
        title: "保存 .aicad 工程",
        category: "文件",
        keywords: ["save", "aicad", "工程", "project"],
        icon: "export",
        execute: () => { void documentState.handleSaveAicadProject?.(); },
      },
      {
        id: "file.importAsset",
        title: "导入外部 3D 资产…",
        category: "文件",
        ribbonPath: "文件 › 文档与互操作",
        keywords: ["import", "step", "stl", "3mf", "obj", "gltf", "资产"],
        icon: "assetImport",
        shortcut: "Ctrl+I",
        scope: "global",
        execute: () => openLeftDock("assets"),
      },
      {
        id: "file.resetDemo",
        title: "重置演示模型",
        category: "文件",
        ribbonPath: "文件 › 样板库",
        keywords: ["reset", "demo", "初始化"],
        icon: "refresh",
        execute: () => setResetConfirmOpen(true),
      },
      {
        id: "export.step",
        title: "导出 STEP AP242",
        category: "文件",
        ribbonPath: "文件 › 工程交付导出",
        keywords: ["step", "stp", "export", "brep", "pmi"],
        icon: "export",
        shortcut: "Ctrl+Shift+E",
        scope: "global",
        when: (ctx) => ctx.canExport && ctx.isHostBackend && !ctx.exporting,
        disabledReason: (ctx) => ctx.exporting ? "已有导出任务正在运行" : !ctx.canExport ? "请先编译包含有效几何的模型" : "需要本地编译服务支持 STEP 导出",
        execute: () => handleExportCommand("step"),
      },
      {
        id: "export.dxf",
        title: "导出 DXF",
        category: "文件",
        ribbonPath: "文件 › 工程交付导出",
        keywords: ["dxf", "export", "切割"],
        icon: "export",
        when: (ctx) => ctx.canExport && !ctx.exporting,
        disabledReason: (ctx) => ctx.exporting ? "已有导出任务正在运行" : "请先编译包含有效几何的模型",
        execute: () => handleExportCommand("dxf"),
      },
      {
        id: "export.drawing",
        title: "导出 2D 工程图 SVG",
        category: "文件",
        ribbonPath: "标注与工程图 › 2D 关联工程图",
        keywords: ["svg", "drawing", "工程图"],
        icon: "drawing2d",
        when: (ctx) => ctx.canExport && !ctx.exporting,
        disabledReason: (ctx) => ctx.exporting ? "已有导出任务正在运行" : "请先编译包含有效几何的模型",
        execute: () => handleExportCommand("drawing"),
      },
      {
        id: "export.3mf",
        title: "导出 3MF",
        category: "文件",
        ribbonPath: "文件 › 工程交付导出",
        keywords: ["3mf", "mesh", "print", "export", "3d打印"],
        icon: "export",
        when: (ctx) => ctx.canExport && ctx.isHostBackend && !ctx.exporting,
        disabledReason: (ctx) => ctx.exporting ? "已有导出任务正在运行" : !ctx.canExport ? "请先编译包含有效几何的模型" : "需要本地编译服务支持 3MF 导出",
        execute: () => handleExportCommand("3mf"),
      },
      {
        id: "export.mesh",
        title: "导出 Scene-Mesh JSON",
        category: "文件",
        ribbonPath: "文件 › 工程交付导出",
        keywords: ["mesh", "json", "网格"],
        icon: "export",
        when: (ctx) => ctx.canExport && !ctx.exporting,
        disabledReason: (ctx) => ctx.exporting ? "已有导出任务正在运行" : "请先编译包含有效几何的模型",
        execute: () => handleExportCommand("mesh"),
      },

      // 编辑与再生
      {
        id: "edit.undo",
        title: "撤销",
        category: "编辑",
        ribbonPath: "快速访问工具栏",
        keywords: ["undo", "撤回"],
        icon: "undo",
        shortcut: "Ctrl+Z",
        scope: "global",
        when: (ctx) => ctx.canUndo,
        disabledReason: () => "暂无可撤销的历史步骤",
        execute: handleUndo,
      },
      {
        id: "edit.redo",
        title: "重做",
        category: "编辑",
        ribbonPath: "快速访问工具栏",
        keywords: ["redo", "恢复"],
        icon: "redo",
        shortcut: "Ctrl+Y",
        scope: "global",
        when: (ctx) => ctx.canRedo,
        disabledReason: () => "暂无可重做的历史步骤",
        execute: handleRedo,
      },
      {
        id: "model.regenerate",
        title: "再生重建与 L0–L4 验证 (Regenerate)",
        description: "对标 Creo Ctrl+G：重新求值工程特征图并刷新五层三值验证报告。",
        category: "建模",
        ribbonPath: "建模 › 编辑与再生",
        keywords: ["regen", "regenerate", "rebuild", "validate", "再生", "重建", "验证"],
        icon: "refresh",
        shortcut: "Ctrl+G",
        scope: "global",
        execute: onRegenerateModel,
      },
      {
        id: "selection.clear",
        title: "退出仪表板 / 清除选择",
        category: "编辑",
        keywords: ["clear", "deselect", "esc", "cancel"],
        icon: "close",
        shortcut: "Esc",
        scope: "global",
        execute: () => {
          if (activeDashboard) {
            onCancelFeatureDashboard();
            return;
          }
          if (measurementMode) {
            setMeasurementMode(null);
            return;
          }
          if (displayState.isolatedIds) {
            setDisplayState((prev) => exitIsolate(prev));
            return;
          }
          setSelectedTarget(null);
          setContextMenuPos(null);
        },
      },
      {
        id: "selection.filter.all",
        title: "选择过滤器：全部几何",
        category: "编辑",
        ribbonPath: "Top Border Bar › 选择过滤器",
        keywords: ["filter", "select", "all", "选择过滤", "全部"],
        icon: "filterAll",
        shortcut: "Alt+1",
        scope: "viewport",
        isActive: (ctx) => (ctx.selectionFilter ?? "all") === "all",
        execute: () => {
          setViewportTab("3d");
          setSelectionFilter("all");
        },
      },
      {
        id: "selection.filter.face",
        title: "选择过滤器：仅选面",
        category: "编辑",
        ribbonPath: "Top Border Bar › 选择过滤器",
        keywords: ["filter", "select", "face", "选择过滤", "仅面"],
        icon: "filterFace",
        shortcut: "Alt+2",
        scope: "viewport",
        isActive: (ctx) => ctx.selectionFilter === "face",
        execute: () => {
          setViewportTab("3d");
          setSelectionFilter("face");
        },
      },
      {
        id: "selection.filter.edge",
        title: "选择过滤器：仅选边",
        category: "编辑",
        ribbonPath: "Top Border Bar › 选择过滤器",
        keywords: ["filter", "select", "edge", "选择过滤", "仅边"],
        icon: "filterEdge",
        shortcut: "Alt+3",
        scope: "viewport",
        isActive: (ctx) => ctx.selectionFilter === "edge",
        execute: () => {
          setViewportTab("3d");
          setSelectionFilter("edge");
        },
      },
      {
        id: "selection.filter.body",
        title: "选择过滤器：仅选实体组件",
        category: "编辑",
        ribbonPath: "Top Border Bar › 选择过滤器",
        keywords: ["filter", "select", "body", "component", "选择过滤", "实体"],
        icon: "filterBody",
        shortcut: "Alt+4",
        scope: "viewport",
        isActive: (ctx) => ctx.selectionFilter === "body",
        execute: () => {
          setViewportTab("3d");
          setSelectionFilter("body");
        },
      },
      {
        id: "source.focusSelection",
        title: "定位选中特征的 KCL 源码",
        category: "编辑",
        ribbonPath: "工具与代码 › 代码与双向投影",
        keywords: ["source", "ast", "code", "定位"],
        icon: "code",
        shortcut: "F12",
        scope: "global",
        when: (ctx) => ctx.hasSelection,
        execute: () => {
          if (selectedTarget?.kind === "face") {
            handleFocusAstSpan(selectedTarget.face.ast_span, `Face: ${selectedTarget.face.face_id}`);
          } else if (selectedTarget?.kind === "edge") {
            handleFocusAstSpan(selectedTarget.edge.ast_span, `Edge: ${selectedTarget.edge.edge_id}`);
          }
          openLeftDock("source");
          window.requestAnimationFrame(() => focusWorkbenchTab("left", "source"));
        },
      },

      // 建模特征 (Feature Dashboard — Mapped 1:1 to Semantic CAD API)
      {
        id: "model.sketch",
        title: "新建 2D 参数化草图 (Sketch)",
        category: "草图",
        ribbonPath: "建模 › 草图",
        keywords: ["sketch", "2d", "gcs", "草图"],
        aliases: ["s", "sketch"],
        icon: "sketch",
        shortcut: "S",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "sketch",
        execute: () => onOpenFeatureDashboard("sketch"),
      },
      {
        id: "model.sketch.xy",
        title: "在 XY 基准面新建草图",
        category: "草图",
        ribbonPath: "建模 › 基准",
        keywords: ["sketch", "xy", "datum", "基准面"],
        icon: "datumPlane",
        execute: () => onOpenFeatureDashboard("sketch", { datumPlane: "xy" }),
      },
      {
        id: "model.sketch.xz",
        title: "在 XZ 基准面新建草图",
        category: "草图",
        ribbonPath: "建模 › 基准",
        keywords: ["sketch", "xz", "datum", "基准面"],
        icon: "datumPlane",
        execute: () => onOpenFeatureDashboard("sketch", { datumPlane: "xz" }),
      },
      {
        id: "model.extrude",
        title: "拉伸成形 (Extrude)",
        category: "建模",
        ribbonPath: "建模 › 基础成形",
        keywords: ["extrude", "pad", "boss", "拉伸"],
        aliases: ["e", "ext", "extrude"],
        icon: "extrude",
        shortcut: "E",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "extrude",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          onOpenFeatureDashboard("extrude", { sizeMm });
        },
      },
      {
        id: "model.revolve",
        title: "旋转成形 (Revolve)",
        category: "建模",
        ribbonPath: "建模 › 基础成形",
        keywords: ["revolve", "lathe", "旋转"],
        aliases: ["r", "rev"],
        icon: "revolve",
        shortcut: "R",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "revolve",
        execute: () => onOpenFeatureDashboard("revolve"),
      },
      {
        id: "model.primitiveBox",
        title: "创建长方体基体 (Box)",
        category: "建模",
        ribbonPath: "建模 › 基础成形",
        keywords: ["box", "block", "primitive", "长方体"],
        icon: "boxPrimitive",
        isActive: (ctx) => ctx.activeDashboardKind === "primitive_box",
        execute: () => onOpenFeatureDashboard("primitive_box"),
      },
      {
        id: "model.primitiveCylinder",
        title: "创建圆柱体基体 (Cylinder)",
        category: "建模",
        ribbonPath: "建模 › 基础成形",
        keywords: ["cylinder", "cyl", "primitive", "圆柱"],
        icon: "cylinderPrimitive",
        isActive: (ctx) => ctx.activeDashboardKind === "primitive_cylinder",
        execute: () => onOpenFeatureDashboard("primitive_cylinder"),
      },
      {
        id: "model.throughHole",
        title: "孔 / ISO 标准孔 (Hole)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["hole", "through", "drill", "iso", "孔"],
        aliases: ["h", "hole"],
        icon: "throughHole",
        shortcut: "H",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "hole",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          if (sizeMm !== undefined) {
            handleApplyQuickFeature("through_hole", sizeMm);
          } else {
            onOpenFeatureDashboard("hole", { holeMode: "simple" });
          }
        },
      },
      {
        id: "model.counterboreHole",
        title: "沉头孔 / 标准沉头孔 (Counterbore)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["counterbore", "cbore", "iso4762", "沉头"],
        aliases: ["ch", "cbore"],
        icon: "counterbore",
        shortcut: "Shift+H",
        scope: "viewport",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          if (sizeMm !== undefined) {
            handleApplyQuickFeature("counterbore_hole", sizeMm);
          } else {
            onOpenFeatureDashboard("hole", { holeMode: "counterbore" });
          }
        },
      },
      {
        id: "model.filletEdges",
        title: "边缘圆角 (Fillet)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["fillet", "round", "倒圆"],
        aliases: ["f", "fillet"],
        icon: "fillet",
        shortcut: "F",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "fillet",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          if (sizeMm !== undefined) {
            handleApplyQuickFeature("fillet_edges", sizeMm);
          } else {
            onOpenFeatureDashboard("fillet");
          }
        },
      },
      {
        id: "model.chamferEdges",
        title: "边缘倒角 (Chamfer)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["chamfer", "bevel", "斜角"],
        aliases: ["c", "chamfer"],
        icon: "chamfer",
        shortcut: "C",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "chamfer",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          if (sizeMm !== undefined) {
            handleApplyQuickFeature("chamfer_edges", sizeMm);
          } else {
            onOpenFeatureDashboard("chamfer");
          }
        },
      },
      {
        id: "model.shell",
        title: "薄壁抽壳 (Shell)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["shell", "thin", "hollow", "抽壳"],
        icon: "shell",
        shortcut: "L",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "shell",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          onOpenFeatureDashboard("shell", { sizeMm });
        },
      },
      {
        id: "model.draft",
        title: "拔模斜度 (Draft)",
        category: "建模",
        ribbonPath: "建模 › 工程修饰",
        keywords: ["draft", "taper", "mold", "拔模"],
        icon: "draft",
        isActive: (ctx) => ctx.activeDashboardKind === "draft",
        execute: () => onOpenFeatureDashboard("draft"),
      },
      {
        id: "model.pattern",
        title: "线性 / 环形阵列 (Pattern)",
        category: "建模",
        ribbonPath: "建模 › 变换与布尔",
        keywords: ["pattern", "array", "polar", "linear", "阵列"],
        icon: "pattern",
        isActive: (ctx) => ctx.activeDashboardKind === "pattern",
        execute: () => onOpenFeatureDashboard("pattern"),
      },
      {
        id: "model.mirror",
        title: "对称镜像 (Mirror)",
        category: "建模",
        ribbonPath: "建模 › 变换与布尔",
        keywords: ["mirror", "symmetry", "镜像"],
        icon: "mirror",
        isActive: (ctx) => ctx.activeDashboardKind === "mirror",
        execute: () => onOpenFeatureDashboard("mirror"),
      },
      {
        id: "model.boolean",
        title: "布尔运算 (并 / 差 / 交)",
        category: "建模",
        ribbonPath: "建模 › 变换与布尔",
        keywords: ["boolean", "union", "subtract", "intersect", "布尔"],
        icon: "booleanOp",
        isActive: (ctx) => ctx.activeDashboardKind === "boolean",
        execute: () => onOpenFeatureDashboard("boolean"),
      },
      {
        id: "model.directEdit",
        title: "同步直接编辑面 (Offset / Move)",
        category: "建模",
        ribbonPath: "建模 › 同步直接编辑",
        keywords: ["direct", "synchronous", "offset", "move", "同步建模", "偏移面"],
        icon: "directEdit",
        shortcut: "D",
        scope: "viewport",
        isActive: (ctx) => ctx.activeDashboardKind === "direct_edit",
        execute: (args) => {
          const sizeMm = (args as { readonly sizeMm?: number } | undefined)?.sizeMm;
          onOpenFeatureDashboard("direct_edit", { sizeMm });
        },
      },

      // 草图上下文命令
      {
        id: "sketch.addRect",
        title: "草图：添加矩形",
        category: "草图",
        ribbonPath: "草图 › 2D 图元",
        icon: "boxPrimitive",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("rect"),
      },
      {
        id: "sketch.addCircle",
        title: "草图：添加圆",
        category: "草图",
        ribbonPath: "草图 › 2D 图元",
        icon: "measureCircle",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("circle"),
      },
      {
        id: "sketch.addLine",
        title: "草图：添加直线",
        category: "草图",
        ribbonPath: "草图 › 2D 图元",
        icon: "wireframe",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("line"),
      },
      {
        id: "sketch.addArc",
        title: "草图：添加圆弧",
        category: "草图",
        ribbonPath: "草图 › 2D 图元",
        icon: "fillet",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("arc"),
      },
      {
        id: "sketch.constrainHorizontal",
        title: "草图：水平 / 竖直约束",
        category: "草图",
        ribbonPath: "草图 › 几何与尺寸约束",
        icon: "flip",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("constrain_hv"),
      },
      {
        id: "sketch.constrainDimension",
        title: "草图：添加尺寸约束",
        category: "草图",
        ribbonPath: "草图 › 几何与尺寸约束",
        icon: "measureFace",
        when: (ctx) => Boolean(ctx.sketchActive),
        execute: () => onSketchQuickAdd("constrain_dim"),
      },
      {
        id: "dashboard.confirm",
        title: "确认并提交当前特征仪表板",
        category: "建模",
        icon: "check",
        when: (ctx) => Boolean(ctx.activeDashboardKind),
        execute: onConfirmFeatureDashboard,
      },
      {
        id: "dashboard.cancel",
        title: "取消当前特征仪表板",
        category: "建模",
        icon: "close",
        when: (ctx) => Boolean(ctx.activeDashboardKind),
        execute: onCancelFeatureDashboard,
      },

      // 装配 / 分析 / 标注
      {
        id: "assembly.addMate",
        title: "添加 6-DOF 装配配合 (Mate)",
        category: "装配",
        ribbonPath: "装配 › 6-DOF 装配约束",
        keywords: ["mate", "assembly", "coincident", "concentric", "配合"],
        icon: "mate",
        isActive: (ctx) => ctx.activeDashboardKind === "mate",
        execute: () => onOpenFeatureDashboard("mate"),
      },
      {
        id: "annotate.tagTopology",
        title: "语义拓扑打标 (Tag Topology / PMI)",
        category: "标注",
        ribbonPath: "标注与工程图 › 三维语义与 PMI",
        keywords: ["tag", "pmi", "semantic", "annotation", "打标", "标注"],
        icon: "tag",
        isActive: (ctx) => ctx.activeDashboardKind === "tag_topology",
        execute: () => onOpenFeatureDashboard("tag_topology"),
      },
      {
        id: "analysis.addAssertion",
        title: "添加设计断言门禁 (Assert Gate)",
        category: "分析",
        ribbonPath: "分析与验证 › L0–L4 三值门禁",
        keywords: ["assert", "gate", "dfm", "wall", "断言", "门禁"],
        icon: "assertGate",
        isActive: (ctx) => ctx.activeDashboardKind === "design_assertion",
        execute: () => onOpenFeatureDashboard("design_assertion"),
      },

      // 检查与测量
      {
        id: "inspect.measureCircle",
        title: "测量圆心距",
        category: "分析",
        ribbonPath: "分析与验证 › 几何测量",
        keywords: ["measure", "circle", "center", "距离"],
        icon: "measureCircle",
        shortcut: "M",
        scope: "viewport",
        isActive: (ctx) => ctx.viewportTab === "3d" && ctx.measurementMode === "circle",
        execute: () => {
          setViewportTab("3d");
          setMeasurementMode((cur) => (cur === "circle" ? null : "circle"));
        },
      },
      {
        id: "inspect.measureFace",
        title: "测量面间距",
        category: "分析",
        ribbonPath: "分析与验证 › 几何测量",
        keywords: ["measure", "face", "plane", "距离"],
        icon: "measureFace",
        shortcut: "Shift+M",
        scope: "viewport",
        isActive: (ctx) => ctx.viewportTab === "3d" && ctx.measurementMode === "face",
        execute: () => {
          setViewportTab("3d");
          setMeasurementMode((cur) => (cur === "face" ? null : "face"));
        },
      },
      {
        id: "inspect.clearMeasurement",
        title: "清除当前测量",
        category: "分析",
        ribbonPath: "分析与验证 › 几何测量",
        keywords: ["clear", "ruler"],
        icon: "trash",
        when: (ctx) => ctx.hasMeasurement,
        execute: () => {
          viewportRef.current?.clearMeasurement();
          setMeasurementMode(null);
        },
      },

      // 导航键位预设
      {
        id: "nav.preset.nxCreo",
        title: "鼠标导航：NX / Creo 工业标准",
        description: "中键旋转 · Shift/右键+中键平移 · 左键点选/框选 · 中键确认",
        category: "工具",
        ribbonPath: "工具与代码 › 导航键位方案",
        keywords: ["nx", "creo", "mouse", "navigation", "导航"],
        icon: "mouse",
        isActive: (ctx) => ctx.navigationPreset === "nx-creo",
        execute: () => {
          setNavigationPreset("nx-creo");
          notify({ level: "info", title: "已切换至 NX / Creo 工业鼠标导航", detail: "中键旋转 · Shift+中键平移 · 左键框选", source: "操控" });
        },
      },
      {
        id: "nav.preset.solidworks",
        title: "鼠标导航：SolidWorks 习惯",
        description: "中键旋转 · Ctrl+中键平移 · 左键点选/框选",
        category: "工具",
        ribbonPath: "工具与代码 › 导航键位方案",
        keywords: ["solidworks", "sw", "mouse", "navigation"],
        icon: "mouse",
        isActive: (ctx) => ctx.navigationPreset === "solidworks",
        execute: () => {
          setNavigationPreset("solidworks");
          notify({ level: "info", title: "已切换至 SolidWorks 鼠标导航", detail: "中键旋转 · Ctrl+中键平移 · 左键框选", source: "操控" });
        },
      },
      {
        id: "nav.preset.web",
        title: "鼠标导航：Web / 触控板兼容",
        description: "左键旋转 · 中键/Shift 平移 · Alt+左键框选",
        category: "工具",
        ribbonPath: "工具与代码 › 导航键位方案",
        keywords: ["web", "trackpad", "touchpad", "触控板"],
        icon: "mouse",
        isActive: (ctx) => ctx.navigationPreset === "web-trackpad",
        execute: () => {
          setNavigationPreset("web-trackpad");
          notify({ level: "info", title: "已切换至 Web / 触控板导航", detail: "左键旋转 · 中键/Shift 平移 · Alt+左键框选", source: "操控" });
        },
      },

      // 视图
      {
        id: "view.mode3d",
        title: "三维模型视图",
        category: "视图",
        ribbonPath: "视图 › 视图模式",
        keywords: ["3d", "viewport"],
        icon: "view3d",
        shortcut: "V",
        scope: "viewport",
        isActive: (ctx) => ctx.viewportTab === "3d",
        execute: () => setViewportTab("3d"),
      },
      {
        id: "view.modeDrawing",
        title: "2D 工程图视图",
        category: "视图",
        ribbonPath: "标注与工程图 › 2D 关联工程图",
        keywords: ["2d", "drawing", "svg"],
        icon: "drawing2d",
        shortcut: "Shift+V",
        scope: "viewport",
        isActive: (ctx) => ctx.viewportTab === "drawing",
        execute: () => setViewportTab("drawing"),
      },
      {
        id: "view.preset.iso",
        title: "等轴测视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["iso", "isometric"],
        icon: "viewIso",
        shortcut: "1",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("iso");
        },
      },
      {
        id: "view.preset.top",
        title: "俯视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["top"],
        icon: "viewTop",
        shortcut: "2",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("top");
        },
      },
      {
        id: "view.preset.front",
        title: "主视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["front"],
        icon: "viewFront",
        shortcut: "3",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("front");
        },
      },
      {
        id: "view.preset.right",
        title: "右视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["right"],
        icon: "viewRight",
        shortcut: "4",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("right");
        },
      },
      {
        id: "view.preset.left",
        title: "左视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["left"],
        icon: "viewRight",
        shortcut: "5",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("left");
        },
      },
      {
        id: "view.preset.back",
        title: "后视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["back"],
        icon: "viewFront",
        shortcut: "6",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("back");
        },
      },
      {
        id: "view.preset.bottom",
        title: "仰视视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["bottom"],
        icon: "viewTop",
        shortcut: "7",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("bottom");
        },
      },
      {
        id: "view.preset.isoBack",
        title: "后等轴测视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["iso", "back", "rear", "后等轴测"],
        icon: "viewIso",
        shortcut: "8",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.applyPreset("iso-back");
        },
      },
      {
        id: "view.zoomIn",
        title: "放大视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["zoom", "in", "放大"],
        icon: "zoomIn",
        shortcut: "=",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.zoomBy(1.25);
        },
      },
      {
        id: "view.zoomOut",
        title: "缩小视角",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["zoom", "out", "缩小"],
        icon: "zoomOut",
        shortcut: "-",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.zoomBy(0.8);
        },
      },
      {
        id: "view.rotateLeft",
        title: "视角左转 90°",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["rotate", "left", "turn", "左转"],
        icon: "rotateLeft",
        shortcut: "[",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.rotateStep(TURN_LEFT_YAW);
        },
      },
      {
        id: "view.rotateRight",
        title: "视角右转 90°",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["rotate", "right", "turn", "右转"],
        icon: "rotateRight",
        shortcut: "]",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.rotateStep(TURN_RIGHT_YAW);
        },
      },
      {
        id: "view.fit",
        title: "适应窗口 (Zoom to Fit)",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["fit", "zoom", "center", "适配", "居中"],
        icon: "fitView",
        shortcut: "Home",
        scope: "viewport",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.fitToView();
        },
      },
      {
        id: "view.normalTo",
        title: "正视于选中面 (Normal To)",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["normal", "align", "f8", "正视"],
        icon: "normalTo",
        shortcut: "F8",
        scope: "viewport",
        when: (ctx) => ctx.selectionKind === "face",
        disabledReason: () => "请先在 3D 视口中选择一个平面",
        execute: () => {
          setViewportTab("3d");
          viewportRef.current?.normalToSelection();
        },
      },
      {
        id: "display.cycleStyle",
        title: "切换带边着色 / 半透明透视",
        category: "视图",
        ribbonPath: "径向手势 › 右",
        keywords: ["cycle", "translucent", "shaded", "透视"],
        icon: "translucent",
        execute: handleCycleVisualStyle,
      },
      {
        id: "display.style.shadedEdges",
        title: "显示模式：带边线上色",
        category: "视图",
        ribbonPath: "视图 › 显示样式",
        keywords: ["shaded", "edges", "style", "带边"],
        icon: "cube",
        isActive: (ctx) => ctx.visualStyle === "shaded-edges",
        execute: () => handleChangeVisualStyle("shaded-edges"),
      },
      {
        id: "display.style.shaded",
        title: "显示模式：无边线上色",
        category: "视图",
        ribbonPath: "视图 › 显示样式",
        keywords: ["shaded", "smooth", "纯着色"],
        icon: "shaded",
        isActive: (ctx) => ctx.visualStyle === "shaded",
        execute: () => handleChangeVisualStyle("shaded"),
      },
      {
        id: "display.style.translucent",
        title: "显示模式：半透明透视",
        category: "视图",
        ribbonPath: "视图 › 显示样式",
        keywords: ["translucent", "xray", "ghosted", "半透明"],
        icon: "translucent",
        isActive: (ctx) => ctx.visualStyle === "translucent",
        execute: () => handleChangeVisualStyle("translucent"),
      },
      {
        id: "display.style.wireframe",
        title: "显示模式：纯线框",
        category: "视图",
        ribbonPath: "视图 › 显示样式",
        keywords: ["wireframe", "lines", "线框"],
        icon: "wireframe",
        isActive: (ctx) => ctx.visualStyle === "wireframe",
        execute: () => handleChangeVisualStyle("wireframe"),
      },
      {
        id: "display.style.hlr",
        title: "显示模式：隐藏线消除 (HLR)",
        category: "视图",
        ribbonPath: "视图 › 显示样式",
        keywords: ["hlr", "technical", "线稿", "隐藏线"],
        icon: "drawing2d",
        isActive: (ctx) => ctx.visualStyle === "hlr",
        execute: () => handleChangeVisualStyle("hlr"),
      },
      {
        id: "display.toggleProjection",
        title: "切换正交 / 透视投影",
        category: "视图",
        ribbonPath: "视图 › 标准相机视角",
        keywords: ["projection", "ortho", "perspective", "投影", "正交", "透视"],
        icon: "perspective",
        shortcut: "P",
        scope: "viewport",
        isActive: (ctx) => ctx.projection === "perspective",
        execute: handleToggleProjection,
      },
      {
        id: "display.projection.ortho",
        title: "切换为正交投影",
        category: "视图",
        keywords: ["ortho", "正交"],
        icon: "orthographic",
        isActive: (ctx) => ctx.projection === "orthographic",
        execute: () => {
          setViewportTab("3d");
          setDisplayState((prev) => setProjectionMode(prev, "orthographic"));
        },
      },
      {
        id: "display.projection.perspective",
        title: "切换为透视投影",
        category: "视图",
        keywords: ["perspective", "persp", "透视"],
        icon: "perspective",
        isActive: (ctx) => ctx.projection === "perspective",
        execute: () => {
          setViewportTab("3d");
          setDisplayState((prev) => setProjectionMode(prev, "perspective"));
        },
      },
      {
        id: "display.hideSelected",
        title: "隐藏选中对象",
        category: "视图",
        ribbonPath: "视图 › 显示辅助",
        keywords: ["hide", "visibility", "隐藏"],
        icon: "eyeOff",
        shortcut: "Ctrl+B",
        scope: "viewport",
        when: (ctx) => ctx.hasSelection || selectedComponentId !== null,
        disabledReason: () => "请先选择要隐藏的组件或面",
        execute: () => {
          if (selectedTarget?.kind === "face") {
            const compId = selectedTarget.face.component_id;
            if (isAssembly && compId) {
              handleToggleEntityHidden("component", compId);
            } else {
              handleToggleEntityHidden("face", selectedTarget.face.face_id);
            }
            setSelectedTarget(null);
          } else if (selectedTarget?.kind === "edge" && selectedTarget.edge.component_id) {
            handleToggleEntityHidden("component", selectedTarget.edge.component_id);
            setSelectedTarget(null);
          } else if (selectedComponentId) {
            handleToggleEntityHidden("component", selectedComponentId);
          }
        },
      },
      {
        id: "display.toggleTranslucent",
        title: "切换选中对象半透明",
        category: "视图",
        ribbonPath: "装配 › 装配审查",
        keywords: ["translucent", "transparent", "opacity", "半透明"],
        icon: "translucent",
        shortcut: "T",
        scope: "viewport",
        when: (ctx) => ctx.hasSelection || selectedComponentId !== null,
        disabledReason: () => "请先选择要调整透明度的组件或面",
        execute: () => {
          if (selectedTarget?.kind === "face") {
            const compId = selectedTarget.face.component_id;
            if (isAssembly && compId) {
              handleToggleEntityTranslucent("component", compId);
            } else {
              handleToggleEntityTranslucent("face", selectedTarget.face.face_id);
            }
          } else if (selectedTarget?.kind === "edge" && selectedTarget.edge.component_id) {
            handleToggleEntityTranslucent("component", selectedTarget.edge.component_id);
          } else if (selectedComponentId) {
            handleToggleEntityTranslucent("component", selectedComponentId);
          }
        },
      },
      {
        id: "display.isolate",
        title: "孤立选中对象",
        category: "视图",
        ribbonPath: "装配 › 装配审查",
        keywords: ["isolate", "solo", "孤立"],
        icon: "isolate",
        shortcut: "I",
        scope: "viewport",
        when: (ctx) => ctx.hasSelection || selectedComponentId !== null || Boolean(ctx.isIsolated),
        disabledReason: () => "请先选择要孤立显示的组件或面",
        isActive: (ctx) => Boolean(ctx.isIsolated),
        execute: () => {
          if (selectedTarget?.kind === "face") {
            const id = isAssembly && selectedTarget.face.component_id
              ? selectedTarget.face.component_id
              : selectedTarget.face.face_id;
            handleIsolateEntity(id);
          } else if (selectedTarget?.kind === "edge") {
            const id = selectedTarget.edge.component_id ?? selectedTarget.edge.edge_id;
            handleIsolateEntity(id);
          } else if (selectedComponentId) {
            handleIsolateEntity(selectedComponentId);
          } else {
            handleExitIsolate();
          }
        },
      },
      {
        id: "display.showAll",
        title: "显示全部对象",
        category: "视图",
        ribbonPath: "视图 › 显示辅助",
        keywords: ["show", "unhide", "all", "显示全部"],
        icon: "eye",
        shortcut: "Ctrl+Shift+B",
        scope: "viewport",
        execute: handleShowAllEntities,
      },
      {
        id: "display.clearOverrides",
        title: "清除全部局部显示覆写",
        category: "视图",
        keywords: ["reset", "clear", "overrides", "恢复显示"],
        icon: "refresh",
        execute: handleClearAllOverrides,
      },
      {
        id: "view.toggleKeepInBox",
        title: "显示 / 隐藏 Keep-In 包络盒",
        category: "视图",
        ribbonPath: "分析与验证 › 剖切与质量属性",
        keywords: ["bbox", "box", "envelope", "包络"],
        icon: "boundingBox",
        shortcut: "B",
        scope: "viewport",
        isActive: (ctx) => ctx.showKeepInBox,
        execute: () => {
          setViewportTab("3d");
          setShowKeepInBox((shown) => {
            const next = !shown;
            setDisplayState((prev) => setCategoryVisibility(prev, "keepInBox", next));
            return next;
          });
        },
      },
      {
        id: "view.flipSectionCut",
        title: "翻转剖切方向",
        category: "视图",
        keywords: ["section", "flip", "翻转剖切"],
        icon: "flip",
        when: (ctx) => ctx.sectionCutEnabled,
        execute: () => {
          setViewportTab("3d");
          setSectionCut((cut) => ({ ...cut, flip: !cut.flip }));
        },
      },
      {
        id: "view.toggleSectionCut",
        title: "开启 / 关闭实时剖切",
        category: "视图",
        ribbonPath: "分析与验证 › 剖切与质量属性",
        keywords: ["section", "cut", "clip", "剖面"],
        icon: "sectionCut",
        shortcut: "X",
        scope: "viewport",
        isActive: (ctx) => ctx.sectionCutEnabled,
        execute: () => {
          setViewportTab("3d");
          setSectionCut((cut) => ({ ...cut, enabled: !cut.enabled }));
        },
      },
      {
        id: "view.setSectionCut",
        title: "设置实时剖切参数",
        category: "视图",
        icon: "sectionCut",
        execute: (args) => {
          const patch = args as Partial<SectionCutState> | undefined;
          setViewportTab("3d");
          setSectionCut((cut) => ({
            enabled: patch?.enabled ?? cut.enabled,
            axis: patch?.axis ?? cut.axis,
            ratio: patch?.ratio ?? cut.ratio,
          }));
        },
      },
      {
        id: "view.toggleExplode",
        title: "切换装配爆炸视图",
        category: "装配",
        ribbonPath: "装配 › 装配审查",
        keywords: ["explode", "assembly", "爆炸"],
        icon: "explode",
        shortcut: "Shift+E",
        scope: "viewport",
        when: (ctx) => ctx.isAssembly,
        disabledReason: () => "仅多件装配体支持爆炸视图",
        isActive: (ctx) => ctx.explodeActive,
        execute: () => {
          setViewportTab("3d");
          setExplodeFactor((cur) => (cur > 0 ? 0 : 0.6));
        },
      },

      // AI 协作
      {
        id: "ai.inlinePrompt",
        title: "视口内联 AI Point-and-Prompt",
        description: "在 3D 视口就地唤起 AI 修改框并自动挂载选中 TopoRef。",
        category: "AI",
        ribbonPath: "AI 协作 › AI 生成与编辑",
        keywords: ["ai", "point", "prompt", "inline", "内联"],
        icon: "sparkles",
        shortcut: "Alt+A",
        scope: "global",
        execute: onOpenInlineAi,
      },
      {
        id: "ai.openGenerate",
        title: "AI 0→1 新建零件",
        category: "AI",
        ribbonPath: "AI 协作 › AI 生成与编辑",
        keywords: ["ai", "generate", "新建"],
        icon: "sparkles",
        shortcut: "Ctrl+Shift+A",
        scope: "global",
        execute: () => {
          setAiGenMode("generate");
          openRightDock("ai");
        },
      },
      {
        id: "ai.openEdit",
        title: "AI 1→N 增量修改当前模型",
        category: "AI",
        ribbonPath: "AI 协作 › AI 生成与编辑",
        keywords: ["ai", "edit", "修改"],
        icon: "sparkles",
        shortcut: "Ctrl+Shift+M",
        scope: "global",
        execute: () => {
          setAiGenMode("edit");
          openRightDock("ai");
        },
      },
      {
        id: "ai.editSelected",
        title: "让 AI 修改选中特征",
        category: "AI",
        ribbonPath: "AI 协作 › 任务控制",
        keywords: ["ai", "selected", "face", "edge"],
        icon: "sparkles",
        when: (ctx) => ctx.hasSelection,
        execute: () => {
          setAiGenMode("edit");
          openRightDock("ai");
        },
      },
      {
        id: "ai.cancel",
        title: "中止当前 AI 任务",
        category: "AI",
        ribbonPath: "AI 协作 › 任务控制",
        icon: "close",
        when: (ctx) => ctx.aiGenerating,
        execute: handleCancelAi,
      },

      // 面板 & 角色布局
      {
        id: "panel.openModelTree",
        title: "打开模型导航器",
        category: "窗口",
        ribbonPath: "左侧资源栏 › 模型树",
        icon: "cube",
        shortcut: "Ctrl+1",
        scope: "global",
        execute: () => openLeftDock("model"),
      },
      {
        id: "panel.openSource",
        title: "打开 KCL 源码视图",
        category: "窗口",
        ribbonPath: "工具与代码 › 代码与双向投影",
        icon: "code",
        shortcut: "Ctrl+2",
        scope: "global",
        execute: () => openLeftDock("source"),
      },
      {
        id: "panel.openChecks",
        title: "打开 L0-L4 验证与契约",
        category: "窗口",
        ribbonPath: "分析与验证 › L0–L4 三值门禁",
        icon: "shieldCheck",
        shortcut: "Ctrl+3",
        scope: "global",
        execute: () => openLeftDock("checks"),
      },
      {
        id: "panel.openProperties",
        title: "打开属性检查器与参数表",
        category: "窗口",
        ribbonPath: "右侧工具轨 › 属性检查",
        icon: "properties",
        shortcut: "Ctrl+4",
        scope: "global",
        execute: () => openRightDock("properties"),
      },
      {
        id: "panel.openAssembly",
        title: "打开装配导航器与 BOM",
        category: "装配",
        ribbonPath: "装配 › 组件管理",
        icon: "assembly",
        execute: () => openLeftDock("assembly"),
      },
      {
        id: "panel.openExport",
        title: "打开工程导出面板",
        category: "文件",
        ribbonPath: "右侧工具轨 › 工程导出",
        icon: "export",
        execute: () => openRightDock("export"),
      },
      {
        id: "layout.toggleLeft",
        title: "切换左侧资源导航栏",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "sidebarLeft",
        shortcut: "Ctrl+[",
        scope: "global",
        isActive: (ctx) => ctx.leftOpen,
        execute: () => setLayoutPrefs((prev) => ({ ...prev, leftOpen: !prev.leftOpen })),
      },
      {
        id: "layout.toggleRight",
        title: "切换右侧 AI/属性抽屉",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "sidebarRight",
        shortcut: "Ctrl+J",
        scope: "global",
        isActive: (ctx) => ctx.rightOpen,
        execute: () => setLayoutPrefs((prev) => ({ ...prev, rightOpen: !prev.rightOpen })),
      },
      {
        id: "layout.preset.standard",
        title: "工作角色：机械设计 (NX/Creo)",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "layout",
        execute: () => applyLayoutPreset("standard"),
      },
      {
        id: "layout.preset.code",
        title: "工作角色：代码协同 (KCL)",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "code",
        execute: () => applyLayoutPreset("code"),
      },
      {
        id: "layout.preset.ai",
        title: "工作角色：AI 创作 (Copilot)",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "sparkles",
        execute: () => applyLayoutPreset("ai"),
      },
      {
        id: "layout.toggleZen",
        title: "纯净视口模式 (Zen)",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        icon: "maximize",
        shortcut: "Ctrl+\\",
        scope: "global",
        isActive: (ctx) => ctx.zenMode,
        execute: () =>
          applyLayoutPreset(!layoutPrefs.leftOpen && !layoutPrefs.rightOpen ? "standard" : "zen"),
      },
      {
        id: "theme.toggle",
        title: layoutPrefs.theme === "dark" ? "切换到浅色蓝图主题" : "切换到深色工业主题",
        category: "窗口",
        ribbonPath: "视图 › 工作区与角色",
        keywords: ["theme", "dark", "light", "主题"],
        icon: layoutPrefs.theme === "dark" ? "sun" : "moon",
        shortcut: "Ctrl+Shift+T",
        scope: "global",
        execute: () =>
          setLayoutPrefs((prev) => ({ ...prev, theme: prev.theme === "dark" ? "light" : "dark" })),
      },
      {
        id: "command.openPalette",
        title: "打开 Command Finder 命令查找器",
        category: "工具",
        ribbonPath: "标题栏 › Command Finder",
        keywords: ["palette", "finder", "search", "命令"],
        icon: "command",
        shortcut: "Ctrl+K",
        scope: "global",
        execute: () => setPaletteOpen(true),
      },
      {
        id: "help.shortcuts",
        title: "快捷键与命令行速查",
        category: "帮助",
        ribbonPath: "工具与代码 › 命令与帮助",
        keywords: ["help", "shortcut", "hotkey", "帮助"],
        icon: "keyboard",
        shortcut: "?",
        scope: "viewport",
        execute: () => setShortcutsOpen(true),
      },
      ...(experimentalEnabled
        ? [
            {
              id: "experimental.subd",
              title: "SubD",
              category: "建模" as const,
              keywords: ["subd", "subdivision", "实验"],
              icon: "cube" as const,
              scope: "viewport" as const,
              when: () => experimentalTracksEnabled(),
              execute: () => notify({ level: "info", title: "SubD 实验轨道已启用", source: "实验" }),
            },
            {
              id: "experimental.tpms",
              title: "TPMS",
              category: "建模" as const,
              keywords: ["tpms", "gyroid", "field", "实验"],
              icon: "cube" as const,
              scope: "viewport" as const,
              when: () => experimentalTracksEnabled(),
              execute: () => notify({ level: "info", title: "TPMS 实验轨道已启用", source: "实验" }),
            },
            {
              id: "experimental.gpuSlang",
              title: "GPU Slang",
              category: "建模" as const,
              keywords: ["gpu", "slang", "实验"],
              icon: "cube" as const,
              scope: "viewport" as const,
              when: () => experimentalTracksEnabled(),
              execute: () => notify({ level: "info", title: "GPU Slang 实验轨道已启用", source: "实验" }),
            },
          ]
        : []),
    ],
    [
      activeDashboard,
      applyLayoutPreset,
      displayState.isolatedIds,
      experimentalEnabled,
      handleApplyQuickFeature,
      handleCancelAi,
      handleChangeVisualStyle,
      handleClearAllOverrides,
      handleCycleVisualStyle,
      handleDownloadSource,
      handleExitIsolate,
      handleExportCommand,
      handleFocusAstSpan,
      handleIsolateEntity,
      handleRedo,
      handleShowAllEntities,
      handleToggleEntityHidden,
      handleToggleEntityTranslucent,
      handleToggleProjection,
      handleUndo,
      isAssembly,
      layoutPrefs.leftOpen,
      layoutPrefs.rightOpen,
      layoutPrefs.theme,
      measurementMode,
      notify,
      onCancelFeatureDashboard,
      onConfirmFeatureDashboard,
      onOpenFeatureDashboard,
      onOpenFileBackstage,
      onOpenInlineAi,
      onRegenerateModel,
      onSketchQuickAdd,
      openLeftDock,
      openRightDock,
      selectedComponentId,
      selectedTarget,
      setAiGenMode,
      setDisplayState,
      setExplodeFactor,
      setLayoutPrefs,
      setMeasurementMode,
      setNavigationPreset,
      setSectionCut,
      setSelectedTarget,
      setSelectionFilter,
      setShowKeepInBox,
      setViewportTab,
      sourceFileInput,
      viewportRef,
    ],
  );

  const commandMap = useMemo(() => {
    const map = new Map<string, CommandDefinition>();
    for (const cmd of commands) map.set(cmd.id, cmd);
    return map;
  }, [commands]);

  const executeCommand = useCallback(
    (commandId: string, args?: unknown) => {
      const cmd = commandMap.get(commandId);
      if (!cmd) return;
      if (!isCommandEnabled(cmd, commandContext)) {
        const reason = cmd.disabledReason?.(commandContext);
        if (reason) {
          notify({ level: "warning", title: `${cmd.title}暂不可用`, detail: reason, source: "命令" });
        }
        return;
      }
      void cmd.execute(args);
    },
    [commandContext, commandMap, notify],
  );

  // Global keyboard shortcut dispatcher (scope-aware + secondary industrial aliases)
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "F1") {
        event.preventDefault();
        setPaletteOpen(true);
        return;
      }
      const inEditable = isEditableElement(event.target);

      // Secondary industrial CAD aliases: Ctrl+Shift+Z (Redo), Ctrl+F / 0 (Fit), N (Normal To), Y/Shift+Y (Hide/Show), Alt+L (Left Dock)
      if (!inEditable) {
        if ((event.ctrlKey || event.metaKey) && event.shiftKey && event.key.toLowerCase() === "z") {
          event.preventDefault();
          executeCommand("edit.redo");
          return;
        }
        if (((event.ctrlKey || event.metaKey) && !event.shiftKey && event.key.toLowerCase() === "f") || (!event.ctrlKey && !event.metaKey && !event.altKey && event.key === "0")) {
          event.preventDefault();
          executeCommand("view.fit");
          return;
        }
        if (!event.ctrlKey && !event.metaKey && !event.altKey && event.key.toLowerCase() === "n") {
          event.preventDefault();
          executeCommand("view.normalTo");
          return;
        }
        if (!event.ctrlKey && !event.metaKey && !event.altKey && event.key.toLowerCase() === "y") {
          event.preventDefault();
          executeCommand(event.shiftKey ? "display.showAll" : "display.hideSelected");
          return;
        }
        if (!event.ctrlKey && !event.metaKey && event.altKey && event.key.toLowerCase() === "l") {
          event.preventDefault();
          executeCommand("layout.toggleLeft");
          return;
        }
      }

      for (const cmd of commands) {
        if (!cmd.shortcut) continue;
        if (inEditable && cmd.scope !== "global") continue;
        if (inEditable && (cmd.id === "edit.undo" || cmd.id === "edit.redo")) continue;
        if (matchesKeyboardEvent(cmd.shortcut, event)) {
          event.preventDefault();
          executeCommand(cmd.id);
          return;
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [commands, executeCommand]);

  const handleSubmitCommandLine = useCallback(
    (raw: string) => {
      const parsed = parseCommandLine(raw);
      if (parsed.kind === "empty") return;
      if (parsed.kind === "error") {
        notify({ level: "warning", title: "命令解析提示", detail: parsed.message, source: "命令行" });
        return;
      }
      if (parsed.kind === "ai") {
        setAiGenMode(parsed.mode);
        setAiExternalPrompt({ text: parsed.prompt, seq: Date.now() });
        openRightDock("ai");
        void handleAiGenerate(parsed.prompt, selectedMaterial.id, "CNC_3AXIS_MILLING", {
          mode: parsed.mode,
          llm: { provider: "auto" },
        });
        return;
      }
      executeCommand(parsed.commandId, parsed.args);
    },
    [executeCommand, handleAiGenerate, notify, openRightDock, selectedMaterial.id, setAiGenMode],
  );

  const contextMenuItems: readonly ContextMenuItem[] = useMemo(() => {
    if (selectedTarget?.kind === "face") {
      return [
        { id: "ctx-sketch", label: "在此面新建草图", icon: "sketch", shortcut: "S", onSelect: () => executeCommand("model.sketch") },
        { id: "ctx-hole", label: "在此面添加孔 / 标准孔…", icon: "throughHole", shortcut: "H", onSelect: () => executeCommand("model.throughHole") },
        { id: "ctx-shell", label: "以此面为开口抽壳…", icon: "shell", shortcut: "L", onSelect: () => executeCommand("model.shell") },
        { id: "ctx-direct", label: "同步偏移 / 移动此面…", icon: "directEdit", shortcut: "D", onSelect: () => executeCommand("model.directEdit") },
        { id: "ctx-normal", label: "正视于此面 (Normal To)", icon: "normalTo", shortcut: "F8", separatorBefore: true, onSelect: () => executeCommand("view.normalTo") },
        { id: "ctx-hide", label: "隐藏选中对象", icon: "eyeOff", shortcut: "Y", onSelect: () => executeCommand("display.hideSelected") },
        { id: "ctx-trans", label: "切换半透明", icon: "translucent", shortcut: "T", onSelect: () => executeCommand("display.toggleTranslucent") },
        { id: "ctx-isolate", label: "孤立选中对象", icon: "isolate", shortcut: "I", onSelect: () => executeCommand("display.isolate") },
        { id: "ctx-tag", label: "添加语义拓扑标签…", icon: "tag", separatorBefore: true, onSelect: () => executeCommand("annotate.tagTopology") },
        { id: "ctx-mface", label: "测量面间距", icon: "measureFace", shortcut: "Shift+M", onSelect: () => executeCommand("inspect.measureFace") },
        { id: "ctx-src", label: "定位 KCL 源码行", icon: "code", shortcut: "F12", onSelect: () => executeCommand("source.focusSelection") },
        { id: "ctx-ai", label: "AI 内联修改此面…", icon: "sparkles", shortcut: "Alt+A", onSelect: () => executeCommand("ai.inlinePrompt") },
        { id: "ctx-clear", label: "清除选择", icon: "close", shortcut: "Esc", separatorBefore: true, onSelect: () => executeCommand("selection.clear") },
      ];
    }
    if (selectedTarget?.kind === "edge") {
      return [
        { id: "ctx-fillet", label: "边缘圆角…", icon: "fillet", shortcut: "F", onSelect: () => executeCommand("model.filletEdges") },
        { id: "ctx-chamfer", label: "边缘倒角…", icon: "chamfer", shortcut: "C", onSelect: () => executeCommand("model.chamferEdges") },
        { id: "ctx-isolate", label: "孤立选中对象", icon: "isolate", shortcut: "I", separatorBefore: true, onSelect: () => executeCommand("display.isolate") },
        { id: "ctx-mcircle", label: "测量圆心距", icon: "measureCircle", shortcut: "M", separatorBefore: true, onSelect: () => executeCommand("inspect.measureCircle") },
        { id: "ctx-src", label: "定位 KCL 源码行", icon: "code", shortcut: "F12", onSelect: () => executeCommand("source.focusSelection") },
        { id: "ctx-ai", label: "AI 内联修改此边…", icon: "sparkles", shortcut: "Alt+A", onSelect: () => executeCommand("ai.inlinePrompt") },
        { id: "ctx-clear", label: "清除选择", icon: "close", shortcut: "Esc", separatorBefore: true, onSelect: () => executeCommand("selection.clear") },
      ];
    }
    return [
      { id: "ctx-fit", label: "适应窗口 (Zoom to Fit)", icon: "fitView", shortcut: "Home", onSelect: () => executeCommand("view.fit") },
      { id: "ctx-proj", label: displayState.projection === "orthographic" ? "切换为透视投影" : "切换为正交投影", icon: "perspective", shortcut: "P", onSelect: () => executeCommand("display.toggleProjection") },
      ...(hasActiveDisplayOverrides(displayState)
        ? [{ id: "ctx-showall", label: "显示全部对象", icon: "eye" as const, shortcut: "Shift+Y", onSelect: () => executeCommand("display.showAll") }]
        : []),
      { id: "ctx-iso", label: "等轴测视角", icon: "viewIso", shortcut: "1", separatorBefore: true, onSelect: () => executeCommand("view.preset.iso") },
      { id: "ctx-top", label: "俯视视角", icon: "viewTop", shortcut: "2", onSelect: () => executeCommand("view.preset.top") },
      { id: "ctx-front", label: "主视视角", icon: "viewFront", shortcut: "3", onSelect: () => executeCommand("view.preset.front") },
      { id: "ctx-right", label: "右视视角", icon: "viewRight", shortcut: "4", onSelect: () => executeCommand("view.preset.right") },
      { id: "ctx-sketch", label: "新建 2D 草图", icon: "sketch", shortcut: "S", separatorBefore: true, onSelect: () => executeCommand("model.sketch") },
      { id: "ctx-sec", label: "切换实时剖切", icon: "sectionCut", shortcut: "X", onSelect: () => executeCommand("view.toggleSectionCut") },
      { id: "ctx-ai", label: "视口内联 AI 指令…", icon: "sparkles", shortcut: "Alt+A", separatorBefore: true, onSelect: () => executeCommand("ai.inlinePrompt") },
      { id: "ctx-palette", label: "打开 Command Finder…", icon: "command", shortcut: "Ctrl+K", onSelect: () => executeCommand("command.openPalette") },
    ];
  }, [displayState, executeCommand, selectedTarget]);

  const handleViewportContextMenu = useCallback((event: ReactMouseEvent<HTMLCanvasElement>) => {
    setContextMenuPos({ x: event.clientX, y: event.clientY });
  }, []);

  return {
    commandContext,
    commands,
    commandMap,
    executeCommand,
    handleSubmitCommandLine,
    contextMenuItems,
    handleViewportContextMenu,
    paletteOpen,
    setPaletteOpen,
    shortcutsOpen,
    setShortcutsOpen,
    resetConfirmOpen,
    setResetConfirmOpen,
    contextMenuPos,
    setContextMenuPos,
    aiExternalPrompt,
  };
}
