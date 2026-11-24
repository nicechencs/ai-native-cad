"""Shared parsing, formatting, path validation, and progress redaction helpers."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from .contracts import ContractValidationError
from .llm import LLMClient


def _strip_code_fences(text: str) -> str:
    """Extract raw code or JSON when an LLM wraps output in markdown fences."""
    stripped = text.strip()
    fence_match = re.search(r"```(?:[a-zA-Z0-9_-]+)?\s*\n(.*?)```", stripped, re.DOTALL)
    if fence_match:
        return fence_match.group(1).strip() + "\n"
    return stripped


def _extract_json_payload(text: str) -> dict[str, Any]:
    """Extract a JSON object from LLM output, tolerating markdown fences and surrounding prose."""
    cleaned = _strip_code_fences(text).strip()
    try:
        parsed = json.loads(cleaned)
        if isinstance(parsed, dict):
            return parsed
    except json.JSONDecodeError:
        pass

    json_fence = re.search(r"```(?:json)?\s*\n(.*?)```", text, re.DOTALL | re.IGNORECASE)
    if json_fence:
        try:
            parsed = json.loads(json_fence.group(1).strip())
            if isinstance(parsed, dict):
                return parsed
        except json.JSONDecodeError:
            pass

    decoder = json.JSONDecoder()
    for idx, ch in enumerate(text):
        if ch != "{":
            continue
        try:
            parsed, _ = decoder.raw_decode(text, idx)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed

    raise ValueError("LLM response did not contain a valid JSON object for DesignContract")


def _to_pascal_case(raw: str, fallback: str = "GeneratedPart") -> str:
    tokens = re.findall(r"[A-Za-z0-9]+", raw)
    if not tokens:
        return fallback
    joined = "".join(tok[:1].upper() + tok[1:] for tok in tokens[:4])
    if not joined[0].isalpha():
        return f"Part{joined}"
    return joined


class _RepairCountingLLMClient:
    """Thin LLMClient wrapper that counts repair completions invoked by `parse_contract_with_repair`."""

    def __init__(self, inner: LLMClient) -> None:
        self._inner = inner
        self.repair_calls: int = 0

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        self.repair_calls += 1
        return self._inner.complete(system_prompt, user_prompt)


def _assert_within_base(base_dir: Path, target: Path) -> Path:
    resolved_base = base_dir.resolve()
    resolved_target = target.resolve()
    if not resolved_target.is_relative_to(resolved_base):
        raise ContractValidationError(
            [f"Refusing path outside work_dir: {resolved_target} not under {resolved_base}"]
        )
    return resolved_target


_PROGRESS_SECRET_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"sk-[A-Za-z0-9_\-]{8,}"),
    re.compile(r"AIza[0-9A-Za-z_\-]{16,}"),
    re.compile(r"(Bearer\s+)[A-Za-z0-9._\-]{8,}", re.IGNORECASE),
    re.compile(r"(api[_-]?key\s*[=:]\s*)[^\s,;\"']+", re.IGNORECASE),
)


def _sanitize_progress_text(text: str, secrets: tuple[str, ...]) -> str:
    sanitized = text
    for secret in secrets:
        if secret:
            sanitized = sanitized.replace(secret, "***")
    for pattern in _PROGRESS_SECRET_PATTERNS:
        if pattern.groups:
            sanitized = pattern.sub(r"\1***", sanitized)
        else:
            sanitized = pattern.sub("***", sanitized)
    return sanitized


def _sanitize_progress_value(value: Any, secrets: tuple[str, ...]) -> Any:
    if isinstance(value, str):
        return _sanitize_progress_text(value, secrets)
    if isinstance(value, list):
        return [_sanitize_progress_value(item, secrets) for item in value]
    if isinstance(value, dict):
        return {k: _sanitize_progress_value(v, secrets) for k, v in value.items()}
    return value
