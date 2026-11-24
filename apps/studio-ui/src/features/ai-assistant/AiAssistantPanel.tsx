import { useEffect, useMemo, useRef, useState } from "react";
import { MATERIAL_PRESETS } from "../../data/demoWorkbenchState.ts";
import { loadWorkbenchUiPrefs, updateWorkbenchUiPrefs } from "../../core/layout/workbenchPrefsStore.ts";
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
  MaterialPreset,
  SelectionTarget,
} from "../../types/scene.ts";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";
import {
  buildLlmConfigFromProfile,
  loadAiConfig,
  resolveActiveAiStatus,
  saveAiConfig,
} from "./aiConfigStore.ts";
import {
  AI_QUICK_EDIT_CHIPS,
  AI_STARTER_PROMPTS,
  type AiThreadItem,
} from "./aiDiff.ts";
import { buildPointAndPromptPayload } from "./pointAndPrompt.ts";
import { AiPipelineCard } from "./AiPipelineCard.tsx";
import { AiSettingsModal } from "./AiSettingsModal.tsx";

export interface AiAssistantPanelProps {
  readonly mode: AiGenerateMode;
  readonly onSelectMode: (mode: AiGenerateMode) => void;
  readonly externalPrompt?: { readonly text: string; readonly seq: number } | null;
  readonly onGenerate: (
    prompt: string,
    material: string,
    manufacturingMethod: string,
    options?: AiRequestOptions,
    contextLabel?: string,
  ) => void | boolean | Promise<void | boolean>;
  readonly onCancel?: () => void;
  readonly onUndoLast?: () => void;
  readonly canUndo?: boolean;
  readonly currentRevision?: number;
  readonly configStatus?: AiConfigStatus | null;
  readonly profiles?: readonly AiProfile[];
  readonly activeProfileId?: string | null;
  readonly storageKind?: "server-file" | "browser-local";
  readonly onSaveProfiles?: (payload: {
    readonly activeProfileId: string | null;
    readonly profiles: readonly AiProfileDraftInput[];
  }) => Promise<boolean>;
  readonly onSelectProfile?: (profileId: string | null) => void | Promise<void>;
  readonly onResetProfiles?: () => Promise<void>;
  readonly onTestConnection?: (payload: AiTestConnectionPayload) => Promise<AiTestConnectionResult>;
  readonly generating?: boolean;
  readonly feedback?: AiRunFeedback | null;
  readonly progressSteps?: readonly AiProgressStep[];
  readonly thread?: readonly AiThreadItem[];
  readonly onClearThread?: () => void;
  readonly selectedTarget?: SelectionTarget;
  readonly selectedTopoRefs?: readonly string[];
  readonly selectedNodeIds?: readonly string[];
  readonly selectedMaterial?: MaterialPreset;
  readonly onSelectMaterial?: (material: MaterialPreset) => void;
}

export function AiAssistantPanel({
  mode,
  onSelectMode,
  externalPrompt,
  onGenerate,
  onCancel,
  onUndoLast,
  canUndo,
  currentRevision,
  configStatus,
  profiles = [],
  activeProfileId = null,
  storageKind = "browser-local",
  onSaveProfiles,
  onSelectProfile,
  onResetProfiles,
  onTestConnection,
  generating = false,
  thread = [],
  onClearThread,
  selectedTarget = null,
  selectedTopoRefs = [],
  selectedNodeIds = [],
  selectedMaterial,
  onSelectMaterial,
}: AiAssistantPanelProps) {
  const [initialUiPrefs] = useState(loadWorkbenchUiPrefs);
  const [prompt, setPromptState] = useState(initialUiPrefs.aiPrompt);
  const [method, setMethodState] = useState(() => loadAiConfig().method);
  const [localMaterialId, setLocalMaterialId] = useState(() => loadAiConfig().material);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [attachSelection, setAttachSelection] = useState(true);

  const threadEndRef = useRef<HTMLDivElement>(null);

  const materialId = selectedMaterial?.id ?? localMaterialId;

  const setPrompt = (next: string) => {
    setPromptState(next);
    updateWorkbenchUiPrefs({ aiPrompt: next });
  };

  const handleMaterialChange = (nextId: string) => {
    setLocalMaterialId(nextId);
    const prev = loadAiConfig();
    saveAiConfig({ ...prev, material: nextId });
    const preset = MATERIAL_PRESETS.find((item) => item.id === nextId);
    if (preset && onSelectMaterial) {
      onSelectMaterial(preset);
    }
  };

  const handleMethodChange = (nextMethod: string) => {
    setMethodState(nextMethod);
    const prev = loadAiConfig();
    saveAiConfig({ ...prev, method: nextMethod });
  };

  useEffect(() => {
    if (externalPrompt?.text) {
      setPrompt(externalPrompt.text);
    }
  }, [externalPrompt]);

  useEffect(() => {
    if (selectedTarget) {
      setAttachSelection(true);
    }
  }, [selectedTarget]);

  useEffect(() => {
    threadEndRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [thread.length, generating]);

  const activeProfile = useMemo(
    () => profiles.find((item) => item.id === activeProfileId) ?? null,
    [profiles, activeProfileId],
  );

  const aiStatus = useMemo(
    () => resolveActiveAiStatus(activeProfile, configStatus, storageKind),
    [activeProfile, configStatus, storageKind],
  );

  const pointAndPrompt = useMemo(
    () => buildPointAndPromptPayload({
      prompt,
      attachSelection,
      selectedTarget,
      selectedTopoRefs,
      selectedNodeIds,
    }),
    [attachSelection, prompt, selectedNodeIds, selectedTarget, selectedTopoRefs],
  );

  const selectionContextLabel = useMemo(() => {
    if (!attachSelection) return undefined;
    const parts: string[] = [];
    if (pointAndPrompt.selectedTopoRefs.length > 0) parts.push(`TopoRef ${pointAndPrompt.selectedTopoRefs.join(", ")}`);
    if (pointAndPrompt.selectedNodeIds.length > 0) parts.push(`节点 ${pointAndPrompt.selectedNodeIds.join(", ")}`);
    return parts.length > 0 ? parts.join(" · ") : undefined;
  }, [attachSelection, pointAndPrompt.selectedNodeIds, pointAndPrompt.selectedTopoRefs]);

  const handleSubmit = (overridePrompt?: string, overrideMode?: AiGenerateMode) => {
    const rawPrompt = (overridePrompt ?? prompt).trim();
    if (generating || !rawPrompt) return;
    if (!aiStatus.ready) {
      setSettingsOpen(true);
      return;
    }
    const effectiveMode = overrideMode ?? mode;
    const payload = buildPointAndPromptPayload({
      prompt: rawPrompt,
      attachSelection,
      selectedTarget,
      selectedTopoRefs,
      selectedNodeIds,
    });
    const llm = buildLlmConfigFromProfile(activeProfile, storageKind);
    void Promise.resolve(
      onGenerate(
        payload.prompt,
        materialId,
        method,
        {
          mode: effectiveMode,
          llm,
          selectedTopoRefs: payload.selectedTopoRefs,
          selectedNodeIds: payload.selectedNodeIds,
        },
        selectionContextLabel,
      ),
    ).then((ok) => {
      if (ok && !overridePrompt) setPrompt("");
    });
  };

  useEffect(() => {
    const openSettings = () => setSettingsOpen(true);
    const retryLast = () => {
      const lastError = [...thread].reverse().find((item) => item.status === "error" && item.prompt.trim());
      if (lastError) handleSubmit(lastError.prompt, lastError.mode);
    };
    window.addEventListener("aicad:open-ai-settings", openSettings);
    window.addEventListener("aicad:retry-last-ai", retryLast);
    return () => {
      window.removeEventListener("aicad:open-ai-settings", openSettings);
      window.removeEventListener("aicad:retry-last-ai", retryLast);
    };
  }, [thread, generating, materialId, method, mode, attachSelection, selectedTarget, selectedTopoRefs, selectedNodeIds, activeProfile, storageKind]);

  return (
    <section className="ai-copilot-shell" aria-label="AI 建模助手">
      {/* Top compact header bar */}
      <header className="ai-copilot-header">
        <div className="ai-model-switcher">
          <span
            className={`ai-status-dot is-${aiStatus.tone}`}
            title={`${aiStatus.label} (${aiStatus.sourceLabel})`}
          />
          <select
            className="ai-model-select"
            value={activeProfile?.id ?? "__env__"}
            onChange={(e) => {
              const val = e.target.value;
              if (val === "__open_settings__") {
                setSettingsOpen(true);
                return;
              }
              void onSelectProfile?.(val === "__env__" ? null : val);
            }}
            aria-label="切换当前 AI 模型方案"
          >
            {profiles.length > 0 ? (
              profiles.map((prof) => (
                <option key={prof.id} value={prof.id}>
                  {prof.name}
                  {prof.model ? ` · ${prof.model}` : ""}
                </option>
              ))
            ) : (
              <option value="__env__">{aiStatus.label}</option>
            )}
            <option value="__env__">环境变量 / 离线默认</option>
            <option value="__open_settings__">+ 管理与新建模型配置…</option>
          </select>
        </div>

        <div className="ai-header-actions">
          {thread.length > 0 && onClearThread && (
            <button
              type="button"
              className="icon-ghost-btn"
              onClick={onClearThread}
              title="清空对话记录"
              aria-label="清空对话记录"
            >
              <CadIcon name="trash" size={14} />
            </button>
          )}
          <button
            type="button"
            className="icon-ghost-btn ai-settings-trigger"
            onClick={() => setSettingsOpen(true)}
            title="AI 模型与 API Key 配置"
            aria-label="AI 模型与 API Key 配置"
          >
            <CadIcon name="settings" size={15} />
          </button>
        </div>
      </header>

      {/* Lightweight config nudge when API key is not ready */}
      {!aiStatus.ready && (
        <div className="ai-config-nudge">
          <span className="ai-config-nudge-text">
            {activeProfile && activeProfile.provider !== "template"
              ? `「${activeProfile.name}」尚未配置 API Key`
              : "尚未配置真实 LLM 模型或 API Key，请配置后使用 AI 建模与增量修改"}
          </span>
          <button
            type="button"
            className="studio-btn studio-btn-sm"
            onClick={() => setSettingsOpen(true)}
          >
            配置密钥
          </button>
        </div>
      )}

      {/* Main conversation & pipeline stream */}
      <div className="ai-thread-scroll" role="log" aria-live="polite" aria-label="AI 建模对话流">
        {thread.length === 0 ? (
          <div className="ai-empty-state">
            <div className="ai-empty-hero">
              <div className="ai-empty-icon">
                <CadIcon name="sparkles" size={18} />
              </div>
              <div>
                <h3 className="ai-empty-title">AI 工程协作者</h3>
                <p className="ai-empty-desc">
                  描述工程需求生成参数化 KCL 零件，或直接在 3D 视口选中面/边进行增量修改，自动通过 OCCT 与 L0–L4 几何门禁。
                </p>
              </div>
            </div>

            <div className="ai-starter-list">
              <span className="ai-starter-heading">快速示例</span>
              {AI_STARTER_PROMPTS.map((starter) => (
                <button
                  key={starter.id}
                  type="button"
                  className="ai-starter-card"
                  onClick={() => {
                    onSelectMode(starter.mode);
                    setPrompt(starter.prompt);
                  }}
                >
                  <div className="ai-starter-card-head">
                    <strong>{starter.title}</strong>
                    <span className="mini-badge">{starter.tag}</span>
                  </div>
                  <p>{starter.prompt}</p>
                </button>
              ))}
            </div>
          </div>
        ) : (
          <div className="ai-thread-list">
            {thread.map((item) => {
              const canUndoThis =
                Boolean(canUndo) &&
                item.undoRevision !== undefined &&
                currentRevision !== undefined &&
                item.undoRevision === currentRevision;
              return (
                <article key={item.id} className="ai-turn">
                  <div className="ai-user-bubble">
                    <div className="ai-user-bubble-meta">
                      <span className="ai-mode-pill">
                        {item.mode === "edit" ? "增量修改" : "新建零件"}
                      </span>
                      {item.contextLabel && (
                        <span className="ai-context-badge">{item.contextLabel}</span>
                      )}
                      <span className="ai-turn-material">
                        {item.material} · {item.method === "CNC_3AXIS_MILLING" ? "CNC 3轴" : item.method === "CNC_5AXIS_MILLING" ? "CNC 5轴" : "SLM 3D打印"}
                      </span>
                    </div>
                    <p className="ai-user-bubble-text">{item.prompt}</p>
                  </div>

                  <AiPipelineCard
                    item={item}
                    canUndo={canUndoThis}
                    onUndo={onUndoLast}
                    onRetry={() => handleSubmit(item.prompt, item.mode)}
                    onConfigure={() => setSettingsOpen(true)}
                  />
                </article>
              );
            })}
            <div ref={threadEndRef} />
          </div>
        )}
      </div>

      {/* Bottom floating composer dock */}
      <div className="ai-composer-dock">
        {/* Context chips & quick edit suggestions */}
        {(selectedTarget || selectedNodeIds.length > 0 || mode === "edit") && (
          <div className="ai-composer-chips">
            {selectedNodeIds.length > 0 && !selectedTarget ? (
              <span className="ai-context-chip is-active">节点 {selectedNodeIds.join(", ")}</span>
            ) : null}
            {selectedTarget && (
              <button
                type="button"
                className={`ai-context-chip${attachSelection ? " is-active" : ""}`}
                onClick={() => setAttachSelection((v) => !v)}
                title={attachSelection ? "点击取消附带此 3D 选区" : "点击将此 3D 选区作为 AI 上下文"}
              >
                <CadIcon name={selectedTarget.kind === "face" ? "filterFace" : "filterEdge"} size={12} />
                <span>
                  {selectedTarget.kind === "face"
                    ? `面 ${selectedTarget.face.face_id}`
                    : `边 ${selectedTarget.edge.edge_id}`}
                </span>
                <span className="ai-chip-toggle">{attachSelection ? "×" : "+"}</span>
              </button>
            )}
            {mode === "edit" &&
              AI_QUICK_EDIT_CHIPS.map((chip) => (
                <button
                  key={chip.label}
                  type="button"
                  className="ai-quick-chip"
                  disabled={generating}
                  onClick={() => setPrompt(prompt.trim() ? `${prompt.trim()}；${chip.text}` : chip.text)}
                >
                  {chip.label}
                </button>
              ))}
          </div>
        )}

        <div className="ai-composer-box">
          <textarea
            className="ai-composer-textarea"
            rows={3}
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
                event.preventDefault();
                handleSubmit();
              }
            }}
            placeholder={
              mode === "edit"
                ? "描述增量修改，如：将 4 个安装孔改为 M5 沉头孔，底板增厚到 12mm… (Ctrl+Enter)"
                : "描述目标零件的工程需求、接口尺寸与制造工艺… (Ctrl+Enter)"
            }
            aria-label="AI 自然语言设计需求"
          />

          <div className="ai-composer-toolbar">
            <div className="ai-composer-controls">
              <div className="ai-mode-switch" role="group" aria-label="AI 工作模式">
                <button
                  type="button"
                  className={`ai-mode-btn${mode === "generate" ? " is-active" : ""}`}
                  onClick={() => onSelectMode("generate")}
                >
                  新建
                </button>
                <button
                  type="button"
                  className={`ai-mode-btn${mode === "edit" ? " is-active" : ""}`}
                  onClick={() => onSelectMode("edit")}
                >
                  修改
                </button>
              </div>

              <select
                className="ai-compact-select"
                value={materialId}
                onChange={(e) => handleMaterialChange(e.target.value)}
                aria-label="AI 生成材料"
                title="工程材料"
              >
                {MATERIAL_PRESETS.map((mat) => (
                  <option key={mat.id} value={mat.id}>
                    {mat.id}
                  </option>
                ))}
              </select>

              <select
                className="ai-compact-select"
                value={method}
                onChange={(e) => handleMethodChange(e.target.value)}
                aria-label="AI 生成工艺"
                title="制造工艺"
              >
                <option value="CNC_3AXIS_MILLING">CNC 3轴</option>
                <option value="CNC_5AXIS_MILLING">CNC 5轴</option>
                <option value="ADDITIVE_SLM">金属 3D 打印</option>
              </select>
            </div>

            <div className="ai-composer-submit">
              {generating && onCancel ? (
                <button
                  type="button"
                  className="studio-btn studio-btn-sm ai-stop-btn"
                  onClick={onCancel}
                >
                  <CadIcon name="close" size={13} />
                  <span>停止</span>
                </button>
              ) : (
                <button
                  type="button"
                  className="studio-btn studio-btn-sm studio-btn-ai ai-send-btn"
                  disabled={generating || !prompt.trim()}
                  onClick={() => handleSubmit()}
                  title="发送 (Ctrl+Enter)"
                >
                  <CadIcon name="sparkles" size={13} />
                  <span>{mode === "edit" ? "应用修改" : "生成零件"}</span>
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

      {/* Standalone dual-pane AI Settings Modal */}
      <AiSettingsModal
        open={settingsOpen}
        profiles={profiles}
        activeProfileId={activeProfileId}
        storageKind={storageKind}
        envStatus={configStatus}
        onClose={() => setSettingsOpen(false)}
        onSave={async (payload) => {
          if (onSaveProfiles) {
            return onSaveProfiles(payload);
          }
          return false;
        }}
        onResetAll={async () => {
          if (onResetProfiles) {
            await onResetProfiles();
          }
        }}
        onTestConnection={onTestConnection}
      />
    </section>
  );
}
