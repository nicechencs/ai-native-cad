import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import {
  compileErrorReport,
  compileWorkbenchPreview,
  EMPTY_SCENE,
  exportWorkbenchModel,
  StepExportDiscardError,
  ThreeMfExportDiscardError,
  transportErrorFrom,
  transportFailureModel,
  type ExportKind,
  type WorkbenchBackend,
  type WorkbenchModel,
} from "../sync/workbenchBackend.ts";
import {
  dispatchSessionCommand,
  holeRadiusCommand,
  SessionCommandError,
  type ChangePreviewInput,
} from "../core/session/engineeringSession.ts";
import type { AiRequestOptions, HoleOffsetHandle } from "../types/scene.ts";
import { applyFaceQuickFeatureToKcl, type QuickFeatureKind } from "../utils/studioHelpers.ts";
import { createTraceId, traceIdFrom } from "../utils/trace.ts";
import { useEngineeringSession } from "./useEngineeringSession.ts";
import { useWorkbenchAi } from "./useWorkbenchAi.ts";
import type { useWorkbenchViewport } from "./useWorkbenchViewport.ts";
import { selectDisplayedSolid, type SolidSelection } from "./displayedSolid.ts";
import { quickFeatureNotice } from "./quickFeatureOutcome.ts";
import { sourceUndoAction, WORKBENCH_FILE, type ApplySourceUpdate, type WorkbenchNotify } from "./workbenchSource.ts";

function triggerDownload(filename: string, content: string | Uint8Array<ArrayBuffer>, mimeType: string): void {
  if (typeof document === "undefined" || typeof URL === "undefined") return;
  const blob = new Blob([content], { type: mimeType });
  const link = document.createElement("a");
  const url = URL.createObjectURL(blob);
  link.href = url;
  link.download = filename;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

type DocumentViewport = Pick<ReturnType<typeof useWorkbenchViewport>,
  "clearDocumentSelection" | "selectedTarget" | "selectedTargetRef" | "quickParamInput" | "setSelectedMaterial"
>;

export function useWorkbenchDocument(
  backend: WorkbenchBackend,
  notify: WorkbenchNotify,
  { clearDocumentSelection, selectedTarget, selectedTargetRef, quickParamInput, setSelectedMaterial }: DocumentViewport,
  onSourceLoaded: (tab: "source") => void,
  activeProjectPath: string | null = null,
) {
  const session = useEngineeringSession();
  const {
    kclSource,
    revision,
    designContract,
    verificationReport,
    canUndo,
    canRedo,
    dispatchSemanticCommand,
    undo,
    redo,
    getStore,
  } = session;
  const sourceRevisionRef = useRef(revision);
  sourceRevisionRef.current = getStore().state.revision;
  const [preview, setPreview] = useState<{
    revision: number;
    backend: WorkbenchBackend;
    model: WorkbenchModel;
  }>();

  const sourceFileInput = useRef<HTMLInputElement>(null);
  const sourceFileRead = useRef<AbortController | null>(null);
  const exportRequest = useRef<AbortController | null>(null);
  const [exportState, setExportState] = useState<{
    revision: number;
    backend: WorkbenchBackend;
    message: string;
  }>();
  const [exporting, setExporting] = useState<ExportKind>();
  const currentExport = exportState?.revision === revision && exportState.backend === backend ? exportState : undefined;
  const previewRequest = useRef<AbortController | null>(null);
  const holeOffsetRequest = useRef<AbortController | null>(null);
  const [compiled, setCompiled] = useState<{
    revision: number;
    backend: WorkbenchBackend;
    model?: WorkbenchModel;
    error?: string;
  } | null>(null);
  const [holeOffsetPending, setHoleOffsetPending] = useState(false);
  const activePreview = preview?.revision === revision && preview.backend === backend ? preview.model : undefined;
  const pendingQuickFeature = useRef<{ revision: number; summary: string } | null>(null);
  const lastSolidRef = useRef<WorkbenchModel | undefined>(undefined);
  const displayCache = useRef<SolidSelection | undefined>(undefined);
  const currentCompile = compiled?.revision === revision && compiled.backend === backend ? compiled : null;
  const liveModel = activePreview ?? currentCompile?.model;
  const selection = selectDisplayedSolid(liveModel, lastSolidRef.current);
  const cached = displayCache.current;
  if (
    !cached
    || cached.live !== selection.live
    || cached.retained !== selection.retained
    || cached.pending !== selection.pending
    || cached.showingLastAccepted !== selection.showingLastAccepted
  ) {
    displayCache.current = selection;
  }
  const displayed = displayCache.current ?? selection;
  lastSolidRef.current = displayed.retained;
  const model = displayed.model;
  const compilePending = displayed.pending;
  const showingLastAccepted = displayed.showingLastAccepted;
  const currentMetrics = model?.diagnostics.metrics;

  const pendingPreviewMetaRef = useRef<{
    readonly previewBranch: string;
    readonly previewModel?: WorkbenchModel;
    readonly materialId?: string;
    readonly runId?: string;
  } | null>(null);
  const [previewActionPending, setPreviewActionPending] = useState(false);

  const handleOpenAiPreview = useCallback(
    (
      nextPreview: ChangePreviewInput,
      meta?: {
        readonly previewModel?: WorkbenchModel;
        readonly materialId?: string;
        readonly runId?: string;
      },
    ) => {
      pendingPreviewMetaRef.current = {
        previewBranch: nextPreview.previewBranch,
        previewModel: meta?.previewModel,
        materialId: meta?.materialId,
        runId: meta?.runId,
      };
      dispatchSemanticCommand({ op: "open_preview", preview: nextPreview }, `预览 ${nextPreview.previewBranch}`);
      sourceRevisionRef.current = getStore().state.revision;
    },
    [dispatchSemanticCommand, getStore],
  );

  const {
    abortAiRequest,
    cancelAiOnSourceUpdate,
    setAiGenerating,
    generateAi,
    handleCancelAi,
    attachCommittedPreviewRevision,
    aiConfigStatus,
    aiProfiles,
    activeAiProfileId,
    aiProfileStorage,
    handleSaveAiProfiles,
    handleSelectAiProfile,
    handleResetAiProfiles,
    handleTestAiConnection,
    aiGenerating,
    aiFeedback,
    aiUndoRevision,
    aiProgressSteps,
    aiThread,
    handleClearAiThread,
  } = useWorkbenchAi({
    backend,
    kclSource,
    activeProjectPath,
    currentMetrics,
    revision,
    sourceRevisionRef,
    setSelectedMaterial,
    onOpenPreview: handleOpenAiPreview,
    notify,
  });

  const cancelInFlightDocumentOps = useCallback(() => {
    sourceFileRead.current?.abort();
    sourceFileRead.current = null;
    exportRequest.current?.abort();
    previewRequest.current?.abort();
    holeOffsetRequest.current?.abort();
    holeOffsetRequest.current = null;
    setHoleOffsetPending(false);
    cancelAiOnSourceUpdate();
    clearDocumentSelection();
  }, [cancelAiOnSourceUpdate, clearDocumentSelection]);

  const applySourceUpdate = useCallback<ApplySourceUpdate>(
    (nextSource, options) => {
      const current = getStore().state;
      const isReset = options?.summary === "重置演示模型";
      if (!isReset && nextSource === current.kclSource) return undefined;

      const recordHistory = options?.recordHistory !== false;
      const summary = options?.summary ?? "编辑 KCL 源码";
      const committed = isReset
        ? dispatchSemanticCommand({ op: "reset_document" }, summary)
        : options?.replaceDocument
          ? dispatchSemanticCommand({ op: "load_kcl_document", source: nextSource }, summary)
          : dispatchSemanticCommand(
            { op: "sync_kcl_view", source: nextSource, recordHistory },
            summary,
          );
      if (committed === undefined) return undefined;

      pendingPreviewMetaRef.current = null;
      cancelInFlightDocumentOps();
      sourceRevisionRef.current = getStore().state.revision;
      if (options?.previewModel) {
        setPreview({ revision: committed, backend, model: options.previewModel });
      } else {
        setPreview(undefined);
      }
      return recordHistory ? committed : undefined;
    },
    [backend, cancelInFlightDocumentOps, dispatchSemanticCommand, getStore],
  );

  useEffect(() => {
    setHoleOffsetPending(false);
    setAiGenerating(false);
    return () => {
      sourceFileRead.current?.abort();
      sourceFileRead.current = null;
      exportRequest.current?.abort();
      holeOffsetRequest.current?.abort();
      holeOffsetRequest.current = null;
      abortAiRequest();
    };
  }, [backend, abortAiRequest, setAiGenerating]);

  // Debounced compile. A preview belongs only to the revision that produced it.
  const suppliedPreview = activePreview;
  useEffect(() => {
    setCompiled(null);
    if (suppliedPreview) return;
    const abort = new AbortController();
    previewRequest.current = abort;
    const isCurrent = () => !abort.signal.aborted && sourceRevisionRef.current === revision;
    const timer = window.setTimeout(async () => {
      try {
        const model = await compileWorkbenchPreview(
          backend,
          { fileName: WORKBENCH_FILE, source: kclSource },
          abort.signal,
        );
        if (isCurrent()) setCompiled({ revision, backend, model });
      } catch (error) {
        if (isCurrent()) {
          const info = transportErrorFrom(error, traceIdFrom(error) ?? createTraceId(), "compile");
          setCompiled({ revision, backend, model: transportFailureModel(WORKBENCH_FILE, info) });
        }
      }
    }, 240);
    return () => {
      window.clearTimeout(timer);
      abort.abort();
      if (previewRequest.current === abort) previewRequest.current = null;
    };
  }, [backend, kclSource, revision, suppliedPreview]);

  const compiling = compilePending || (!liveModel && !currentCompile);
  const evaluated = useMemo(
    () =>
      model ?? {
        sceneMesh: EMPTY_SCENE,
        diagnostics: compileErrorReport(WORKBENCH_FILE),
        origin: "demo" as const,
      },
    [model],
  );
  const transportError = model?.transportError;
  const reportedTraceIds = useRef(new Set<string>());
  useEffect(() => {
    const compileFailed = !transportError && model?.diagnostics.status === "error";
    const traceId = transportError?.traceId ?? (compileFailed ? model?.traceId : undefined);
    if (!traceId || reportedTraceIds.current.has(traceId)) return;
    reportedTraceIds.current.add(traceId);
    if (transportError) {
      notify({
        level: "error",
        title: "工作台传输失败",
        detail: transportError.message,
        source: "编译",
        traceId: transportError.traceId,
      });
      return;
    }
    notify({
      level: "error",
      title: "编译未通过",
      detail: model?.diagnostics.diagnostics.find((item) => item.severity === "error")?.message,
      source: "编译",
      traceId,
      persistent: false,
    });
  }, [model, notify, transportError]);

  const isAssembly = useMemo(() => {
    const { kind, components, faces } = evaluated.sceneMesh;
    if (kind === "assembly" || (components?.length ?? 0) > 1) return true;
    const firstId = components?.[0]?.component_id ?? faces.find((face) => face.component_id)?.component_id;
    return firstId !== undefined && faces.some((face) => face.component_id && face.component_id !== firstId);
  }, [evaluated.sceneMesh]);

  const canExport =
    liveModel !== undefined && !liveModel.transportError && liveModel.diagnostics.status === "ok" &&
    (liveModel.sceneMesh.faces.length > 0 || liveModel.sceneMesh.edges.length > 0);
  useEffect(() => {
    const pending = pendingQuickFeature.current;
    if (!pending) return;
    if (revision !== pending.revision) {
      pendingQuickFeature.current = null;
      return;
    }
    if (!liveModel || currentCompile?.revision !== pending.revision) return;
    pendingQuickFeature.current = null;
    const notice = quickFeatureNotice("compiled", pending.summary, liveModel.diagnostics.status);
    if (!notice || liveModel.transportError) return;
    notify({
      level: notice.level,
      title: notice.title,
      source: notice.source,
      action: sourceUndoAction(pending.revision),
    });
  }, [currentCompile, liveModel, notify, revision]);
  const errorCount = evaluated.diagnostics.diagnostics.filter((d) => d.severity === "error").length;
  const statusErrorCount = errorCount + (transportError ? 1 : 0);

  const handleCommitPreview = useCallback(async () => {
    const currentPreview = getStore().state.preview;
    if (!currentPreview || previewActionPending) return;
    setPreviewActionPending(true);
    try {
      const targetProject = currentPreview.projectPath ?? activeProjectPath;
      let remoteKcl: string | undefined;
      if (
        targetProject &&
        backend.commitPreviewBranch &&
        currentPreview.previewBranch &&
        currentPreview.previewBranch !== "main"
      ) {
        const result = await backend.commitPreviewBranch({
          projectPath: targetProject,
          branchName: currentPreview.previewBranch,
        });
        remoteKcl = result.kcl;
      }

      const meta =
        pendingPreviewMetaRef.current?.previewBranch === currentPreview.previewBranch
          ? pendingPreviewMetaRef.current
          : null;
      pendingPreviewMetaRef.current = null;

      const committedRevision = dispatchSemanticCommand(
        { op: "commit_preview" },
        `确认合入预览 ${currentPreview.previewBranch}`,
      );
      if (remoteKcl && remoteKcl.trim() && remoteKcl !== getStore().state.kclSource) {
        dispatchSemanticCommand(
          { op: "sync_kcl_view", source: remoteKcl, recordHistory: false },
          `同步合入后 KCL ${currentPreview.previewBranch}`,
        );
      }
      const nextRevision = getStore().state.revision;
      sourceRevisionRef.current = nextRevision;
      cancelInFlightDocumentOps();
      if (meta?.previewModel && committedRevision !== undefined) {
        setPreview({ revision: nextRevision, backend, model: meta.previewModel });
      } else {
        setPreview(undefined);
      }
      if (meta?.materialId) {
        const preset = MATERIAL_PRESETS.find((item) => item.id === meta.materialId);
        if (preset) setSelectedMaterial(preset);
      }
      attachCommittedPreviewRevision(meta?.runId, committedRevision);
      notify({
        level: "success",
        title: `已确认合入预览 ${currentPreview.previewBranch}`,
        detail: currentPreview.planSummary,
        source: "AI",
        action: sourceUndoAction(committedRevision),
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const traceId = traceIdFrom(err);
      notify({
        level: "error",
        title: "确认合入预览失败",
        detail: message,
        source: "AI",
        ...(traceId ? { traceId } : {}),
      });
    } finally {
      setPreviewActionPending(false);
    }
  }, [
    activeProjectPath,
    attachCommittedPreviewRevision,
    backend,
    cancelInFlightDocumentOps,
    dispatchSemanticCommand,
    getStore,
    notify,
    previewActionPending,
    setSelectedMaterial,
  ]);

  const handleDiscardPreview = useCallback(async () => {
    const currentPreview = getStore().state.preview;
    if (!currentPreview || previewActionPending) return;
    setPreviewActionPending(true);
    try {
      const targetProject = currentPreview.projectPath ?? activeProjectPath;
      if (
        targetProject &&
        backend.discardPreviewBranch &&
        currentPreview.previewBranch &&
        currentPreview.previewBranch !== "main"
      ) {
        await backend.discardPreviewBranch({
          projectPath: targetProject,
          branchName: currentPreview.previewBranch,
        });
      }
      pendingPreviewMetaRef.current = null;
      dispatchSemanticCommand({ op: "discard_preview" }, `放弃预览 ${currentPreview.previewBranch}`);
      sourceRevisionRef.current = getStore().state.revision;
      notify({
        level: "info",
        title: `已放弃预览 ${currentPreview.previewBranch}`,
        detail: "已保留原分支与当前工程模型。",
        source: "AI",
      });
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      const traceId = traceIdFrom(err);
      notify({
        level: "error",
        title: "放弃预览失败",
        detail: message,
        source: "AI",
        ...(traceId ? { traceId } : {}),
      });
    } finally {
      setPreviewActionPending(false);
    }
  }, [activeProjectPath, backend, dispatchSemanticCommand, getStore, notify, previewActionPending]);

  const handleUndo = useCallback(() => {
    const entry = undo();
    if (!entry) return;
    pendingPreviewMetaRef.current = null;
    cancelInFlightDocumentOps();
    sourceRevisionRef.current = getStore().state.revision;
    setPreview(undefined);
    notify({ level: "info", title: `已撤销：${entry.summary}`, source: "编辑" });
  }, [cancelInFlightDocumentOps, getStore, notify, undo]);

  const handleRedo = useCallback(() => {
    const entry = redo();
    if (!entry) return;
    pendingPreviewMetaRef.current = null;
    cancelInFlightDocumentOps();
    sourceRevisionRef.current = getStore().state.revision;
    setPreview(undefined);
    notify({ level: "info", title: `已重做：${entry.summary}`, source: "编辑" });
  }, [cancelInFlightDocumentOps, getStore, notify, redo]);

  const handleCommitKclSource = useCallback(
    (nextSource: string, summary: string) => {
      if (sourceRevisionRef.current !== revision) return;
      const undoRevision = applySourceUpdate(nextSource, { summary });
      notify({
        level: "success",
        title: summary,
        source: "双向写回",
        action: sourceUndoAction(undoRevision),
      });
    },
    [revision, applySourceUpdate, notify],
  );

  const handleOffsetHole = useCallback(
    async (scene: typeof evaluated.sceneMesh, handle: HoleOffsetHandle, deltaMm: number) => {
      if (holeOffsetRequest.current) return;
      const selection = selectedTarget;
      const isCurrent = () =>
        sourceRevisionRef.current === revision && selectedTargetRef.current === selection;
      const staleMessage = "孔壁拖拽已失效，模型或选择已变化，请重新选择孔壁。";
      if (
        !isCurrent() ||
        scene !== evaluated.sceneMesh ||
        !scene.hole_offsets?.includes(handle) ||
        selection?.kind !== "face" ||
        selection.face.face_id !== handle.face_id ||
        !scene.faces.includes(selection.face)
      ) {
        notify({ level: "warning", title: staleMessage, source: "孔壁直编" });
        return;
      }

      let command;
      try {
        command = holeRadiusCommand(handle.face_id, handle.radius_mm, deltaMm);
      } catch (err) {
        const message = err instanceof SessionCommandError ? err.message : err instanceof Error ? err.message : String(err);
        notify({ level: "error", title: "孔半径修改失败，源码保持原样", detail: message, source: "孔壁直编" });
        return;
      }
      if (!command) return;

      const abort = new AbortController();
      holeOffsetRequest.current = abort;
      setHoleOffsetPending(true);
      try {
        const candidate = dispatchSessionCommand(getStore(), command).state.kclSource;
        const compiledModel = await backend.compile(
          { fileName: WORKBENCH_FILE, source: candidate },
          abort.signal,
        );
        if (abort.signal.aborted) return;
        if (!isCurrent()) {
          notify({ level: "warning", title: staleMessage, source: "孔壁直编" });
          return;
        }
        if (compiledModel.diagnostics.status !== "ok" || !compiledModel.sceneMesh.faces.length) {
          throw new Error(
            compiledModel.diagnostics.diagnostics.find((item) => item.severity === "error")
              ?.message ?? "编译未返回有效模型。",
          );
        }
        holeOffsetRequest.current = null;
        const radius = Number((handle.radius_mm + deltaMm).toPrecision(6));
        const summary = `孔半径调整为 ${radius} mm`;
        const undoRevision = dispatchSemanticCommand(command, summary);
        if (undoRevision !== undefined) {
          sourceRevisionRef.current = getStore().state.revision;
          setPreview({ revision: undoRevision, backend, model: compiledModel });
        }
        notify({
          level: "success",
          title: `孔半径已更新为 ${radius} mm`,
          source: "孔壁直编",
          action: sourceUndoAction(undoRevision),
        });
      } catch (err) {
        if (!abort.signal.aborted) {
          const message = err instanceof Error ? err.message : String(err);
          const traceId = traceIdFrom(err);
          notify({
            level: "error",
            title: "孔半径修改失败，源码保持原样",
            detail: isCurrent() ? message : staleMessage,
            source: "孔壁直编",
            ...(traceId ? { traceId } : {}),
          });
        }
      } finally {
        setHoleOffsetPending(false);
        if (holeOffsetRequest.current === abort) holeOffsetRequest.current = null;
      }
    },
    [backend, dispatchSemanticCommand, evaluated.sceneMesh, getStore, notify, revision, selectedTarget, selectedTargetRef],
  );

  const handleApplyQuickFeature = useCallback(
    (featureKind: QuickFeatureKind, explicitSizeMm?: number) => {
      const scene = evaluated.sceneMesh;
      const targetId =
        selectedTarget?.kind === "face"
          ? selectedTarget.face.face_id
          : selectedTarget?.kind === "edge"
            ? selectedTarget.edge.edge_id
            : scene.faces[0]?.face_id ?? scene.part_name;
      const parsedSize = explicitSizeMm ?? Number.parseFloat(quickParamInput);
      const { nextSource, summary } = applyFaceQuickFeatureToKcl(
        kclSource,
        targetId,
        featureKind,
        Number.isFinite(parsedSize) && parsedSize > 0 ? parsedSize : undefined,
      );
      const undoRevision = applySourceUpdate(nextSource, { summary });
      if (undoRevision !== undefined) {
        pendingQuickFeature.current = { revision: undoRevision, summary };
      }
    },
    [evaluated.sceneMesh, kclSource, notify, quickParamInput, selectedTarget, applySourceUpdate],
  );

  const handleExportCommand = useCallback(
    async (kind: ExportKind) => {
      if (!canExport || !model) {
        notify({ level: "warning", title: "当前模型不可导出", detail: "请先确保 KCL 编译通过且包含有效几何。", source: "导出" });
        return;
      }
      if (exportRequest.current || sourceRevisionRef.current !== revision) return;
      const abort = new AbortController();
      exportRequest.current = abort;
      setExporting(kind);
      try {
        const output = await exportWorkbenchModel(
          backend,
          kind,
          { fileName: WORKBENCH_FILE, source: kclSource },
          model,
          abort.signal,
        );
        if (abort.signal.aborted) return;
        if (output.download) {
          triggerDownload(output.download.filename, output.download.content, output.download.mimeType);
        }
        setExportState({ revision, backend, message: output.message });
        notify({ level: "success", title: output.message, source: "导出" });
      } catch (err) {
        if (abort.signal.aborted && !(err instanceof StepExportDiscardError || err instanceof ThreeMfExportDiscardError)) return;
        const message = err instanceof Error ? err.message : String(err);
        const traceId = traceIdFrom(err) ?? model.traceId;
        setExportState({ revision, backend, message });
        notify({
          level: "error",
          title: `导出 ${kind.toUpperCase()} 失败`,
          detail: message,
          source: "导出",
          ...(traceId ? { traceId } : {}),
        });
      } finally {
        exportRequest.current = null;
        setExporting(undefined);
      }
    },
    [backend, canExport, kclSource, model, notify, revision],
  );

  const handleAiGenerate = useCallback(
    (
      prompt: string,
      material: string,
      manufacturingMethod: string,
      options?: AiRequestOptions,
      contextLabel?: string,
    ) => generateAi(applySourceUpdate, prompt, material, manufacturingMethod, options, contextLabel),
    [generateAi, applySourceUpdate],
  );

  const handleLoadKclFile = useCallback(async (file: File) => {
    sourceFileRead.current?.abort();
    const read = new AbortController();
    sourceFileRead.current = read;
    try {
      if (!/\.kcl$/i.test(file.name)) throw new Error("请选择 .kcl 源码文件。");
      const source = new TextDecoder("utf-8", { fatal: true }).decode(await file.arrayBuffer());
      if (read.signal.aborted) return;
      const summary = `载入 ${file.name}`;
      applySourceUpdate(source, { summary, replaceDocument: true });
      onSourceLoaded("source");
      notify({ level: "success", title: summary, source: "文件" });
    } catch (error) {
      if (read.signal.aborted) return;
      const detail = error instanceof TypeError ? "请使用 UTF-8 编码的 KCL 文件。" : error instanceof Error ? error.message : String(error);
      notify({ level: "error", title: "载入 KCL 源码失败", detail, source: "文件" });
    } finally {
      if (sourceFileRead.current === read) sourceFileRead.current = null;
    }
  }, [applySourceUpdate, notify, onSourceLoaded]);

  const handleDownloadSource = useCallback(() => {
    triggerDownload(WORKBENCH_FILE, kclSource, "text/plain;charset=utf-8");
  }, [kclSource]);

  return {
    kclSource,
    revision,
    designContract,
    verificationReport,
    dispatchSemanticCommand,
    sourceRevisionRef,
    sourceFileInput,
    applySourceUpdate,
    canUndo,
    canRedo,
    model,
    evaluated,
    compiling,
    compilePending,
    showingLastAccepted,
    transportError,
    isAssembly,
    canExport,
    exporting,
    currentExport,
    errorCount,
    statusErrorCount,
    holeOffsetPending,
    handleUndo,
    handleRedo,
    handleCommitPreview,
    handleDiscardPreview,
    previewActionPending,
    handleCommitKclSource,
    handleOffsetHole,
    handleApplyQuickFeature,
    handleExportCommand,
    handleLoadKclFile,
    handleDownloadSource,
    aiConfigStatus,
    aiProfiles,
    activeAiProfileId,
    aiProfileStorage,
    handleSaveAiProfiles,
    handleSelectAiProfile,
    handleResetAiProfiles,
    handleTestAiConnection,
    aiGenerating,
    aiFeedback,
    aiUndoRevision,
    aiProgressSteps,
    aiThread,
    handleClearAiThread,
    handleCancelAi,
    handleAiGenerate,
  };
}
