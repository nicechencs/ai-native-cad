import type {
  AiConfigStatus,
  AiLlmConfig,
  AiPresetId,
  AiProfile,
  AiProfileDraftInput,
  AiProvider,
} from "../../types/scene.ts";

export interface StoredAiConfig {
  readonly provider: AiProvider;
  readonly model: string;
  readonly baseUrl: string;
  readonly apiKey: string;
  readonly enableVlm: boolean;
  readonly fewShotK: number;
  readonly material: string;
  readonly method: string;
}

const STORAGE_KEY = "ai_native_cad_studio_ai_config_v1";
const PROFILES_STORAGE_KEY = "ai_native_cad_studio_ai_profiles_v2";

export const DEFAULT_AI_CONFIG: StoredAiConfig = {
  provider: "auto",
  model: "",
  baseUrl: "",
  apiKey: "",
  enableVlm: false,
  fewShotK: 2,
  material: "Al-6061-T6",
  method: "CNC_3AXIS_MILLING",
};

export interface AiProviderPreset {
  readonly id: AiPresetId;
  readonly label: string;
  readonly description: string;
  readonly provider: AiProvider;
  readonly defaultBaseUrl: string;
  readonly defaultModel: string;
  readonly suggestedModels: readonly string[];
  readonly requiresApiKey: boolean;
  readonly keyPlaceholder: string;
}

export const AI_PROVIDER_PRESETS: readonly AiProviderPreset[] = [
  {
    id: "openai",
    label: "OpenAI",
    description: "GPT-4o / o3 等官方 OpenAI 接口",
    provider: "openai",
    defaultBaseUrl: "https://api.openai.com/v1",
    defaultModel: "gpt-4o-mini",
    suggestedModels: ["gpt-4o-mini", "gpt-4o", "gpt-4.1", "o3-mini"],
    requiresApiKey: true,
    keyPlaceholder: "sk-...",
  },
  {
    id: "gemini",
    label: "Google Gemini",
    description: "Gemini 2.5 Flash / Pro 原生多模态接口",
    provider: "gemini",
    defaultBaseUrl: "",
    defaultModel: "gemini-2.5-flash",
    suggestedModels: ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.0-flash"],
    requiresApiKey: true,
    keyPlaceholder: "AIza...",
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    description: "DeepSeek Chat / Reasoner (OpenAI 兼容)",
    provider: "openai",
    defaultBaseUrl: "https://api.deepseek.com/v1",
    defaultModel: "deepseek-chat",
    suggestedModels: ["deepseek-chat", "deepseek-reasoner"],
    requiresApiKey: true,
    keyPlaceholder: "sk-...",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    description: "聚合多家旗舰模型的 OpenAI 兼容网关",
    provider: "openai",
    defaultBaseUrl: "https://openrouter.ai/api/v1",
    defaultModel: "anthropic/claude-sonnet-4",
    suggestedModels: ["anthropic/claude-sonnet-4", "openai/gpt-4o", "google/gemini-2.5-pro"],
    requiresApiKey: true,
    keyPlaceholder: "sk-or-...",
  },
  {
    id: "ollama",
    label: "本地 Ollama / vLLM",
    description: "本机或局域网部署的开源模型（通常无需 Key）",
    provider: "openai",
    defaultBaseUrl: "http://127.0.0.1:11434/v1",
    defaultModel: "qwen2.5-coder:14b",
    suggestedModels: ["qwen2.5-coder:14b", "qwen2.5-coder:32b", "deepseek-r1:14b"],
    requiresApiKey: false,
    keyPlaceholder: "本地模型通常可留空",
  },
  {
    id: "custom",
    label: "自定义 OpenAI 兼容",
    description: "企业内部代理、私有网关或其他兼容端点",
    provider: "openai",
    defaultBaseUrl: "",
    defaultModel: "",
    suggestedModels: [],
    requiresApiKey: true,
    keyPlaceholder: "输入代理 API Key 或留空使用环境变量",
  },
  {
    id: "template",
    label: "离线模板引擎",
    description: "不调用外部大模型，纯本地参数化模板生成",
    provider: "template",
    defaultBaseUrl: "",
    defaultModel: "",
    suggestedModels: [],
    requiresApiKey: false,
    keyPlaceholder: "离线模式无需密钥",
  },
];

export function getPresetById(id: AiPresetId): AiProviderPreset {
  return AI_PROVIDER_PRESETS.find((item) => item.id === id) ?? AI_PROVIDER_PRESETS[0];
}

export function maskApiKey(apiKey: string): string {
  const raw = apiKey.trim();
  if (!raw) return "";
  if (raw.length <= 8) return "••••••••";
  return `${raw.slice(0, 4)}••••${raw.slice(-4)}`;
}

function normalizeProvider(value: unknown): AiProvider {
  if (value === "template" || value === "openai" || value === "gemini" || value === "auto") {
    return value;
  }
  return DEFAULT_AI_CONFIG.provider;
}

function normalizePreset(value: unknown, provider: AiProvider): AiPresetId {
  if (
    value === "openai" ||
    value === "gemini" ||
    value === "deepseek" ||
    value === "openrouter" ||
    value === "ollama" ||
    value === "custom" ||
    value === "template"
  ) {
    return value;
  }
  if (provider === "gemini") return "gemini";
  if (provider === "template") return "template";
  if (provider === "openai") return "openai";
  return "custom";
}

function normalizeFewShotK(value: unknown): number {
  if (typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 3) {
    return value;
  }
  return DEFAULT_AI_CONFIG.fewShotK;
}

export function loadAiConfig(storage?: Pick<Storage, "getItem"> | null): StoredAiConfig {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) {
    return DEFAULT_AI_CONFIG;
  }
  try {
    const raw = target.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_AI_CONFIG;
    const parsed = JSON.parse(raw) as Partial<StoredAiConfig>;
    return {
      provider: normalizeProvider(parsed.provider),
      model: typeof parsed.model === "string" ? parsed.model : DEFAULT_AI_CONFIG.model,
      baseUrl: typeof parsed.baseUrl === "string" ? parsed.baseUrl : DEFAULT_AI_CONFIG.baseUrl,
      apiKey: typeof parsed.apiKey === "string" ? parsed.apiKey : DEFAULT_AI_CONFIG.apiKey,
      enableVlm: typeof parsed.enableVlm === "boolean" ? parsed.enableVlm : DEFAULT_AI_CONFIG.enableVlm,
      fewShotK: normalizeFewShotK(parsed.fewShotK),
      material: typeof parsed.material === "string" && parsed.material ? parsed.material : DEFAULT_AI_CONFIG.material,
      method: typeof parsed.method === "string" && parsed.method ? parsed.method : DEFAULT_AI_CONFIG.method,
    };
  } catch {
    return DEFAULT_AI_CONFIG;
  }
}

export function saveAiConfig(
  config: StoredAiConfig,
  storage?: Pick<Storage, "setItem"> | null,
): void {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return;
  try {
    target.setItem(STORAGE_KEY, JSON.stringify(config));
  } catch {
    // Ignore storage quota or private-mode errors.
  }
}

export function clearAiConfig(storage?: Pick<Storage, "removeItem"> | null): void {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return;
  try {
    target.removeItem(STORAGE_KEY);
  } catch {
    // Ignore storage errors.
  }
}

export interface LocalAiProfilesState {
  readonly activeProfileId: string | null;
  readonly profiles: readonly AiProfile[];
}

export function createProfileFromPreset(presetId: AiPresetId, existingNames: readonly string[] = []): AiProfile {
  const preset = getPresetById(presetId);
  let candidateName = preset.label;
  let suffix = 2;
  while (existingNames.includes(candidateName)) {
    candidateName = `${preset.label} ${suffix++}`;
  }
  return {
    id: `prof_${presetId}_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 6)}`,
    name: candidateName,
    preset: preset.id,
    provider: preset.provider,
    model: preset.defaultModel,
    baseUrl: preset.defaultBaseUrl,
    hasApiKey: false,
    apiKeyMask: "",
    enableVlm: false,
    fewShotK: 2,
    updatedAt: new Date().toISOString(),
  };
}

function normalizeStoredProfile(raw: unknown): AiProfile | null {
  if (!raw || typeof raw !== "object") return null;
  const obj = raw as Record<string, unknown>;
  const id = typeof obj.id === "string" && obj.id.trim() ? obj.id.trim() : "";
  if (!id) return null;
  const provider = normalizeProvider(obj.provider);
  const preset = normalizePreset(obj.preset, provider);
  const localApiKey = typeof obj.localApiKey === "string" ? obj.localApiKey.trim() : "";
  const hasApiKey = typeof obj.hasApiKey === "boolean" ? obj.hasApiKey : Boolean(localApiKey);
  const apiKeyMask =
    typeof obj.apiKeyMask === "string" && obj.apiKeyMask ? obj.apiKeyMask : maskApiKey(localApiKey);
  return {
    id,
    name: typeof obj.name === "string" && obj.name.trim() ? obj.name.trim() : getPresetById(preset).label,
    preset,
    provider,
    model: typeof obj.model === "string" ? obj.model.trim() : "",
    baseUrl: typeof obj.baseUrl === "string" ? obj.baseUrl.trim() : "",
    hasApiKey,
    apiKeyMask,
    ...(localApiKey ? { localApiKey } : {}),
    enableVlm: typeof obj.enableVlm === "boolean" ? obj.enableVlm : false,
    fewShotK: normalizeFewShotK(obj.fewShotK),
    ...(typeof obj.updatedAt === "string" ? { updatedAt: obj.updatedAt } : {}),
  };
}

export function loadLocalAiProfiles(storage?: Pick<Storage, "getItem"> | null): LocalAiProfilesState {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return { activeProfileId: null, profiles: [] };
  try {
    const raw = target.getItem(PROFILES_STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<LocalAiProfilesState>;
      const profiles = Array.isArray(parsed.profiles)
        ? parsed.profiles.map(normalizeStoredProfile).filter((p): p is AiProfile => p !== null)
        : [];
      const activeProfileId =
        typeof parsed.activeProfileId === "string" && profiles.some((p) => p.id === parsed.activeProfileId)
          ? parsed.activeProfileId
          : (profiles[0]?.id ?? null);
      return { activeProfileId, profiles };
    }
    // Migrate legacy v1 config if the user had configured an API key / custom model previously.
    const legacy = loadAiConfig(target);
    if (legacy.apiKey || legacy.baseUrl || legacy.model || legacy.provider !== "auto") {
      const presetId: AiPresetId =
        legacy.provider === "gemini"
          ? "gemini"
          : legacy.provider === "template"
            ? "template"
            : legacy.baseUrl.includes("deepseek")
              ? "deepseek"
              : legacy.baseUrl.includes("11434")
                ? "ollama"
                : "openai";
      const migrated: AiProfile = {
        id: "prof_migrated_v1",
        name: `${getPresetById(presetId).label} (已迁移)`,
        preset: presetId,
        provider: legacy.provider === "auto" ? (legacy.apiKey.startsWith("AIza") ? "gemini" : "openai") : legacy.provider,
        model: legacy.model,
        baseUrl: legacy.baseUrl,
        hasApiKey: Boolean(legacy.apiKey.trim()),
        apiKeyMask: maskApiKey(legacy.apiKey),
        ...(legacy.apiKey.trim() ? { localApiKey: legacy.apiKey.trim() } : {}),
        enableVlm: legacy.enableVlm,
        fewShotK: legacy.fewShotK,
        updatedAt: new Date().toISOString(),
      };
      return { activeProfileId: migrated.id, profiles: [migrated] };
    }
    return { activeProfileId: null, profiles: [] };
  } catch {
    return { activeProfileId: null, profiles: [] };
  }
}

export function saveLocalAiProfiles(
  state: { readonly activeProfileId: string | null; readonly profiles: readonly AiProfileDraftInput[] },
  existing: readonly AiProfile[] = [],
  storage?: Pick<Storage, "setItem"> | null,
): LocalAiProfilesState {
  const existingById = new Map(existing.map((p) => [p.id, p]));
  const nextProfiles: AiProfile[] = state.profiles.map((item) => {
    const prev = existingById.get(item.id);
    const nextKey = item.clearApiKey
      ? ""
      : item.apiKey && item.apiKey.trim()
        ? item.apiKey.trim()
        : (prev?.localApiKey ?? "");
    return {
      id: item.id,
      name: item.name.trim() || getPresetById(item.preset).label,
      preset: item.preset,
      provider: item.provider,
      model: item.model.trim(),
      baseUrl: item.baseUrl.trim(),
      hasApiKey: Boolean(nextKey),
      apiKeyMask: maskApiKey(nextKey),
      ...(nextKey ? { localApiKey: nextKey } : {}),
      enableVlm: item.enableVlm,
      fewShotK: normalizeFewShotK(item.fewShotK),
      updatedAt: new Date().toISOString(),
    };
  });

  const activeProfileId =
    state.activeProfileId && nextProfiles.some((p) => p.id === state.activeProfileId)
      ? state.activeProfileId
      : (nextProfiles[0]?.id ?? null);

  const normalized: LocalAiProfilesState = { activeProfileId, profiles: nextProfiles };
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (target) {
    try {
      target.setItem(PROFILES_STORAGE_KEY, JSON.stringify(normalized));
    } catch {
      // Ignore quota / private mode errors.
    }
  }
  return normalized;
}

export function clearLocalAiProfiles(storage?: Pick<Storage, "removeItem"> | null): void {
  const target = storage ?? (typeof window !== "undefined" ? window.localStorage : null);
  if (!target) return;
  try {
    target.removeItem(PROFILES_STORAGE_KEY);
  } catch {
    // Ignore storage errors.
  }
}

function isLocalEndpointUrl(baseUrl: string): boolean {
  try {
    const host = new URL(baseUrl).hostname.toLowerCase();
    return host === "localhost" || host === "127.0.0.1" || host === "::1" || host.endsWith(".local");
  } catch {
    return false;
  }
}

export interface ResolvedAiStatus {
  readonly ready: boolean;
  readonly tone: "ok" | "warn" | "muted";
  readonly label: string;
  readonly sourceLabel: string;
}

export function resolveActiveAiStatus(
  activeProfile: AiProfile | null,
  envStatus: AiConfigStatus | null | undefined,
  storageKind: "server-file" | "browser-local",
): ResolvedAiStatus {
  if (activeProfile) {
    if (activeProfile.provider === "template") {
      return {
        ready: false,
        tone: "warn",
        label: "已停用离线模板 · 请配置真实 LLM",
        sourceLabel: "需配置真实 LLM",
      };
    }
    const modelLabel =
      activeProfile.model ||
      (activeProfile.provider === "gemini"
        ? envStatus?.gemini_model || "gemini-2.5-flash"
        : envStatus?.openai_model || "gpt-4o-mini");
    const isLocal = Boolean(activeProfile.baseUrl && isLocalEndpointUrl(activeProfile.baseUrl));
    const hasEnvFallback =
      (activeProfile.provider === "gemini" && Boolean(envStatus?.has_gemini_key)) ||
      (activeProfile.provider === "openai" && Boolean(envStatus?.has_openai_key)) ||
      (activeProfile.provider === "auto" && Boolean(envStatus?.has_openai_key || envStatus?.has_gemini_key));

    if (activeProfile.hasApiKey) {
      return {
        ready: true,
        tone: "ok",
        label: `${activeProfile.name} · ${modelLabel}`,
        sourceLabel: storageKind === "server-file" ? "本机安全保存" : "浏览器本地保存",
      };
    }
    if (isLocal) {
      return {
        ready: true,
        tone: "ok",
        label: `${activeProfile.name} · ${modelLabel}`,
        sourceLabel: "本地端点",
      };
    }
    if (hasEnvFallback) {
      return {
        ready: true,
        tone: "ok",
        label: `${activeProfile.name} · ${modelLabel}`,
        sourceLabel: "服务端环境变量",
      };
    }
    return {
      ready: false,
      tone: "warn",
      label: `${activeProfile.name} · 未配置密钥`,
      sourceLabel: "需填写 API Key",
    };
  }

  if (envStatus && envStatus.default_provider !== "template" && (envStatus.has_openai_key || envStatus.has_gemini_key)) {
    const model =
      envStatus.default_provider === "gemini"
        ? envStatus.gemini_model || "gemini-2.5-flash"
        : envStatus.openai_model || "gpt-4o-mini";
    return {
      ready: true,
      tone: "ok",
      label: `环境变量 · ${model}`,
      sourceLabel: "服务端环境变量",
    };
  }

  return {
    ready: false,
    tone: "muted",
    label: "未配置 LLM 模型",
    sourceLabel: "点击配置 API",
  };
}

export function buildLlmConfigFromProfile(
  activeProfile: AiProfile | null,
  storageKind: "server-file" | "browser-local",
): AiLlmConfig {
  if (!activeProfile) {
    return {
      provider: "auto",
      enable_vlm: false,
      few_shot_k: 2,
    };
  }
  return {
    provider: activeProfile.provider,
    ...(activeProfile.model ? { model: activeProfile.model } : {}),
    ...(activeProfile.baseUrl ? { base_url: activeProfile.baseUrl } : {}),
    ...(storageKind === "server-file"
      ? { profile_id: activeProfile.id }
      : activeProfile.localApiKey
        ? { api_key: activeProfile.localApiKey }
        : {}),
    enable_vlm: activeProfile.enableVlm,
    few_shot_k: activeProfile.fewShotK,
  };
}
