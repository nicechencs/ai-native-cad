#!/usr/bin/env python3
"""Offline CLI entrypoint for the H2-E08 Synthetic CAD SFT/GRPO Data Factory.

Example::

    python services/cad-agent/scripts/run_synthetic_grpo_factory.py \\
        --count 10 --candidates 3 --seed 42 --out-dir ./synthetic_out
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Allow direct invocation without requiring callers to set PYTHONPATH.
_SRC_DIR = Path(__file__).resolve().parents[1] / "src"
if str(_SRC_DIR) not in sys.path:
    sys.path.insert(0, str(_SRC_DIR))

from cad_agent.synthetic_factory import (  # noqa: E402
    PART_CATEGORIES,
    SyntheticFactoryConfig,
    run_synthetic_factory,
)


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Run the deterministic offline Synthetic CAD SFT/GRPO Data Factory "
            "against local cad-cli SQLite branches and L0-L4 verification."
        )
    )
    parser.add_argument(
        "--count",
        type=int,
        default=10,
        help="Total number of synthetic parametric CAD tasks to sample (default: 10).",
    )
    parser.add_argument(
        "--candidates",
        type=int,
        default=3,
        help="Number of candidate trajectories per prompt for GRPO grouping (default: 3).",
    )
    parser.add_argument(
        "--seed",
        type=int,
        default=42,
        help="Deterministic PRNG seed (default: 42).",
    )
    parser.add_argument(
        "--out-dir",
        type=Path,
        required=True,
        help="Output directory for grpo_train.jsonl, sft_train.jsonl, and factory_summary.json.",
    )
    parser.add_argument(
        "--categories",
        nargs="+",
        choices=PART_CATEGORIES,
        default=list(PART_CATEGORIES),
        help="Subset of industrial part categories to sample (default: all 5).",
    )
    parser.add_argument(
        "--cli-bin",
        type=Path,
        default=None,
        help="Explicit path to cad-cli binary (defaults to auto-discovery).",
    )
    parser.add_argument(
        "--min-wall-mm",
        type=float,
        default=1.5,
        help="Minimum wall thickness threshold in mm for DFM checks (default: 1.5).",
    )
    parser.add_argument(
        "--require-full-pass",
        action="store_true",
        help="Require overall_status == PASS (including L4) instead of L0/L1/L2 PASS & L3 != FAIL.",
    )
    parser.add_argument(
        "--keep-projects",
        action="store_true",
        help="Persist generated SQLite .aicad project files under <out-dir>/projects.",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_arg_parser()
    args = parser.parse_args(argv)

    try:
        config = SyntheticFactoryConfig(
            count=args.count,
            candidates_per_prompt=args.candidates,
            seed=args.seed,
            categories=tuple(args.categories),
            out_dir=args.out_dir,
            cli_bin=args.cli_bin,
            min_wall_mm=args.min_wall_mm,
            require_full_pass=args.require_full_pass,
            keep_projects=args.keep_projects,
        )
        stats = run_synthetic_factory(config)
    except Exception as exc:
        print(f"synthetic_grpo_factory error: {exc}", file=sys.stderr)
        return 1

    print(json.dumps(stats.to_dict(), ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
