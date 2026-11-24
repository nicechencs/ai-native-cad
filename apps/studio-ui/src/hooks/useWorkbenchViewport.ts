import { useCallback, useEffect, useRef, useState } from "react";
import type { ViewportCommands, ViewportTab } from "../components/CenterViewport.tsx";
import type { WorkbenchUiPrefs } from "../core/layout/workbenchPrefsStore.ts";
import {
  applyDisplayPreset,
  captureDisplayState,
  clearAllOverrides,
  createDefaultDisplayState,
  exitIsolate,
  isolateEntities,
  loadPersistedDisplayPrefs,
  resetCategoryVisibility,
  restoreDisplayState,
  savePersistedDisplayPrefs,
  setSemanticFilter,
  setVisualStyle,
  showAllEntities,
  toggleCategoryVisibility,
  toggleEntityHidden,
  toggleEntityTranslucent,
  toggleProjectionMode,
  type DisplayCategoryKey,
  type DisplayStatePreset,
  type SavedDisplayState,
  type SelectionFilterMode,
  type ViewportDisplayState,
  type VisualStyle,
} from "../core/viewport/displayState.ts";
import type { NavigationPresetId, SelectionIntentRule } from "../core/viewport/navigationPreset.ts";
import { MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import type { MaterialPreset, SectionCutState, SelectionTarget, SourceSpan } from "../types/scene.ts";
import type { MeasurementMode } from "../utils/viewportMeasurement.ts";
import type { WorkbenchNotify } from "./workbenchSource.ts";

export function useWorkbenchViewport(initialUiPrefs: WorkbenchUiPrefs, notify: WorkbenchNotify) {
  const [viewportTab, setViewportTab] = useState<ViewportTab>(initialUiPrefs.viewportTab);
  const [measurementMode, setMeasurementMode] = useState<MeasurementMode | null>(null);
  const [showKeepInBox, setShowKeepInBox] = useState(initialUiPrefs.showKeepInBox);
  const [initialDisplayPrefs] = useState(() => loadPersistedDisplayPrefs(initialUiPrefs.showKeepInBox));
  const [displayState, setDisplayState] = useState<ViewportDisplayState>(() => ({
    ...createDefaultDisplayState(initialDisplayPrefs.categories.keepInBox),
    visualStyle: initialDisplayPrefs.visualStyle,
    projection: initialDisplayPrefs.projection,
    categories: initialDisplayPrefs.categories,
  }));
  const [savedDisplayStates, setSavedDisplayStates] = useState<readonly SavedDisplayState[]>(
    () => initialDisplayPrefs.savedStates,
  );
  useEffect(() => {
    savePersistedDisplayPrefs({
      visualStyle: displayState.visualStyle,
      projection: displayState.projection,
      categories: displayState.categories,
      savedStates: savedDisplayStates,
    });
  }, [displayState.visualStyle, displayState.projection, displayState.categories, savedDisplayStates]);

  const [cameraOrientation, setCameraOrientation] = useState<{ readonly yaw: number; readonly pitch: number }>({
    yaw: 0.65,
    pitch: 0.58,
  });
  const [sectionCut, setSectionCut] = useState<SectionCutState>(initialUiPrefs.sectionCut);
  const [explodeFactor, setExplodeFactor] = useState(0);
  const [quickParamInput, setQuickParamInput] = useState(initialUiPrefs.quickParamInput);
  const [selectedMaterial, setSelectedMaterial] = useState<MaterialPreset>(
    () => MATERIAL_PRESETS.find((item) => item.id === initialUiPrefs.materialId) ?? MATERIAL_PRESETS[0],
  );
  const [selectedComponentId, setSelectedComponentId] = useState<string | null>(null);
  const [selectionFilter, setSelectionFilter] = useState<SelectionFilterMode>("all");
  const [selectionIntent, setSelectionIntent] = useState<SelectionIntentRule>(initialUiPrefs.selectionIntent);
  const [navigationPreset, setNavigationPreset] = useState<NavigationPresetId>(initialUiPrefs.navigationPreset);
  const [highlightedSpan, setHighlightedSpan] = useState<SourceSpan | null>(null);
  const [highlightLabel, setHighlightLabel] = useState<string | null>(null);
  const [selectedTarget, setSelectedTargetState] = useState<SelectionTarget>(null);
  const [selectedTargets, setSelectedTargetsState] = useState<readonly NonNullable<SelectionTarget>[]>([]);
  const selectedTargetRef = useRef<SelectionTarget>(null);
  const viewportRef = useRef<ViewportCommands>(null);

  const setSelectedTarget = useCallback((target: SelectionTarget) => {
    selectedTargetRef.current = target;
    setSelectedTargetState(target);
    setSelectedTargetsState(target ? [target] : []);
  }, []);

  const setSelectedTargets = useCallback((targets: readonly NonNullable<SelectionTarget>[]) => {
    setSelectedTargetsState(targets);
    const primary = targets[targets.length - 1] ?? null;
    selectedTargetRef.current = primary;
    setSelectedTargetState(primary);
  }, []);

  const clearDocumentSelection = useCallback(() => {
    setSelectedTarget(null);
    setSelectedComponentId(null);
    setHighlightedSpan(null);
    setHighlightLabel(null);
  }, [setSelectedTarget]);

  const handleFocusAstSpan = useCallback((span: SourceSpan, label: string) => {
    setHighlightedSpan(span);
    setHighlightLabel(label);
  }, []);

  const handleChangeVisualStyle = useCallback((style: VisualStyle) => {
    setViewportTab("3d");
    setDisplayState((prev) => setVisualStyle(prev, style));
  }, []);

  const handleCycleVisualStyle = useCallback(() => {
    setViewportTab("3d");
    setDisplayState((prev) =>
      setVisualStyle(prev, prev.visualStyle === "translucent" ? "shaded-edges" : "translucent"),
    );
  }, []);

  const handleToggleProjection = useCallback(() => {
    setViewportTab("3d");
    setDisplayState((prev) => toggleProjectionMode(prev));
  }, []);

  const handleToggleCategory = useCallback((key: DisplayCategoryKey) => {
    setViewportTab("3d");
    setDisplayState((prev) => {
      const next = toggleCategoryVisibility(prev, key);
      if (key === "keepInBox") setShowKeepInBox(next.categories.keepInBox);
      return next;
    });
  }, []);

  const handleResetCategories = useCallback(() => {
    setDisplayState((prev) => resetCategoryVisibility(prev, true));
    setShowKeepInBox(true);
  }, []);

  const handleApplyDisplayPreset = useCallback((preset: DisplayStatePreset) => {
    setViewportTab("3d");
    setDisplayState((prev) => {
      const next = applyDisplayPreset(prev, preset);
      if (preset.categories.keepInBox !== undefined) {
        setShowKeepInBox(preset.categories.keepInBox);
      }
      return next;
    });
    notify({ level: "info", title: `已应用显示方案：${preset.name}`, source: "视图", showToast: false });
  }, [notify]);

  const handleToggleEntityHidden = useCallback((scope: "component" | "face", id: string) => {
    setDisplayState((prev) => toggleEntityHidden(prev, scope, id));
  }, []);

  const handleToggleEntityTranslucent = useCallback((scope: "component" | "face", id: string) => {
    setDisplayState((prev) => toggleEntityTranslucent(prev, scope, id));
  }, []);

  const handleIsolateEntity = useCallback((id: string) => {
    setDisplayState((prev) => isolateEntities(prev, [id]));
  }, []);

  const handleExitIsolate = useCallback(() => {
    setDisplayState((prev) => exitIsolate(prev));
  }, []);

  const handleShowAllEntities = useCallback(() => {
    setDisplayState((prev) => showAllEntities(prev));
  }, []);

  const handleClearAllOverrides = useCallback(() => {
    setDisplayState((prev) => clearAllOverrides(prev));
  }, []);

  const handleSaveDisplayState = useCallback((name: string) => {
    const captured = captureDisplayState(displayState, name);
    setSavedDisplayStates((prev) => [captured, ...prev].slice(0, 12));
    notify({ level: "success", title: `已保存显示状态：${captured.name}`, source: "视图", showToast: false });
  }, [displayState, notify]);

  const handleRestoreDisplayState = useCallback((saved: SavedDisplayState) => {
    setViewportTab("3d");
    setDisplayState(restoreDisplayState(saved));
    setShowKeepInBox(saved.categories.keepInBox);
    notify({ level: "info", title: `已切换显示状态：${saved.name}`, source: "视图", showToast: false });
  }, [notify]);

  const handleDeleteDisplayState = useCallback((id: string) => {
    setSavedDisplayStates((prev) => prev.filter((item) => item.id !== id));
  }, []);

  const handleSelectSemanticFilter = useCallback((tag: string | null) => {
    setViewportTab("3d");
    setDisplayState((prev) => setSemanticFilter(prev, tag));
  }, []);

  return {
    viewportRef,
    viewportTab,
    setViewportTab,
    measurementMode,
    setMeasurementMode,
    showKeepInBox,
    setShowKeepInBox,
    displayState,
    setDisplayState,
    savedDisplayStates,
    cameraOrientation,
    setCameraOrientation,
    sectionCut,
    setSectionCut,
    explodeFactor,
    setExplodeFactor,
    quickParamInput,
    setQuickParamInput,
    selectedMaterial,
    setSelectedMaterial,
    selectedComponentId,
    setSelectedComponentId,
    selectionFilter,
    setSelectionFilter,
    selectionIntent,
    setSelectionIntent,
    navigationPreset,
    setNavigationPreset,
    highlightedSpan,
    highlightLabel,
    selectedTarget,
    selectedTargets,
    selectedTargetRef,
    setSelectedTarget,
    setSelectedTargets,
    clearDocumentSelection,
    handleFocusAstSpan,
    handleChangeVisualStyle,
    handleCycleVisualStyle,
    handleToggleProjection,
    handleToggleCategory,
    handleResetCategories,
    handleApplyDisplayPreset,
    handleToggleEntityHidden,
    handleToggleEntityTranslucent,
    handleIsolateEntity,
    handleExitIsolate,
    handleShowAllEntities,
    handleClearAllOverrides,
    handleSaveDisplayState,
    handleRestoreDisplayState,
    handleDeleteDisplayState,
    handleSelectSemanticFilter,
  };
}
