import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

const VALID_PROVIDERS = new Set(["auto", "template", "openai", "gemini"]);
const VALID_PRESETS = new Set([
  "openai",
  "gemini",
  "deepseek",
  "openrouter",
  "ollama",
  "custom",
  "template",
]);

export function getAiConfigFilePath(env = process.env) {
  const override = (env.AICAD_STUDIO_AI_CONFIG_PATH || "").trim();
  if (override) {
    return path.resolve(override);
  }
  return path.join(os.homedir(), ".aicad-studio", "ai-config.json");
}

export function maskApiKey(apiKey) {
  const raw = typeof apiKey === "string" ? apiKey.trim() : "";
  if (!raw) return "";
  if (raw.length <= 8) return "••••••••";
  return `${raw.slice(0, 4)}••••${raw.slice(-4)}`;
}

function sanitizeSecretText(text, secrets = []) {
  if (typeof text !== "string" || !text) return "";
  let out = text;
  for (const secret of secrets) {
    if (typeof secret === "string" && secret.trim()) {
      out = out.split(secret.trim()).join("***");
    }
  }
  out = out.replace(/sk-[A-Za-z0-9_\-]{8,}/g, "***");
  out = out.replace(/AIza[0-9A-Za-z_\-]{16,}/g, "***");
  out = out.replace(/(Bearer\s+)[A-Za-z0-9._\-]{8,}/gi, "$1***");
  return out;
}

function normalizeProvider(value) {
  return typeof value === "string" && VALID_PROVIDERS.has(value) ? value : "auto";
}

function normalizePreset(value, provider) {
  if (typeof value === "string" && VALID_PRESETS.has(value)) return value;
  if (provider === "gemini") return "gemini";
  if (provider === "template") return "template";
  if (provider === "openai") return "openai";
  return "custom";
}

function normalizeFewShotK(value) {
  if (typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 5) {
    return value;
  }
  return 2;
}

function normalizeProfileRecord(raw, existing = null) {
  const id =
    typeof raw?.id === "string" && raw.id.trim()
      ? raw.id.trim().slice(0, 64)
      : existing?.id || `prof_${Date.now().toString(36)}`;
  const provider = normalizeProvider(raw?.provider ?? existing?.provider);
  const preset = normalizePreset(raw?.preset ?? existing?.preset, provider);
  const name =
    typeof raw?.name === "string" && raw.name.trim()
      ? raw.name.trim().slice(0, 80)
      : existing?.name || "未命名模型配置";
  const model =
    typeof raw?.model === "string" ? raw.model.trim().slice(0, 120) : (existing?.model ?? "");
  const baseUrl =
    typeof raw?.baseUrl === "string"
      ? raw.baseUrl.trim().slice(0, 300)
      : (existing?.baseUrl ?? "");
  const enableVlm =
    typeof raw?.enableVlm === "boolean" ? raw.enableVlm : Boolean(existing?.enableVlm);
  const fewShotK = normalizeFewShotK(
    raw?.fewShotK !== undefined ? raw.fewShotK : existing?.fewShotK,
  );

  let apiKey = existing?.apiKey ?? "";
  if (raw?.clearApiKey === true) {
    apiKey = "";
  } else if (typeof raw?.apiKey === "string") {
    const trimmed = raw.apiKey.trim();
    if (trimmed) {
      apiKey = trimmed;
    } else if (raw?.clearApiKey !== false && raw?.apiKey === "") {
      // Explicit empty string without preserving flag only clears if clearApiKey is set;
      // otherwise keep existing key so editing model/name does not accidentally wipe saved key.
    }
  }

  return {
    id,
    name,
    preset,
    provider,
    model,
    baseUrl,
    apiKey,
    enableVlm,
    fewShotK,
    updatedAt:
      typeof raw?.updatedAt === "string" && raw.updatedAt
        ? raw.updatedAt
        : new Date().toISOString(),
  };
}

export function sanitizeProfileForClient(profile) {
  const rawKey = typeof profile.apiKey === "string" ? profile.apiKey.trim() : "";
  return {
    id: profile.id,
    name: profile.name,
    preset: profile.preset,
    provider: profile.provider,
    model: profile.model,
    baseUrl: profile.baseUrl,
    hasApiKey: Boolean(rawKey),
    apiKeyMask: maskApiKey(rawKey),
    enableVlm: Boolean(profile.enableVlm),
    fewShotK: normalizeFewShotK(profile.fewShotK),
    updatedAt: profile.updatedAt,
  };
}

export async function readRawAiProfileStore(env = process.env) {
  const filePath = getAiConfigFilePath(env);
  try {
    const rawText = await fs.readFile(filePath, "utf8");
    const parsed = JSON.parse(rawText);
    const profiles = Array.isArray(parsed?.profiles)
      ? parsed.profiles
          .filter((item) => item && typeof item === "object")
          .map((item) => normalizeProfileRecord(item))
      : [];
    const activeProfileId =
      typeof parsed?.activeProfileId === "string" &&
      profiles.some((item) => item.id === parsed.activeProfileId)
        ? parsed.activeProfileId
        : (profiles[0]?.id ?? null);
    return { version: 1, activeProfileId, profiles };
  } catch {
    return { version: 1, activeProfileId: null, profiles: [] };
  }
}

export async function loadClientAiProfileStore(env = process.env) {
  const raw = await readRawAiProfileStore(env);
  return {
    storage: "server-file",
    activeProfileId: raw.activeProfileId,
    profiles: raw.profiles.map(sanitizeProfileForClient),
  };
}

export async function saveAiProfileStore(payload, env = process.env) {
  const existingStore = await readRawAiProfileStore(env);
  const existingById = new Map(existingStore.profiles.map((p) => [p.id, p]));

  const incomingProfiles = Array.isArray(payload?.profiles) ? payload.profiles : [];
  const nextProfiles = incomingProfiles
    .filter((item) => item && typeof item === "object")
    .slice(0, 30)
    .map((item) => {
      const id = typeof item.id === "string" ? item.id.trim() : "";
      const prev = id ? existingById.get(id) ?? null : null;
      return normalizeProfileRecord(item, prev);
    });

  const requestedActiveId =
    typeof payload?.activeProfileId === "string" ? payload.activeProfileId.trim() : "";
  const activeProfileId = nextProfiles.some((p) => p.id === requestedActiveId)
    ? requestedActiveId
    : (nextProfiles[0]?.id ?? null);

  const storeData = {
    version: 1,
    activeProfileId,
    profiles: nextProfiles,
  };

  const filePath = getAiConfigFilePath(env);
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  await fs.writeFile(filePath, `${JSON.stringify(storeData, null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600,
  });
  try {
    await fs.chmod(filePath, 0o600);
  } catch {
    // Windows filesystems may ignore POSIX chmod bits.
  }

  return {
    storage: "server-file",
    activeProfileId: storeData.activeProfileId,
    profiles: storeData.profiles.map(sanitizeProfileForClient),
  };
}

export async function resolveLlmWithSavedProfile(llm, env = process.env) {
  const input = llm && typeof llm === "object" ? { ...llm } : {};
  const profileId =
    typeof input.profile_id === "string" && input.profile_id.trim()
      ? input.profile_id.trim()
      : "";
  if (!profileId) {
    return input;
  }

  const store = await readRawAiProfileStore(env);
  const profile = store.profiles.find((item) => item.id === profileId);
  if (!profile) {
    return input;
  }

  return {
    provider: input.provider && input.provider !== "auto" ? input.provider : profile.provider,
    model:
      typeof input.model === "string" && input.model.trim() ? input.model.trim() : profile.model || undefined,
    base_url:
      typeof input.base_url === "string" && input.base_url.trim()
        ? input.base_url.trim()
        : profile.baseUrl || undefined,
    api_key:
      typeof input.api_key === "string" && input.api_key.trim()
        ? input.api_key.trim()
        : profile.apiKey || undefined,
    enable_vlm: input.enable_vlm !== undefined ? Boolean(input.enable_vlm) : Boolean(profile.enableVlm),
    few_shot_k:
      typeof input.few_shot_k === "number" ? input.few_shot_k : normalizeFewShotK(profile.fewShotK),
    profile_id: profile.id,
  };
}

function isLocalEndpoint(baseUrl) {
  try {
    const host = new URL(baseUrl).hostname.toLowerCase();
    return host === "localhost" || host === "127.0.0.1" || host === "::1" || host.endsWith(".local");
  } catch {
    return false;
  }
}

export async function testAiProfileConnection(payload, env = process.env, fetcher = fetch) {
  const startedAt = Date.now();
  const profileId = typeof payload?.profileId === "string" ? payload.profileId.trim() : "";
  let savedProfile = null;
  if (profileId) {
    const store = await readRawAiProfileStore(env);
    savedProfile = store.profiles.find((item) => item.id === profileId) ?? null;
  }

  let provider = normalizeProvider(payload?.provider ?? savedProfile?.provider ?? "auto");
  const modelInput =
    typeof payload?.model === "string" ? payload.model.trim() : (savedProfile?.model ?? "");
  const baseUrlInput =
    typeof payload?.baseUrl === "string" ? payload.baseUrl.trim() : (savedProfile?.baseUrl ?? "");
  const clearApiKey = payload?.clearApiKey === true;
  const directKey = typeof payload?.apiKey === "string" ? payload.apiKey.trim() : "";
  let apiKey = clearApiKey ? "" : directKey || savedProfile?.apiKey || "";

  if (provider === "auto") {
    if (baseUrlInput && /generativelanguage\.googleapis\.com/i.test(baseUrlInput)) {
      provider = "gemini";
    } else if (/gemini/i.test(modelInput) || /^AIza/.test(apiKey)) {
      provider = "gemini";
    } else if (apiKey || baseUrlInput) {
      provider = "openai";
    } else if ((env.OPENAI_API_KEY || "").trim()) {
      provider = "openai";
    } else if ((env.GEMINI_API_KEY || "").trim()) {
      provider = "gemini";
    } else {
      provider = "template";
    }
  }

  if (provider === "template") {
    return {
      ok: true,
      provider: "template",
      model: "offline-template",
      latencyMs: Date.now() - startedAt,
      message: "离线模板引擎就绪（无需 API Key 或网络连接）",
    };
  }

  if (!apiKey) {
    apiKey =
      provider === "gemini"
        ? (env.GEMINI_API_KEY || "").trim()
        : (env.OPENAI_API_KEY || "").trim();
  }

  const secrets = [apiKey, env.OPENAI_API_KEY, env.GEMINI_API_KEY];
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 12000);

  try {
    if (provider === "gemini") {
      const model = modelInput || (env.GEMINI_MODEL || "").trim() || "gemini-2.5-flash";
      if (!apiKey) {
        return {
          ok: false,
          provider: "gemini",
          model,
          latencyMs: Date.now() - startedAt,
          message: "未提供 Gemini API Key，请填写密钥或配置服务端 GEMINI_API_KEY。",
        };
      }
      const endpoint = `https://generativelanguage.googleapis.com/v1beta/models/${encodeURIComponent(model)}`;
      const resp = await fetcher(endpoint, {
        method: "GET",
        headers: { "x-goog-api-key": apiKey },
        signal: controller.signal,
      });
      const latencyMs = Date.now() - startedAt;
      if (resp.ok) {
        return {
          ok: true,
          provider: "gemini",
          model,
          status: resp.status,
          latencyMs,
          message: `连接成功 · Gemini (${model}) · ${latencyMs}ms`,
        };
      }
      const errText = sanitizeSecretText(await resp.text().catch(() => ""), secrets).slice(0, 160);
      return {
        ok: false,
        provider: "gemini",
        model,
        status: resp.status,
        latencyMs,
        message:
          resp.status === 401 || resp.status === 403
            ? `认证失败 (HTTP ${resp.status})：请检查 Gemini API Key 是否有效。`
            : resp.status === 404
              ? `模型未找到 (HTTP 404)：请检查模型名称「${model}」。`
              : `Gemini 探测失败 (HTTP ${resp.status})${errText ? `：${errText}` : ""}`,
      };
    }

    const baseUrl = (
      baseUrlInput ||
      (env.OPENAI_BASE_URL || "").trim() ||
      "https://api.openai.com/v1"
    ).replace(/\/+$/, "");
    const model = modelInput || (env.OPENAI_MODEL || "").trim() || "gpt-4o-mini";
    if (!apiKey && !isLocalEndpoint(baseUrl)) {
      return {
        ok: false,
        provider: "openai",
        model,
        latencyMs: Date.now() - startedAt,
        message: "未提供 API Key（非本地端点需要填写 API Key 或配置 OPENAI_API_KEY）。",
      };
    }

    const headers = { Accept: "application/json" };
    if (apiKey) {
      headers.Authorization = `Bearer ${apiKey}`;
    }
    const resp = await fetcher(`${baseUrl}/models`, {
      method: "GET",
      headers,
      signal: controller.signal,
    });
    const latencyMs = Date.now() - startedAt;
    if (resp.ok) {
      return {
        ok: true,
        provider: "openai",
        model,
        status: resp.status,
        latencyMs,
        message: `连接成功 · ${model} · ${latencyMs}ms`,
      };
    }
    const errText = sanitizeSecretText(await resp.text().catch(() => ""), secrets).slice(0, 160);
    return {
      ok: false,
      provider: "openai",
      model,
      status: resp.status,
      latencyMs,
      message:
        resp.status === 401 || resp.status === 403
          ? `认证失败 (HTTP ${resp.status})：请检查 API Key 权限或余额。`
          : `端点响应异常 (HTTP ${resp.status})${errText ? `：${errText}` : ""}`,
    };
  } catch (err) {
    const latencyMs = Date.now() - startedAt;
    const isTimeout = err?.name === "AbortError";
    const detail = sanitizeSecretText(err instanceof Error ? err.message : String(err), secrets);
    return {
      ok: false,
      provider,
      model: modelInput || undefined,
      latencyMs,
      message: isTimeout ? "连接超时（>12s），请检查 Base URL 或网络代理。" : `网络请求失败：${detail}`,
    };
  } finally {
    clearTimeout(timer);
  }
}
