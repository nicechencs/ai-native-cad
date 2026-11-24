"""Subprocess bridge to the unified ``cad-cli`` V2 document runtime.

The client shells out to ``cad-cli``. It does not open the project container.
Unit tests inject a ``CliTransport`` so no live binary or model key is required.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

from . import observe
from .compiler_bridge import discover_cad_cli

_DEFAULT_TIMEOUT_S = 120.0
_MAX_INLINE_COMMANDS_BYTES = 8192
_REVISION_CONFLICT = "E409_REVISION_CONFLICT"
_REVISION_RE = re.compile(
    r"expected revision\s+(\d+)\s*,\s*actual\s+(\d+)",
    re.IGNORECASE,
)


class RuntimeToolError(RuntimeError):
    """``cad-cli`` failed, timed out, or was asked to leave the workspace."""

    def __init__(
        self,
        message: str,
        *,
        code: str,
        payload: Mapping[str, Any] | None = None,
        expected_revision: int | None = None,
        actual_revision: int | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.payload = dict(payload or {})
        self.expected_revision = expected_revision
        self.actual_revision = actual_revision

    def to_dict(self) -> dict[str, Any]:
        body: dict[str, Any] = {
            "code": self.code,
            "message": str(self),
        }
        if self.expected_revision is not None:
            body["expected_revision"] = self.expected_revision
        if self.actual_revision is not None:
            body["actual_revision"] = self.actual_revision
        if self.payload:
            body["payload"] = dict(self.payload)
        return body


class CliTransport(Protocol):
    """Runs one ``cad-cli`` argv list and returns the completed process."""

    def run(
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout_s: float,
    ) -> subprocess.CompletedProcess[str]:
        """Execute ``argv`` in ``cwd`` and stop after ``timeout_s`` seconds."""


class SubprocessCliTransport:
    """Real process runner. Tests substitute their own ``CliTransport``."""

    def run(
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout_s: float,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            list(argv),
            cwd=str(cwd),
            timeout=timeout_s,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            check=False,
            env=observe.subprocess_env(),
        )


@dataclass(slots=True)
class SessionStatus:
    """``cad-cli session status --json`` payload."""

    mode: str
    project_path: str
    revision: int
    active_branch: str
    design_contract: dict[str, Any] | None
    selected_topo_refs: list[str] = field(default_factory=list)
    selected_node_ids: list[str] = field(default_factory=list)
    viewport: dict[str, Any] = field(default_factory=dict)
    raw: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "mode": self.mode,
            "project_path": self.project_path,
            "revision": self.revision,
            "active_branch": self.active_branch,
            "design_contract": self.design_contract,
            "selected_topo_refs": list(self.selected_topo_refs),
            "selected_node_ids": list(self.selected_node_ids),
            "viewport": dict(self.viewport),
        }


@dataclass(slots=True)
class ApplyOutcome:
    """Successful ``cad-cli api apply --json`` body (``ExecOutcome``)."""

    revision: int
    branch: str
    summary: dict[str, Any]
    created_nodes: list[str]
    modified_nodes: list[str]
    deleted_nodes: list[str]
    solids: dict[str, Any]
    raw: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "revision": self.revision,
            "branch": self.branch,
            "summary": self.summary,
            "created_nodes": list(self.created_nodes),
            "modified_nodes": list(self.modified_nodes),
            "deleted_nodes": list(self.deleted_nodes),
            "solids": self.solids,
        }


def _as_str_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)]


def _parse_json_document(text: str) -> Any:
    stripped = text.strip()
    if not stripped:
        return None
    try:
        return json.loads(stripped)
    except json.JSONDecodeError:
        return None


def _conflict_revisions(message: str) -> tuple[int | None, int | None]:
    match = _REVISION_RE.search(message)
    if match is None:
        return None, None
    return int(match.group(1)), int(match.group(2))


def _error_from_payload(payload: Mapping[str, Any], fallback: str) -> RuntimeToolError:
    code = str(payload.get("code") or "E_RUNTIME")
    message = payload.get("message")
    text = message.strip() if isinstance(message, str) and message.strip() else fallback
    expected, actual = _conflict_revisions(text)
    if code == _REVISION_CONFLICT and expected is None:
        expected, actual = _conflict_revisions(json.dumps(payload, ensure_ascii=False))
    return RuntimeToolError(
        text,
        code=code,
        payload=payload,
        expected_revision=expected,
        actual_revision=actual,
    )


class RuntimeToolClient:
    """One workspace-scoped handle for the V2 ``cad-cli`` tool surface.

    ``session status``, ``query``, and ``verify`` read the container's active
    branch. After ``api apply --branch``, call ``checkout_branch`` before those
    reads so they observe the preview instead of ``main``.
    """

    def __init__(
        self,
        workspace_root: str | Path,
        project: str | Path,
        *,
        cli_bin: str | Path | None = None,
        transport: CliTransport | None = None,
        timeout_s: float = _DEFAULT_TIMEOUT_S,
    ) -> None:
        root = Path(workspace_root)
        if not root.is_dir():
            raise RuntimeToolError(
                f"workspace root does not exist or is not a directory: {root}",
                code="E_PATH_ESCAPE",
            )
        self.workspace_root = root.resolve()
        self.project_path = self._confine_project(project)
        if timeout_s <= 0:
            raise ValueError("timeout_s must be > 0")
        self.timeout_s = float(timeout_s)
        self.transport: CliTransport = transport if transport is not None else SubprocessCliTransport()
        if cli_bin is not None:
            self.cli_bin = str(cli_bin)
        elif transport is not None:
            self.cli_bin = "cad-cli"
        else:
            discovered = discover_cad_cli(workspace_root=self.workspace_root)
            self.cli_bin = str(discovered) if discovered is not None else "cad-cli"

    def session_status(self) -> SessionStatus:
        payload = self._invoke(
            [
                self.cli_bin,
                "session",
                "status",
                "--project",
                self._project_arg(),
                "--workspace",
                str(self.workspace_root),
                "--json",
            ],
            operation="session_status",
        )
        if not isinstance(payload, dict):
            raise RuntimeToolError("session status did not return a JSON object", code="E_IO")
        viewport = payload.get("viewport")
        viewport_dict = dict(viewport) if isinstance(viewport, dict) else {}
        contract = payload.get("design_contract")
        revision = payload.get("revision")
        if isinstance(revision, bool) or not isinstance(revision, int):
            raise RuntimeToolError("session status is missing an integer revision", code="E_IO")
        return SessionStatus(
            mode=str(payload.get("mode") or ""),
            project_path=str(payload.get("project_path") or self.project_path),
            revision=revision,
            active_branch=str(payload.get("active_branch") or "main"),
            design_contract=dict(contract) if isinstance(contract, dict) else None,
            selected_topo_refs=_as_str_list(viewport_dict.get("selected_topo_refs")),
            selected_node_ids=_as_str_list(viewport_dict.get("selected_node_ids")),
            viewport=viewport_dict,
            raw=dict(payload),
        )

    def apply_commands(
        self,
        commands: Sequence[Mapping[str, Any]],
        *,
        expected_revision: int | None,
        branch: str | None,
    ) -> ApplyOutcome:
        if isinstance(expected_revision, bool) or (
            expected_revision is not None and not isinstance(expected_revision, int)
        ):
            raise RuntimeToolError(
                "expected_revision must be an integer",
                code="E400_INVALID_ARGUMENT",
            )
        if expected_revision is not None and expected_revision < 0:
            raise RuntimeToolError(
                "expected_revision must be >= 0",
                code="E400_INVALID_ARGUMENT",
            )
        encoded = json.dumps(list(commands), ensure_ascii=False, separators=(",", ":"))
        temp_path: Path | None = None
        commands_arg = encoded
        if len(encoded.encode("utf-8")) > _MAX_INLINE_COMMANDS_BYTES:
            fd, raw_path = tempfile.mkstemp(prefix="aicad-v2-commands-", suffix=".json")
            temp_path = Path(raw_path)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    handle.write(encoded)
            except Exception:
                temp_path.unlink(missing_ok=True)
                raise
            commands_arg = f"@{temp_path}"
        argv = [
            self.cli_bin,
            "api",
            "apply",
            "--project",
            self._project_arg(),
            "--workspace",
            str(self.workspace_root),
            "--commands",
            commands_arg,
        ]
        if expected_revision is not None:
            argv.extend(["--expected-revision", str(expected_revision)])
        if branch is not None and branch.strip():
            argv.extend(["--branch", branch.strip()])
        argv.append("--json")
        try:
            payload = self._invoke(argv, operation="api_apply")
        finally:
            if temp_path is not None:
                temp_path.unlink(missing_ok=True)
        if not isinstance(payload, dict):
            raise RuntimeToolError("api apply did not return a JSON object", code="E_IO")
        revision = payload.get("revision")
        if isinstance(revision, bool) or not isinstance(revision, int):
            raise RuntimeToolError("api apply is missing an integer revision", code="E_IO")
        summary = payload.get("summary")
        solids = payload.get("solids")
        return ApplyOutcome(
            revision=revision,
            branch=str(payload.get("branch") or branch or ""),
            summary=dict(summary) if isinstance(summary, dict) else {},
            created_nodes=_node_id_list(payload.get("created_nodes")),
            modified_nodes=_node_id_list(payload.get("modified_nodes")),
            deleted_nodes=_node_id_list(payload.get("deleted_nodes")),
            solids=dict(solids) if isinstance(solids, dict) else {},
            raw=dict(payload),
        )

    def query_summary(self) -> dict[str, Any]:
        payload = self._invoke(self._query_argv("summary"), operation="query_summary")
        if not isinstance(payload, dict):
            raise RuntimeToolError("query summary did not return a JSON object", code="E_IO")
        return payload

    def query_explain(self, target: str) -> dict[str, Any]:
        text = target.strip()
        if not text:
            raise RuntimeToolError("query explain requires a target id", code="E400_INVALID_ARGUMENT")
        argv = self._query_argv("explain")
        argv.extend(["--target", text])
        payload = self._invoke(argv, operation="query_explain")
        if not isinstance(payload, dict):
            raise RuntimeToolError("query explain did not return a JSON object", code="E_IO")
        return payload

    def verify(self) -> dict[str, Any]:
        payload = self._invoke(
            [
                self.cli_bin,
                "verify",
                "--project",
                self._project_arg(),
                "--workspace",
                str(self.workspace_root),
                "--json",
            ],
            operation="verify",
        )
        if not isinstance(payload, dict):
            raise RuntimeToolError("verify did not return a JSON object", code="E_IO")
        return payload

    def export_openai_tool_schema(self) -> list[Any]:
        payload = self._invoke(
            [
                self.cli_bin,
                "mcp",
                "--workspace",
                str(self.workspace_root),
                "--export-tool-schema",
                "openai",
            ],
            operation="export_tool_schema",
        )
        if not isinstance(payload, list):
            raise RuntimeToolError(
                "mcp --export-tool-schema openai did not return a JSON array",
                code="E_IO",
            )
        return payload

    def checkout_branch(self, branch: str) -> dict[str, Any]:
        """Point the active branch at ``branch`` so later reads see the preview.

        ``cad-cli query`` and ``cad-cli verify`` have no branch flag. They read
        whichever branch is checked out.
        """
        name = branch.strip()
        if not name or ".." in name or "\\" in name:
            raise RuntimeToolError(
                f"refusing branch name {branch!r}",
                code="E400_INVALID_ARGUMENT",
            )
        payload = self._invoke(
            [
                self.cli_bin,
                "project",
                "--json",
                "branch",
                self._project_arg(),
                "checkout",
                name,
            ],
            operation="checkout_branch",
        )
        if not isinstance(payload, dict):
            raise RuntimeToolError("branch checkout did not return a JSON object", code="E_IO")
        return payload

    def emit_kcl(self, out_path: str | Path) -> dict[str, Any]:
        """Emit the active branch's canonical `.kcl 2.0` view to ``out_path``."""
        target = Path(out_path)
        payload = self._invoke(
            [
                self.cli_bin,
                "project",
                "--json",
                "emit-kcl",
                self._project_arg(),
                "--out",
                str(target),
            ],
            operation="emit_kcl",
        )
        if not isinstance(payload, dict):
            raise RuntimeToolError("emit-kcl did not return a JSON object", code="E_IO")
        return payload

    def slice_context(
        self,
        focus_nodes: Sequence[str],
        *,
        upstream_depth: int = 1,
        downstream_depth: int = 1,
    ) -> dict[str, Any]:
        """Run ``cad-cli project slice-context`` for a focus neighborhood."""
        focus = [str(node).strip() for node in focus_nodes if str(node).strip()]
        if not focus:
            raise RuntimeToolError(
                "slice-context requires at least one focus node",
                code="E400_INVALID_ARGUMENT",
            )
        if upstream_depth < 0 or downstream_depth < 0:
            raise RuntimeToolError(
                "slice depths must be >= 0",
                code="E400_INVALID_ARGUMENT",
            )
        payload = self._invoke(
            [
                self.cli_bin,
                "project",
                "--json",
                "slice-context",
                self._project_arg(),
                "--focus",
                ",".join(focus),
                "--up",
                str(int(upstream_depth)),
                "--down",
                str(int(downstream_depth)),
            ],
            operation="slice_context",
        )
        if not isinstance(payload, dict):
            raise RuntimeToolError("slice-context did not return a JSON object", code="E_IO")
        return payload

    def _query_argv(self, subcommand: str) -> list[str]:
        # ``--project`` and ``--workspace`` must precede the query subcommand.
        return [
            self.cli_bin,
            "query",
            "--project",
            self._project_arg(),
            "--workspace",
            str(self.workspace_root),
            "--json",
            subcommand,
        ]

    def _invoke(self, argv: Sequence[str], *, operation: str) -> Any:
        try:
            completed = self.transport.run(
                list(argv),
                cwd=self.workspace_root,
                timeout_s=self.timeout_s,
            )
        except subprocess.TimeoutExpired as exc:
            observe.log(
                "error",
                "cad-agent",
                f"runtime.{operation}",
                f"cad-cli timed out after {self.timeout_s:.0f}s",
                code="E_TIMEOUT",
            )
            raise RuntimeToolError(
                f"cad-cli timed out after {self.timeout_s:.0f}s during {operation}",
                code="E_TIMEOUT",
            ) from exc
        except OSError as exc:
            observe.log(
                "error",
                "cad-agent",
                f"runtime.{operation}",
                f"cad-cli could not be started: {exc}",
                code="E_IO",
            )
            raise RuntimeToolError(
                f"cad-cli could not be started: {exc}",
                code="E_IO",
            ) from exc
        return self._decode(completed, operation)

    def _decode(self, completed: subprocess.CompletedProcess[str], operation: str) -> Any:
        stdout = completed.stdout or ""
        payload = _parse_json_document(stdout)
        if completed.returncode != 0:
            if isinstance(payload, dict):
                error = _error_from_payload(
                    payload,
                    fallback=f"cad-cli {operation} exited {completed.returncode}",
                )
            else:
                detail = (completed.stderr or stdout).strip()
                message = detail or f"cad-cli {operation} exited {completed.returncode}"
                expected, actual = _conflict_revisions(message)
                code = _REVISION_CONFLICT if actual is not None else "E_RUNTIME"
                error = RuntimeToolError(
                    message,
                    code=code,
                    expected_revision=expected,
                    actual_revision=actual,
                )
            observe.log(
                "error",
                "cad-agent",
                f"runtime.{operation}",
                error.code,
                code=error.code,
            )
            raise error
        if payload is None:
            raise RuntimeToolError(
                f"cad-cli {operation} returned no JSON",
                code="E_IO",
            )
        if isinstance(payload, dict) and payload.get("status") == "error":
            raise _error_from_payload(payload, fallback=f"cad-cli {operation} failed")
        return payload

    def _project_arg(self) -> str:
        return str(self.project_path)

    def _confine_project(self, project: str | Path) -> Path:
        raw = Path(project)
        if any(part == ".." for part in raw.parts):
            raise RuntimeToolError(
                f"project path {project!r} escapes the workspace",
                code="E_PATH_ESCAPE",
            )
        candidate = raw if raw.is_absolute() else self.workspace_root / raw
        if any(part == ".." for part in candidate.parts):
            raise RuntimeToolError(
                f"project path {project!r} escapes the workspace",
                code="E_PATH_ESCAPE",
            )
        resolved = candidate.resolve()
        try:
            resolved.relative_to(self.workspace_root)
        except ValueError as exc:
            raise RuntimeToolError(
                f"project path {project!r} is outside workspace {self.workspace_root}",
                code="E_PATH_ESCAPE",
            ) from exc
        if resolved == self.workspace_root:
            raise RuntimeToolError(
                "project path must name a file inside the workspace",
                code="E_PATH_ESCAPE",
            )
        return resolved


def _node_id_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    found: list[str] = []
    for item in value:
        if isinstance(item, str) and item.strip():
            found.append(item)
        elif isinstance(item, dict):
            node_id = item.get("node_id") or item.get("id")
            if isinstance(node_id, str) and node_id.strip():
                found.append(node_id)
    return found
