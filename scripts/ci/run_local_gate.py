#!/usr/bin/env python3
"""Layered parallel local pre-merge verification gate for AI-Native CAD.

Executes cross-stack checks in a safe 3-phase DAG:
  - Phase 0 (Serial pre-build): builds `cad-cli` once so downstream consumers
    never race with Cargo overwriting `target/debug/cad-cli(.exe)`.
  - Phase 1 (Bounded parallel): runs independent tracks concurrently
    (`rust-workspace`, `desktop-tauri`, `frontend`, `python`, `functional`).
    Auto-uses `cargo-nextest` and `pytest-xdist` when installed, with graceful
    fallback to `cargo test` and standard `pytest`.
  - Phase 2 (Low-load exclusive): runs latency-sensitive P95 tests alone so CPU
    saturation from parallel compilation does not cause false-positive failures.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from typing import Callable

REPO_ROOT = Path(__file__).resolve().parents[2]
ALL_TRACKS = ("rust-workspace", "desktop-tauri", "frontend", "python", "functional")
CLI_CONSUMER_TRACKS = frozenset({"frontend", "python", "functional"})
LATENCY_TEST_NAME = "v04_live_selection_round_trip_p95_under_30ms"


@dataclass(frozen=True)
class CommandStep:
    label: str
    cmd: list[str]
    cwd: Path
    env_overrides: dict[str, str]


@dataclass(frozen=True)
class TrackSpec:
    name: str
    phase: int  # 0 = prebuild, 1 = parallel, 2 = exclusive
    steps: list[CommandStep]


@dataclass(frozen=True)
class TrackOutcome:
    name: str
    phase: int
    ok: bool
    returncode: int
    duration_sec: float
    failed_step: str | None
    output_tail: str


def has_cargo_nextest() -> bool:
    return shutil.which("cargo-nextest") is not None


def has_pytest_xdist() -> bool:
    return importlib.util.find_spec("xdist") is not None


def resolve_pnpm_cmd() -> str:
    if os.name == "nt":
        for candidate in ("pnpm.cmd", "pnpm"):
            found = shutil.which(candidate)
            if found:
                return found
    return shutil.which("pnpm") or "pnpm"


def default_cad_cli_path(repo_root: Path) -> Path:
    exe = "cad-cli.exe" if os.name == "nt" else "cad-cli"
    return repo_root / "target" / "debug" / exe


def build_gate_plan(
    repo_root: Path,
    selected_tracks: list[str],
    *,
    use_nextest: bool,
    use_xdist: bool,
    skip_build: bool = False,
    include_phase2: bool = True,
    functional_suites: str = "agent,desktop,mcp",
    functional_jobs: str = "auto",
) -> list[TrackSpec]:
    selected_set = set(selected_tracks)
    cli_bin = default_cad_cli_path(repo_root)
    pnpm = resolve_pnpm_cmd()
    py = sys.executable
    py_path = f"{repo_root / 'packages' / 'python-sdk' / 'src'}{os.pathsep}{repo_root / 'services' / 'cad-agent' / 'src'}"
    rust_warn_env = {"RUSTFLAGS": "-D warnings"}
    plan: list[TrackSpec] = []

    needs_cli = bool(selected_set & CLI_CONSUMER_TRACKS)
    if needs_cli and not skip_build:
        plan.append(
            TrackSpec(
                name="prebuild-cad-cli",
                phase=0,
                steps=[
                    CommandStep(
                        label="cargo build -p cad-cli",
                        cmd=["cargo", "build", "-p", "cad-cli", "--locked"],
                        cwd=repo_root,
                        env_overrides=rust_warn_env,
                    )
                ],
            )
        )

    if "rust-workspace" in selected_set:
        test_cmd = (
            ["cargo", "nextest", "run", "--workspace", "--locked"]
            if use_nextest
            else ["cargo", "test", "--workspace", "--locked"]
        )
        plan.append(
            TrackSpec(
                name="rust-workspace",
                phase=1,
                steps=[
                    CommandStep("rust workspace tests", test_cmd, repo_root, rust_warn_env),
                    CommandStep(
                        "check wgpu-renderer",
                        ["cargo", "check", "-p", "cad-render", "-p", "cad-cli", "--features", "wgpu-renderer", "--locked"],
                        repo_root,
                        rust_warn_env,
                    ),
                    CommandStep(
                        "check wgpu-compute",
                        ["cargo", "check", "-p", "cad-gpu-compute", "--features", "wgpu-compute", "--locked"],
                        repo_root,
                        rust_warn_env,
                    ),
                    CommandStep(
                        "check occt-ffi",
                        ["cargo", "check", "-p", "cad-kernel-occt", "--features", "occt-ffi", "--tests", "--locked"],
                        repo_root,
                        rust_warn_env,
                    ),
                ],
            )
        )

    if "desktop-tauri" in selected_set:
        manifest = "apps/desktop/src-tauri/Cargo.toml"
        if use_nextest:
            desktop_cmd = [
                "cargo",
                "nextest",
                "run",
                "--manifest-path",
                manifest,
                "--locked",
            ]
            if include_phase2:
                desktop_cmd.extend(["-E", f"not test({LATENCY_TEST_NAME})"])
        else:
            desktop_cmd = ["cargo", "test", "--manifest-path", manifest, "--locked"]
            if include_phase2:
                desktop_cmd.extend(["--", "--skip", LATENCY_TEST_NAME])
        plan.append(
            TrackSpec(
                name="desktop-tauri",
                phase=1,
                steps=[CommandStep("desktop tauri tests", desktop_cmd, repo_root, rust_warn_env)],
            )
        )

    if "frontend" in selected_set:
        fe_env = {"CAD_CLI_BIN": str(cli_bin)}
        plan.append(
            TrackSpec(
                name="frontend",
                phase=1,
                steps=[
                    CommandStep("pnpm typecheck", [pnpm, "typecheck"], repo_root, fe_env),
                    CommandStep("pnpm test", [pnpm, "test"], repo_root, fe_env),
                    CommandStep("pnpm build", [pnpm, "build"], repo_root, fe_env),
                ],
            )
        )

    if "python" in selected_set:
        py_env = {
            "CAD_CLI_BIN": str(cli_bin),
            "PYTHONPATH": py_path,
            "PYTHONUTF8": "1",
        }
        pytest_cmd = [py, "-X", "utf8", "-m", "pytest"]
        if use_xdist:
            pytest_cmd.extend(["-n", "auto"])
        pytest_cmd.extend(
            [
                "scripts/ci/test_check_repo_hygiene.py",
                "scripts/ci/test_run_local_gate.py",
                "scripts/test_package_release.py",
                "scripts/test_public_snapshot.py",
                "scripts/test_verify_agent_interop.py",
                "tests/functional/test_run_suite_parallel.py",
                "packages/python-sdk/tests",
                "services/cad-agent/tests",
                "-q",
            ]
        )
        plan.append(
            TrackSpec(
                name="python",
                phase=1,
                steps=[
                    CommandStep(
                        "repo hygiene",
                        [py, "-X", "utf8", "scripts/ci/check_repo_hygiene.py"],
                        repo_root,
                        {"PYTHONUTF8": "1"},
                    ),
                    CommandStep("pytest suites", pytest_cmd, repo_root, py_env),
                ],
            )
        )

    if "functional" in selected_set:
        plan.append(
            TrackSpec(
                name="functional",
                phase=1,
                steps=[
                    CommandStep(
                        "functional smoke suites",
                        [
                            py,
                            "-X",
                            "utf8",
                            "tests/functional/run_suite.py",
                            "--tier",
                            "smoke",
                            "--jobs",
                            functional_jobs,
                            "--suite",
                            functional_suites,
                            "--cli",
                            str(cli_bin),
                        ],
                        repo_root,
                        {"CAD_CLI_BIN": str(cli_bin), "PYTHONUTF8": "1"},
                    )
                ],
            )
        )

    if include_phase2 and "desktop-tauri" in selected_set:
        plan.append(
            TrackSpec(
                name="desktop-latency",
                phase=2,
                steps=[
                    CommandStep(
                        "desktop P95 latency gate",
                        [
                            "cargo",
                            "test",
                            "--manifest-path",
                            "apps/desktop/src-tauri/Cargo.toml",
                            "--test",
                            "live_attach_tests",
                            LATENCY_TEST_NAME,
                            "--locked",
                        ],
                        repo_root,
                        rust_warn_env,
                    )
                ],
            )
        )

    return plan


def _default_runner(step: CommandStep) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.update(step.env_overrides)
    return subprocess.run(
        step.cmd,
        cwd=step.cwd,
        env=env,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )


def run_track(
    track: TrackSpec,
    *,
    runner: Callable[[CommandStep], subprocess.CompletedProcess[str]] = _default_runner,
) -> TrackOutcome:
    t0 = time.perf_counter()
    last_tail = ""
    for step in track.steps:
        completed = runner(step)
        combined = "\n".join(part for part in (completed.stdout, completed.stderr) if part).strip()
        tail_lines = combined.splitlines()[-30:] if combined else []
        last_tail = "\n".join(tail_lines)
        if completed.returncode != 0:
            return TrackOutcome(
                name=track.name,
                phase=track.phase,
                ok=False,
                returncode=completed.returncode,
                duration_sec=round(time.perf_counter() - t0, 2),
                failed_step=step.label,
                output_tail=last_tail,
            )
    return TrackOutcome(
        name=track.name,
        phase=track.phase,
        ok=True,
        returncode=0,
        duration_sec=round(time.perf_counter() - t0, 2),
        failed_step=None,
        output_tail=last_tail,
    )


def execute_plan(
    plan: list[TrackSpec],
    *,
    jobs: int,
    runner: Callable[[CommandStep], subprocess.CompletedProcess[str]] = _default_runner,
) -> list[TrackOutcome]:
    outcomes: list[TrackOutcome] = []
    for phase in (0, 1, 2):
        phase_tracks = [track for track in plan if track.phase == phase]
        if not phase_tracks:
            continue
        if phase == 0 or phase == 2 or jobs <= 1 or len(phase_tracks) == 1:
            for track in phase_tracks:
                outcome = run_track(track, runner=runner)
                outcomes.append(outcome)
                if phase == 0 and not outcome.ok:
                    return outcomes
        else:
            workers = max(1, min(jobs, len(phase_tracks)))
            phase_results: dict[str, TrackOutcome] = {}
            with ThreadPoolExecutor(max_workers=workers) as pool:
                future_map = {
                    pool.submit(run_track, track, runner=runner): track.name
                    for track in phase_tracks
                }
                for future in as_completed(future_map):
                    name = future_map[future]
                    phase_results[name] = future.result()
            for track in phase_tracks:
                outcomes.append(phase_results[track.name])
    return outcomes


def _parse_track_list(raw: str | None) -> list[str]:
    if not raw:
        return []
    return [item.strip().lower() for item in raw.split(",") if item.strip()]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Run the layered parallel pre-merge verification gate."
    )
    parser.add_argument(
        "-j",
        "--jobs",
        type=int,
        default=min(4, max(1, (os.cpu_count() or 2))),
        help="Max concurrent Phase 1 tracks (default: min(4, cpu_count)).",
    )
    parser.add_argument(
        "--only",
        default=None,
        help=f"Comma-separated subset of tracks to run: {','.join(ALL_TRACKS)}.",
    )
    parser.add_argument(
        "--skip",
        default=None,
        help=f"Comma-separated tracks to skip: {','.join(ALL_TRACKS)}.",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="Skip Phase 0 `cargo build -p cad-cli` (use existing binary).",
    )
    parser.add_argument(
        "--no-phase2",
        action="store_true",
        help="Run latency-sensitive tests inline with Phase 1 instead of Phase 2 exclusive.",
    )
    parser.add_argument(
        "--no-nextest",
        action="store_true",
        help="Disable auto-detected cargo-nextest and use `cargo test`.",
    )
    parser.add_argument(
        "--no-xdist",
        action="store_true",
        help="Disable auto-detected pytest-xdist (`-n auto`).",
    )
    parser.add_argument(
        "--functional-suites",
        default="agent,desktop,mcp",
        help="Comma-separated suites for the functional smoke track (default: agent,desktop,mcp).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the resolved 3-phase execution plan without running commands.",
    )
    args = parser.parse_args(argv)

    if args.jobs < 1:
        print("--jobs must be >= 1", file=sys.stderr)
        return 2

    only = _parse_track_list(args.only)
    skip = set(_parse_track_list(args.skip))
    unknown = [t for t in (*only, *skip) if t not in ALL_TRACKS]
    if unknown:
        print(f"Unknown track(s): {unknown}. Valid: {ALL_TRACKS}", file=sys.stderr)
        return 2

    selected = [t for t in (only or list(ALL_TRACKS)) if t not in skip]
    use_nextest = (not args.no_nextest) and has_cargo_nextest()
    use_xdist = (not args.no_xdist) and has_pytest_xdist()

    plan = build_gate_plan(
        REPO_ROOT,
        selected,
        use_nextest=use_nextest,
        use_xdist=use_xdist,
        skip_build=args.skip_build,
        include_phase2=not args.no_phase2,
        functional_suites=args.functional_suites,
    )

    print(
        f"=== Local Parallel Gate | jobs={args.jobs} | nextest={use_nextest} "
        f"| pytest-xdist={use_xdist} | tracks={','.join(selected)} ==="
    )
    if args.dry_run:
        for track in plan:
            print(f"[Phase {track.phase}] {track.name}")
            for step in track.steps:
                print(f"  - {step.label}: {' '.join(step.cmd)}")
        return 0

    t0 = time.perf_counter()
    outcomes = execute_plan(plan, jobs=args.jobs)
    wall_sec = round(time.perf_counter() - t0, 2)
    sum_sec = round(sum(o.duration_sec for o in outcomes), 2)
    speedup = (sum_sec / wall_sec) if wall_sec > 0 else 1.0

    print("\n=== Gate Summary ===")
    for item in outcomes:
        badge = "PASS" if item.ok else "FAIL"
        extra = f" (failed: {item.failed_step})" if item.failed_step else ""
        print(f"  [Phase {item.phase}] {item.name:<18} {badge:<4} {item.duration_sec:6.2f}s{extra}")
    print(f"Wall time: {wall_sec:.2f}s | Cumulative: {sum_sec:.2f}s | Speedup: {speedup:.2f}x")

    failed = [o for o in outcomes if not o.ok]
    for item in failed:
        print(f"\n--- Failure tail: {item.name} ({item.failed_step}) ---", file=sys.stderr)
        if item.output_tail:
            print(item.output_tail, file=sys.stderr)
    return 0 if not failed else 1


if __name__ == "__main__":
    raise SystemExit(main())
