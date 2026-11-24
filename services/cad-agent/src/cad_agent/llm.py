"""Pluggable LLM client abstractions and factory for `cad_agent`."""

from __future__ import annotations

import http.client
import json
import os
import ssl
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Protocol

DEFAULT_OPENAI_BASE_URL = "https://api.openai.com/v1"
DEFAULT_OPENAI_MODEL = "gpt-4o-mini"
DEFAULT_GEMINI_MODEL = "gemini-2.5-flash"
VALID_PROVIDERS: tuple[str, ...] = ("auto", "template", "openai", "gemini")
_LOCAL_HOSTS: frozenset[str] = frozenset({"localhost", "127.0.0.1", "::1", "0.0.0.0"})


class LLMConfigError(ValueError):
    """Raised when LLM provider configuration is invalid or incomplete."""


class LLMRequestError(RuntimeError):
    """Raised when an LLM HTTP request fails or returns a malformed payload."""


class LLMClient(Protocol):
    """Minimal protocol for pluggable LLM backends (OpenAI, Gemini, vLLM, mock)."""

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        """Return model completion text given system and user prompts."""


@dataclass(slots=True)
class LLMConfig:
    """Configuration for constructing a pluggable `LLMClient`."""

    provider: str = "auto"
    model: str | None = None
    base_url: str | None = None
    api_key: str | None = field(default=None, repr=False)
    temperature: float = 0.1
    timeout_sec: float = 60.0


@dataclass(slots=True)
class ResolvedLLMInfo:
    """Safe, non-sensitive metadata describing the resolved LLM backend."""

    provider: str
    model: str | None = None
    base_url: str | None = None
    used_llm: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "provider": self.provider,
            "model": self.model,
            "base_url": self.base_url,
            "used_llm": self.used_llm,
        }


def _clean_opt(value: str | None) -> str | None:
    if value is None:
        return None
    stripped = value.strip()
    return stripped if stripped else None


def _is_local_base_url(base_url: str) -> bool:
    try:
        parsed = urllib.parse.urlparse(base_url)
    except ValueError:
        return False
    host = (parsed.hostname or "").strip().lower()
    if not host:
        return False
    return host in _LOCAL_HOSTS or host.endswith(".local")


def _extract_image_mime_and_base64(img: Any) -> tuple[str, str]:
    """Extract `(mime_type, data_base64)` from a dict or dataclass image block."""
    if isinstance(img, dict):
        mime_type = str(img.get("mime_type") or "image/png").strip() or "image/png"
        data_base64 = str(img.get("data_base64") or img.get("data") or "").strip()
    else:
        mime_type = str(getattr(img, "mime_type", None) or "image/png").strip() or "image/png"
        data_base64 = str(
            getattr(img, "data_base64", None) or getattr(img, "data", None) or ""
        ).strip()
    return mime_type, data_base64


class OpenAICompatibleClient:
    """Zero-dependency OpenAI / vLLM / DeepSeek REST client via `urllib.request`."""

    def __init__(
        self,
        *,
        model: str = DEFAULT_OPENAI_MODEL,
        api_key: str | None = None,
        base_url: str = DEFAULT_OPENAI_BASE_URL,
        temperature: float = 0.1,
        timeout_sec: float = 60.0,
    ) -> None:
        self.model = model
        self.api_key = (
            api_key if api_key is not None else os.environ.get("OPENAI_API_KEY", "")
        )
        self.base_url = base_url.rstrip("/")
        self.temperature = temperature
        self.timeout_sec = timeout_sec

    def __repr__(self) -> str:
        return (
            f"OpenAICompatibleClient(model={self.model!r}, base_url={self.base_url!r}, "
            f"temperature={self.temperature!r}, timeout_sec={self.timeout_sec!r})"
        )

    def _send_chat_request(
        self,
        system_prompt: str,
        user_content: str | list[dict[str, Any]],
    ) -> str:
        url = f"{self.base_url}/chat/completions"
        body = json.dumps(
            {
                "model": self.model,
                "temperature": self.temperature,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_content},
                ],
            }
        ).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout_sec) as resp:
                raw_body = resp.read().decode("utf-8", errors="replace")
            payload = json.loads(raw_body)
        except urllib.error.HTTPError as exc:
            detail = f"OpenAI-compatible API returned HTTP {exc.code}: {exc.reason}"
            if self.api_key:
                detail = detail.replace(self.api_key, "***")
            raise LLMRequestError(detail) from exc
        except (
            urllib.error.URLError,
            http.client.HTTPException,
            TimeoutError,
            ssl.SSLError,
            OSError,
            json.JSONDecodeError,
        ) as exc:
            detail = f"OpenAI-compatible API request failed: {exc}"
            if self.api_key:
                detail = detail.replace(self.api_key, "***")
            raise LLMRequestError(detail) from exc

        if not isinstance(payload, dict):
            raise LLMRequestError("OpenAI-compatible response payload must be a JSON object")
        choices = payload.get("choices")
        if not isinstance(choices, list) or not choices or not isinstance(choices[0], dict):
            raise LLMRequestError("OpenAI-compatible response missing choices[0]")
        message = choices[0].get("message")
        if not isinstance(message, dict):
            raise LLMRequestError("OpenAI-compatible response missing choices[0].message")
        content = message.get("content")
        if not isinstance(content, str) or not content.strip():
            raise LLMRequestError(
                "OpenAI-compatible response missing non-empty text in choices[0].message.content"
            )
        return content

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        return self._send_chat_request(system_prompt, user_prompt)

    def complete_multimodal(
        self,
        system_prompt: str,
        user_prompt: str,
        images: list[Any],
    ) -> str:
        if not images:
            return self.complete(system_prompt, user_prompt)
        user_content: list[dict[str, Any]] = [{"type": "text", "text": user_prompt}]
        for img in images:
            mime_type, data_base64 = _extract_image_mime_and_base64(img)
            user_content.append(
                {
                    "type": "image_url",
                    "image_url": {"url": f"data:{mime_type};base64,{data_base64}"},
                }
            )
        return self._send_chat_request(system_prompt, user_content)


class GeminiRestClient:
    """Zero-dependency Gemini REST API client via `urllib.request`."""

    def __init__(
        self,
        *,
        model: str = DEFAULT_GEMINI_MODEL,
        api_key: str | None = None,
        temperature: float = 0.1,
        timeout_sec: float = 60.0,
    ) -> None:
        self.model = model
        self.api_key = (
            api_key if api_key is not None else os.environ.get("GEMINI_API_KEY", "")
        )
        self.temperature = temperature
        self.timeout_sec = timeout_sec

    def __repr__(self) -> str:
        return (
            f"GeminiRestClient(model={self.model!r}, "
            f"temperature={self.temperature!r}, timeout_sec={self.timeout_sec!r})"
        )

    def _send_generate_request(
        self,
        system_prompt: str,
        user_parts: list[dict[str, Any]],
    ) -> str:
        url = (
            f"https://generativelanguage.googleapis.com/v1beta/models/"
            f"{self.model}:generateContent"
        )
        body = json.dumps(
            {
                "systemInstruction": {"parts": [{"text": system_prompt}]},
                "contents": [{"role": "user", "parts": user_parts}],
                "generationConfig": {"temperature": self.temperature},
            }
        ).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["x-goog-api-key"] = self.api_key
        req = urllib.request.Request(
            url,
            data=body,
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout_sec) as resp:
                raw_body = resp.read().decode("utf-8", errors="replace")
            payload = json.loads(raw_body)
        except urllib.error.HTTPError as exc:
            detail = f"Gemini API returned HTTP {exc.code}: {exc.reason}"
            if self.api_key:
                detail = detail.replace(self.api_key, "***")
            raise LLMRequestError(detail) from exc
        except (
            urllib.error.URLError,
            http.client.HTTPException,
            TimeoutError,
            ssl.SSLError,
            OSError,
            json.JSONDecodeError,
        ) as exc:
            detail = f"Gemini API request failed: {exc}"
            if self.api_key:
                detail = detail.replace(self.api_key, "***")
            raise LLMRequestError(detail) from exc

        if not isinstance(payload, dict):
            raise LLMRequestError("Gemini response payload must be a JSON object")
        candidates = payload.get("candidates")
        if not isinstance(candidates, list) or not candidates or not isinstance(candidates[0], dict):
            prompt_feedback = payload.get("promptFeedback")
            raise LLMRequestError(
                f"Gemini response missing candidates (promptFeedback={prompt_feedback!r})"
            )
        candidate = candidates[0]
        content = candidate.get("content")
        parts = content.get("parts") if isinstance(content, dict) else None
        if not isinstance(parts, list) or not parts:
            finish_reason = candidate.get("finishReason")
            raise LLMRequestError(
                f"Gemini response missing content parts (finishReason={finish_reason!r})"
            )
        texts = [
            part["text"]
            for part in parts
            if isinstance(part, dict) and isinstance(part.get("text"), str)
        ]
        combined = "".join(texts)
        if not combined.strip():
            finish_reason = candidate.get("finishReason")
            raise LLMRequestError(
                f"Gemini response contained no text content (finishReason={finish_reason!r})"
            )
        return combined

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        return self._send_generate_request(system_prompt, [{"text": user_prompt}])

    def complete_multimodal(
        self,
        system_prompt: str,
        user_prompt: str,
        images: list[Any],
    ) -> str:
        if not images:
            return self.complete(system_prompt, user_prompt)
        user_parts: list[dict[str, Any]] = [{"text": user_prompt}]
        for img in images:
            mime_type, data_base64 = _extract_image_mime_and_base64(img)
            user_parts.append(
                {
                    "inline_data": {
                        "mime_type": mime_type,
                        "data": data_base64,
                    }
                }
            )
        return self._send_generate_request(system_prompt, user_parts)


def _resolve_auto_provider(config: LLMConfig) -> str:
    # Request-level explicit base_url or api_key takes precedence over environment CAD_AGENT_PROVIDER.
    explicit_base_url = _clean_opt(config.base_url)
    if explicit_base_url is not None:
        if "generativelanguage.googleapis.com" in explicit_base_url.lower():
            return "gemini"
        return "openai"

    explicit_key = _clean_opt(config.api_key)
    if explicit_key is not None:
        model_hint = (_clean_opt(config.model) or "").lower()
        if model_hint.startswith("gemini"):
            return "gemini"
        return "openai"

    env_provider = _clean_opt(os.environ.get("CAD_AGENT_PROVIDER"))
    if env_provider is not None:
        normalized = env_provider.lower()
        if normalized not in VALID_PROVIDERS:
            raise LLMConfigError(
                f"Invalid CAD_AGENT_PROVIDER={env_provider!r}. "
                f"Expected one of: {', '.join(VALID_PROVIDERS)}."
            )
        if normalized != "auto":
            return normalized

    if _clean_opt(os.environ.get("OPENAI_API_KEY")) is not None:
        return "openai"
    if _clean_opt(os.environ.get("GEMINI_API_KEY")) is not None:
        return "gemini"
    return "template"


def create_llm_client(
    config: LLMConfig | None = None,
) -> tuple[LLMClient | None, dict[str, Any]]:
    """Resolve configuration and instantiate a pluggable `LLMClient` (or None for template mode)."""
    cfg = config if config is not None else LLMConfig()
    requested_provider = (_clean_opt(cfg.provider) or "auto").lower()
    if requested_provider not in VALID_PROVIDERS:
        raise LLMConfigError(
            f"Unsupported LLM provider {cfg.provider!r}. "
            f"Expected one of: {', '.join(VALID_PROVIDERS)}."
        )

    resolved_provider = (
        _resolve_auto_provider(cfg) if requested_provider == "auto" else requested_provider
    )

    if resolved_provider == "template":
        info = ResolvedLLMInfo(
            provider="template",
            model=None,
            base_url=None,
            used_llm=False,
        )
        return None, info.to_dict()

    if resolved_provider == "openai":
        api_key = _clean_opt(cfg.api_key) or _clean_opt(os.environ.get("OPENAI_API_KEY"))
        base_url = (
            _clean_opt(cfg.base_url)
            or _clean_opt(os.environ.get("OPENAI_BASE_URL"))
            or DEFAULT_OPENAI_BASE_URL
        ).rstrip("/")
        model = (
            _clean_opt(cfg.model)
            or _clean_opt(os.environ.get("OPENAI_MODEL"))
            or DEFAULT_OPENAI_MODEL
        )
        if not api_key and not _is_local_base_url(base_url):
            raise LLMConfigError(
                "OpenAI provider requires an API key (set OPENAI_API_KEY or pass api_key), "
                "unless using a local base_url such as http://localhost:8000/v1 or http://127.0.0.1:8000/v1."
            )
        client = OpenAICompatibleClient(
            model=model,
            api_key=api_key or "",
            base_url=base_url,
            temperature=cfg.temperature,
            timeout_sec=cfg.timeout_sec,
        )
        info = ResolvedLLMInfo(
            provider="openai",
            model=model,
            base_url=base_url,
            used_llm=True,
        )
        return client, info.to_dict()

    if resolved_provider == "gemini":
        api_key = _clean_opt(cfg.api_key) or _clean_opt(os.environ.get("GEMINI_API_KEY"))
        model = (
            _clean_opt(cfg.model)
            or _clean_opt(os.environ.get("GEMINI_MODEL"))
            or DEFAULT_GEMINI_MODEL
        )
        if not api_key:
            raise LLMConfigError(
                "Gemini provider requires an API key (set GEMINI_API_KEY or pass api_key)."
            )
        client = GeminiRestClient(
            model=model,
            api_key=api_key,
            temperature=cfg.temperature,
            timeout_sec=cfg.timeout_sec,
        )
        info = ResolvedLLMInfo(
            provider="gemini",
            model=model,
            base_url=None,
            used_llm=True,
        )
        return client, info.to_dict()

    raise LLMConfigError(f"Unhandled LLM provider: {resolved_provider!r}")
