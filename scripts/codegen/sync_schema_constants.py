#!/usr/bin/env python3
"""Synchronize and verify cross-language constants generated from ``schemas/contracts-ssot.json``.

Targets:
- ``apps/studio-ui/src/types/schemaConstants.ts``
- ``packages/python-sdk/src/aicad/_schema_constants.py``
"""

from __future__ import annotations

import argparse
import difflib
import json
import re
import sys
from pathlib import Path
from typing import Any

SSOT_REL_PATH = Path("schemas") / "contracts-ssot.json"
TS_TARGET_REL_PATH = Path("apps") / "studio-ui" / "src" / "types" / "schemaConstants.ts"
PY_TARGET_REL_PATH = Path("packages") / "python-sdk" / "src" / "aicad" / "_schema_constants.py"

_REQUIRED_VERSIONS = (
    "IR_SCHEMA_VERSION",
    "CONTAINER_FORMAT_VERSION",
    "VERIFICATION_REPORT_SCHEMA_VERSION",
)
_VALID_LAYERS = frozenset({"L0", "L1", "L2", "L3", "L4"})
_VALID_EXIT_STATUSES = frozenset({"ok", "error"})
_KEY_RE = re.compile(r"^[A-Z][A-Z0-9_]*$")
_DIAG_CODE_RE = re.compile(r"^E(?:[0-9]{3})?_[A-Z0-9_]+$")
_RUNTIME_CODE_RE = re.compile(r"^R[0-9]{3}_[A-Z0-9_]+$")


def _is_non_bool_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _is_non_empty_str(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def load_contracts_ssot(repo_root: Path) -> tuple[dict[str, Any] | None, list[str]]:
    """Load and validate ``schemas/contracts-ssot.json``."""
    ssot_path = repo_root / SSOT_REL_PATH
    if not ssot_path.is_file():
        return None, [f"missing {SSOT_REL_PATH.as_posix()}"]

    try:
        data = json.loads(ssot_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return None, [f"{SSOT_REL_PATH.as_posix()} is invalid JSON: {exc}"]

    errors = validate_contracts_ssot(data)
    if errors:
        return None, errors
    return data, []


def validate_contracts_ssot(data: object) -> list[str]:
    """Validate structure and uniqueness of ``schemas/contracts-ssot.json``."""
    prefix = SSOT_REL_PATH.as_posix()
    if not isinstance(data, dict):
        return [f"{prefix}: top-level value must be a JSON object"]

    errors: list[str] = []

    versions = data.get("versions")
    if not isinstance(versions, dict):
        errors.append(f"{prefix}: `versions` must be an object")
    else:
        for key in _REQUIRED_VERSIONS:
            val = versions.get(key)
            if not _is_non_bool_int(val) or val < 1:
                errors.append(f"{prefix}: `versions.{key}` must be an integer >= 1")
        for key, val in versions.items():
            if not isinstance(key, str) or not _KEY_RE.match(key):
                errors.append(f"{prefix}: invalid version key `{key}`")
            elif not _is_non_bool_int(val) or val < 1:
                errors.append(f"{prefix}: `versions.{key}` must be an integer >= 1")

    errors.extend(_validate_cli_exit_codes(data.get("cli_exit_codes"), prefix))
    errors.extend(
        _validate_code_entries(
            data.get("diagnostic_codes"),
            prefix=prefix,
            section="diagnostic_codes",
            code_re=_DIAG_CODE_RE,
            require_layer=True,
        )
    )
    errors.extend(
        _validate_code_entries(
            data.get("runtime_codes"),
            prefix=prefix,
            section="runtime_codes",
            code_re=_RUNTIME_CODE_RE,
            require_layer=False,
        )
    )
    return errors


def _validate_cli_exit_codes(entries: object, prefix: str) -> list[str]:
    if not isinstance(entries, list) or not entries:
        return [f"{prefix}: `cli_exit_codes` must be a non-empty list"]

    errors: list[str] = []
    seen_keys: set[str] = set()
    seen_codes: set[int] = set()

    for idx, entry in enumerate(entries):
        if not isinstance(entry, dict):
            errors.append(f"{prefix}: cli_exit_codes[{idx}] must be an object")
            continue
        key = entry.get("key")
        code = entry.get("code")
        status = entry.get("status")
        description = entry.get("description")

        if not isinstance(key, str) or not _KEY_RE.match(key):
            errors.append(f"{prefix}: cli_exit_codes[{idx}].key `{key}` is invalid")
        elif key in seen_keys:
            errors.append(f"{prefix}: duplicate cli_exit_codes key `{key}`")
        else:
            seen_keys.add(key)

        if not _is_non_bool_int(code) or code < 0 or code > 255:
            errors.append(f"{prefix}: cli_exit_codes[{idx}].code must be an integer in 0..255")
        elif code in seen_codes:
            errors.append(f"{prefix}: duplicate cli_exit_codes code `{code}`")
        else:
            seen_codes.add(code)

        if status not in _VALID_EXIT_STATUSES:
            errors.append(f"{prefix}: cli_exit_codes[{idx}].status must be 'ok' or 'error'")

        if not _is_non_empty_str(description):
            errors.append(f"{prefix}: cli_exit_codes[{idx}].description must be a non-empty string")

    return errors


def _validate_code_entries(
    entries: object,
    *,
    prefix: str,
    section: str,
    code_re: re.Pattern[str],
    require_layer: bool,
) -> list[str]:
    if not isinstance(entries, list) or not entries:
        return [f"{prefix}: `{section}` must be a non-empty list"]

    errors: list[str] = []
    seen_codes: set[str] = set()

    for idx, entry in enumerate(entries):
        if not isinstance(entry, dict):
            errors.append(f"{prefix}: {section}[{idx}] must be an object")
            continue

        code = entry.get("code")
        if not isinstance(code, str) or not code_re.match(code):
            errors.append(f"{prefix}: {section}[{idx}].code `{code}` is invalid")
        elif code in seen_codes:
            errors.append(f"{prefix}: duplicate {section} code `{code}`")
        else:
            seen_codes.add(code)

        if require_layer:
            if not _is_non_empty_str(entry.get("category")):
                errors.append(f"{prefix}: {section}[{idx}].category must be a non-empty string")
            if entry.get("layer") not in _VALID_LAYERS:
                errors.append(f"{prefix}: {section}[{idx}].layer must be one of {sorted(_VALID_LAYERS)}")

        if not _is_non_empty_str(entry.get("description")):
            errors.append(f"{prefix}: {section}[{idx}].description must be a non-empty string")

    return errors


def render_typescript(data: dict[str, Any]) -> str:
    """Render ``apps/studio-ui/src/types/schemaConstants.ts`` from validated SSOT data."""
    versions: dict[str, int] = data["versions"]
    exit_codes: list[dict[str, Any]] = data["cli_exit_codes"]
    diag_codes: list[dict[str, Any]] = data["diagnostic_codes"]
    runtime_codes: list[dict[str, Any]] = data["runtime_codes"]

    lines: list[str] = [
        "// AUTO-GENERATED from schemas/contracts-ssot.json by scripts/codegen/sync_schema_constants.py --write.",
        "// Do not edit manually.",
        "",
    ]

    for key, val in versions.items():
        lines.append(f"export const {key} = {val} as const;")

    lines.extend(
        [
            "",
            "export const CLI_EXIT_CODES = {",
        ]
    )
    for item in exit_codes:
        lines.append(f"  {item['key']}: {item['code']},")
    lines.extend(
        [
            "} as const;",
            "",
            "export type CliExitCode = (typeof CLI_EXIT_CODES)[keyof typeof CLI_EXIT_CODES];",
            "",
            "export const DIAGNOSTIC_CODES = {",
        ]
    )
    for item in diag_codes:
        code = item["code"]
        lines.append(f'  {code}: "{code}",')
    lines.extend(
        [
            "} as const;",
            "",
            "export type DiagnosticCode = (typeof DIAGNOSTIC_CODES)[keyof typeof DIAGNOSTIC_CODES];",
            "",
            "export const ALL_DIAGNOSTIC_CODES: readonly DiagnosticCode[] = [",
        ]
    )
    for item in diag_codes:
        lines.append(f"  DIAGNOSTIC_CODES.{item['code']},")
    lines.extend(
        [
            "] as const;",
            "",
            "export const RUNTIME_CODES = {",
        ]
    )
    for item in runtime_codes:
        code = item["code"]
        lines.append(f'  {code}: "{code}",')
    lines.extend(
        [
            "} as const;",
            "",
            "export type RuntimeCode = (typeof RUNTIME_CODES)[keyof typeof RUNTIME_CODES];",
            "",
            "export const ALL_RUNTIME_CODES: readonly RuntimeCode[] = [",
        ]
    )
    for item in runtime_codes:
        lines.append(f"  RUNTIME_CODES.{item['code']},")
    lines.extend(["] as const;", ""])

    return "\n".join(lines)


def render_python(data: dict[str, Any]) -> str:
    """Render ``packages/python-sdk/src/aicad/_schema_constants.py`` from validated SSOT data."""
    versions: dict[str, int] = data["versions"]
    exit_codes: list[dict[str, Any]] = data["cli_exit_codes"]
    diag_codes: list[dict[str, Any]] = data["diagnostic_codes"]
    runtime_codes: list[dict[str, Any]] = data["runtime_codes"]

    lines: list[str] = [
        '"""Auto-generated cross-language schema constants from ``schemas/contracts-ssot.json``.',
        "",
        "Generated by ``scripts/codegen/sync_schema_constants.py --write``. Do not edit manually.",
        '"""',
        "",
        "from __future__ import annotations",
        "",
        "from types import MappingProxyType",
        "from typing import Final",
        "",
    ]

    for key, val in versions.items():
        lines.append(f"{key}: Final[int] = {val}")

    lines.extend(
        [
            "",
            "CLI_EXIT_CODES: Final[MappingProxyType[str, int]] = MappingProxyType(",
            "    {",
        ]
    )
    for item in exit_codes:
        lines.append(f'        "{item["key"]}": {item["code"]},')
    lines.extend(
        [
            "    }",
            ")",
            "",
            "DIAGNOSTIC_CODES: Final[tuple[str, ...]] = (",
        ]
    )
    for item in diag_codes:
        lines.append(f'    "{item["code"]}",')
    lines.extend(
        [
            ")",
            "DIAGNOSTIC_CODE_SET: Final[frozenset[str]] = frozenset(DIAGNOSTIC_CODES)",
            "",
            "RUNTIME_CODES: Final[tuple[str, ...]] = (",
        ]
    )
    for item in runtime_codes:
        lines.append(f'    "{item["code"]}",')
    lines.extend(
        [
            ")",
            "RUNTIME_CODE_SET: Final[frozenset[str]] = frozenset(RUNTIME_CODES)",
            "",
        ]
    )

    return "\n".join(lines)


def render_outputs(repo_root: Path) -> tuple[dict[Path, str] | None, list[str]]:
    """Load SSOT and return expected target file contents keyed by relative Path."""
    data, errors = load_contracts_ssot(repo_root)
    if errors or data is None:
        return None, errors
    return {
        TS_TARGET_REL_PATH: render_typescript(data),
        PY_TARGET_REL_PATH: render_python(data),
    }, []


def check_synced(repo_root: Path) -> list[str]:
    """Verify generated TypeScript and Python constant files match ``schemas/contracts-ssot.json``."""
    targets, errors = render_outputs(repo_root)
    if errors or targets is None:
        return errors

    drift: list[str] = []
    for rel_path, expected in targets.items():
        full_path = repo_root / rel_path
        rel_posix = rel_path.as_posix()
        if not full_path.is_file():
            drift.append(
                f"missing generated file `{rel_posix}` "
                "(run `python scripts/codegen/sync_schema_constants.py --write`)"
            )
            continue

        actual = full_path.read_text(encoding="utf-8")
        if actual != expected:
            diff_lines = list(
                difflib.unified_diff(
                    actual.splitlines(),
                    expected.splitlines(),
                    fromfile=f"{rel_posix} (actual)",
                    tofile=f"{rel_posix} (expected)",
                    lineterm="",
                )
            )
            preview = "\n".join(diff_lines[:20])
            drift.append(
                f"generated file `{rel_posix}` is out of sync with {SSOT_REL_PATH.as_posix()} "
                f"(run `python scripts/codegen/sync_schema_constants.py --write`):\n{preview}"
            )
    return drift


def write_synced(repo_root: Path) -> list[str]:
    """Write generated TypeScript and Python constant files from ``schemas/contracts-ssot.json``."""
    targets, errors = render_outputs(repo_root)
    if errors or targets is None:
        return errors

    for rel_path, content in targets.items():
        full_path = repo_root / rel_path
        full_path.parent.mkdir(parents=True, exist_ok=True)
        full_path.write_text(content, encoding="utf-8", newline="\n")
    return []


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Synchronize or check cross-language schema constants from schemas/contracts-ssot.json."
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--check",
        action="store_true",
        help="Verify generated files match schemas/contracts-ssot.json without writing.",
    )
    mode.add_argument(
        "--write",
        action="store_true",
        help="Regenerate TypeScript and Python constant files from schemas/contracts-ssot.json.",
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="Repository root directory (defaults to checkout root).",
    )
    args = parser.parse_args(argv)
    repo_root = args.root.resolve()

    if args.write:
        errors = write_synced(repo_root)
        if errors:
            for err in errors:
                print(err, file=sys.stderr)
            return 1
        print(
            "Synchronized schema constants to "
            f"{TS_TARGET_REL_PATH.as_posix()} and {PY_TARGET_REL_PATH.as_posix()}."
        )
        return 0

    errors = check_synced(repo_root)
    if errors:
        print("Schema constants sync check failed:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1
    print("Schema constants are in sync with schemas/contracts-ssot.json.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
