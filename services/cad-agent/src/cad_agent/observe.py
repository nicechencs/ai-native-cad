"""Runtime observability: structured JSONL event log with trace/span context.

Separate from the frozen training ``TrajectoryLogger`` channel. Never log prompts,
source text, patches, mesh data or secrets here.
"""

from __future__ import annotations

import contextvars
import json
import os
import secrets
import sys
import tempfile
import threading
import time
from collections.abc import Mapping
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath, PureWindowsPath
from typing import Any, Iterator

LEVELS = ("error", "warn", "info", "debug", "trace")
_LEVEL_RANK = {name: idx for idx, name in enumerate(LEVELS)}
REQUIRED_FIELDS = ("ts", "level", "trace_id", "span_id", "component", "operation", "message")
OPTIONAL_FIELDS = (
    "parent_span_id", "duration_ms", "code", "error_type", "node_id", "session_id", "path",
)
CODE_IPC_FAILED = "R011_IPC_FAILED"
CODE_VLM_DEGRADED = "R014_VLM_DEGRADED"
CODE_DAEMON_RECONNECT = "R015_DAEMON_RECONNECT"
CODE_IO_ERROR = "R016_IO_ERROR"

MAX_LOG_BYTES = 10 * 1024 * 1024
KEEP_FILES = 5
LOG_FILE_NAME = "cad-agent.jsonl"

_trace_id: contextvars.ContextVar[str | None] = contextvars.ContextVar("aicad_trace_id", default=None)
_span_id: contextvars.ContextVar[str | None] = contextvars.ContextVar("aicad_span_id", default=None)
_parent_span_id: contextvars.ContextVar[str | None] = contextvars.ContextVar(
    "aicad_parent_span_id", default=None
)
_fields: contextvars.ContextVar[dict[str, Any]] = contextvars.ContextVar("aicad_fields", default={})

_process_trace_id: str | None = None
_lock = threading.Lock()


def new_trace_id() -> str:
    return secrets.token_hex(16)


def new_span_id() -> str:
    return secrets.token_hex(8)


def process_trace_id() -> str:
    """Trace id for this process: ``AICAD_TRACE_ID`` or a generated one."""
    global _process_trace_id
    env = os.environ.get("AICAD_TRACE_ID", "").strip()
    if env:
        return env
    if _process_trace_id is None:
        _process_trace_id = new_trace_id()
    return _process_trace_id


def current_trace_id() -> str:
    return _trace_id.get() or process_trace_id()


def current_span_id() -> str | None:
    return _span_id.get()


def set_context(**fields: Any) -> None:
    """Attach context fields (e.g. ``session_id``) to subsequent events."""
    merged = dict(_fields.get())
    merged.update({k: v for k, v in fields.items() if v is not None})
    _fields.set(merged)


def configured_level() -> str:
    raw = os.environ.get("AICAD_LOG", "info").strip().lower()
    return raw if raw in _LEVEL_RANK else "info"


def _env_path(env: Mapping[str, str], key: str) -> str | None:
    value = env.get(key, "").strip()
    return value or None


def resolve_log_dir(env: Mapping[str, str], platform: str, temp_dir: Path) -> Path:
    """Same rule as Rust `cad_observe::log_dir::resolve_log_dir`.

    `AICAD_LOG_DIR` wins. Otherwise `<data>/aicad/logs`. On Windows `<data>` is
    `%LOCALAPPDATA%`, then `%APPDATA%`.
    """
    override = _env_path(env, "AICAD_LOG_DIR")
    if override:
        return Path(override)
    if platform == "win32":
        base = _env_path(env, "LOCALAPPDATA") or _env_path(env, "APPDATA")
        root = Path(base) if base else temp_dir
    elif platform == "darwin":
        home = _env_path(env, "HOME")
        root = Path(home) / "Library" / "Application Support" if home else temp_dir
    else:
        xdg = _env_path(env, "XDG_DATA_HOME")
        home = _env_path(env, "HOME")
        if xdg:
            root = Path(xdg)
        elif home:
            root = Path(home) / ".local" / "share"
        else:
            root = temp_dir
    return root / "aicad" / "logs"


def default_log_dir() -> Path:
    return resolve_log_dir(os.environ, sys.platform, Path(tempfile.gettempdir()))


def sanitize_path(path: str | os.PathLike[str] | None, workspace_root: str | os.PathLike[str] | None) -> str | None:
    """Workspace-relative POSIX path, or ``<external>/<basename>`` outside the workspace."""
    if path is None:
        return None
    raw = os.fspath(path)
    win = "\\" in raw or (len(raw) > 1 and raw[1] == ":")
    pure = PureWindowsPath(raw) if win else PurePosixPath(raw)
    if workspace_root is not None:
        root_raw = os.fspath(workspace_root)
        root = PureWindowsPath(root_raw) if (win or "\\" in root_raw) else PurePosixPath(root_raw)
        try:
            if pure.is_absolute():
                rel = pure.relative_to(root)
            else:
                rel = pure
                if ".." in rel.parts:
                    raise ValueError
            return PurePosixPath(*rel.parts).as_posix() or "."
        except ValueError:
            if isinstance(pure, PureWindowsPath) and isinstance(root, PureWindowsPath):
                try:
                    rel = PureWindowsPath(raw.lower()).relative_to(PureWindowsPath(root_raw.lower()))
                    return PurePosixPath(*PureWindowsPath(raw).parts[len(root.parts):]).as_posix() or "."
                except ValueError:
                    pass
    elif not pure.is_absolute() and ".." not in pure.parts:
        return PurePosixPath(*pure.parts).as_posix()
    return f"<external>/{pure.name}"


def _rotate(path: Path, max_bytes: int, keep: int) -> None:
    try:
        if not path.exists() or path.stat().st_size < max_bytes:
            return
    except OSError:
        return
    oldest = path.with_name(f"{path.name}.{keep}")
    if oldest.exists():
        oldest.unlink()
    for idx in range(keep - 1, 0, -1):
        src = path.with_name(f"{path.name}.{idx}")
        if src.exists():
            src.replace(path.with_name(f"{path.name}.{idx + 1}"))
    path.replace(path.with_name(f"{path.name}.1"))


class RuntimeLogger:
    def __init__(
        self,
        log_dir: str | os.PathLike[str] | None = None,
        *,
        level: str | None = None,
        max_bytes: int = MAX_LOG_BYTES,
        keep: int = KEEP_FILES,
    ) -> None:
        self._log_dir = Path(log_dir) if log_dir is not None else None
        self._level = level
        self.max_bytes = max_bytes
        self.keep = keep

    @property
    def path(self) -> Path:
        return (self._log_dir or default_log_dir()) / LOG_FILE_NAME

    def enabled(self, level: str) -> bool:
        threshold = self._level or configured_level()
        return _LEVEL_RANK.get(level, 2) <= _LEVEL_RANK.get(threshold, 2)

    def emit(
        self,
        level: str,
        component: str,
        operation: str,
        message: str,
        **extra: Any,
    ) -> dict[str, Any] | None:
        if level not in _LEVEL_RANK:
            level = "info"
        if not self.enabled(level):
            return None
        now = datetime.now(timezone.utc)
        event: dict[str, Any] = {
            "ts": now.strftime("%Y-%m-%dT%H:%M:%S.") + f"{now.microsecond // 1000:03d}Z",
            "level": level,
            "trace_id": current_trace_id(),
            "span_id": current_span_id() or new_span_id(),
            "component": component,
            "operation": operation,
            "message": message,
        }
        parent = _parent_span_id.get()
        if parent:
            event["parent_span_id"] = parent
        for key, value in {**_fields.get(), **extra}.items():
            if key in OPTIONAL_FIELDS and value is not None:
                event[key] = value
        line = json.dumps(event, ensure_ascii=False, separators=(",", ":")) + "\n"
        try:
            with _lock:
                target = self.path
                target.parent.mkdir(parents=True, exist_ok=True)
                _rotate(target, self.max_bytes, self.keep)
                with target.open("a", encoding="utf-8") as fh:
                    fh.write(line)
        except OSError:
            return None  # logging must never break the pipeline
        return event


_default_logger = RuntimeLogger()


def get_logger() -> RuntimeLogger:
    return _default_logger


def set_logger(logger: RuntimeLogger) -> RuntimeLogger:
    global _default_logger
    previous, _default_logger = _default_logger, logger
    return previous


def log(level: str, component: str, operation: str, message: str, **extra: Any) -> dict[str, Any] | None:
    return _default_logger.emit(level, component, operation, message, **extra)


def warn(component: str, operation: str, message: str, **extra: Any) -> dict[str, Any] | None:
    return log("warn", component, operation, message, **extra)


@contextmanager
def trace_span(component: str, operation: str, **fields: Any) -> Iterator[str]:
    """Open a child span; emits a debug event with ``duration_ms`` on exit (error on failure)."""
    trace = _trace_id.get() or process_trace_id()
    parent = _span_id.get()
    span = new_span_id()
    tokens = (
        _trace_id.set(trace),
        _span_id.set(span),
        _parent_span_id.set(parent),
        _fields.set({**_fields.get(), **{k: v for k, v in fields.items() if v is not None}}),
    )
    start = time.perf_counter()
    try:
        yield span
    except BaseException as exc:
        log(
            "error", component, operation, f"{operation} failed",
            duration_ms=int((time.perf_counter() - start) * 1000),
            error_type=type(exc).__name__,
        )
        raise
    else:
        log(
            "debug", component, operation, f"{operation} finished",
            duration_ms=int((time.perf_counter() - start) * 1000),
        )
    finally:
        _fields.reset(tokens[3])
        _parent_span_id.reset(tokens[2])
        _span_id.reset(tokens[1])
        _trace_id.reset(tokens[0])


def subprocess_env(base: dict[str, str] | None = None) -> dict[str, str]:
    """Environment for child processes carrying the trace across the process boundary."""
    env = dict(os.environ if base is None else base)
    env["AICAD_TRACE_ID"] = current_trace_id()
    span = current_span_id()
    if span:
        env["AICAD_PARENT_SPAN_ID"] = span
    else:
        env.pop("AICAD_PARENT_SPAN_ID", None)
    return env
