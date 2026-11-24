import { useCallback, useEffect, useMemo, useState } from "react";
import { INITIAL_DEMO_KCL, MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import { parameterFromDimension } from "../core/session/engineeringSession.ts";
import { materialIdInSource, sourceWithMaterial } from "../sync/materialAnnotation.ts";
import type { WorkbenchBackend } from "../sync/workbenchBackend.ts";
import type { DatumPlane } from "../types/apiCommands.ts";
import type { AiGenerateMode } from "../types/scene.ts";
import {
  clearNotificationLogs,
  countUnreadNotifications,
  createInitialNotificationState,
  dismissToast,
  markAllLogsRead,
  pushNotification,
  type PushNotificationInput,
} from "../core/notifications/notificationStore.ts";
import {
  applyPresetToPrefs,
  clampLeftWidth,
  clampRightWidth,
  enforceResponsiveDockBudget,
  loadLayoutPrefs,
  saveLayoutPrefs,
  type LayoutPreset,
} from "../core/layout/layoutStore.ts";
import {
  loadWorkbenchUiPrefs,
  updateWorkbenchUiPrefs,
  type PersistedLeftTab,
  type PersistedRightTab,
} from "../core/layout/workbenchPrefsStore.ts";
import {
  buildFeatureCueText,
  buildFeatureExecutionPlan,
  createFeatureDashboardDraft,
  FEATURE_DASHBOARD_META,
  type FeatureDashboardDraft,
  type FeatureDashboardKind,
} from "../core/modeling/featureDashboardModel.ts";
import { getNavigationPresetMeta } from "../core/viewport/navigationPreset.ts";
import { collectSemanticTags } from "../core/viewport/displayState.ts";
import { SessionChangePreviewCard } from "../features/ai-assistant/ChangePreviewCard.tsx";
import { ViewportChangePreviewBar } from "../features/ai-assistant/ViewportChangePreviewBar.tsx";
import { InlinePointPromptPopover } from "../features/ai-assistant/InlinePointPromptPopover.tsx";
import { AiAssistantPanel } from "../features/ai-assistant/AiAssistantPanel.tsx";
import { FeatureDashboard } from "../features/modeling/FeatureDashboard.tsx";
import { useDesktopUpdater } from "../hooks/useDesktopUpdater.ts";
import { useEngineeringSession } from "../hooks/useEngineeringSession.ts";
import { useWorkbenchCommands } from "../hooks/useWorkbenchCommands.ts";
import { useWorkbenchDocument } from "../hooks/useWorkbenchDocument.ts";
import { useWorkbenchViewport } from "../hooks/useWorkbenchViewport.ts";
import { sourceUndoAction } from "../hooks/workbenchSource.ts";
import { RibbonBar } from "../ui/chrome/RibbonBar.tsx";
import { FileBackstage } from "../ui/chrome/FileBackstage.tsx";
import { TopBorderBar } from "../ui/chrome/TopBorderBar.tsx";
import { CommandPalette } from "../ui/chrome/CommandPalette.tsx";
import { CommandStatusBar } from "../ui/chrome/CommandStatusBar.tsx";
import { NotificationDrawer, ToastStack } from "../ui/chrome/NotificationCenter.tsx";
import { ConfirmDialog, ContextMenu, ShortcutsModal } from "../ui/chrome/StudioOverlays.tsx";
import { ActivityBar, DockPanel, type DockTabItem } from "../ui/chrome/DockPanel.tsx";
import { ViewportHudOverlay } from "../features/viewport/ViewportHudOverlay.tsx";
import { CenterViewport } from "./CenterViewport.tsx";
import { LeftPanel, useFeatureTreeMount } from "./LeftPanel.tsx";
import { RightPanel } from "./RightPanel.tsx";
import { TraceIdChip } from "./TraceIdChip.tsx";
import { WorkbenchTabPanel } from "./WorkbenchTabs.tsx";

export function WorkbenchLayout({
  backend,
  activeProjectPath = null,
}: {
  readonly backend: WorkbenchBackend;
  readonly activeProjectPath?: string | null;
}) {
  const {
    graphIr,
    activeBranch,
    selectedNodeId,
    selectedNodeIds,
    selectedTopoRefs,
    preview: changePreview,
    applySemanticCommands,
  } = useEngineeringSession();
  const featureTree = useFeatureTreeMount();
  const [initialUiPrefs] = useState(loadWorkbenchUiPrefs);
  const [layoutPrefs, setLayoutPrefs] = useState(loadLayoutPrefs);
  const [leftTab, setLeftTab] = useState<PersistedLeftTab>(initialUiPrefs.leftTab);
  const [rightTab, setRightTab] = useState<PersistedRightTab>(initialUiPrefs.rightTab);
  const [fileBackstageOpen, setFileBackstageOpen] = useState(false);
  const [inlineAiOpen, setInlineAiOpen] = useState(false);
  const [activeDashboard, setActiveDashboard] = useState<FeatureDashboardDraft | null>(null);

  useEffect(() => {
    saveLayoutPrefs(layoutPrefs);
    if (typeof document !== "undefined") {
      document.documentElement.dataset.theme = layoutPrefs.theme;
    }
  }, [layoutPrefs]);

  const openLeftDock = useCallback((tab: PersistedLeftTab) => {
    setLeftTab(tab);
    setLayoutPrefs((prev) => {
      const opened = prev.leftOpen ? prev : { ...prev, leftOpen: true };
      const vw = typeof window !== "undefined" ? window.innerWidth : 1440;
      return enforceResponsiveDockBudget(opened, vw, "left");
    });
  }, []);

  const openRightDock = useCallback((tab: PersistedRightTab) => {
    setRightTab(tab);
    setLayoutPrefs((prev) => {
      const opened = prev.rightOpen ? prev : { ...prev, rightOpen: true };
      const vw = typeof window !== "undefined" ? window.innerWidth : 1440;
      return enforceResponsiveDockBudget(opened, vw, "right");
    });
  }, []);

  const applyLayoutPreset = useCallback((preset: LayoutPreset) => {
    setLayoutPrefs((prev) => applyPresetToPrefs(prev, preset));
    if (preset === "code") setLeftTab("source");
    if (preset === "ai") setRightTab("ai");
  }, []);

  const [notifications, setNotifications] = useState(createInitialNotificationState);
  const notify = useCallback((input: PushNotificationInput) => {
    setNotifications((prev) => pushNotification(prev, input).state);
  }, []);
  const desktopUpdater = useDesktopUpdater(backend, notify);

  const viewport = useWorkbenchViewport(initialUiPrefs, notify);
  const {
    viewportRef, viewportTab, setViewportTab, measurementMode, setMeasurementMode, showKeepInBox,
    displayState, savedDisplayStates, cameraOrientation, setCameraOrientation, sectionCut, setSectionCut,
    explodeFactor, setExplodeFactor, quickParamInput, selectedMaterial, setSelectedMaterial,
    selectedComponentId, setSelectedComponentId, selectionFilter, setSelectionFilter,
    selectionIntent, setSelectionIntent, navigationPreset, setNavigationPreset,
    highlightedSpan, highlightLabel, selectedTarget, selectedTargets, setSelectedTarget, setSelectedTargets,
    handleFocusAstSpan, handleChangeVisualStyle, handleToggleProjection, handleToggleCategory, handleResetCategories,
    handleApplyDisplayPreset, handleToggleEntityHidden, handleToggleEntityTranslucent, handleIsolateEntity,
    handleExitIsolate, handleShowAllEntities, handleClearAllOverrides, handleSaveDisplayState,
    handleRestoreDisplayState, handleDeleteDisplayState, handleSelectSemanticFilter,
  } = viewport;

  const [aiGenMode, setAiGenMode] = useState<AiGenerateMode>(initialUiPrefs.aiGenMode);
  useEffect(() => {
    updateWorkbenchUiPrefs({
      leftTab,
      rightTab,
      viewportTab,
      showKeepInBox,
      sectionCut,
      quickParamInput,
      materialId: selectedMaterial.id,
      aiGenMode,
      navigationPreset,
      selectionIntent,
    });
  }, [leftTab, rightTab, viewportTab, showKeepInBox, sectionCut, quickParamInput, selectedMaterial.id, aiGenMode, navigationPreset, selectionIntent]);

  const documentState = useWorkbenchDocument(backend, notify, viewport, openLeftDock, activeProjectPath);
  const {
    kclSource, revision, designContract, verificationReport, dispatchSemanticCommand,
    sourceRevisionRef, sourceFileInput, applySourceUpdate, canUndo,
    model, evaluated, compiling, compilePending, showingLastAccepted, transportError, isAssembly, canExport, exporting, currentExport,
    errorCount, statusErrorCount, holeOffsetPending, handleUndo, handleCommitPreview, handleDiscardPreview, previewActionPending,
    handleOffsetHole, handleLoadKclFile, documentFileName, setDocumentFileName, aicadFileInput,
    pendingPreviewExport, confirmPreviewExport, cancelPreviewExport,
    aiConfigStatus, aiProfiles, activeAiProfileId, aiProfileStorage,
    handleSaveAiProfiles, handleSelectAiProfile, handleResetAiProfiles, handleTestAiConnection,
    aiGenerating, aiFeedback, aiUndoRevision, aiProgressSteps, aiThread, handleClearAiThread,
    handleCancelAi, handleAiGenerate,
  } = documentState;

  useEffect(() => {
    const annotated = materialIdInSource(kclSource);
    const preset = MATERIAL_PRESETS.find((item) => item.id === annotated);
    if (preset && preset.id !== selectedMaterial.id) setSelectedMaterial(preset);
  }, [kclSource, selectedMaterial.id, setSelectedMaterial]);

  // Sync live 3D selection into active Feature Dashboard collector (Creo Collector behavior)
  useEffect(() => {
    if (!activeDashboard) return;
    const meta = FEATURE_DASHBOARD_META[activeDashboard.kind];
    if (meta.collectorTarget === "none") return;
    const matchingRefs = selectedTargets
      .filter((t) => {
        if (meta.collectorTarget === "face") return t.kind === "face";
        if (meta.collectorTarget === "edge") return t.kind === "edge";
        return true;
      })
      .map((t) => (t.kind === "face" ? t.face.face_id : t.edge.edge_id));
    if (matchingRefs.length === 0) return;
    setActiveDashboard((prev) => {
      if (!prev) return prev;
      const nextRefs = meta.allowsMultipleRefs
        ? Array.from(new Set([...prev.references, ...matchingRefs]))
        : [matchingRefs[matchingRefs.length - 1]];
      if (nextRefs.length === prev.references.length && nextRefs.every((r, i) => r === prev.references[i])) {
        return prev;
      }
      return { ...prev, references: nextRefs };
    });
  }, [activeDashboard?.kind, selectedTargets]);

  const handleOpenFeatureDashboard = useCallback(
    (
      kind: FeatureDashboardKind,
      options?: {
        readonly holeMode?: FeatureDashboardDraft["holeMode"];
        readonly sizeMm?: number;
        readonly datumPlane?: DatumPlane;
      },
    ) => {
      setViewportTab("3d");
      setMeasurementMode(null);
      const meta = FEATURE_DASHBOARD_META[kind];
      const initialRefs = selectedTargets
        .filter((t) => {
          if (meta.collectorTarget === "face") return t.kind === "face";
          if (meta.collectorTarget === "edge") return t.kind === "edge";
          if (meta.collectorTarget === "none") return false;
          return true;
        })
        .map((t) => (t.kind === "face" ? t.face.face_id : t.edge.edge_id));
      setActiveDashboard(createFeatureDashboardDraft(kind, initialRefs, options));
    },
    [selectedTargets, setMeasurementMode, setViewportTab],
  );

  const commitDashboardDraft = useCallback(
    (keepOpen: boolean) => {
      if (!activeDashboard) return;
      const plan = buildFeatureExecutionPlan(activeDashboard, kclSource, graphIr ?? null);
      try {
        applySemanticCommands(
          [{ op: "apply_graph_patch", patch: plan.graphPatch }],
          plan.summary,
        );
        let undoRev: number | undefined;
        if (plan.nextKclSource && plan.nextKclSource !== kclSource) {
          undoRev = applySourceUpdate(plan.nextKclSource, { summary: plan.summary });
        }
        notify({
          level: "success",
          title: plan.summary,
          detail: `Semantic API: ${plan.semanticCommand.command}`,
          source: "建模",
          action: sourceUndoAction(undoRev),
        });
        if (!keepOpen) {
          setActiveDashboard(null);
        } else {
          setActiveDashboard((prev) =>
            prev ? createFeatureDashboardDraft(prev.kind, [], { holeMode: prev.holeMode, datumPlane: prev.datumPlane }) : null,
          );
        }
      } catch (error) {
        notify({
          level: "error",
          title: "特征提交失败",
          detail: error instanceof Error ? error.message : String(error),
          source: "建模",
        });
      }
    },
    [activeDashboard, applySemanticCommands, applySourceUpdate, graphIr, kclSource, notify],
  );

  const handleSketchQuickAdd = useCallback(
    (action: "rect" | "circle" | "line" | "arc" | "constrain_hv" | "constrain_dim") => {
      setActiveDashboard((prev) => {
        if (!prev || prev.kind !== "sketch") return prev;
        const n = prev.sketchEntities.length + 1;
        if (action === "rect") {
          return {
            ...prev,
            sketchEntities: [
              ...prev.sketchEntities,
              { kind: "rectangle", id: `rect_${n}`, origin_mm: [0, 0], width_mm: 40, height_mm: 30, centered: true },
            ],
          };
        }
        if (action === "circle") {
          return {
            ...prev,
            sketchEntities: [
              ...prev.sketchEntities,
              { kind: "circle", id: `circle_${n}`, center_mm: [0, 0], radius_mm: 12 },
            ],
          };
        }
        if (action === "line") {
          return {
            ...prev,
            sketchEntities: [
              ...prev.sketchEntities,
              { kind: "line", id: `line_${n}`, start_mm: [0, 0], end_mm: [30, 0] },
            ],
          };
        }
        if (action === "arc") {
          return {
            ...prev,
            sketchEntities: [
              ...prev.sketchEntities,
              { kind: "arc", id: `arc_${n}`, center_mm: [0, 0], start_mm: [15, 0], end_mm: [0, 15], ccw: true },
            ],
          };
        }
        const targetEntity = prev.sketchEntities[prev.sketchEntities.length - 1];
        if (!targetEntity) return prev;
        if (action === "constrain_hv") {
          return {
            ...prev,
            sketchConstraints: [...prev.sketchConstraints, { kind: "horizontal", entity: targetEntity.id }],
          };
        }
        return {
          ...prev,
          sketchConstraints: [
            ...prev.sketchConstraints,
            {
              kind: "diameter",
              entity: targetEntity.id,
              value: { binding: "literal", dimension: "length", value: 24, unit: "mm" },
            },
          ],
        };
      });
    },
    [],
  );

  const handleRegenerateModel = useCallback(() => {
    notify({
      level: "info",
      title: "已触发再生重建与 L0–L4 验证",
      detail: `当前版本 rev.${revision} · ${evaluated.diagnostics.metrics.face_count} 面 / ${evaluated.diagnostics.metrics.edge_count} 边`,
      source: "验证",
    });
  }, [evaluated.diagnostics.metrics.edge_count, evaluated.diagnostics.metrics.face_count, notify, revision]);

  const handleUpdateParameter = useCallback((name: string, value: number, unit: string) => {
    try {
      const undoRevision = dispatchSemanticCommand({
        op: "upsert_parameter",
        value: parameterFromDimension(name, value, unit),
      });
      notify({
        level: "success",
        title: `已更新参数 ${name}`,
        source: "参数",
        action: sourceUndoAction(undoRevision),
      });
    } catch (error) {
      notify({
        level: "error",
        title: "参数更新失败",
        detail: error instanceof Error ? error.message : String(error),
        source: "参数",
      });
    }
  }, [dispatchSemanticCommand, notify]);

  const handleSelectMaterial = useCallback((material: (typeof MATERIAL_PRESETS)[number]) => {
    setSelectedMaterial(material);
    if (materialIdInSource(kclSource) === material.id) return;
    applySourceUpdate(sourceWithMaterial(kclSource, material.id), { summary: `材料改为 ${material.id}` });
  }, [applySourceUpdate, kclSource, setSelectedMaterial]);

  const {
    commandContext, commands, commandMap, executeCommand, handleSubmitCommandLine, contextMenuItems,
    handleViewportContextMenu, paletteOpen, setPaletteOpen, shortcutsOpen, setShortcutsOpen,
    resetConfirmOpen, setResetConfirmOpen, contextMenuPos, setContextMenuPos, aiExternalPrompt,
  } = useWorkbenchCommands({
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
    hasPreview: Boolean(changePreview),
    onOpenFeatureDashboard: handleOpenFeatureDashboard,
    onConfirmFeatureDashboard: () => commitDashboardDraft(false),
    onCancelFeatureDashboard: () => setActiveDashboard(null),
    onSketchQuickAdd: handleSketchQuickAdd,
    onOpenFileBackstage: () => setFileBackstageOpen(true),
    onOpenInlineAi: () => setInlineAiOpen(true),
    onRegenerateModel: handleRegenerateModel,
  });

  const availableSemanticTags = useMemo(
    () => collectSemanticTags(evaluated.sceneMesh),
    [evaluated.sceneMesh],
  );

  // Unified Left Resource Bar tabs (5 orthogonal navigators — Assembly BOM merged into assembly tab)
  const leftDockTabs: readonly DockTabItem<PersistedLeftTab>[] = useMemo(
    () => [
      { id: "model", label: "模型树", icon: "cube" },
      {
        id: "assembly",
        label: "装配树",
        icon: "assembly",
        badge: evaluated.sceneMesh.components?.length || undefined,
      },
      {
        id: "checks",
        label: "诊断",
        icon: "shieldCheck",
        badge: errorCount > 0 ? errorCount : undefined,
        accent: errorCount > 0 ? "error" : undefined,
      },
      { id: "source", label: "源码", icon: "code" },
      { id: "assets", label: "资产", icon: "assetImport" },
    ],
    [errorCount, evaluated.sceneMesh.components?.length],
  );

  // Right On-Demand Tool Rail tabs (3 clean entries: AI Copilot | Unified Inspector | Export)
  const rightDockTabs: readonly DockTabItem<PersistedRightTab>[] = useMemo(
    () => [
      {
        id: "ai",
        label: "AI 助手",
        icon: "sparkles",
        accent: "ai",
        badge: changePreview ? "预览" : undefined,
      },
      { id: "properties", label: "属性", icon: "properties" },
      { id: "export", label: "导出", icon: "export" },
    ],
    [changePreview],
  );

  const runtimeBadge =
    model?.origin === "demo"
      ? "离线演示"
      : model?.origin === "browser-step"
        ? "离线 STEP"
        : backend.kind === "tauri"
          ? "Tauri · OCCT"
          : "本地编译服务";

  const runtimeTone: "host" | "demo" | "native" =
    model?.origin === "demo" || model?.origin === "browser-step"
      ? "demo"
      : backend.kind === "tauri"
        ? "native"
        : "host";

  const activeModeLabel =
    activeDashboard
      ? FEATURE_DASHBOARD_META[activeDashboard.kind].title
      : measurementMode === "circle"
        ? "测量：圆心距"
        : measurementMode === "face"
          ? "测量：面间距"
          : sectionCut.enabled
            ? `剖切：${sectionCut.axis} 轴`
            : null;

  // Dynamic NX-style Cue Line prompt
  const cueLine = useMemo(() => {
    if (activeDashboard) {
      return {
        text: buildFeatureCueText(activeDashboard),
        tone: activeDashboard.kind === "sketch" ? ("sketch" as const) : ("active" as const),
      };
    }
    if (measurementMode === "circle") {
      return { text: "测量圆心距：请依次点击两个圆弧边或圆柱孔边 · 按 Esc 退出", tone: "active" as const };
    }
    if (measurementMode === "face") {
      return { text: "测量平行面间距：请依次点击两个平行平面 · 按 Esc 退出", tone: "active" as const };
    }
    if (changePreview) {
      return { text: `AI 候选分支预览就绪 (${changePreview.previewBranch})：请检查 L0–L4 门禁并确认合入或放弃回滚`, tone: "warn" as const };
    }
    if (selectedTargets.length > 0) {
      const primary = selectedTargets[selectedTargets.length - 1];
      const name = primary.kind === "face" ? `面 ${primary.face.face_id}` : `边 ${primary.edge.edge_id}`;
      return {
        text: selectedTargets.length > 1
          ? `已多选 ${selectedTargets.length} 项（最新：${name}）· 可直接按 F 圆角 / C 倒角 / H 打孔 / Alt+A 唤起 AI 修改`
          : `已选 ${name} · 按 H 打孔 / F 圆角 / C 倒角 / F8 正视 / Alt+A AI 改选区 · 按住 Ctrl 多选`,
        tone: "active" as const,
      };
    }
    return {
      text: getNavigationPresetMeta(navigationPreset).idleHint,
      tone: "idle" as const,
    };
  }, [activeDashboard, changePreview, measurementMode, navigationPreset, selectedTargets]);

  const activeSelectionTopoRefs = useMemo(() => {
    const refs = new Set<string>(selectedTopoRefs);
    for (const item of selectedTargets) {
      refs.add(item.kind === "face" ? item.face.face_id : item.edge.edge_id);
    }
    return Array.from(refs);
  }, [selectedTargets, selectedTopoRefs]);

  const selectionSummaryText = useMemo(() => {
    if (selectedTargets.length === 0) return null;
    const first = selectedTargets[selectedTargets.length - 1];
    return first.kind === "face" ? first.face.face_id : first.edge.edge_id;
  }, [selectedTargets]);

  const unreadCount = countUnreadNotifications(notifications);

  return (
    <div className="studio-shell">
      <input
        ref={sourceFileInput}
        type="file"
        accept=".kcl"
        hidden
        onChange={(event) => {
          const file = event.currentTarget.files?.[0];
          event.currentTarget.value = "";
          if (file) void handleLoadKclFile(file);
        }}
      />
      <input
        ref={aicadFileInput}
        type="file"
        accept=".aicad,application/octet-stream"
        hidden
        onChange={(event) => {
          const file = event.currentTarget.files?.[0];
          event.currentTarget.value = "";
          if (file) void documentState.handleLoadAicadFile?.(file);
        }}
      />

      {/* Zone 1 & 2: Title Bar + QAT + Task-Oriented NX/Creo Ribbon */}
      <RibbonBar
        commandMap={commandMap}
        context={commandContext}
        onExecute={(id) => executeCommand(id)}
        onOpenFileBackstage={() => setFileBackstageOpen(true)}
        documentName={documentFileName}
        partName={evaluated.sceneMesh.part_name}
        activeBranch={activeBranch}
        compiling={compiling}
        hasError={evaluated.diagnostics.status === "error" || Boolean(transportError)}
        runtimeBadge={runtimeBadge}
        runtimeTone={runtimeTone}
        theme={layoutPrefs.theme}
        activeRole={layoutPrefs.activeRole}
        onApplyPreset={applyLayoutPreset}
        sketchActive={activeDashboard?.kind === "sketch"}
        drawingActive={viewportTab === "drawing"}
      />

      {/* Zone 3: NX Top Border Selection Bar & Cue Line */}
      <TopBorderBar
        selectionFilter={selectionFilter}
        onChangeSelectionFilter={(mode) => {
          setViewportTab("3d");
          setSelectionFilter(mode);
        }}
        selectionIntent={selectionIntent}
        onChangeSelectionIntent={setSelectionIntent}
        cueText={cueLine.text}
        cueTone={cueLine.tone}
        selectedCount={selectedTargets.length}
        selectionSummary={selectionSummaryText}
        onClearSelection={() => executeCommand("selection.clear")}
        onTriggerInlineAi={() => setInlineAiOpen(true)}
        hasPreview={Boolean(changePreview)}
        previewBranch={changePreview?.previewBranch}
        onFocusPreview={() => openRightDock("ai")}
      />

      {/* Main Workspace: Zone 4 (Left Resource Bar) + Zone 5 (Center Viewport) + Zone 6 (Right On-Demand Drawer & Rail) */}
      <div className="studio-workspace">
        <ActivityBar
          side="left"
          ariaLabel="左侧资源导航栏"
          items={leftDockTabs}
          activeId={leftTab}
          panelOpen={layoutPrefs.leftOpen}
          onSelect={(id) => {
            if (layoutPrefs.leftOpen && leftTab === id) {
              setLayoutPrefs((prev) => ({ ...prev, leftOpen: false }));
            } else {
              openLeftDock(id);
            }
          }}
        />

        <DockPanel
          side="left"
          open={layoutPrefs.leftOpen}
          pinned={layoutPrefs.leftPinned}
          onTogglePin={() => setLayoutPrefs((prev) => ({ ...prev, leftPinned: !prev.leftPinned }))}
          width={layoutPrefs.leftWidth}
          tabs={leftDockTabs}
          activeTab={leftTab}
          onSelectTab={setLeftTab}
          onResize={(w) => setLayoutPrefs((prev) => ({ ...prev, leftWidth: clampLeftWidth(w) }))}
          onClose={() => setLayoutPrefs((prev) => ({ ...prev, leftOpen: false }))}
        >
          <LeftPanel
            embedded
            activeTab={leftTab}
            onSelectTab={(tab) => {
              if (tab === "ai") openRightDock("ai");
              else setLeftTab(tab);
            }}
            backend={backend}
            aiGenMode={aiGenMode}
            onSelectAiMode={setAiGenMode}
            sceneMesh={evaluated.sceneMesh}
            selectedTarget={selectedTarget}
            onSelectTarget={(target) => {
              setSelectedTarget(target);
              if (target.kind === "face") {
                handleFocusAstSpan(target.face.ast_span, `Face: ${target.face.face_id}`);
              } else {
                handleFocusAstSpan(target.edge.ast_span, `Edge: ${target.edge.edge_id}`);
              }
            }}
            contract={designContract}
            verificationReport={verificationReport}
            diagnosticsReport={evaluated.diagnostics}
            traceId={model?.traceId}
            kclSource={kclSource}
            highlightedSpan={highlightedSpan}
            highlightLabel={highlightLabel}
            onChangeSource={(next) => applySourceUpdate(next, { summary: "编辑 KCL 源码" })}
            onSelectSpan={handleFocusAstSpan}
            displayState={displayState}
            selectedComponentId={selectedComponentId}
            onSelectComponent={setSelectedComponentId}
            onToggleEntityHidden={handleToggleEntityHidden}
            onToggleEntityTranslucent={handleToggleEntityTranslucent}
            onIsolateEntity={handleIsolateEntity}
            onShowAllEntities={handleShowAllEntities}
            onStartSketchOnDatum={(plane) => handleOpenFeatureDashboard("sketch", { datumPlane: plane })}
            onAiRepairNode={(nodeId, reason, failedRefs) => {
              setAiGenMode("edit");
              openRightDock("ai");
              void handleAiGenerate(
                `修复回退节点 ${nodeId}：${reason}`,
                selectedMaterial.id,
                "CNC_3AXIS_MILLING",
                {
                  mode: "edit",
                  llm: { provider: "auto" },
                  selectedNodeIds: [nodeId],
                  selectedTopoRefs: failedRefs,
                },
                `修复节点 ${nodeId}`,
              );
            }}
          />
        </DockPanel>

        <div className="studio-viewport-host">
          {transportError && (
            <div className="transport-error-banner" role="alert">
              <strong>{transportError.code}</strong>
              <span>{transportError.message}</span>
              <TraceIdChip traceId={transportError.traceId} />
            </div>
          )}
          <CenterViewport
            ref={viewportRef}
            viewportTab={viewportTab}
            onSelectViewportTab={setViewportTab}
            measurementMode={measurementMode}
            onExitMeasurement={() => setMeasurementMode(null)}
            showKeepInBox={showKeepInBox && !!model && model.diagnostics.status === "ok" && !model.transportError}
            displayState={displayState}
            selectionFilter={selectionFilter}
            selectionIntent={selectionIntent}
            navigationPreset={navigationPreset}
            onCameraOrientationChange={setCameraOrientation}
            sectionCut={sectionCut}
            explodeFactor={explodeFactor}
            isAssembly={isAssembly}
            kclSource={kclSource}
            sceneMesh={evaluated.sceneMesh}
            compactScene={evaluated.compactScene ?? null}
            changePreview={changePreview}
            keepInBbox={designContract?.keep_in_bbox_mm ?? null}
            metrics={evaluated.diagnostics.metrics}
            selectedComponentId={selectedComponentId}
            onSelectComponent={setSelectedComponentId}
            selectedTarget={selectedTarget}
            onSelectTarget={setSelectedTarget}
            selectedTargets={selectedTargets}
            onChangeSelectedTargets={setSelectedTargets}
            onUpdateParameter={handleUpdateParameter}
            onOffsetHole={handleOffsetHole}
            holeOffsetPending={holeOffsetPending}
            onFocusAstSpan={handleFocusAstSpan}
            onViewportContextMenu={handleViewportContextMenu}
            onConfirmActiveTool={() => commitDashboardDraft(false)}
            onExecuteViewportCommand={(id) => executeCommand(id)}
            dashboardActive={Boolean(activeDashboard)}
            topViewportOverlay={
              <div className="viewport-contextual-stack">
                <ViewportChangePreviewBar
                  busy={previewActionPending}
                  onCommit={handleCommitPreview}
                  onDiscard={handleDiscardPreview}
                />
                <FeatureDashboard
                  draft={activeDashboard}
                  onChange={setActiveDashboard}
                  onApply={() => commitDashboardDraft(true)}
                  onConfirm={() => commitDashboardDraft(false)}
                  onCancel={() => setActiveDashboard(null)}
                  onRemoveReference={(refId) =>
                    setActiveDashboard((prev) =>
                      prev ? { ...prev, references: prev.references.filter((r) => r !== refId) } : null,
                    )
                  }
                  onClearReferences={() =>
                    setActiveDashboard((prev) => (prev ? { ...prev, references: [] } : null))
                  }
                />
                <InlinePointPromptPopover
                  open={inlineAiOpen}
                  selectedTopoRefs={activeSelectionTopoRefs}
                  selectedNodeIds={selectedNodeIds}
                  busy={aiGenerating}
                  onSubmit={(promptText) => {
                    setAiGenMode("edit");
                    void handleAiGenerate(
                      promptText,
                      selectedMaterial.id,
                      "CNC_3AXIS_MILLING",
                      {
                        mode: "edit",
                        llm: { provider: "auto" },
                        selectedTopoRefs: activeSelectionTopoRefs,
                        selectedNodeIds,
                      },
                      activeSelectionTopoRefs.length > 0 ? `选区 ${activeSelectionTopoRefs[0]}` : undefined,
                    );
                  }}
                  onOpenFullCopilot={() => openRightDock("ai")}
                  onClose={() => setInlineAiOpen(false)}
                />
              </div>
            }
            hudOverlay={
              <ViewportHudOverlay
                viewportTab={viewportTab}
                onSelectViewportTab={setViewportTab}
                onApplyPreset={(preset) => {
                  setViewportTab("3d");
                  viewportRef.current?.applyPreset(preset);
                }}
                onFitView={() => {
                  setViewportTab("3d");
                  viewportRef.current?.fitToView();
                }}
                onNormalToSelection={() => {
                  setViewportTab("3d");
                  viewportRef.current?.normalToSelection();
                }}
                onZoomBy={(factor) => {
                  setViewportTab("3d");
                  viewportRef.current?.zoomBy(factor);
                }}
                onRotateStep={(deltaYaw) => {
                  setViewportTab("3d");
                  viewportRef.current?.rotateStep(deltaYaw);
                }}
                onOrbitBy={(deltaYaw, deltaPitch) => {
                  viewportRef.current?.orbitBy(deltaYaw, deltaPitch);
                }}
                cameraOrientation={cameraOrientation}
                displayState={displayState}
                onChangeVisualStyle={handleChangeVisualStyle}
                onToggleProjection={handleToggleProjection}
                onToggleCategory={handleToggleCategory}
                onResetCategories={handleResetCategories}
                onApplyDisplayPreset={handleApplyDisplayPreset}
                savedDisplayStates={savedDisplayStates}
                onSaveDisplayState={handleSaveDisplayState}
                onRestoreDisplayState={handleRestoreDisplayState}
                onDeleteDisplayState={handleDeleteDisplayState}
                availableSemanticTags={availableSemanticTags}
                onSelectSemanticFilter={handleSelectSemanticFilter}
                onExitIsolate={handleExitIsolate}
                onShowAllEntities={handleShowAllEntities}
                onClearAllOverrides={handleClearAllOverrides}
                selectedTarget={selectedTarget}
                measurementMode={measurementMode}
                onExitMeasurement={() => setMeasurementMode(null)}
                sectionCut={sectionCut}
                onChangeSectionCut={setSectionCut}
                isAssembly={isAssembly}
                explodeFactor={explodeFactor}
                onChangeExplodeFactor={setExplodeFactor}
                onExecuteCommand={(id) => executeCommand(id)}
              />
            }
          />

          <ToastStack
            toasts={notifications.toasts}
            sourceRevision={revision}
            onDismiss={(id) => setNotifications((prev) => dismissToast(prev, id))}
            onExecuteCommand={(id, expectedRevision) => {
              if (expectedRevision === undefined || expectedRevision === sourceRevisionRef.current) {
                executeCommand(id);
              }
            }}
          />
        </div>

        <DockPanel
          side="right"
          open={layoutPrefs.rightOpen}
          keepMounted
          width={layoutPrefs.rightWidth}
          tabs={rightDockTabs}
          activeTab={rightTab}
          onSelectTab={setRightTab}
          onResize={(w) => setLayoutPrefs((prev) => ({ ...prev, rightWidth: clampRightWidth(w) }))}
          onClose={() => setLayoutPrefs((prev) => ({ ...prev, rightOpen: false }))}
        >
          <SessionChangePreviewCard
            busy={previewActionPending}
            onCommit={handleCommitPreview}
            onDiscard={handleDiscardPreview}
          />
          <WorkbenchTabPanel prefix="right" id="ai" active={rightTab === "ai"} className="workbench-ai-tabpanel">
            <AiAssistantPanel
              mode={aiGenMode}
              onSelectMode={setAiGenMode}
              externalPrompt={aiExternalPrompt}
              onGenerate={handleAiGenerate}
              onCancel={handleCancelAi}
              onUndoLast={() => {
                if (aiUndoRevision === sourceRevisionRef.current) handleUndo();
              }}
              canUndo={aiUndoRevision === revision && canUndo}
              currentRevision={revision}
              configStatus={aiConfigStatus}
              profiles={aiProfiles}
              activeProfileId={activeAiProfileId}
              storageKind={aiProfileStorage}
              onSaveProfiles={handleSaveAiProfiles}
              onSelectProfile={handleSelectAiProfile}
              onResetProfiles={handleResetAiProfiles}
              onTestConnection={handleTestAiConnection}
              generating={aiGenerating}
              feedback={aiFeedback}
              progressSteps={aiProgressSteps}
              thread={aiThread}
              onClearThread={handleClearAiThread}
              selectedTarget={selectedTarget}
              selectedTopoRefs={activeSelectionTopoRefs}
              selectedNodeIds={selectedNodeIds}
              selectedMaterial={selectedMaterial}
              onSelectMaterial={handleSelectMaterial}
            />
          </WorkbenchTabPanel>
          <RightPanel
            embedded
            activeTab={rightTab === "ai" ? null : rightTab}
            onSelectTab={setRightTab}
            onExport={(kind) => executeCommand(`export.${kind}`)}
            backendKind={backend.kind}
            canExport={canExport}
            compilePending={compilePending}
            showingLastAccepted={showingLastAccepted}
            hostModel={commandContext.isHostBackend}
            busyFormat={exporting}
            exportMessage={currentExport?.message}
            contract={designContract}
            verificationReport={verificationReport}
            diagnosticsReport={evaluated.diagnostics}
            sceneMesh={evaluated.sceneMesh}
            selectedMaterial={selectedMaterial}
            onSelectMaterial={handleSelectMaterial}
            selectedTarget={selectedTarget}
            selectedCount={selectedTargets.length}
            graphIr={graphIr}
            selectedNodeId={selectedNodeId}
            onRenameNode={featureTree?.onRenameNode ?? ((nodeId, name) =>
              dispatchSemanticCommand({ op: "rename_node", nodeId, name }))}
            onSetNodeSuppressed={featureTree?.onSetNodeSuppressed ?? ((nodeId, suppressed) =>
              dispatchSemanticCommand({ op: "set_node_suppressed", nodeId, suppressed }))}
            onUpsertParameter={featureTree?.onUpsertParameter ?? ((value) =>
              dispatchSemanticCommand({ op: "upsert_parameter", value }))}
            onEmitGraphPatch={featureTree?.onEmitGraphPatch ?? ((patch) =>
              applySemanticCommands([{ op: "apply_graph_patch", patch }], patch.description))}
            onFocusSource={() => executeCommand("source.focusSelection")}
            onTagSelected={() => executeCommand("annotate.tagTopology")}
            onJumpToViolation={(target) => {
              dispatchSemanticCommand({
                op: "select",
                nodeId: target.nodeId,
                nodeIds: target.nodeId ? [target.nodeId] : [],
                topoRefs: target.topoRef ? [target.topoRef] : [],
              });
            }}
          />
        </DockPanel>

        {/* Slim Right On-Demand Tool Rail (keeps viewport >= 78% when right drawer is collapsed) */}
        <ActivityBar
          side="right"
          ariaLabel="右侧工具轨"
          items={rightDockTabs}
          activeId={rightTab}
          panelOpen={layoutPrefs.rightOpen}
          onSelect={(id) => {
            if (layoutPrefs.rightOpen && rightTab === id) {
              setLayoutPrefs((prev) => ({ ...prev, rightOpen: false }));
            } else {
              openRightDock(id);
            }
          }}
        />
      </div>

      {/* Zone 7: Bottom Command Line + Navigation Preset + OCCT/Watertight Status Bar */}
      <CommandStatusBar
        onSubmitCommandLine={handleSubmitCommandLine}
        compiling={compiling}
        statusOk={evaluated.diagnostics.status === "ok" && !transportError}
        errorCount={statusErrorCount}
        watertight={evaluated.diagnostics.metrics.watertight}
        openEdgeCount={evaluated.diagnostics.metrics.open_edge_count}
        faceCount={evaluated.diagnostics.metrics.face_count}
        edgeCount={evaluated.diagnostics.metrics.edge_count}
        compileTimeMs={evaluated.diagnostics.metrics.compile_time_ms}
        activeModeLabel={activeModeLabel}
        onExitActiveMode={() => {
          if (activeDashboard) setActiveDashboard(null);
          else if (measurementMode) setMeasurementMode(null);
          else if (sectionCut.enabled) setSectionCut((cut) => ({ ...cut, enabled: false }));
        }}
        unreadNotifications={unreadCount}
        onToggleNotifications={() =>
          setNotifications((prev) => {
            const nextOpen = !prev.drawerOpen;
            return nextOpen ? { ...markAllLogsRead(prev), drawerOpen: true } : { ...prev, drawerOpen: false };
          })
        }
        units={evaluated.sceneMesh.units || "mm"}
        onOpenDiagnostics={() => openLeftDock("checks")}
        kernelAuthoritative={evaluated.diagnostics.metrics.kernel_authoritative}
        kernelBackend={evaluated.diagnostics.metrics.kernel_backend}
        kernelFallbackReason={evaluated.diagnostics.metrics.kernel_fallback_reason}
        navigationPreset={navigationPreset}
        onChangeNavigationPreset={setNavigationPreset}
        onRegenerate={handleRegenerateModel}
      />

      <FileBackstage
        open={fileBackstageOpen}
        onClose={() => setFileBackstageOpen(false)}
        documentName={documentFileName}
        partName={evaluated.sceneMesh.part_name}
        activeBranch={activeBranch}
        activeProjectPath={activeProjectPath}
        commandMap={commandMap}
        context={commandContext}
        onExecute={(id) => executeCommand(id)}
        selectedMaterial={selectedMaterial}
        onSelectMaterial={handleSelectMaterial}
        navigationPreset={navigationPreset}
        onChangeNavigationPreset={setNavigationPreset}
        units={evaluated.sceneMesh.units || "mm"}
        updater={desktopUpdater}
      />

      <NotificationDrawer
        open={notifications.drawerOpen}
        logs={notifications.logs}
        onClose={() => setNotifications((prev) => ({ ...prev, drawerOpen: false }))}
        onClear={() => setNotifications((prev) => clearNotificationLogs(prev))}
      />

      <CommandPalette
        open={paletteOpen}
        commands={commands}
        context={commandContext}
        graphIr={graphIr}
        onSelectNode={(nodeId) =>
          dispatchSemanticCommand({ op: "select", nodeId, nodeIds: [nodeId], topoRefs: [] })
        }
        onClose={() => setPaletteOpen(false)}
        onExecute={(id) => executeCommand(id)}
      />

      <ContextMenu
        position={contextMenuPos}
        title={
          selectedTarget?.kind === "face"
            ? `实体面 · ${selectedTarget.face.face_id}`
            : selectedTarget?.kind === "edge"
              ? `拓扑边 · ${selectedTarget.edge.edge_id}`
              : "3D 视口（长按右键划动触发 8 向手势）"
        }
        items={contextMenuItems}
        onClose={() => setContextMenuPos(null)}
      />

      <ConfirmDialog
        open={resetConfirmOpen}
        title="重置演示模型"
        description="将当前 KCL 源码恢复为默认法兰轴承座演示模型。此操作支持通过 Ctrl+Z 撤销。"
        confirmLabel="恢复演示模型"
        onConfirm={() => {
          setResetConfirmOpen(false);
          setDocumentFileName("model.kcl");
          const undoRevision = applySourceUpdate(INITIAL_DEMO_KCL, { summary: "重置演示模型" });
          setSelectedMaterial(MATERIAL_PRESETS[0]);
          setSectionCut({ enabled: false, axis: "Z", ratio: 0.55 });
          setExplodeFactor(0);
          setActiveDashboard(null);
          notify({
            level: "info",
            title: "已恢复默认演示模型",
            source: "文件",
            action: sourceUndoAction(undoRevision),
          });
        }}
        onCancel={() => setResetConfirmOpen(false)}
      />

      <ConfirmDialog
        open={pendingPreviewExport === "step"}
        title="导出非权威预览 STEP"
        description="当前内核是解析预览，不是权威 OCCT B-Rep。导出的 STEP 仅供预览，不能当作精确可交付几何。确定仍要导出吗？"
        confirmLabel="仍导出预览 STEP"
        cancelLabel="取消"
        onConfirm={() => confirmPreviewExport()}
        onCancel={() => cancelPreviewExport()}
      />

      <ShortcutsModal
        open={shortcutsOpen}
        commands={commands}
        onClose={() => setShortcutsOpen(false)}
      />
    </div>
  );
}
