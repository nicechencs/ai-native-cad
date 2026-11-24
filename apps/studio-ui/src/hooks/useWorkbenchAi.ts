import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { MATERIAL_PRESETS } from "../data/demoWorkbenchState.ts";
import type { ChangePreviewInput } from "../core/session/engineeringSession.ts";
import {
  clearAiConfig,
  clearLocalAiProfiles,
  loadLocalAiProfiles,
  resolveActiveAiStatus,
  saveLocalAiProfiles,
} from "../features/ai-assistant/aiConfigStore.ts";
import type { AiThreadItem } from "../features/ai-assistant/aiDiff.ts";
import {
  isHardFailedV2Preview,
  parseV2ChangePreview,
} from "../features/ai-assistant/changePreviewModel.ts";
import { buildAiGenerateRequest } from "../features/ai-assistant/pointAndPrompt.ts";
import {
  aiPreviewFromResponse,
  type WorkbenchBackend,
  type WorkbenchModel,
} from "../sync/workbenchBackend.ts";
import type {
  AiConfigStatus,
  AiGenerateMode,
  AiProfile,
  AiProfileDraftInput,
  AiProgressStep,
  AiRequestOptions,
  AiRunFeedback,
  AiTestConnectionPayload,
  AiTestConnectionResult,
  GeometryMetrics,
  MaterialPreset,
} from "../types/scene.ts";
import { readTraceId, traceIdFrom } from "../utils/trace.ts";
import { sourceUndoAction, type ApplySourceUpdate, type WorkbenchNotify } from "./workbenchSource.ts";

function parseAiProgressStep(raw: unknown): AiProgressStep | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const obj = raw as Record<string, unknown>;
  if (typeof obj.stage !== "string" || typeof obj.message !== "string") return null;
  return {
    stage: obj.stage,
    message: obj.message,
    ...(typeof obj.iteration === "number" ? { iteration: obj.iteration } : {}),
    ...(typeof obj.max_iters === "number" ? { max_iters: obj.max_iters } : {}),
    ...(typeof obj.passed === "boolean" ? { passed: obj.passed } : {}),
    timestamp: Date.now(),
  };
}

export type AiRunOutcome =
  | {
      readonly kind: "offline_error";
      readonly title: string;
      readonly message: string;
    }
  | {
      readonly kind: "config_error";
      readonly title: string;
      readonly message: string;
      readonly errorCode: string;
    }
  | {
      readonly kind: "error";
      readonly title: string;
      readonly message: string;
      readonly source?: string;
      readonly previewModel?: WorkbenchModel;
    }
  | {
      readonly kind: "preview_ready";
      readonly preview: ChangePreviewInput;
      readonly candidateSource: string;
      readonly previewModel?: WorkbenchModel;
      readonly summaryMsg: string;
      readonly detail: string;
      readonly feedback: AiRunFeedback;
    }
  | {
      readonly kind: "direct_apply";
      readonly source: string;
      readonly previewModel?: WorkbenchModel;
      readonly summaryMsg: string;
      readonly detail: string;
      readonly feedback: AiRunFeedback;
    };

export interface ClassifyAiRunInput {
  readonly hasResponse: boolean;
  readonly responseOk: boolean;
  readonly responseStatus: number;
  readonly streamErrorStatus: number | null;
  readonly payload: Record<string, unknown> | null;
  readonly mode: AiGenerateMode;
  readonly prompt: string;
  readonly material: string;
  readonly manufacturingMethod: string;
  readonly beforeSource: string;
  readonly beforeMetrics?: GeometryMetrics;
  readonly projectPath?: string | null;
  readonly backend: WorkbenchBackend;
}

/**
 * Pure classification of an AI generation/edit response for both Web HTTP and Desktop Tauri native backends.
 * Never falls back to offline hardcoded templates; distinguishes honest UNKNOWN L4 verdicts from hard FAIL.
 */

function aiRecoveryAction(kind: "config_error" | "error" | "offline_error"): { label: string; onClick: () => void } {
  if (kind === "config_error") {
    return {
      label: "配置密钥",
      onClick: () => window.dispatchEvent(new Event("aicad:open-ai-settings")),
    };
  }
  return {
    label: "重试",
    onClick: () => window.dispatchEvent(new Event("aicad:retry-last-ai")),
  };
}

export function classifyAiRunOutcome(input: ClassifyAiRunInput): AiRunOutcome {
  const {
    hasResponse,
    responseOk,
    responseStatus,
    streamErrorStatus,
    payload,
    mode,
    prompt,
    material,
    manufacturingMethod,
    beforeSource,
    beforeMetrics,
    projectPath,
    backend,
  } = input;

  if (!hasResponse) {
    return {
      kind: "offline_error",
      title: mode === "edit" ? "AI 增量修改不可用" : "AI 服务未连接",
      message:
        mode === "edit"
          ? "工作台 AI 服务未连接，无法执行增量修改。请连接工作台服务并在 AI 设置中配置真实 LLM API Key 后重试。"
          : "工作台 AI 服务未连接，无法生成模型（已禁用离线假模板兜底）。请连接工作台服务并在 AI 设置中配置真实 LLM API Key 后重试。",
    };
  }

  const errorCode =
    payload && typeof payload.error_code === "string" && payload.error_code.trim()
      ? payload.error_code.trim()
      : "";
  const errorText =
    payload && typeof payload.error === "string" && payload.error.trim()
      ? payload.error.trim()
      : payload && typeof payload.message === "string" && payload.message.trim()
        ? payload.message.trim()
        : "";

  if (errorCode === "llm_not_configured" || errorCode === "edit_requires_llm") {
    return {
      kind: "config_error",
      title: "需要配置真实 LLM",
      message:
        errorText ||
        "当前未配置可用的 LLM（OpenAI / Gemini / 本地兼容接口），请点击 AI 设置填写 API Key 或模型端点后重试。",
      errorCode,
    };
  }

  const sourceText =
    payload && typeof payload.source === "string" && payload.source.trim()
      ? payload.source
      : payload && typeof payload.kcl_code === "string" && payload.kcl_code.trim()
        ? payload.kcl_code
        : "";

  const summaryObj =
    payload?.agentSummary && typeof payload.agentSummary === "object"
      ? (payload.agentSummary as Record<string, unknown>)
      : null;

  const rawChangePreview =
    payload?.changePreview && typeof payload.changePreview === "object" && !Array.isArray(payload.changePreview)
      ? (payload.changePreview as Record<string, unknown>)
      : null;

  const hasV2Preview = rawChangePreview !== null;
  const hardFailedPreview = hasV2Preview && isHardFailedV2Preview(rawChangePreview);

  // On non-OCCT builds, V2 honestly reports `verdict: "UNKNOWN"` (e.g. L4 roundtrip unavailable)
  // alongside `passed: false`. Do NOT mistake honest UNKNOWN for a hard failure when a valid source exists.
  const isHardValidationFailure = hasV2Preview
    ? hardFailedPreview
    : mode === "edit" && (payload?.ok === false || summaryObj?.passed === false);

  if (
    streamErrorStatus !== null ||
    !responseOk ||
    errorText ||
    !sourceText.trim() ||
    isHardValidationFailure
  ) {
    const errMsg =
      errorText ||
      (isHardValidationFailure
        ? "模型变更未通过编译或几何门禁校验，已保留当前工程状态。"
        : !sourceText.trim()
          ? "AI 后端未返回有效 KCL 预览源码，已保留当前工程状态。"
          : `AI 生成请求失败 (HTTP ${streamErrorStatus ?? responseStatus ?? 500})`);
    return {
      kind: "error",
      title: mode === "edit" ? "AI 增量修改失败" : "AI 生成失败",
      message: errMsg,
    };
  }

  const generatedPreview = payload ? aiPreviewFromResponse(backend, payload) : undefined;
  const usedLlm = summaryObj?.used_llm === true;
  const llmProvider = typeof summaryObj?.llm_provider === "string" ? summaryObj.llm_provider : undefined;
  const llmModel = typeof summaryObj?.llm_model === "string" ? summaryObj.llm_model : undefined;
  const vlmEnabled = typeof summaryObj?.vlm_enabled === "boolean" ? summaryObj.vlm_enabled : undefined;
  const repairAttempts =
    typeof summaryObj?.contract_repair_attempts === "number"
      ? summaryObj.contract_repair_attempts
      : undefined;
  const retrievedExamples = Array.isArray(summaryObj?.retrieved_examples)
    ? summaryObj.retrieved_examples.filter((x): x is string => typeof x === "string")
    : undefined;

  const detail = usedLlm
    ? `${llmProvider ?? "llm"}${llmModel ? ` · ${llmModel}` : ""} · ${material}`
    : `${material} / ${manufacturingMethod}`;

  if (hasV2Preview) {
    const parsedPreview = parseV2ChangePreview(rawChangePreview, {
      mode,
      candidateSource: sourceText,
      fallbackPrompt: prompt,
      fallbackProjectPath: projectPath ?? null,
    });
    if (parsedPreview) {
      const verdict = parsedPreview.verificationReport.overall_status;
      const summaryMsg =
        verdict === "UNKNOWN"
          ? "AI 变更预览已就绪（部分门禁 UNKNOWN，请审阅后确认合入）"
          : mode === "edit"
            ? "AI 增量修改预览已就绪，请审阅后确认合入"
            : "AI 零件生成预览已就绪，请审阅后确认合入";
      return {
        kind: "preview_ready",
        preview: parsedPreview,
        candidateSource: sourceText,
        previewModel: generatedPreview,
        summaryMsg,
        detail,
        feedback: {
          status: "success",
          message: summaryMsg,
          detail,
          vlmEnabled,
          contractRepairAttempts: repairAttempts,
          retrievedExamples,
          beforeSource,
          afterSource: sourceText,
          beforeMetrics,
          afterMetrics: generatedPreview?.diagnostics.metrics,
        },
      };
    }
  }

  if (payload?.ok === false) {
    return {
      kind: "error",
      title: "已生成 KCL 源码，但服务端编译失败，请查看诊断。",
      message: "已生成 KCL 源码，但服务端编译失败，请查看诊断。",
      source: sourceText,
      previewModel: generatedPreview,
    };
  }

  const summaryMsg = mode === "edit" ? "AI 增量修改已通过门禁并同步模型" : "AI 新零件已生成并编译";
  return {
    kind: "direct_apply",
    source: sourceText,
    previewModel: generatedPreview,
    summaryMsg,
    detail,
    feedback: {
      status: "success",
      message: summaryMsg,
      detail,
      vlmEnabled,
      contractRepairAttempts: repairAttempts,
      retrievedExamples,
      beforeSource,
      afterSource: sourceText,
      beforeMetrics,
      afterMetrics: generatedPreview?.diagnostics.metrics,
    },
  };
}

interface WorkbenchAiOptions {
  readonly backend: WorkbenchBackend;
  readonly kclSource: string;
  readonly activeProjectPath?: string | null;
  readonly currentMetrics?: GeometryMetrics;
  readonly revision: number;
  readonly sourceRevisionRef: RefObject<number>;
  readonly setSelectedMaterial: (material: MaterialPreset) => void;
  readonly onOpenPreview?: (
    preview: ChangePreviewInput,
    options?: {
      readonly previewModel?: WorkbenchModel;
      readonly materialId?: string;
      readonly runId?: string;
    },
  ) => void;
  readonly notify: WorkbenchNotify;
}

export function useWorkbenchAi({
  backend,
  kclSource,
  activeProjectPath = null,
  currentMetrics,
  revision,
  sourceRevisionRef,
  setSelectedMaterial,
  onOpenPreview,
  notify,
}: WorkbenchAiOptions) {
  const aiRequest = useRef<AbortController | null>(null);
  const activeRunIdRef = useRef<string | null>(null);

  const [aiConfigStatus, setAiConfigStatus] = useState<AiConfigStatus | null>(null);
  const [aiProfiles, setAiProfiles] = useState<readonly AiProfile[]>(() => loadLocalAiProfiles().profiles);
  const [activeAiProfileId, setActiveAiProfileId] = useState<string | null>(() => loadLocalAiProfiles().activeProfileId);
  const [aiProfileStorage, setAiProfileStorage] = useState<"server-file" | "browser-local">("browser-local");

  const [aiGenerating, setAiGenerating] = useState(false);
  const [aiFeedback, setAiFeedback] = useState<AiRunFeedback | null>(null);
  const [aiUndoRevision, setAiUndoRevision] = useState<number>();
  const [aiProgressSteps, setAiProgressSteps] = useState<readonly AiProgressStep[]>([]);
  const [aiThread, setAiThread] = useState<readonly AiThreadItem[]>([]);

  useEffect(() => {
    const abort = new AbortController();
    void backend
      .getAiConfig(abort.signal)
      .then((config) => {
        if (!abort.signal.aborted) setAiConfigStatus(config);
      })
      .catch(() => {
        if (!abort.signal.aborted) setAiConfigStatus(null);
      });

    if (backend.getAiProfiles) {
      void backend
        .getAiProfiles(abort.signal)
        .then((res) => {
          if (abort.signal.aborted || !res) return;
          setAiProfileStorage(res.storage);
          if (res.profiles.length > 0) {
            setAiProfiles(res.profiles);
            setActiveAiProfileId(res.activeProfileId);
          } else {
            const local = loadLocalAiProfiles();
            setAiProfiles(local.profiles);
            setActiveAiProfileId(local.activeProfileId);
          }
          if (res.envStatus) {
            setAiConfigStatus(res.envStatus);
          }
        })
        .catch(() => {
          if (!abort.signal.aborted) {
            const local = loadLocalAiProfiles();
            setAiProfileStorage("browser-local");
            setAiProfiles(local.profiles);
            setActiveAiProfileId(local.activeProfileId);
          }
        });
    }
    return () => abort.abort();
  }, [backend]);

  const handleSaveAiProfiles = useCallback(
    async (payload: {
      readonly activeProfileId: string | null;
      readonly profiles: readonly AiProfileDraftInput[];
    }): Promise<boolean> => {
      if (backend.saveAiProfiles) {
        try {
          const saved = await backend.saveAiProfiles(payload);
          if (saved) {
            setAiProfileStorage(saved.storage);
            setAiProfiles(saved.profiles);
            setActiveAiProfileId(saved.activeProfileId);
            if (saved.envStatus) setAiConfigStatus(saved.envStatus);
            notify({
              level: "success",
              title: "AI 模型配置已安全保存",
              detail: saved.storage === "server-file" ? "已写入本机配置文件 (~/.aicad-studio)" : "已保存到浏览器本地",
              source: "AI",
            });
            return true;
          }
        } catch {
          // Fall back to local browser storage if server profile endpoint is unavailable.
        }
      }
      const localSaved = saveLocalAiProfiles(payload, aiProfiles);
      setAiProfileStorage("browser-local");
      setAiProfiles(localSaved.profiles);
      setActiveAiProfileId(localSaved.activeProfileId);
      notify({
        level: "success",
        title: "AI 模型配置已保存到本地浏览器",
        source: "AI",
      });
      return true;
    },
    [backend, aiProfiles, notify],
  );

  const handleSelectAiProfile = useCallback(
    async (profileId: string | null) => {
      setActiveAiProfileId(profileId);
      const drafts: AiProfileDraftInput[] = aiProfiles.map((p) => ({
        id: p.id,
        name: p.name,
        preset: p.preset,
        provider: p.provider,
        model: p.model,
        baseUrl: p.baseUrl,
        enableVlm: p.enableVlm,
        fewShotK: p.fewShotK,
      }));
      if (aiProfileStorage === "server-file" && backend.saveAiProfiles) {
        try {
          const saved = await backend.saveAiProfiles({ activeProfileId: profileId, profiles: drafts });
          if (saved) {
            setAiProfiles(saved.profiles);
            setActiveAiProfileId(saved.activeProfileId);
            return;
          }
        } catch {
          // Ignore quick-switch persistence failure.
        }
      }
      saveLocalAiProfiles({ activeProfileId: profileId, profiles: drafts }, aiProfiles);
    },
    [aiProfiles, aiProfileStorage, backend],
  );

  const handleResetAiProfiles = useCallback(async () => {
    clearAiConfig();
    clearLocalAiProfiles();
    if (backend.saveAiProfiles) {
      try {
        const saved = await backend.saveAiProfiles({ activeProfileId: null, profiles: [] });
        if (saved) {
          setAiProfileStorage(saved.storage);
          setAiProfiles(saved.profiles);
          setActiveAiProfileId(saved.activeProfileId);
          notify({ level: "info", title: "已清空所有已保存的 AI 模型配置", source: "AI" });
          return;
        }
      } catch {
        // Fall through to local reset.
      }
    }
    setAiProfiles([]);
    setActiveAiProfileId(null);
    notify({ level: "info", title: "已重置 AI 模型配置", source: "AI" });
  }, [backend, notify]);

  const handleTestAiConnection = useCallback(
    async (payload: AiTestConnectionPayload): Promise<AiTestConnectionResult> => {
      if (backend.testAiConnection) {
        return backend.testAiConnection(payload);
      }
      return {
        ok: false,
        provider: payload.provider,
        message: "当前离线环境无法执行网络连通性测试。",
      };
    },
    [backend],
  );

  const updateThreadItem = useCallback(
    (runId: string, updater: (item: AiThreadItem) => AiThreadItem) => {
      setAiThread((prev) => prev.map((item) => (item.id === runId ? updater(item) : item)));
    },
    [],
  );

  const handleClearAiThread = useCallback(() => {
    setAiThread((prev) => prev.filter((item) => item.status === "running"));
    setAiFeedback(null);
    setAiProgressSteps([]);
  }, []);

  const abortAiRequest = useCallback(() => {
    if (!aiRequest.current) return false;
    aiRequest.current.abort();
    aiRequest.current = null;
    if (activeRunIdRef.current) {
      const runId = activeRunIdRef.current;
      activeRunIdRef.current = null;
      updateThreadItem(runId, (item) =>
        item.status === "running"
          ? {
              ...item,
              status: "cancelled",
              feedback: { status: "error", message: "任务已取消", durationMs: Date.now() - item.createdAt },
            }
          : item,
      );
    }
    return true;
  }, [updateThreadItem]);

  const cancelAiOnSourceUpdate = useCallback(() => {
    if (!abortAiRequest()) return;
    setAiGenerating(false);
    setAiProgressSteps([]);
    notify({ level: "info", title: "源码已更新，已取消上次 AI 任务", source: "AI" });
  }, [abortAiRequest, notify]);

  const handleCancelAi = useCallback(() => {
    if (!abortAiRequest()) return;
    setAiGenerating(false);
    setAiProgressSteps([]);
    notify({ level: "info", title: "已中止当前 AI 生成任务", source: "AI" });
  }, [abortAiRequest, notify]);

  // The document supplies its writer at launch so all AI results use the normal source-update path.
  const generateAi = useCallback(
    async (
      applySourceUpdate: ApplySourceUpdate,
      prompt: string,
      material: string,
      manufacturingMethod: string,
      options?: AiRequestOptions,
      contextLabel?: string,
    ) => {
      const mode: AiGenerateMode = options?.mode ?? "generate";
      abortAiRequest();
      const abort = new AbortController();
      aiRequest.current = abort;
      const isCurrent = () =>
        !abort.signal.aborted && aiRequest.current === abort && sourceRevisionRef.current === revision;

      const startedAt = Date.now();
      const beforeSource = kclSource;
      const beforeMetrics = currentMetrics;
      const runId = `run_${startedAt.toString(36)}_${Math.random().toString(36).slice(2, 6)}`;
      activeRunIdRef.current = runId;

      const activeProfile = aiProfiles.find((p) => p.id === activeAiProfileId) ?? null;
      const activeStatus = resolveActiveAiStatus(activeProfile, aiConfigStatus, aiProfileStorage);

      const newEntry: AiThreadItem = {
        id: runId,
        createdAt: startedAt,
        mode,
        prompt: prompt.trim(),
        material,
        method: manufacturingMethod,
        ...(contextLabel ? { contextLabel } : {}),
        modelLabel: activeStatus.label,
        status: "running",
        steps: [],
      };

      setAiThread((prev) => [...prev.slice(-19), newEntry]);
      setAiGenerating(true);
      setAiFeedback(null);
      setAiProgressSteps([]);
      notify({
        level: "info",
        title: mode === "edit" ? "AI 正在增量修改当前模型…" : "AI 正在生成新零件 KCL…",
        source: "AI",
        showToast: false,
      });

      const finishError = (feedback: AiRunFeedback) => {
        const enriched: AiRunFeedback = {
          ...feedback,
          durationMs: Date.now() - startedAt,
        };
        setAiFeedback(enriched);
        updateThreadItem(runId, (item) => ({
          ...item,
          status: "error",
          feedback: enriched,
        }));
      };

      const finishSuccess = (feedback: AiRunFeedback, undoRevision?: number) => {
        const enriched: AiRunFeedback = {
          ...feedback,
          durationMs: Date.now() - startedAt,
        };
        setAiFeedback(enriched);
        updateThreadItem(runId, (item) => ({
          ...item,
          status: "success",
          feedback: enriched,
          ...(undoRevision !== undefined ? { undoRevision } : {}),
        }));
      };

      const requestBody = buildAiGenerateRequest({
        prompt,
        material,
        manufacturingMethod,
        mode,
        existingSource: kclSource,
        projectPath: activeProjectPath,
        llm: options?.llm,
        selectedTopoRefs: options?.selectedTopoRefs,
        selectedNodeIds: options?.selectedNodeIds,
      });

      try {
        const response = await backend.requestAi(requestBody, abort.signal);
        if (!isCurrent()) {
          void response?.body?.cancel().catch(() => {});
          return;
        }
        let payload: Record<string, unknown> | null = null;
        let streamErrorStatus: number | null = null;

        if (response) {
          const contentType = response.headers.get("content-type") ?? "";
          if (contentType.includes("text/event-stream") && response.body) {
            const reader = response.body.getReader();
            const decoder = new TextDecoder("utf-8");
            let sseBuffer = "";

            const handleSseBlock = (block: string) => {
              const lines = block.split(/\r?\n/);
              let eventName = "message";
              const dataLines: string[] = [];
              for (const line of lines) {
                if (line.startsWith("event:")) eventName = line.slice(6).trim();
                else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
              }
              if (dataLines.length === 0) return;
              let parsed: unknown;
              try {
                parsed = JSON.parse(dataLines.join("\n"));
              } catch {
                return;
              }
              if (eventName === "progress") {
                const step = parseAiProgressStep(parsed);
                if (step && isCurrent()) {
                  setAiProgressSteps((prev) => [...prev, step]);
                  updateThreadItem(runId, (item) => ({
                    ...item,
                    steps: [...item.steps, step],
                  }));
                }
              } else if (eventName === "result") {
                if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
                  payload = parsed as Record<string, unknown>;
                }
              } else if (eventName === "error") {
                if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
                  const errObj = parsed as Record<string, unknown>;
                  payload = errObj;
                  streamErrorStatus = typeof errObj.status === "number" ? errObj.status : 422;
                }
              }
            };

            try {
              while (true) {
                const { value, done } = await reader.read();
                if (!isCurrent()) {
                  void reader.cancel().catch(() => {});
                  return;
                }
                if (done) break;
                sseBuffer += decoder.decode(value, { stream: true });
                const blocks = sseBuffer.split(/\r?\n\r?\n/);
                sseBuffer = blocks.pop() ?? "";
                for (const block of blocks) {
                  if (block.trim()) handleSseBlock(block);
                }
              }
              sseBuffer += decoder.decode();
              if (sseBuffer.trim()) handleSseBlock(sseBuffer);
            } catch (readErr) {
              if (isCurrent()) {
                const errMsg =
                  readErr instanceof Error && readErr.message
                    ? `SSE 流读取中断: ${readErr.message}`
                    : "SSE 流读取意外中断";
                finishError({ status: "error", message: errMsg });
                const streamTrace = readTraceId(response.headers.get("x-trace-id"), null, "") || undefined;
                notify({
                  level: "error",
                  title: "AI 流式响应中断",
                  detail: errMsg,
                  source: "AI",
                  ...(streamTrace ? { traceId: streamTrace } : {}),
                });
              }
              return;
            } finally {
              reader.releaseLock();
            }
          } else {
            try {
              const parsed: unknown = await response.json();
              if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
                payload = parsed as Record<string, unknown>;
              }
            } catch {
              // Malformed response handled below.
            }
          }
        }

        if (!isCurrent()) return false;
        const aiTraceId = response
          ? readTraceId(response.headers.get("x-trace-id"), payload, "") || undefined
          : undefined;

        const outcome = classifyAiRunOutcome({
          hasResponse: response !== null,
          responseOk: response?.ok ?? false,
          responseStatus: response?.status ?? 500,
          streamErrorStatus,
          payload,
          mode,
          prompt,
          material,
          manufacturingMethod,
          beforeSource,
          beforeMetrics,
          projectPath: activeProjectPath,
          backend,
        });

        if (outcome.kind === "offline_error" || outcome.kind === "config_error") {
          finishError({ status: "error", message: outcome.message });
          notify({
            level: "error",
            title: outcome.title,
            detail: outcome.message,
            source: "AI",
            action: aiRecoveryAction(outcome.kind),
            ...(aiTraceId ? { traceId: aiTraceId } : {}),
          });
          return false;
        }

        if (outcome.kind === "error") {
          if (outcome.source) {
            aiRequest.current = null;
            activeRunIdRef.current = null;
            setAiGenerating(false);
            const undoRevision = applySourceUpdate(outcome.source, {
              summary: mode === "edit" ? "AI 增量修改" : "AI 生成新零件",
              previewModel: outcome.previewModel,
            });
            setAiUndoRevision(undoRevision);
            finishError({
              status: "error",
              message: outcome.message,
              beforeSource,
              afterSource: outcome.source,
            });
            notify({
              level: "warning",
              title: outcome.title,
              source: "AI",
              action: aiRecoveryAction("error"),
              ...(aiTraceId ? { traceId: aiTraceId } : {}),
            });
            return false;
          }
          finishError({ status: "error", message: outcome.message });
          notify({
            level: "error",
            title: outcome.title,
            detail: outcome.message,
            source: "AI",
            action: aiRecoveryAction("error"),
            ...(aiTraceId ? { traceId: aiTraceId } : {}),
          });
          return false;
        }

        aiRequest.current = null;
        activeRunIdRef.current = null;
        setAiGenerating(false);

        if (outcome.kind === "preview_ready" && onOpenPreview) {
          onOpenPreview(outcome.preview, {
            previewModel: outcome.previewModel,
            materialId: material,
            runId,
          });
          finishSuccess(outcome.feedback);
          notify({
            level: "info",
            title: outcome.summaryMsg,
            detail: `${outcome.preview.previewBranch} · ${outcome.detail}`,
            source: "AI",
          });
          return true;
        }

        const sourceToApply =
          outcome.kind === "preview_ready" ? outcome.candidateSource : outcome.source;
        const undoRevision = applySourceUpdate(sourceToApply, {
          summary: mode === "edit" ? "AI 增量修改" : "AI 生成新零件",
          previewModel: outcome.previewModel,
        });
        setAiUndoRevision(undoRevision);
        const preset = MATERIAL_PRESETS.find((item) => item.id === material);
        if (preset) setSelectedMaterial(preset);
        finishSuccess(outcome.feedback, undoRevision);
        notify({
          level: "success",
          title: outcome.summaryMsg,
          detail: outcome.detail,
          source: "AI",
          action: sourceUndoAction(undoRevision),
        });
        return true;
      } catch (err) {
        if (!abort.signal.aborted && isCurrent()) {
          const msg = err instanceof Error ? err.message : String(err);
          const traceId = traceIdFrom(err);
          finishError({ status: "error", message: msg });
          notify({
            level: "error",
            title: "AI 请求异常",
            detail: msg,
            source: "AI",
            action: aiRecoveryAction("error"),
            ...(traceId ? { traceId } : {}),
          });
        }
        return false;
      } finally {
        if (aiRequest.current === abort) {
          aiRequest.current = null;
          activeRunIdRef.current = null;
          setAiGenerating(false);
        }
      }
    },
    [
      abortAiRequest,
      activeProjectPath,
      backend,
      kclSource,
      currentMetrics,
      notify,
      onOpenPreview,
      revision,
      sourceRevisionRef,
      setSelectedMaterial,
      aiProfiles,
      activeAiProfileId,
      aiConfigStatus,
      aiProfileStorage,
      updateThreadItem,
    ],
  );

  const attachCommittedPreviewRevision = useCallback(
    (runId: string | undefined, committedRevision: number | undefined) => {
      setAiUndoRevision(committedRevision);
      if (!runId || committedRevision === undefined) return;
      updateThreadItem(runId, (item) => ({
        ...item,
        undoRevision: committedRevision,
      }));
    },
    [updateThreadItem],
  );

  return {
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
  };
}
