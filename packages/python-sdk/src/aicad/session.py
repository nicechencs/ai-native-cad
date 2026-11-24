"""Talk to one ``.aicad`` project through ``cad-cli`` session, api, query, and verify.

``RuntimeSession`` is the only session type. ``RuntimeClient`` and
``EngineeringSession`` are the same class under deprecated names. They do not
keep a second daemon or in-memory transport.
"""

from __future__ import annotations

import json
import math
import os
import shutil
import subprocess
import tempfile
from collections.abc import Callable, Mapping, Sequence
from numbers import Real
from pathlib import Path
from typing import Any

from aicad.container import save_aicad_directory
from aicad.graph import GraphBuilder

CliRunner = Callable[[Sequence[str], str | None], Any]
DEFAULT_CLI_TIMEOUT: float = 120.0


class AicadProject:
    """A named graph that can be saved as an unpacked ``.aicad`` project."""

    def __init__(self, project_name: str, builder: GraphBuilder) -> None:
        self.project_name = project_name
        self.builder = builder

    def save(
        self,
        path: str | Path,
        *,
        kernel_fingerprint: Mapping[str, str] | None = None,
    ) -> Path:
        """Write the project directory and return its path."""
        return save_aicad_directory(
            Path(path),
            self.project_name,
            self.builder,
            kernel_fingerprint=kernel_fingerprint,
        )


def find_cad_cli(explicit_bin: str | Path | None = None) -> Path | None:
    """Look for the CLI in the explicit argument, environment, checkout, and PATH."""
    for override in (explicit_bin, os.environ.get("CAD_CLI_BIN")):
        if override:
            path = Path(override)
            if path.is_file() and os.access(path, os.X_OK):
                return path.resolve()
            if found := shutil.which(str(override)):
                return Path(found).resolve()

    source = Path(__file__).resolve()
    if source.parent.parts[-4:] == ("packages", "python-sdk", "src", "aicad"):
        target = source.parents[4] / "target"
        executable = "cad-cli.exe" if os.name == "nt" else "cad-cli"
        for profile in ("debug", "release"):
            path = target / profile / executable
            if path.is_file() and os.access(path, os.X_OK):
                return path

    found = shutil.which("cad-cli")
    return Path(found).resolve() if found else None


def _require_cad_cli(explicit_bin: str | Path | None) -> Path:
    binary = find_cad_cli(explicit_bin)
    if binary is None:
        raise FileNotFoundError("cad-cli was not found. Set CAD_CLI_BIN or add it to PATH.")
    return binary


def _json_object(data: str, context: str) -> dict[str, Any]:
    try:
        value = json.loads(data)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"{context} returned invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise RuntimeError(f"{context} did not return a JSON object")
    return value


def _validate_timeout(timeout: float | None) -> float | None:
    if timeout is None:
        return None
    if isinstance(timeout, bool) or not isinstance(timeout, Real):
        raise TypeError("timeout must be a positive real number or None")
    value = float(timeout)
    if not math.isfinite(value) or value <= 0.0:
        raise ValueError("timeout must be a positive finite number")
    return value


def _parse_error_payload(stdout: str) -> dict[str, Any] | None:
    text = stdout.strip()
    if not text:
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None


def _extract_recovery_hints(payload: Mapping[str, Any] | None) -> list[Any]:
    if payload is None:
        return []
    direct = payload.get("recovery_hints")
    if isinstance(direct, list):
        return list(direct)
    if isinstance(direct, Mapping):
        return [dict(direct)]
    message = payload.get("message")
    if isinstance(message, str):
        marker = "recovery_hints="
        idx = message.find(marker)
        if idx != -1:
            candidate = message[idx + len(marker) :].strip()
            try:
                parsed = json.loads(candidate)
            except json.JSONDecodeError:
                parsed = None
            if isinstance(parsed, list):
                return parsed
            if isinstance(parsed, Mapping):
                return [dict(parsed)]
            if candidate:
                return [candidate]
    return []


class CliError(RuntimeError):
    """``cad-cli`` exited non-zero or returned a structured tool error.

    Structured error fields (``.code``, ``.payload``, ``.recovery_hints``) are
    populated whenever ``cad-cli`` emits a JSON error payload.
    """

    def __init__(
        self,
        argv: Sequence[str],
        completed: Any,
        *,
        payload: dict[str, Any] | None = None,
    ) -> None:
        self.argv = [str(part) for part in argv]
        self.returncode = int(completed.returncode)
        self.stdout = completed.stdout or ""
        self.stderr = completed.stderr or ""
        parsed = dict(payload) if isinstance(payload, Mapping) else _parse_error_payload(self.stdout)
        self.payload: dict[str, Any] | None = parsed
        raw_code = parsed.get("code") if isinstance(parsed, dict) else None
        self.code: str | None = str(raw_code) if raw_code is not None and str(raw_code).strip() else None
        self.recovery_hints: list[Any] = _extract_recovery_hints(parsed)
        detail = _cli_detail(self.stdout, self.stderr, parsed=parsed)
        super().__init__(f"cad-cli failed ({self.returncode}): {detail}")


class UnsupportedSemanticTransport(RuntimeError):
    """Kept so older imports still resolve. The runtime no longer swallows CLI errors."""


def _cli_detail(
    stdout: str,
    stderr: str,
    *,
    parsed: dict[str, Any] | None = None,
) -> str:
    text = stdout.strip()
    if parsed is None:
        parsed = _parse_error_payload(stdout)
    parts: list[str] = []
    if isinstance(parsed, dict) and (
        parsed.get("code") or parsed.get("message") or parsed.get("status") == "error"
    ):
        code = parsed.get("code")
        message = parsed.get("message")
        if code and message:
            parts.append(f"{code}: {message}")
        elif message:
            parts.append(str(message))
        elif code:
            parts.append(str(code))
        elif text:
            parts.append(text)
    elif text:
        parts.append(text)
    error_text = stderr.strip()
    if error_text and error_text not in "\n".join(parts):
        parts.append(error_text)
    return "\n".join(parts) if parts else "cad-cli produced no error output"


def _branch_component(raw: str) -> str:
    cleaned: list[str] = []
    for char in raw.strip():
        if char.isascii() and (char.isalnum() or char in "-_."):
            cleaned.append(char)
        else:
            cleaned.append("_")
    text = "".join(cleaned).strip("_")
    return text or "sdk"


def _revision_token(expected_revision: int | None) -> str | None:
    if expected_revision is None:
        return None
    if isinstance(expected_revision, bool) or not isinstance(expected_revision, int):
        raise TypeError("expected_revision must be a non-negative int")
    if expected_revision < 0:
        raise ValueError("expected_revision must be a non-negative int")
    return str(expected_revision)


def _command_batch(commands: Sequence[Mapping[str, Any]]) -> list[dict[str, Any]]:
    if isinstance(commands, (str, bytes)) or not isinstance(commands, Sequence):
        raise TypeError("commands must be a list of SemanticCommand objects")
    batch: list[dict[str, Any]] = []
    for index, command in enumerate(commands):
        if not isinstance(command, Mapping):
            raise TypeError(f"commands[{index}] must be a SemanticCommand object")
        name = command.get("command")
        args = command.get("args")
        if not isinstance(name, str) or not name.strip():
            raise ValueError(f"commands[{index}] is missing a command name")
        if not isinstance(args, Mapping):
            raise ValueError(f"commands[{index}].args must be an object")
        batch.append({"command": name, "args": dict(args)})
    if not batch:
        raise ValueError("commands must contain at least one SemanticCommand")
    return batch


def _branch_name(branch: str, label: str) -> str:
    if not isinstance(branch, str) or not branch.strip():
        raise ValueError(f"{label} cannot be empty")
    return branch


_MAX_INLINE_COMMANDS_BYTES = 8192


class RuntimeSession:
    """One project session driven by ``cad-cli session``, ``api``, ``query``, and ``verify``.

    Pass ``runner`` in tests. It receives ``(argv, stdin)`` and returns an object
    with ``returncode``, ``stdout``, and ``stderr``. A non-zero exit becomes
    :class:`CliError`. A missing binary is reported when no runner is installed.
    """

    def __init__(
        self,
        project: str | Path,
        *,
        workspace: str | Path | None = None,
        cad_cli_bin: str | Path | None = None,
        runner: CliRunner | None = None,
        client: str = "python-sdk",
        session_id: str = "sdk",
        timeout: float | None = DEFAULT_CLI_TIMEOUT,
    ) -> None:
        if not isinstance(client, str) or not client.strip():
            raise ValueError("client cannot be empty")
        if not isinstance(session_id, str) or not session_id.strip():
            raise ValueError("session_id cannot be empty")
        self.project = Path(project)
        self.workspace = Path(workspace) if workspace is not None else Path.cwd()
        self._cad_cli_bin = cad_cli_bin
        self._runner = runner
        self.client = client
        self.session_id = session_id
        self.timeout = _validate_timeout(timeout)
        self.active_candidate: str | None = None

    @property
    def candidate_branch(self) -> str:
        """Preview branch name ``agent/<client>/<session_id>``."""
        return f"agent/{_branch_component(self.client)}/{_branch_component(self.session_id)}"

    def __enter__(self) -> RuntimeSession:
        return self

    def __exit__(self, *exc_info: object) -> None:
        return None

    def status(self) -> dict[str, Any]:
        """Return ``cad-cli session status --json``."""
        return self._invoke(
            [
                "session",
                "status",
                "--project",
                str(self.project),
                "--workspace",
                str(self.workspace),
                "--json",
            ],
            "cad-cli session status",
        )

    def apply(
        self,
        commands: Sequence[Mapping[str, Any]],
        *,
        expected_revision: int | None = None,
        branch: str | None = None,
    ) -> dict[str, Any]:
        """Apply a SemanticCommand batch with ``cad-cli api apply``.

        ``expected_revision`` is sent as ``--expected-revision`` when it is not
        ``None``. ``branch`` selects a candidate or any other branch. When
        ``branch`` is omitted, an open candidate is used if :meth:`open_candidate`
        was called. CLI failures raise :class:`CliError`.
        """
        batch = _command_batch(commands)
        revision = _revision_token(expected_revision)
        target = self.active_candidate if branch is None else _branch_name(branch, "branch")
        serialized = json.dumps(batch, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        temp_path: Path | None = None
        commands_arg = serialized
        if len(serialized.encode("utf-8")) > _MAX_INLINE_COMMANDS_BYTES:
            fd, raw_path = tempfile.mkstemp(prefix="aicad-commands-", suffix=".json")
            temp_path = Path(raw_path)
            try:
                with os.fdopen(fd, "w", encoding="utf-8") as handle:
                    handle.write(serialized)
            except Exception:
                temp_path.unlink(missing_ok=True)
                raise
            commands_arg = f"@{temp_path}"
        argv = [
            "api",
            "apply",
            "--project",
            str(self.project),
            "--workspace",
            str(self.workspace),
            "--commands",
            commands_arg,
        ]
        if revision is not None:
            argv.extend(["--expected-revision", revision])
        if target is not None:
            argv.extend(["--branch", target])
        argv.append("--json")
        try:
            return self._invoke(argv, "cad-cli api apply")
        finally:
            if temp_path is not None:
                temp_path.unlink(missing_ok=True)

    def apply_candidate(
        self,
        commands: Sequence[Mapping[str, Any]],
        *,
        expected_revision: int | None = None,
        branch: str | None = None,
    ) -> dict[str, Any]:
        """Apply onto the candidate branch, defaulting to :attr:`candidate_branch`."""
        target = self.candidate_branch if branch is None else _branch_name(branch, "branch")
        return self.apply(commands, expected_revision=expected_revision, branch=target)

    def query_summary(self) -> dict[str, Any]:
        """Return ``cad-cli query summary --json``."""
        return self._invoke(self._query_prefix() + ["summary"], "cad-cli query summary")

    def explain(self, target: str) -> dict[str, Any]:
        """Return ``cad-cli query explain --target`` for a node or parameter id."""
        if not isinstance(target, str) or not target.strip():
            raise ValueError("explain target cannot be empty")
        return self._invoke(
            self._query_prefix() + ["explain", "--target", target],
            "cad-cli query explain",
        )

    def verify(self) -> dict[str, Any]:
        """Return ``cad-cli verify --json``."""
        return self._invoke(
            [
                "verify",
                "--project",
                str(self.project),
                "--workspace",
                str(self.workspace),
                "--json",
            ],
            "cad-cli verify",
        )

    def _branch_project_arg(self) -> str:
        if self.project.is_absolute():
            return str(self.project)
        return str(self.workspace / self.project)

    def create_candidate(self, name: str | None = None, *, from_branch: str = "main") -> dict[str, Any]:
        """Create an ``ai_candidate`` branch with ``cad-cli project branch create``."""
        branch = self.candidate_branch if name is None else _branch_name(name, "candidate branch")
        source = _branch_name(from_branch, "from_branch")
        return self._invoke(
            [
                "project",
                "branch",
                self._branch_project_arg(),
                "create",
                branch,
                "--from",
                source,
                "--purpose",
                "ai_candidate",
                "--json",
            ],
            "cad-cli project branch create",
        )

    def list_branches(self) -> dict[str, Any]:
        """Return ``cad-cli project branch list --json``."""
        return self._invoke(
            ["project", "branch", self._branch_project_arg(), "list", "--json"],
            "cad-cli project branch list",
        )

    def checkout_branch(self, branch: str) -> dict[str, Any]:
        """Switch the active branch with ``cad-cli project branch checkout``."""
        name = _branch_name(branch, "branch")
        return self._invoke(
            ["project", "branch", self._branch_project_arg(), "checkout", name, "--json"],
            "cad-cli project branch checkout",
        )

    def open_candidate(self, name: str | None = None, *, from_branch: str = "main") -> dict[str, Any]:
        """Create a candidate branch and use it for later :meth:`apply` calls."""
        branch = self.candidate_branch if name is None else _branch_name(name, "candidate branch")
        report = self.create_candidate(branch, from_branch=from_branch)
        self.active_candidate = branch
        return report

    def undo(self) -> dict[str, Any]:
        """Step back one edit in the project history via ``cad_branch_control``."""
        return self._invoke_mcp_tool(
            "cad_branch_control",
            {"action": "undo"},
            "cad-cli mcp cad_branch_control(undo)",
        )

    def redo(self) -> dict[str, Any]:
        """Re-apply one undone edit via ``cad_branch_control``.

        Note: ``DocumentSession`` keeps the redo stack in memory per process, so
        one-shot headless invocations raise :class:`CliError` with
        ``code == "E_NOTHING_TO_REDO"`` when no live session holds undone edits.
        """
        return self._invoke_mcp_tool(
            "cad_branch_control",
            {"action": "redo"},
            "cad-cli mcp cad_branch_control(redo)",
        )

    def diff_branch(self, branch: str | None = None) -> dict[str, Any]:
        """Diff a preview/candidate branch against ``main`` via ``cad_branch_control``."""
        target = self._resolve_preview_branch(branch)
        return self._invoke_mcp_tool(
            "cad_branch_control",
            {"action": "diff", "branch": target},
            "cad-cli mcp cad_branch_control(diff)",
        )

    def request_review(self, branch: str | None = None) -> dict[str, Any]:
        """Build a review diff payload for a preview branch via ``cad_branch_control``."""
        args: dict[str, Any] = {"action": "request_review"}
        if branch is not None:
            args["branch"] = _branch_name(branch, "branch")
        elif self.active_candidate is not None:
            args["branch"] = self.active_candidate
        return self._invoke_mcp_tool(
            "cad_branch_control",
            args,
            "cad-cli mcp cad_branch_control(request_review)",
        )

    def commit_preview(self, branch: str | None = None) -> dict[str, Any]:
        """Commit a preview branch onto ``main`` via ``cad_branch_control``."""
        target = self._resolve_preview_branch(branch)
        report = self._invoke_mcp_tool(
            "cad_branch_control",
            {"action": "commit_preview", "branch": target},
            "cad-cli mcp cad_branch_control(commit_preview)",
        )
        if self.active_candidate == target:
            self.active_candidate = None
        return report

    def discard_preview(self, branch: str | None = None) -> dict[str, Any]:
        """Discard a preview branch via ``cad_branch_control``."""
        target = self._resolve_preview_branch(branch)
        report = self._invoke_mcp_tool(
            "cad_branch_control",
            {"action": "discard_preview", "branch": target},
            "cad-cli mcp cad_branch_control(discard_preview)",
        )
        if self.active_candidate == target:
            self.active_candidate = None
        return report

    def export_artifacts(
        self,
        format: str,
        path: str | Path,
        *,
        part_label: str | None = None,
    ) -> dict[str, Any]:
        """Export STEP, 3MF, DXF, or SVG inside the workspace via ``cad_export_artifacts``."""
        if not isinstance(format, str) or not format.strip():
            raise ValueError("export format cannot be empty")
        out_path = str(path).strip()
        if not out_path:
            raise ValueError("export path cannot be empty")
        args: dict[str, Any] = {"format": format.strip(), "path": out_path}
        if part_label is not None:
            args["part_label"] = part_label
        return self._invoke_mcp_tool(
            "cad_export_artifacts",
            args,
            "cad-cli mcp cad_export_artifacts",
        )

    def _resolve_preview_branch(self, branch: str | None) -> str:
        if branch is not None:
            return _branch_name(branch, "branch")
        if self.active_candidate is not None:
            return self.active_candidate
        return self.candidate_branch

    def _query_prefix(self) -> list[str]:
        return [
            "query",
            "--project",
            str(self.project),
            "--workspace",
            str(self.workspace),
            "--json",
        ]

    def _binary(self) -> str:
        if self._runner is not None:
            if self._cad_cli_bin is not None:
                return str(self._cad_cli_bin)
            found = find_cad_cli(None)
            return str(found) if found is not None else "cad-cli"
        return str(_require_cad_cli(self._cad_cli_bin))

    def _run_subprocess(self, argv: Sequence[str], stdin: str | None) -> Any:
        if self._runner is not None:
            return self._runner(argv, stdin)
        return subprocess.run(
            argv,
            input=stdin,
            capture_output=True,
            check=False,
            encoding="utf-8",
            timeout=self.timeout,
        )

    def _invoke(self, args: Sequence[str], context: str) -> dict[str, Any]:
        argv = [self._binary(), *args]
        completed = self._run_subprocess(argv, None)
        if int(completed.returncode) != 0:
            raise CliError(argv, completed)
        return _json_object(completed.stdout or "", context)

    def _invoke_mcp_tool(
        self,
        tool_name: str,
        arguments: Mapping[str, Any],
        context: str,
    ) -> dict[str, Any]:
        argv = [
            self._binary(),
            "mcp",
            "--transport",
            "stdio",
            "--headless",
            "--project",
            str(self.project),
            "--workspace",
            str(self.workspace),
        ]
        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool_name, "arguments": dict(arguments)},
        }
        stdin = json.dumps(request, ensure_ascii=False, allow_nan=False, separators=(",", ":")) + "\n"
        completed = self._run_subprocess(argv, stdin)
        if int(completed.returncode) != 0:
            raise CliError(argv, completed)
        lines = [line for line in (completed.stdout or "").splitlines() if line.strip()]
        if not lines:
            raise RuntimeError(f"{context} produced no JSON-RPC response")
        envelope = _json_object(lines[-1], context)
        if "error" in envelope and envelope["error"] is not None:
            err_obj = envelope["error"]
            payload = err_obj if isinstance(err_obj, dict) else {"message": str(err_obj)}
            raise CliError(argv, completed, payload=payload)
        result = envelope.get("result")
        if not isinstance(result, dict):
            raise RuntimeError(f"{context} returned an invalid MCP result envelope")
        content = result.get("content")
        text = ""
        if isinstance(content, list) and content and isinstance(content[0], dict):
            raw_text = content[0].get("text")
            if isinstance(raw_text, str):
                text = raw_text
        payload = _json_object(text, context)
        if result.get("isError") is True:
            raise CliError(argv, completed, payload=payload)
        return payload


# Deprecated names. They are RuntimeSession, not a second implementation.
RuntimeClient = RuntimeSession
EngineeringSession = RuntimeSession


def export_cadpack(
    project_path: str | Path,
    out_path: str | Path,
    *,
    cad_cli_bin: str | Path | None = None,
    timeout: float | None = DEFAULT_CLI_TIMEOUT,
) -> dict[str, Any]:
    """Export a project to a CADPack and return the CLI's JSON report."""
    validated_timeout = _validate_timeout(timeout)
    argv = [
        str(_require_cad_cli(cad_cli_bin)),
        "project",
        "export-cadpack",
        str(project_path),
        "--out",
        str(out_path),
        "--json",
    ]
    completed = subprocess.run(
        argv,
        capture_output=True,
        check=False,
        encoding="utf-8",
        timeout=validated_timeout,
    )
    if completed.returncode != 0:
        raise CliError(argv, completed)
    return _json_object(completed.stdout, "cad-cli project export-cadpack")
