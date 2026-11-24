"""Persistent NDJSON JSON-RPC client for `cad-cli daemon`."""

from __future__ import annotations

import json
import subprocess
from collections.abc import Sequence
from contextlib import suppress
from itertools import count
from pathlib import Path
from threading import RLock
from typing import Any

from . import observe
from .compiler_bridge import discover_cad_cli


_RETRYABLE_METHODS = frozenset(
    {"ping", "check_kcl", "eval_project", "dfm_check", "cadpack_tree", "cadpack_digest"}
)
_SHUTDOWN_TIMEOUT_SECONDS = 1.0


class CadDaemonError(RuntimeError):
    """A daemon transport, protocol, or JSON-RPC error."""

    def __init__(self, message: str, *, code: int | None = None, data: Any = None) -> None:
        super().__init__(message)
        self.code = code
        self.data = data


class _DaemonDisconnected(CadDaemonError):
    """The transport closed before the request completed."""


class CadDaemonClient:
    """Run sequential requests over a persistent CAD daemon subprocess."""

    def __init__(
        self,
        *,
        workspace_root: Path | None = None,
        cad_cli_bin: Path | None = None,
        auto_restart: bool = True,
    ) -> None:
        self._workspace_root = workspace_root
        self._cad_cli_bin = cad_cli_bin
        self._auto_restart = auto_restart
        self._proc: subprocess.Popen[bytes] | None = None
        self._request_ids = count(1)
        self._lock = RLock()

    def __enter__(self) -> CadDaemonClient:
        self.start()
        return self

    def __exit__(self, *exc_info: object) -> None:
        self.close()

    def start(self) -> None:
        """Start the daemon if there is no live process."""
        with self._lock:
            if self._proc is not None:
                if self._proc.poll() is None:
                    return
                self._stop_process(self._proc)

            binary = discover_cad_cli(self._cad_cli_bin)
            if binary is None:
                raise CadDaemonError(
                    "Could not locate `cad-cli`. Build it with `cargo build -p cad-cli` "
                    "or set CAD_CLI_BIN."
                )
            command = [str(binary), "daemon"]
            if self._workspace_root is not None:
                command.extend(["--workspace-root", str(self._workspace_root)])
            try:
                self._proc = subprocess.Popen(
                    command,
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    env=observe.subprocess_env(),
                )
            except OSError as exc:
                raise CadDaemonError(f"Failed to launch cad-cli daemon ({binary}): {exc}") from exc

    def close(self) -> None:
        """Request shutdown, then terminate the process if it does not exit promptly."""
        with self._lock:
            proc = self._proc
            if proc is None:
                return
            try:
                if proc.poll() is None:
                    # The shutdown reply fits in the pipe. Wait on the process so a
                    # daemon that never replies cannot block us reading stdout.
                    with suppress(_DaemonDisconnected, subprocess.TimeoutExpired):
                        self._send_request(proc, "shutdown", None)
                        proc.wait(timeout=_SHUTDOWN_TIMEOUT_SECONDS)
            finally:
                self._stop_process(proc)

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        """Call a daemon method, retrying known idempotent methods once on disconnect."""
        can_retry = self._auto_restart and method in _RETRYABLE_METHODS
        with self._lock:
            while True:
                if self._proc is None:
                    self.start()
                proc = self._proc
                assert proc is not None
                try:
                    return self._exchange(proc, method, params)
                except _DaemonDisconnected as exc:
                    self._stop_process(proc)
                    if not can_retry:
                        raise
                    observe.warn(
                        "daemon_client", "reconnect",
                        f"cad-cli daemon disconnected during {method}; restarting and retrying",
                        code=observe.CODE_DAEMON_RECONNECT,
                        error_type=type(exc).__name__,
                    )
                    can_retry = False

    def ping(self) -> dict[str, Any]:
        return self.call("ping")

    def check_kcl(self, source: str, filename: str = "inline.kcl") -> dict[str, Any]:
        return self.call("check_kcl", {"source": source, "filename": filename})

    def eval_project(self, project_path: str | Path) -> dict[str, Any]:
        return self.call("eval_project", {"project_path": str(project_path)})

    def dfm_check(
        self,
        project_path: str | Path,
        *,
        min_wall_mm: float = 1.5,
        min_draft_deg: float = 1.0,
        pull_dir: Sequence[float] = (0.0, 0.0, 1.0),
    ) -> dict[str, Any]:
        return self.call(
            "dfm_check",
            {
                "project_path": str(project_path),
                "min_wall_mm": min_wall_mm,
                "min_draft_deg": min_draft_deg,
                "pull_dir": list(pull_dir),
            },
        )

    def cadpack_tree(
        self,
        cadpack_path: str | Path,
        *,
        offset: int = 0,
        limit: int = 50,
        query: str | None = None,
    ) -> dict[str, Any]:
        return self.call(
            "cadpack_tree",
            {"cadpack_path": str(cadpack_path), "offset": offset, "limit": limit, "query": query},
        )

    def cadpack_digest(
        self,
        cadpack_path: str | Path,
        roi_min_mm: Sequence[float],
        roi_max_mm: Sequence[float],
    ) -> dict[str, Any]:
        return self.call(
            "cadpack_digest",
            {
                "cadpack_path": str(cadpack_path),
                "roi_min_mm": list(roi_min_mm),
                "roi_max_mm": list(roi_max_mm),
            },
        )

    def _stop_process(self, proc: subprocess.Popen[bytes]) -> None:
        self._proc = None
        try:
            if proc.poll() is None:
                with suppress(ProcessLookupError):
                    proc.terminate()
                try:
                    proc.wait(timeout=_SHUTDOWN_TIMEOUT_SECONDS)
                except subprocess.TimeoutExpired:
                    with suppress(ProcessLookupError):
                        proc.kill()
                    proc.wait()
        finally:
            assert proc.stdin is not None and proc.stdout is not None
            with suppress(OSError):
                proc.stdin.close()
            proc.stdout.close()

    def _send_request(
        self, proc: subprocess.Popen[bytes], method: str, params: dict[str, Any] | None
    ) -> int:
        if proc.poll() is not None:
            raise _DaemonDisconnected(
                f"cad-cli daemon exited with code {proc.returncode} before {method}"
            )

        request_id = next(self._request_ids)
        request = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        request["trace_id"] = observe.current_trace_id()
        line = json.dumps(request, ensure_ascii=False, allow_nan=False).encode("utf-8") + b"\n"
        assert proc.stdin is not None
        try:
            proc.stdin.write(line)
            proc.stdin.flush()
        except OSError as exc:
            raise _DaemonDisconnected(f"Could not send {method} to cad-cli daemon: {exc}") from exc
        return request_id

    def _exchange(
        self, proc: subprocess.Popen[bytes], method: str, params: dict[str, Any] | None
    ) -> dict[str, Any]:
        request_id = self._send_request(proc, method, params)
        assert proc.stdout is not None
        try:
            line = proc.stdout.readline()
        except OSError as exc:
            raise _DaemonDisconnected(
                f"Could not read {method} reply from cad-cli daemon: {exc}"
            ) from exc
        if not line.endswith(b"\n"):
            raise _DaemonDisconnected(f"cad-cli daemon closed stdout before completing {method}")
        try:
            response = json.loads(line.decode("utf-8"))
        except (UnicodeError, json.JSONDecodeError) as exc:
            self._stop_process(proc)
            raise CadDaemonError(f"Invalid JSON from cad-cli daemon for {method}") from exc
        if (
            not isinstance(response, dict)
            or response.get("jsonrpc") != "2.0"
            or type(response.get("id")) is not int
            or response["id"] != request_id
            or ("result" in response) == ("error" in response)
        ):
            self._stop_process(proc)
            raise CadDaemonError(f"Invalid JSON-RPC response from cad-cli daemon for {method}")

        if "error" in response:
            error = response["error"]
            if not isinstance(error, dict) or not isinstance(error.get("message"), str):
                raise CadDaemonError(f"Invalid JSON-RPC error from cad-cli daemon for {method}")
            code = error.get("code")
            raise CadDaemonError(
                error["message"], code=code if type(code) is int else None, data=error.get("data")
            )

        result = response["result"]
        if not isinstance(result, dict):
            raise CadDaemonError(f"Expected an object from cad-cli daemon for {method}")
        return result
