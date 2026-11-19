"""Command-line entry point for `python -m cad_agent.cli`."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from .agents import (
    CadCoderAgent,
    SelfRefinementPipeline,
    SpecParserAgent,
    VisualGeometricCritic,
)
from .compiler_bridge import CompilerBridge, CompilerBridgeError
from .contracts import ContractValidationError, DesignContract


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="python -m cad_agent.cli",
        description="Run the L4 multi-agent CAD generation and verification closed loop.",
    )
    parser.add_argument(
        "--prompt",
        type=str,
        default=None,
        help="Natural-language mechanical design prompt",
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
        "--work-dir",
        type=Path,
        default=None,
        help="Output directory for generated .kcl, .step, .mesh.json, .dxf, and views",
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
        "--json",
        action="store_true",
        help="Print structured JSON result to stdout",
    )
    return parser.parse_args(argv)


def _load_contract(args: argparse.Namespace, spec_parser: SpecParserAgent) -> DesignContract:
    if args.contract_json:
        raw_arg = args.contract_json.strip()
        candidate_path = Path(raw_arg)
        if candidate_path.is_file():
            data = json.loads(candidate_path.read_text(encoding="utf-8"))
        else:
            data = json.loads(raw_arg)
        contract = DesignContract.from_dict(data)
    elif args.prompt:
        contract = spec_parser.parse(args.prompt)
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


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    spec_parser = SpecParserAgent()
    contract: DesignContract | None = None

    try:
        contract = _load_contract(args, spec_parser)
        bridge = CompilerBridge(cli_bin=args.cli_bin)
        pipeline = SelfRefinementPipeline(
            spec_parser=spec_parser,
            cad_coder=CadCoderAgent(workspace_root=bridge.workspace_root),
            compiler_bridge=bridge,
            critic=VisualGeometricCritic(),
        )
        result = pipeline.run(
            contract,
            max_iters=args.max_iters,
            work_dir=args.work_dir,
            build_artifacts=True,
            include_drawings=not args.no_drawings,
        )
    except (ValueError, json.JSONDecodeError, ContractValidationError, CompilerBridgeError) as exc:
        err_payload: dict[str, Any] = {
            "passed": False,
            "error": str(exc),
            "contract": contract.to_dict() if contract is not None else None,
        }
        if args.json:
            print(json.dumps(err_payload, indent=2, ensure_ascii=False))
        else:
            print(f"[cad-agent] Error: {exc}", file=sys.stderr)
        return 1

    payload: dict[str, Any] = {
        "passed": result.passed,
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
            f"[cad-agent] {status_label} part={result.contract.part_name} "
            f"archetype={result.contract.part_archetype} iters={len(result.iterations)} "
            f"kcl={result.kcl_path}"
        )
        if not result.passed:
            for issue in result.final_verdict.issues:
                print(f"  - {issue}", file=sys.stderr)
    return 0 if result.passed else 1


if __name__ == "__main__":
    sys.exit(main())
