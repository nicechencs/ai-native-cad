"""Command-line entry point for `python -m cad_agent.cli`."""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
import time
import urllib.error
from collections.abc import Callable, Iterable
from dataclasses import replace
from pathlib import Path
from typing import Any

from . import observe
from .agents import (
    CadCoderAgent,
    SelfRefinementPipeline,
    SpecParserAgent,
    VisualGeometricCritic,
)
from .compiler_bridge import CompilerBridge, CompilerBridgeError, load_shaded_views_with_masks
from .contracts import (
    EXTERNAL_LENGTH_UNITS,
    ContractValidationError,
    DesignContract,
    ExternalInterfaceSummary,
    VerificationReport,
)
from .llm import LLMConfig, LLMConfigError, LLMRequestError, create_llm_client
from .llm_semantic_planner import LLMSemanticPlanner
from .prompt_support import UnsupportedFeaturesError
from .runtime_client import RuntimeToolClient, RuntimeToolError, SubprocessCliTransport
from .semantic_agent import (
    EngineeringSemanticAgent,
    HeuristicSemanticPlanner,
    SemanticCommandError,
    unknown_external_interference,
)
from .v2_context_adapter import ContextAssetRecord, V2ContextAdapter, V2ContextAdapterError
from .vision import VLMViewInspector, VisualReviewResult, create_view_inspector, review_offscreen_semantic_masks


RUNTIME_AUTO = "auto"
RUNTIME_SEMANTIC_V2 = "semantic-v2"
RUNTIME_KCL_LEGACY = "kcl-legacy"
RUNTIME_ENV_VAR = "CAD_AGENT_RUNTIME"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="python -m cad_agent.cli",
        description=(
            "Run the AI-Native CAD multi-agent CLI. Supports the first-class V2 semantic "
            "runtime (GraphEvaluator / .aicad via --runtime semantic-v2, --semantic-v2, or "
            "--project) as well as the legacy KCL self-refinement loop."
        ),
    )
    parser.add_argument(
        "--prompt",
        type=str,
        default=None,
        help="Natural-language mechanical design prompt or incremental edit instruction",
    )
    parser.add_argument(
        "--contract-json",
        type=str,
        default=None,
        help="Path to a DesignContract JSON file or inline JSON string",
    )
    parser.add_argument(
        "--material",
        type=str,
        default=None,
        help="Override material designation (e.g. Al-6061-T6)",
    )
    parser.add_argument(
        "--manufacturing-method",
        type=str,
        default=None,
        help="Override manufacturing method (e.g. CNC_3AXIS_MILLING)",
    )
    parser.add_argument(
        "--provider",
        choices=["auto", "template", "openai", "gemini", "deterministic"],
        default="auto",
        help=(
            "LLM provider. The V2 semantic runtime uses OpenAI or Gemini when a key is "
            "configured. 'deterministic' selects the offline heuristic planner. Default: auto"
        ),
    )
    parser.add_argument(
        "--model",
        type=str,
        default=None,
        help="Override LLM model name",
    )
    parser.add_argument(
        "--base-url",
        type=str,
        default=None,
        help="Override OpenAI-compatible API base URL",
    )
    parser.add_argument(
        "--api-key",
        type=str,
        default=None,
        help="Optional API key (prefer OPENAI_API_KEY / GEMINI_API_KEY environment variables)",
    )
    parser.add_argument(
        "--mode",
        choices=["generate", "edit"],
        default="generate",
        help="Execution mode: generate new part or incrementally edit existing KCL (default: generate)",
    )
    parser.add_argument(
        "--existing-kcl",
        type=Path,
        default=None,
        help="Path to existing .kcl file when --mode=edit",
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=None,
        help="Output directory for generated .kcl, .step, .mesh.json, .dxf, and views",
    )
    parser.add_argument(
        "--workspace-root",
        type=Path,
        help="Trusted root for referenced CAD assets; required with --context-asset",
    )
    parser.add_argument(
        "--context-asset",
        type=Path,
        help="Reference asset path under --workspace-root, for mounting geometry and clearance",
    )
    parser.add_argument(
        "--context-source-unit",
        choices=EXTERNAL_LENGTH_UNITS,
        help="Optional source-unit override passed to cad-cli inspect",
    )
    parser.add_argument(
        "--cli-bin",
        type=str,
        default=None,
        help="Explicit path to `cad-cli` binary",
    )
    parser.add_argument(
        "--max-iters",
        type=int,
        default=3,
        help="Maximum self-refinement iterations (default: 3)",
    )
    parser.add_argument(
        "--no-drawings",
        action="store_true",
        help="Skip --out-dxf and --out-drawing arguments when invoking cad-cli build",
    )
    parser.add_argument(
        "--enable-vlm",
        action="store_true",
        help="Enable VLM 4-view visual-geometric review when a real LLM client is configured",
    )
    parser.add_argument(
        "--strict-vlm",
        action="store_true",
        help="Fail visual review if VLM is unavailable or returns an error (requires --enable-vlm)",
    )
    parser.add_argument(
        "--few-shot-k",
        type=int,
        default=2,
        help="Number of retrieved golden .kcl examples to inject into LLM prompts (0 disables, default: 2)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print structured JSON result to stdout",
    )
    parser.add_argument(
        "--progress-stderr",
        action="store_true",
        help="Emit real-time NDJSON progress events prefixed with CAD_AGENT_PROGRESS: to stderr",
    )
    v2_group = parser.add_argument_group(
        "V2 Semantic Runtime (GraphEvaluator / .aicad)",
        "First-class semantic command and verification path backed by cad-cli session/api/query/verify.",
    )
    v2_group.add_argument(
        "--runtime",
        choices=[RUNTIME_AUTO, RUNTIME_SEMANTIC_V2, RUNTIME_KCL_LEGACY],
        default=RUNTIME_AUTO,
        help=(
            "Execution runtime: 'semantic-v2' (GraphEvaluator / .aicad semantic runtime), "
            "'kcl-legacy' (KCL self-refinement loop), or 'auto' (routes to semantic-v2 by "
            "default unless CAD_AGENT_RUNTIME=kcl-legacy is set; default: auto)"
        ),
    )
    v2_group.add_argument(
        "--semantic-v2",
        action="store_true",
        help="Alias for --runtime=semantic-v2 (run the V2 GraphEvaluator semantic runtime)",
    )
    v2_group.add_argument(
        "--project",
        type=str,
        default=None,
        help="Project container path inside --workspace-root (auto-selects V2 semantic runtime)",
    )
    v2_group.add_argument(
        "--target",
        type=str,
        default=None,
        help="TopoRef or feature node id for a V2 point-and-prompt edit",
    )
    v2_group.add_argument(
        "--preview-branch",
        type=str,
        default=None,
        help="Preview branch for V2 apply. Defaults to agent/cad-agent/<session-id>",
    )
    v2_group.add_argument(
        "--max-repairs",
        type=int,
        default=3,
        help="Extra FAIL repair rounds for --semantic-v2 (default: 3). UNKNOWN does not spend a round",
    )
    v2_group.add_argument(
        "--session-id",
        type=str,
        default=None,
        help="Session id used to name the V2 preview branch",
    )
    return parser.parse_args(argv)


def resolve_runtime_path(args: argparse.Namespace) -> str:
    """Resolve whether CLI execution routes to `semantic_v2` or `kcl_legacy`."""
    runtime = getattr(args, "runtime", RUNTIME_AUTO) or RUNTIME_AUTO
    semantic_v2_flag = bool(getattr(args, "semantic_v2", False))
    if semantic_v2_flag and runtime == RUNTIME_KCL_LEGACY:
        raise ValueError("--semantic-v2 cannot be combined with --runtime kcl-legacy")
    if semantic_v2_flag or runtime == RUNTIME_SEMANTIC_V2:
        return "semantic_v2"
    if runtime == RUNTIME_KCL_LEGACY:
        return "kcl_legacy"

    env_runtime = os.environ.get(RUNTIME_ENV_VAR, "").strip().lower()
    if env_runtime:
        if env_runtime in {"semantic-v2", "semantic_v2", "v2"}:
            return "semantic_v2"
        if env_runtime in {"kcl-legacy", "kcl_legacy", "legacy"}:
            return "kcl_legacy"
        raise ValueError(
            f"Invalid {RUNTIME_ENV_VAR}={env_runtime!r}: expected 'semantic-v2' or 'kcl-legacy'"
        )
    return "semantic_v2"


def _load_contract_json(raw_arg: str) -> DesignContract:
    stripped = raw_arg.strip()
    candidate_path = Path(stripped)
    if candidate_path.is_file():
        data = json.loads(candidate_path.read_text(encoding="utf-8"))
    else:
        data = json.loads(stripped)
    return DesignContract.from_dict(data)


def _load_contract(
    args: argparse.Namespace, spec_parser: SpecParserAgent,
    external_context: ExternalInterfaceSummary | None = None,
) -> DesignContract:
    if args.contract_json:
        contract = replace(_load_contract_json(args.contract_json), external_context=external_context)
    elif args.prompt:
        contract = (
            spec_parser.parse(args.prompt, external_context=external_context)
            if external_context is not None else spec_parser.parse(args.prompt)
        )
    else:
        raise ValueError("Either --prompt or --contract-json must be provided")

    if args.material:
        contract.material = args.material
    if args.manufacturing_method:
        contract.manufacturing_method = args.manufacturing_method
    contract.assert_valid()
    return contract


def _read_optional_text(path: Path | None) -> str | None:
    if path is not None and path.is_file():
        return path.read_text(encoding="utf-8")
    return None


PROGRESS_STDERR_PREFIX = "CAD_AGENT_PROGRESS:"

_CLI_SECRET_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"sk-[A-Za-z0-9_\-]{8,}"),
    re.compile(r"AIza[0-9A-Za-z_\-]{16,}"),
    re.compile(r"(Bearer\s+)[A-Za-z0-9._\-]{8,}", re.IGNORECASE),
    re.compile(r"(api[_-]?key\s*[=:]\s*)[^\s,;\"']+", re.IGNORECASE),
)


def _sanitize_error_message(message: str, secrets: Iterable[str | None]) -> str:
    sanitized = message
    for secret in secrets:
        if secret and secret.strip():
            sanitized = sanitized.replace(secret.strip(), "***")
    for pattern in _CLI_SECRET_PATTERNS:
        if pattern.groups:
            sanitized = pattern.sub(r"\1***", sanitized)
        else:
            sanitized = pattern.sub("***", sanitized)
    return sanitized


def _sanitize_progress_value(value: Any, secrets: tuple[str | None, ...]) -> Any:
    if isinstance(value, str):
        return _sanitize_error_message(value, secrets)
    if isinstance(value, list):
        return [_sanitize_progress_value(item, secrets) for item in value]
    if isinstance(value, dict):
        return {k: _sanitize_progress_value(v, secrets) for k, v in value.items()}
    return value


def _progress_secrets(args: argparse.Namespace, llm_client: Any) -> tuple[str | None, ...]:
    return (
        args.api_key,
        os.environ.get("OPENAI_API_KEY"),
        os.environ.get("GEMINI_API_KEY"),
        getattr(llm_client, "api_key", None) if llm_client is not None else None,
    )


def _stderr_progress_callback(
    args: argparse.Namespace,
    llm_client: Any,
) -> Callable[[dict[str, Any]], None] | None:
    """Write compact progress JSON to stderr. Stdout stays reserved for the result."""
    if not args.progress_stderr:
        return None
    secrets = _progress_secrets(args, llm_client)

    def emit(event: dict[str, Any]) -> None:
        safe_event = _sanitize_progress_value(event, secrets)
        line = (
            f"{PROGRESS_STDERR_PREFIX}"
            f"{json.dumps(safe_event, ensure_ascii=False, separators=(',', ':'))}\n"
        )
        sys.stderr.write(line)
        sys.stderr.flush()

    return emit


def _emit_cli_progress(
    callback: Callable[[dict[str, Any]], None] | None,
    stage: str,
    message: str,
    **extra: Any,
) -> None:
    if callback is None:
        return
    event: dict[str, Any] = {"event": "progress", "stage": stage, "message": message}
    event.update(extra)
    callback(event)


def main(
    argv: list[str] | None = None,
    *,
    semantic_agent_factory: Callable[..., Any] | None = None,
    context_runner: Callable[..., Any] | None = None,
    offscreen_renderer: Callable[..., Any] | None = None,
) -> int:
    args = parse_args(argv)
    try:
        runtime_path = resolve_runtime_path(args)
    except ValueError as exc:
        err_payload = {
            "passed": False,
            "trace_id": observe.current_trace_id(),
            "runtime_path": "unknown",
            "mode": getattr(args, "mode", "generate"),
            "error": str(exc),
        }
        if getattr(args, "json", False):
            print(json.dumps(err_payload, indent=2, ensure_ascii=False))
        else:
            print(f"[cad-agent] Error: {exc}", file=sys.stderr)
        return 1
    if runtime_path == "semantic_v2":
        op = "cli.semantic_v2"
        with observe.trace_span("cad-agent", op):
            return _run_semantic_v2(
                args,
                agent_factory=semantic_agent_factory,
                context_runner=context_runner,
                offscreen_renderer=offscreen_renderer,
            )
    op = f"cli.{args.mode}"
    with observe.trace_span("cad-agent", op):
        return _run_main(args, op)


def _run_main(args: argparse.Namespace, op: str) -> int:
    start = time.perf_counter()
    observe.log("info", "cad-agent", op, f"command start mode={args.mode}")
    contract: DesignContract | None = None
    llm_client: Any = None
    spec_parser: SpecParserAgent | None = None
    cad_coder: CadCoderAgent | None = None
    llm_info: dict[str, Any] = {
        "provider": "template",
        "model": None,
        "base_url": None,
        "used_llm": False,
    }

    try:
        if args.context_source_unit and args.context_asset is None:
            raise ValueError("--context-source-unit requires --context-asset")
        if args.context_asset is not None and args.workspace_root is None:
            raise ValueError("--context-asset requires an explicit --workspace-root")
        bridge = (
            CompilerBridge(cli_bin=args.cli_bin, workspace_root=args.workspace_root)
            if args.workspace_root is not None else None
        )
        external_context = (
            bridge.inspect_external_asset(args.context_asset, source_unit=args.context_source_unit)
            if bridge is not None and args.context_asset is not None else None
        )
        llm_client, llm_info = create_llm_client(
            LLMConfig(
                provider=args.provider,
                model=args.model,
                base_url=args.base_url,
                api_key=args.api_key,
            )
        )
        spec_parser = SpecParserAgent(llm_client=llm_client)
        view_inspector = create_view_inspector(
            llm_client,
            enable_vlm=args.enable_vlm,
            strict_vlm=args.strict_vlm,
        )
        few_shot_k = max(0, args.few_shot_k)

        progress_callback = _stderr_progress_callback(args, llm_client)

        if args.mode == "edit":
            if args.existing_kcl is None:
                raise ValueError("--existing-kcl is required when --mode=edit")
            if not args.existing_kcl.is_file():
                raise ValueError(f"Existing KCL file not found: {args.existing_kcl}")
            if not args.prompt or not args.prompt.strip():
                raise ValueError("--prompt is required as the edit instruction when --mode=edit")
            existing_code = args.existing_kcl.read_text(encoding="utf-8")
            if llm_client is None:
                raise LLMConfigError(
                    "Incremental KCL editing (--mode=edit) requires a configured real LLM provider "
                    "(e.g. OpenAI or Gemini). Set OPENAI_API_KEY or GEMINI_API_KEY instead of template mode."
                )
            if args.contract_json:
                contract = _load_contract_json(args.contract_json)
                if args.material:
                    contract.material = args.material
                if args.manufacturing_method:
                    contract.manufacturing_method = args.manufacturing_method
                contract.assert_valid()
            bridge = bridge or CompilerBridge(cli_bin=args.cli_bin)
            cad_coder = CadCoderAgent(
                llm_client=llm_client,
                workspace_root=bridge.repo_root if args.workspace_root is not None else bridge.workspace_root,
                few_shot_k=few_shot_k,
            )
            pipeline = SelfRefinementPipeline(
                spec_parser=spec_parser,
                cad_coder=cad_coder,
                compiler_bridge=bridge,
                critic=VisualGeometricCritic(view_inspector=view_inspector),
            )
            result = pipeline.run_edit(
                existing_code,
                args.prompt,
                contract=contract,
                external_context=external_context,
                max_iters=args.max_iters,
                work_dir=args.work_dir,
                build_artifacts=True,
                include_drawings=not args.no_drawings,
                progress_callback=progress_callback,
            )
        else:
            contract = _load_contract(args, spec_parser, external_context)
            bridge = bridge or CompilerBridge(cli_bin=args.cli_bin)
            cad_coder = CadCoderAgent(
                llm_client=llm_client,
                workspace_root=bridge.repo_root if args.workspace_root is not None else bridge.workspace_root,
                few_shot_k=few_shot_k,
            )
            pipeline = SelfRefinementPipeline(
                spec_parser=spec_parser,
                cad_coder=cad_coder,
                compiler_bridge=bridge,
                critic=VisualGeometricCritic(view_inspector=view_inspector),
            )
            result = pipeline.run(
                contract,
                max_iters=args.max_iters,
                work_dir=args.work_dir,
                build_artifacts=True,
                include_drawings=not args.no_drawings,
                progress_callback=progress_callback,
            )
    except (
        ValueError,
        json.JSONDecodeError,
        ContractValidationError,
        CompilerBridgeError,
        LLMConfigError,
        LLMRequestError,
        urllib.error.URLError,
        TimeoutError,
    ) as exc:
        secrets_to_scrub = (
            args.api_key,
            os.environ.get("OPENAI_API_KEY"),
            os.environ.get("GEMINI_API_KEY"),
            getattr(llm_client, "api_key", None),
        )
        safe_error = _sanitize_error_message(str(exc), secrets_to_scrub)
        observe.log(
            "error",
            "cad-agent",
            op,
            safe_error,
            duration_ms=int((time.perf_counter() - start) * 1000),
            code=observe.CODE_IPC_FAILED if isinstance(exc, CompilerBridgeError) else None,
            error_type=type(exc).__name__,
        )
        err_payload: dict[str, Any] = {
            "passed": False,
            "trace_id": observe.current_trace_id(),
            "runtime_path": "kcl_legacy",
            "evaluator": "cad_cli_unified",
            "mode": args.mode,
            "llm_provider": llm_info["provider"],
            "llm_model": llm_info["model"],
            "used_llm": bool(llm_info["used_llm"]),
            "vlm_enabled": bool(args.enable_vlm and llm_client is not None),
            "retrieved_examples": list(cad_coder.last_retrieved_examples) if cad_coder is not None else [],
            "contract_repair_attempts": int(spec_parser.last_repair_attempts) if spec_parser is not None else 0,
            "error": safe_error,
            "contract": contract.to_dict() if contract is not None else None,
        }
        if isinstance(exc, UnsupportedFeaturesError):
            err_payload["error_code"] = "unsupported_features"
            err_payload["unsupported_features"] = exc.features
        elif isinstance(exc, LLMConfigError):
            err_payload["error_code"] = "llm_config_error"
        elif isinstance(exc, LLMRequestError):
            err_payload["error_code"] = "llm_request_error"
        if args.json:
            print(json.dumps(err_payload, indent=2, ensure_ascii=False))
        else:
            print(f"[cad-agent] Error: {safe_error}", file=sys.stderr)
        return 1

    observe.log(
        "info",
        "cad-agent",
        op,
        f"command finished passed={result.passed} iters={len(result.iterations)}",
        duration_ms=int((time.perf_counter() - start) * 1000),
    )

    payload: dict[str, Any] = {
        "passed": result.passed,
        "trace_id": observe.current_trace_id(),
        "runtime_path": "kcl_legacy",
        "evaluator": "cad_cli_unified",
        "mode": args.mode,
        "llm_provider": llm_info["provider"],
        "llm_model": llm_info["model"],
        "used_llm": bool(llm_info["used_llm"]),
        "vlm_enabled": bool(args.enable_vlm and llm_client is not None),
        "retrieved_examples": list(cad_coder.last_retrieved_examples),
        "contract_repair_attempts": int(spec_parser.last_repair_attempts),
        "part_name": result.contract.part_name,
        "archetype": result.contract.part_archetype,
        "iterations_run": len(result.iterations),
        "kcl_code": result.final_code,
        "kcl_path": str(result.kcl_path) if result.kcl_path else None,
        "step_path": str(result.step_path) if result.step_path else None,
        "mesh_path": str(result.mesh_path) if result.mesh_path else None,
        "dxf_path": str(result.dxf_path) if result.dxf_path else None,
        "drawing_path": str(result.drawing_path) if result.drawing_path else None,
        "views_dir": str(result.views_dir) if result.views_dir else None,
        "work_dir": str(result.work_dir) if result.work_dir else None,
        "dxf_content": _read_optional_text(result.dxf_path),
        "drawing_svg": _read_optional_text(result.drawing_path),
        "contract": result.contract.to_dict(),
        "compile_result": result.final_compile_result.to_dict(),
        "verdict": result.final_verdict.to_dict(),
    }

    if args.json:
        print(json.dumps(payload, indent=2, ensure_ascii=False))
    else:
        status_label = "PASSED" if result.passed else "FAILED"
        print(
            f"[cad-agent] {status_label} mode={args.mode} provider={llm_info['provider']} "
            f"part={result.contract.part_name} archetype={result.contract.part_archetype} "
            f"iters={len(result.iterations)} kcl={result.kcl_path}"
        )
        if not result.passed:
            for issue in result.final_verdict.issues:
                print(f"  - {issue}", file=sys.stderr)
    return 0 if result.passed else 1


_V2_MODEL_PROVIDERS = frozenset({"openai", "gemini"})
_V2_KEY_REQUIRED = (
    "V2 semantic runtime requires a configured OpenAI or Gemini API key. "
    "Refusing to generate a template model. "
    "Pass --provider deterministic for offline regression."
)
_SOLID_EXPORT_NAMES = (
    "preview.step",
    "preview.stp",
    "preview.brep",
    "preview.iges",
    "preview.igs",
    "preview.kcl",
    "model.step",
    "model.stp",
    "model.kcl",
)
_SOLID_EXPORT_SUFFIXES = {".step", ".stp", ".brep", ".iges", ".igs", ".kcl"}


def _resolve_v2_planner(
    args: argparse.Namespace,
) -> tuple[Any, Any, dict[str, Any]]:
    """Build the V2 planner. Only ``--provider deterministic`` skips the model."""
    if args.provider == "deterministic":
        return (
            HeuristicSemanticPlanner(),
            None,
            {
                "provider": "deterministic",
                "model": None,
                "base_url": None,
                "used_llm": False,
            },
        )
    client, info = create_llm_client(
        LLMConfig(
            provider=args.provider,
            model=args.model,
            base_url=args.base_url,
            api_key=args.api_key,
        )
    )
    provider = str(info.get("provider") or "")
    if client is None or not info.get("used_llm") or provider not in _V2_MODEL_PROVIDERS:
        raise LLMConfigError(_V2_KEY_REQUIRED)
    return LLMSemanticPlanner(client), client, info


def _load_v2_contract(args: argparse.Namespace, llm_client: Any) -> DesignContract:
    """Load a create contract from JSON, or from ``--prompt`` through the spec parser."""
    if args.contract_json:
        try:
            contract = _load_contract_json(args.contract_json)
        except (json.JSONDecodeError, KeyError, TypeError) as exc:
            raise ValueError(f"invalid --contract-json: {exc}") from exc
    elif args.prompt and str(args.prompt).strip():
        if args.provider != "deterministic" and llm_client is None:
            raise LLMConfigError(_V2_KEY_REQUIRED)
        parser = SpecParserAgent(
            llm_client=None if args.provider == "deterministic" else llm_client,
        )
        contract = parser.parse(str(args.prompt))
    else:
        raise ValueError("--semantic-v2 create requires --prompt or --contract-json")
    if args.material:
        contract.material = args.material
    if args.manufacturing_method:
        contract.manufacturing_method = args.manufacturing_method
    return contract


def _reject_label_leak(record: ContextAssetRecord) -> None:
    blob = json.dumps(record.planner_view(), ensure_ascii=False)
    for label in record.untrusted_asset_labels:
        text = label.text.strip()
        if len(text) >= 12 and text in blob:
            raise V2ContextAdapterError("Untrusted asset label leaked into the planner view")


def _seed_deterministic_dimensions(contract: DesignContract) -> DesignContract:
    """Give the offline box planner numeric size when the prompt only named a bbox."""
    width, depth, height = contract.keep_in_bbox_mm.dimensions
    params = dict(contract.feature_params)
    params.setdefault("width", width)
    params.setdefault("depth", depth)
    params.setdefault("height", height)
    if params == contract.feature_params:
        return contract
    return replace(contract, feature_params=params)


def _interference_source(work_dir: Path | None) -> Path | None:
    if work_dir is None:
        return None
    root = Path(work_dir)
    for name in _SOLID_EXPORT_NAMES:
        candidate = root / name
        if candidate.is_file() and candidate.suffix.lower() in _SOLID_EXPORT_SUFFIXES:
            return candidate
    return None


def _build_interference_checker(
    args: argparse.Namespace,
    adapter: V2ContextAdapter | None,
    record: ContextAssetRecord | None,
    client: RuntimeToolClient | None = None,
) -> Callable[[], Any] | None:
    if adapter is None or record is None:
        return None

    def check() -> Any:
        source = _interference_source(args.work_dir)
        temp_kcl: Path | None = None
        try:
            if (
                source is None
                and client is not None
                and isinstance(getattr(client, "transport", None), SubprocessCliTransport)
            ):
                # A stable name such as preview.kcl is also an export lookup name, so
                # the next repair would reuse the previous attempt.
                directory = Path(args.work_dir) if args.work_dir is not None else None
                if directory is not None:
                    directory.mkdir(parents=True, exist_ok=True)
                fd, raw_path = tempfile.mkstemp(
                    prefix="aicad-v2-interference-",
                    suffix=".kcl",
                    dir=directory,
                )
                os.close(fd)
                staged = Path(raw_path)
                temp_kcl = staged
                try:
                    client.emit_kcl(staged)
                    if staged.is_file() and staged.stat().st_size > 0:
                        source = staged
                except RuntimeToolError:
                    source = None
            if source is None:
                return unknown_external_interference(
                    "No exported solid is available for an exact external interference recheck."
                )
            return adapter.check_exact_interference(source, record)
        finally:
            if temp_kcl is not None:
                temp_kcl.unlink(missing_ok=True)

    return check


def _build_visual_reviewer(
    args: argparse.Namespace,
    llm_client: Any,
    contract: DesignContract | None,
    offscreen_renderer: Callable[..., Any] | None,
) -> Callable[[VerificationReport], VisualReviewResult] | None:
    if not args.enable_vlm:
        return None
    inspector = VLMViewInspector(
        llm_client=llm_client,
        enable_vlm=True,
        strict_vlm=bool(args.strict_vlm),
    )

    def review(report: VerificationReport) -> VisualReviewResult:
        if offscreen_renderer is not None:
            images = offscreen_renderer(args, report)
        elif args.work_dir is not None:
            images = load_shaded_views_with_masks(Path(args.work_dir))
        else:
            images = None
        return review_offscreen_semantic_masks(inspector, images, contract=contract)

    return review


def _run_semantic_v2(
    args: argparse.Namespace,
    *,
    agent_factory: Callable[..., Any] | None,
    context_runner: Callable[..., Any] | None = None,
    offscreen_renderer: Callable[..., Any] | None = None,
) -> int:
    """V2 semantic runtime: real model planner, mating adapter, and advisory masks."""
    op = "cli.semantic_v2"
    llm_client: Any = None
    llm_info: dict[str, Any] = {
        "provider": args.provider,
        "model": args.model,
        "base_url": args.base_url,
        "used_llm": False,
    }
    record: ContextAssetRecord | None = None
    try:
        if args.workspace_root is None or not args.project:
            raise ValueError("--semantic-v2 requires --workspace-root and --project")
        if args.context_source_unit and args.context_asset is None:
            raise ValueError("--context-source-unit requires --context-asset")
        if args.context_asset is not None and args.workspace_root is None:
            raise ValueError("--context-asset requires an explicit --workspace-root")
        editing = args.mode == "edit" or bool(args.target)
        if editing:
            if not args.target:
                raise ValueError("--semantic-v2 --mode edit requires --target")
            if not args.prompt or not str(args.prompt).strip():
                raise ValueError("--semantic-v2 edit requires --prompt")
        elif not args.contract_json and not (args.prompt and str(args.prompt).strip()):
            raise ValueError("--semantic-v2 create requires --prompt or --contract-json")

        planner = None
        if agent_factory is None:
            planner, llm_client, llm_info = _resolve_v2_planner(args)

        contract: DesignContract | None = None
        if not editing:
            contract = _load_v2_contract(args, llm_client)

        adapter: V2ContextAdapter | None = None
        if args.context_asset is not None:
            adapter = V2ContextAdapter(
                workspace_root=args.workspace_root,
                cli_bin=args.cli_bin,
                runner=context_runner,
            )
            record = adapter.inspect_context_asset(
                args.context_asset,
                source_unit=args.context_source_unit,
                part_name=contract.part_name if contract is not None else "ContextMate",
                base_contract=contract,
            )
            _reject_label_leak(record)
            if contract is not None:
                contract = record.contract

        if contract is not None:
            if args.material:
                contract.material = args.material
            if args.manufacturing_method:
                contract.manufacturing_method = args.manufacturing_method
            if args.provider == "deterministic":
                contract = _seed_deterministic_dimensions(contract)
            contract.assert_valid()

        progress = _stderr_progress_callback(args, llm_client)
        planner_name = "injected" if agent_factory is not None else (
            "llm" if llm_info.get("used_llm") else "deterministic"
        )
        _emit_cli_progress(
            progress,
            "runtime",
            "V2 semantic runtime",
            runtime_path="semantic_v2",
            planner=planner_name,
        )
        _emit_cli_progress(
            progress,
            "planner",
            "Semantic planner selected",
            planner=planner_name,
            llm_provider=llm_info.get("provider"),
        )
        if record is not None:
            _emit_cli_progress(
                progress,
                "context",
                "Inspected external mating asset",
                relative_path=record.relative_path,
            )

        client = RuntimeToolClient(
            args.workspace_root,
            args.project,
            cli_bin=args.cli_bin,
        )
        if agent_factory is not None:
            agent = agent_factory(client, args)
        else:
            agent = EngineeringSemanticAgent(
                client,
                planner=planner,
                max_repairs=args.max_repairs,
                session_id=args.session_id or "session",
                preview_branch=args.preview_branch,
                progress_callback=progress,
                context_record=record,
                interference_checker=_build_interference_checker(args, adapter, record, client),
                visual_reviewer=_build_visual_reviewer(
                    args,
                    llm_client,
                    contract,
                    offscreen_renderer,
                ),
            )
        if editing:
            result = agent.edit_local(args.target, args.prompt)
        else:
            assert contract is not None
            result = agent.create_part(contract, args.prompt or "")
    except (
        ValueError,
        json.JSONDecodeError,
        RuntimeToolError,
        LLMRequestError,
        V2ContextAdapterError,
        urllib.error.URLError,
        TimeoutError,
    ) as exc:
        safe_error = _sanitize_error_message(str(exc), _progress_secrets(args, llm_client))
        code = getattr(exc, "code", None)
        if isinstance(exc, ContractValidationError):
            code = "E400_INVALID_CONTRACT"
        elif isinstance(exc, LLMConfigError):
            code = "llm_config_error"
        elif isinstance(exc, (LLMRequestError, urllib.error.URLError, TimeoutError)):
            code = "llm_request_error"
        elif isinstance(exc, UnsupportedFeaturesError):
            code = "unsupported_features"
        elif isinstance(exc, V2ContextAdapterError):
            code = "v2_context_error"
        observe.log(
            "error",
            "cad-agent",
            op,
            safe_error,
            code=code,
            error_type=type(exc).__name__,
        )
        err_payload: dict[str, Any] = {
            "passed": False,
            "trace_id": observe.current_trace_id(),
            "runtime_path": "semantic_v2",
            "evaluator": "graph_evaluator",
            "mode": "local_edit" if (args.mode == "edit" or args.target) else "create",
            "llm_provider": llm_info.get("provider"),
            "llm_model": llm_info.get("model"),
            "used_llm": bool(llm_info.get("used_llm")),
            "vlm_enabled": bool(args.enable_vlm and llm_client is not None),
            "error": safe_error,
            "error_code": code,
        }
        if isinstance(exc, UnsupportedFeaturesError):
            err_payload["unsupported_features"] = exc.features
        if args.json:
            print(json.dumps(err_payload, indent=2, ensure_ascii=False))
        else:
            print(f"[cad-agent] Error: {safe_error}", file=sys.stderr)
        return 1

    payload = result.to_dict()
    payload["trace_id"] = observe.current_trace_id()
    payload["runtime_path"] = "semantic_v2"
    payload["evaluator"] = "graph_evaluator"
    payload["llm_provider"] = llm_info.get("provider")
    payload["llm_model"] = llm_info.get("model")
    payload["used_llm"] = bool(llm_info.get("used_llm"))
    payload["vlm_enabled"] = bool(args.enable_vlm and llm_client is not None)
    payload["planner"] = "injected" if agent_factory is not None else (
        "llm" if llm_info.get("used_llm") else "deterministic"
    )
    if record is not None and not payload.get("untrusted_asset_labels"):
        payload["untrusted_asset_labels"] = [label.to_dict() for label in record.untrusted_asset_labels]
    observe.log(
        "info",
        "cad-agent",
        op,
        f"semantic-v2 finished passed={payload['passed']} mode={payload['mode']}",
    )
    if args.json:
        print(json.dumps(payload, indent=2, ensure_ascii=False))
    else:
        status = payload.get("overall_status", payload.get("verdict"))
        print(
            f"[cad-agent] semantic-v2 mode={payload['mode']} status={status} "
            f"passed={payload['passed']} branch={payload.get('branch')}"
        )
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
