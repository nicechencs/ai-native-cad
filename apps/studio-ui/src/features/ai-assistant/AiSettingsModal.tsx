import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import type {
  AiConfigStatus,
  AiPresetId,
  AiProfile,
  AiProfileDraftInput,
  AiProvider,
  AiTestConnectionPayload,
  AiTestConnectionResult,
} from "../../types/scene.ts";
import { CadIcon } from "../../ui/icons/CadIcons.tsx";
import {
  AI_PROVIDER_PRESETS,
  createProfileFromPreset,
  getPresetById,
} from "./aiConfigStore.ts";

interface ProfileDraft extends AiProfileDraftInput {
  readonly hasSavedKey: boolean;
  readonly savedKeyMask: string;
  readonly updatedAt?: string;
}

export interface AiSettingsModalProps {
  readonly open: boolean;
  readonly profiles: readonly AiProfile[];
  readonly activeProfileId: string | null;
  readonly storageKind: "server-file" | "browser-local";
  readonly envStatus?: AiConfigStatus | null;
  readonly onClose: () => void;
  readonly onSave: (payload: {
    readonly activeProfileId: string | null;
    readonly profiles: readonly AiProfileDraftInput[];
  }) => Promise<boolean>;
  readonly onResetAll: () => Promise<void>;
  readonly onTestConnection?: (payload: AiTestConnectionPayload) => Promise<AiTestConnectionResult>;
}

function toDraft(profile: AiProfile): ProfileDraft {
  return {
    id: profile.id,
    name: profile.name,
    preset: profile.preset,
    provider: profile.provider,
    model: profile.model,
    baseUrl: profile.baseUrl,
    apiKey: "",
    clearApiKey: false,
    hasSavedKey: profile.hasApiKey,
    savedKeyMask: profile.apiKeyMask,
    enableVlm: profile.enableVlm,
    fewShotK: profile.fewShotK,
    updatedAt: profile.updatedAt,
  };
}

export function AiSettingsModal({
  open,
  profiles,
  activeProfileId,
  storageKind,
  envStatus,
  onClose,
  onSave,
  onResetAll,
  onTestConnection,
}: AiSettingsModalProps) {
  const [drafts, setDrafts] = useState<ProfileDraft[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [activeIdDraft, setActiveIdDraft] = useState<string | null>(null);
  const [showKey, setShowKey] = useState(false);
  const [showNewPresetMenu, setShowNewPresetMenu] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveNotice, setSaveNotice] = useState<{ readonly kind: "ok" | "error"; readonly text: string } | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<AiTestConnectionResult | null>(null);

  useEffect(() => {
    if (!open) return;
    const initial =
      profiles.length > 0
        ? profiles.map(toDraft)
        : [toDraft(createProfileFromPreset("openai"))];
    const initialActive =
      activeProfileId && initial.some((item) => item.id === activeProfileId)
        ? activeProfileId
        : (initial[0]?.id ?? null);
    setDrafts(initial);
    setSelectedId(initialActive);
    setActiveIdDraft(initialActive);
    setShowKey(false);
    setShowNewPresetMenu(false);
    setDirty(profiles.length === 0);
    setSaveNotice(null);
    setTestResult(null);
  }, [open, profiles, activeProfileId]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  const current = useMemo(
    () => drafts.find((item) => item.id === selectedId) ?? drafts[0] ?? null,
    [drafts, selectedId],
  );

  if (!open) return null;

  const presetInfo = current ? getPresetById(current.preset) : AI_PROVIDER_PRESETS[0];

  const updateCurrent = (patch: Partial<ProfileDraft>) => {
    if (!current) return;
    setDrafts((prev) =>
      prev.map((item) => (item.id === current.id ? { ...item, ...patch } : item)),
    );
    setDirty(true);
    setSaveNotice(null);
    setTestResult(null);
  };

  const handleAddProfile = (presetId: AiPresetId) => {
    const created = toDraft(
      createProfileFromPreset(
        presetId,
        drafts.map((item) => item.name),
      ),
    );
    setDrafts((prev) => [...prev, created]);
    setSelectedId(created.id);
    setActiveIdDraft(created.id);
    setShowNewPresetMenu(false);
    setShowKey(false);
    setDirty(true);
    setSaveNotice(null);
    setTestResult(null);
  };

  const handleDeleteProfile = (id: string) => {
    setDrafts((prev) => {
      const next = prev.filter((item) => item.id !== id);
      if (selectedId === id) {
        setSelectedId(next[0]?.id ?? null);
      }
      if (activeIdDraft === id) {
        setActiveIdDraft(next[0]?.id ?? null);
      }
      return next;
    });
    setDirty(true);
    setSaveNotice(null);
    setTestResult(null);
  };

  const handleApplyPresetToCurrent = (presetId: AiPresetId) => {
    const preset = getPresetById(presetId);
    updateCurrent({
      preset: preset.id,
      provider: preset.provider,
      baseUrl: preset.defaultBaseUrl,
      model: preset.defaultModel,
    });
  };

  const handleSave = async () => {
    setSaving(true);
    setSaveNotice(null);
    try {
      const ok = await onSave({
        activeProfileId: activeIdDraft,
        profiles: drafts.map((item) => ({
          id: item.id,
          name: item.name,
          preset: item.preset,
          provider: item.provider,
          model: item.model,
          baseUrl: item.baseUrl,
          ...(item.apiKey?.trim() ? { apiKey: item.apiKey.trim() } : {}),
          ...(item.clearApiKey ? { clearApiKey: true } : {}),
          enableVlm: item.enableVlm,
          fewShotK: item.fewShotK,
        })),
      });
      if (ok) {
        setDirty(false);
        setSaveNotice({
          kind: "ok",
          text:
            storageKind === "server-file"
              ? "已安全保存到本机配置文件"
              : "已保存到浏览器本地存储",
        });
      } else {
        setSaveNotice({ kind: "error", text: "保存失败，请检查工作台服务连接" });
      }
    } catch (err) {
      setSaveNotice({
        kind: "error",
        text: err instanceof Error ? err.message : "保存配置失败",
      });
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async () => {
    if (!current || !onTestConnection) return;
    setTesting(true);
    setTestResult(null);
    try {
      const result = await onTestConnection({
        profileId: current.id,
        provider: current.provider,
        model: current.model || undefined,
        baseUrl: current.baseUrl || undefined,
        ...(current.apiKey?.trim() ? { apiKey: current.apiKey.trim() } : {}),
        ...(current.clearApiKey ? { clearApiKey: true } : {}),
      });
      setTestResult(result);
    } catch (err) {
      setTestResult({
        ok: false,
        provider: current.provider,
        message: err instanceof Error ? err.message : "连通性测试请求失败",
      });
    } finally {
      setTesting(false);
    }
  };

  const handleReset = async () => {
    setSaving(true);
    try {
      await onResetAll();
      setSaveNotice({ kind: "ok", text: "已重置为默认配置" });
    } finally {
      setSaving(false);
    }
  };

  const hasEnvKeyForCurrent =
    current &&
    ((current.provider === "gemini" && Boolean(envStatus?.has_gemini_key)) ||
      (current.provider === "openai" && Boolean(envStatus?.has_openai_key)) ||
      (current.provider === "auto" && Boolean(envStatus?.has_openai_key || envStatus?.has_gemini_key)));

  const keySourceBadge = !current
    ? null
    : current.provider === "template"
      ? { tone: "muted", text: "无需密钥" }
      : current.apiKey?.trim()
        ? { tone: "warn", text: "待保存新密钥" }
        : current.hasSavedKey && !current.clearApiKey
          ? { tone: "ok", text: `已保存 ${current.savedKeyMask || "••••"}` }
          : hasEnvKeyForCurrent
            ? { tone: "ok", text: "使用服务端环境变量" }
            : !presetInfo.requiresApiKey
              ? { tone: "muted", text: "本地端点可选" }
              : { tone: "warn", text: "未配置密钥" };

  return createPortal(
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="AI 模型与密钥配置"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="ai-settings-modal">
        <div className="modal-header ai-settings-header">
          <div className="ai-settings-title-group">
            <CadIcon name="settings" size={16} />
            <strong>AI 模型与密钥配置</strong>
            <span className={`mini-badge${storageKind === "server-file" ? " is-ok" : ""}`}>
              {storageKind === "server-file"
                ? "本机安全存储 (~/.aicad-studio)"
                : "浏览器本地记忆"}
            </span>
          </div>
          <button type="button" className="icon-ghost-btn" onClick={onClose} aria-label="关闭设置">
            <CadIcon name="close" size={15} />
          </button>
        </div>

        <div className="ai-settings-body">
          <aside className="ai-settings-sidebar" aria-label="模型配置方案列表">
            <div className="ai-settings-sidebar-top">
              <span className="ai-settings-section-label">模型方案 ({drafts.length})</span>
              <button
                type="button"
                className="studio-btn studio-btn-sm studio-btn-primary"
                onClick={() => setShowNewPresetMenu((v) => !v)}
              >
                + 新建
              </button>
            </div>

            {showNewPresetMenu && (
              <div className="ai-preset-picker" role="menu" aria-label="选择厂商预设">
                {AI_PROVIDER_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    role="menuitem"
                    className="ai-preset-picker-item"
                    onClick={() => handleAddProfile(preset.id)}
                  >
                    <strong>{preset.label}</strong>
                    <span>{preset.description}</span>
                  </button>
                ))}
              </div>
            )}

            <div className="ai-profile-list">
              {drafts.map((item) => {
                const isSelected = current?.id === item.id;
                const isActive = activeIdDraft === item.id;
                const hasKey = Boolean(item.apiKey?.trim()) || (item.hasSavedKey && !item.clearApiKey);
                return (
                  <div
                    key={item.id}
                    className={`ai-profile-item${isSelected ? " is-selected" : ""}`}
                    onClick={() => {
                      setSelectedId(item.id);
                      setShowKey(false);
                      setTestResult(null);
                    }}
                  >
                    <div className="ai-profile-item-main">
                      <span
                        className={`ai-status-dot ${
                          item.provider === "template"
                            ? "is-muted"
                            : hasKey || item.preset === "ollama"
                              ? "is-ok"
                              : "is-warn"
                        }`}
                      />
                      <div className="ai-profile-item-text">
                        <strong>{item.name || "未命名配置"}</strong>
                        <span>{item.model || item.provider}</span>
                      </div>
                    </div>
                    {isActive && <span className="ai-active-pill">使用中</span>}
                  </div>
                );
              })}
            </div>

            <div className="ai-settings-sidebar-footer">
              <button
                type="button"
                className="studio-btn studio-btn-sm"
                onClick={() => void handleReset()}
                disabled={saving}
                title="清空所有已保存的模型配置与密钥"
              >
                <CadIcon name="refresh" size={12} />
                <span>重置默认</span>
              </button>
            </div>
          </aside>

          <div className="ai-settings-editor">
            {current ? (
              <>
                <div className="ai-settings-editor-head">
                  <div className="ai-settings-editor-title">
                    <input
                      type="text"
                      className="studio-input ai-profile-name-input"
                      value={current.name}
                      onChange={(e) => updateCurrent({ name: e.target.value })}
                      placeholder="配置名称，如：Gemini 主力"
                      aria-label="配置名称"
                    />
                  </div>
                  <div className="ai-settings-editor-actions">
                    {activeIdDraft !== current.id ? (
                      <button
                        type="button"
                        className="studio-btn studio-btn-sm"
                        onClick={() => {
                          setActiveIdDraft(current.id);
                          setDirty(true);
                        }}
                      >
                        设为当前使用
                      </button>
                    ) : (
                      <span className="mini-badge is-ok">当前默认使用</span>
                    )}
                    {drafts.length > 1 && (
                      <button
                        type="button"
                        className="icon-ghost-btn"
                        onClick={() => handleDeleteProfile(current.id)}
                        title="删除此配置"
                        aria-label="删除此配置"
                      >
                        <CadIcon name="trash" size={14} />
                      </button>
                    )}
                  </div>
                </div>

                <div className="ai-settings-grid">
                  <label className="field-label">
                    <span>快速厂商预设</span>
                    <select
                      className="studio-select"
                      value={current.preset}
                      onChange={(e) => handleApplyPresetToCurrent(e.target.value as AiPresetId)}
                      aria-label="厂商预设"
                    >
                      {AI_PROVIDER_PRESETS.map((preset) => (
                        <option key={preset.id} value={preset.id}>
                          {preset.label}
                        </option>
                      ))}
                    </select>
                  </label>

                  <label className="field-label">
                    <span>协议类型 (Provider)</span>
                    <select
                      className="studio-select"
                      value={current.provider}
                      onChange={(e) => updateCurrent({ provider: e.target.value as AiProvider })}
                      aria-label="AI Provider"
                    >
                      <option value="openai">OpenAI 兼容 (openai)</option>
                      <option value="gemini">Google Gemini (gemini)</option>
                      <option value="auto">自动识别 (auto)</option>
                      <option value="template">离线模板 (template)</option>
                    </select>
                  </label>
                </div>

                {current.provider !== "template" && (
                  <>
                    <div className="field-label">
                      <span>模型名称 (Model)</span>
                      <input
                        type="text"
                        className="studio-input"
                        value={current.model}
                        onChange={(e) => updateCurrent({ model: e.target.value })}
                        placeholder={
                          current.provider === "gemini"
                            ? envStatus?.gemini_model || "gemini-2.5-flash"
                            : envStatus?.openai_model || "gpt-4o-mini"
                        }
                        aria-label="AI Model"
                      />
                      {presetInfo.suggestedModels.length > 0 && (
                        <div className="ai-chip-row">
                          {presetInfo.suggestedModels.map((modelName) => (
                            <button
                              key={modelName}
                              type="button"
                              className={`ai-mini-chip${current.model === modelName ? " is-active" : ""}`}
                              onClick={() => updateCurrent({ model: modelName })}
                            >
                              {modelName}
                            </button>
                          ))}
                        </div>
                      )}
                    </div>

                    {current.provider !== "gemini" && (
                      <label className="field-label">
                        <span>API Base URL</span>
                        <input
                          type="url"
                          className="studio-input"
                          value={current.baseUrl}
                          onChange={(e) => updateCurrent({ baseUrl: e.target.value })}
                          placeholder={presetInfo.defaultBaseUrl || "https://api.openai.com/v1"}
                          aria-label="AI Base URL"
                        />
                      </label>
                    )}

                    <div className="field-label">
                      <div className="ai-field-header">
                        <span>API Key</span>
                        {keySourceBadge && (
                          <span className={`mini-badge${keySourceBadge.tone === "ok" ? " is-ok" : ""}`}>
                            {keySourceBadge.text}
                          </span>
                        )}
                      </div>
                      <div className="ai-key-input-row">
                        <input
                          type={showKey ? "text" : "password"}
                          className="studio-input"
                          autoComplete="off"
                          value={current.apiKey ?? ""}
                          onChange={(e) =>
                            updateCurrent({ apiKey: e.target.value, clearApiKey: false })
                          }
                          placeholder={
                            current.hasSavedKey && !current.clearApiKey
                              ? `已安全保存 (${current.savedKeyMask})，输入新密钥可覆盖`
                              : presetInfo.keyPlaceholder
                          }
                          aria-label="AI API Key"
                        />
                        <button
                          type="button"
                          className="studio-btn studio-btn-sm"
                          onClick={() => setShowKey((v) => !v)}
                          title={showKey ? "隐藏密钥" : "显示密钥"}
                          aria-label={showKey ? "隐藏密钥" : "显示密钥"}
                        >
                          <CadIcon name={showKey ? "eyeOff" : "eye"} size={14} />
                        </button>
                        {(current.hasSavedKey || current.apiKey) && !current.clearApiKey && (
                          <button
                            type="button"
                            className="studio-btn studio-btn-sm"
                            onClick={() => updateCurrent({ apiKey: "", clearApiKey: true })}
                            title="清除已保存的 API Key"
                          >
                            清除
                          </button>
                        )}
                      </div>
                    </div>
                  </>
                )}

                <div className="ai-settings-divider" />

                <div className="ai-settings-grid">
                  <label className="field-label">
                    <span>Few-shot 黄金样本数</span>
                    <select
                      className="studio-select"
                      value={current.fewShotK}
                      onChange={(e) => updateCurrent({ fewShotK: Number(e.target.value) })}
                      aria-label="Few-shot 参考样本数"
                    >
                      <option value={0}>0（不使用）</option>
                      <option value={1}>1</option>
                      <option value={2}>2（推荐）</option>
                      <option value={3}>3</option>
                    </select>
                  </label>

                  <div className="field-label ai-checkbox-field">
                    <span>视觉几何门禁 (VLM)</span>
                    <label className="studio-checkbox">
                      <input
                        type="checkbox"
                        checked={current.enableVlm}
                        onChange={(e) => updateCurrent({ enableVlm: e.target.checked })}
                        aria-label="启用 VLM 四视图审查"
                      />
                      <span>启用四视图 VLM 视觉审查</span>
                    </label>
                  </div>
                </div>

                {testResult && (
                  <div
                    className={`status-callout ai-test-banner ${testResult.ok ? "is-ok" : "is-error"}`}
                    role="status"
                  >
                    <CadIcon name={testResult.ok ? "check" : "error"} size={14} />
                    <span>{testResult.message}</span>
                  </div>
                )}
              </>
            ) : (
              <div className="ai-settings-empty">请点击左侧「+ 新建」添加一个模型配置。</div>
            )}
          </div>
        </div>

        <div className="ai-settings-footer">
          <div className="ai-settings-footer-left">
            {onTestConnection && current && (
              <button
                type="button"
                className="studio-btn"
                disabled={testing || saving}
                onClick={() => void handleTest()}
              >
                {testing ? <CadIcon name="spinner" size={14} /> : <CadIcon name="shieldCheck" size={14} />}
                <span>{testing ? "正在测试…" : "测试连接"}</span>
              </button>
            )}
            {saveNotice && (
              <span className={`ai-save-notice ${saveNotice.kind === "ok" ? "is-ok" : "is-error"}`}>
                <CadIcon name={saveNotice.kind === "ok" ? "check" : "error"} size={13} />
                <span>{saveNotice.text}</span>
              </span>
            )}
          </div>
          <div className="ai-settings-footer-right">
            <button type="button" className="studio-btn" onClick={onClose}>
              关闭
            </button>
            <button
              type="button"
              className="studio-btn studio-btn-primary"
              disabled={saving || drafts.length === 0}
              onClick={() => void handleSave()}
            >
              {saving ? (
                <>
                  <CadIcon name="spinner" size={14} />
                  <span>保存中…</span>
                </>
              ) : (
                <>
                  <CadIcon name="check" size={14} />
                  <span>
                    {drafts.length === 0
                      ? "无方案可保存"
                      : dirty || profiles.length === 0
                        ? "保存配置"
                        : "已保存"}
                  </span>
                </>
              )}
            </button>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
