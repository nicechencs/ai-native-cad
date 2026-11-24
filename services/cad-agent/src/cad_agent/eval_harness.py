"""Real-LLM CAD evaluation harness with pluggable baselines.

Measures, per runner and per benchmark spec:
  * first-shot compile rate     (`cad-cli check --json` accepts the first generation)
  * first-shot gate pass rate   (compile + watertight + volume + keep-in bbox)
  * repair-loop pass rate       (share of first-shot gate failures fixed within the repair budget)
  * invariant pass rate         (bbox / volume reference formula / interface evidence)
  * exact STEP volume difference (`cad-cli compare --json`, only when a reference STEP is supplied)

The CAD-DSL runner refuses the deterministic `_generate_parametric_template` fallback so a
benchmark can never silently score a template instead of a model. Baselines (cadquery /
build123d code generation) implement `BaselineRunner` and are reported side by side.
"""

from __future__ import annotations

import json
import math
import statistics
import subprocess
import sys
import tempfile
from dataclasses import asdict, dataclass, field, replace
from pathlib import Path
from typing import Any, Callable, Protocol, Sequence

from .agents import CadCoderAgent, CriticVerdict, check_interfaces
from .compiler_bridge import (
    CompilerBridge,
    CompilerBridgeError,
    check_keep_in_bbox,
    check_non_zero_volume,
    check_watertight,
)
from .contracts import CompileResult, ContractValidationError, DesignContract
from .llm import LLMClient

DEFAULT_SUITE_PATH = Path(__file__).resolve().parents[2] / "benchmarks" / "prismatic_suite.json"
KNOWN_CATEGORIES = ("mounting_plate", "bracket", "fixture_jig", "motor_mount", "enclosure")


class BenchmarkError(RuntimeError):
    """Invalid benchmark configuration or dataset."""


class TemplateFallbackError(BenchmarkError):
    """The deterministic parametric template was requested during a benchmark run."""


# --------------------------------------------------------------------------- dataset


@dataclass(slots=True)
class BenchmarkSpec:
    id: str
    category: str
    prompt: str
    contract: DesignContract
    bbox_size_mm: list[float]
    bbox_tolerance_mm: float
    volume_mm3: float
    volume_rel_tolerance: float
    volume_formula: str
    interface_features: list[dict[str, Any]] = field(default_factory=list)
    reference_step: str | None = None


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def validate_suite_dict(data: Any) -> list[str]:
    """Structural validation of a suite document; returns human-readable errors."""
    errors: list[str] = []
    if not isinstance(data, dict) or not isinstance(data.get("specs"), list):
        return ["suite must be an object with a 'specs' list"]
    seen: set[str] = set()
    for index, spec in enumerate(data["specs"]):
        label = f"specs[{index}]"
        if not isinstance(spec, dict):
            errors.append(f"{label} must be an object")
            continue
        spec_id = spec.get("id")
        if not isinstance(spec_id, str) or not spec_id:
            errors.append(f"{label}.id must be a non-empty string")
        elif spec_id in seen:
            errors.append(f"{label}.id {spec_id!r} is duplicated")
        else:
            seen.add(spec_id)
            label = spec_id
        if spec.get("category") not in KNOWN_CATEGORIES:
            errors.append(f"{label}.category must be one of {KNOWN_CATEGORIES}")
        if not isinstance(spec.get("prompt"), str) or not spec["prompt"].strip():
            errors.append(f"{label}.prompt must be a non-empty string")
        contract = spec.get("contract")
        if not isinstance(contract, dict):
            errors.append(f"{label}.contract must be an object")
        else:
            try:
                parsed = DesignContract.from_dict(contract)
            except (KeyError, TypeError, ValueError) as exc:
                errors.append(f"{label}.contract is malformed: {exc!r}")
            else:
                errors.extend(f"{label}.contract: {e}" for e in parsed.validate())
        inv = spec.get("invariants")
        if not isinstance(inv, dict):
            errors.append(f"{label}.invariants must be an object")
            continue
        bbox = inv.get("bbox_mm")
        if not isinstance(bbox, dict) or not (
            isinstance(bbox.get("size"), list)
            and len(bbox["size"]) == 3
            and all(_is_number(v) and v > 0 for v in bbox["size"])
        ):
            errors.append(f"{label}.invariants.bbox_mm.size must be three positive numbers")
        elif not _is_number(bbox.get("tolerance_mm")) or bbox["tolerance_mm"] < 0:
            errors.append(f"{label}.invariants.bbox_mm.tolerance_mm must be >= 0")
        volume = inv.get("volume")
        if not isinstance(volume, dict):
            errors.append(f"{label}.invariants.volume must be an object")
        else:
            if not _is_number(volume.get("expected_mm3")) or volume["expected_mm3"] <= 0:
                errors.append(f"{label}.invariants.volume.expected_mm3 must be positive")
            if not _is_number(volume.get("rel_tolerance")) or not 0 < volume["rel_tolerance"] < 1:
                errors.append(f"{label}.invariants.volume.rel_tolerance must be in (0, 1)")
            if not isinstance(volume.get("formula"), str) or not volume["formula"]:
                errors.append(f"{label}.invariants.volume.formula must be a non-empty string")
        feats = inv.get("interface_features", [])
        if not isinstance(feats, list) or not all(isinstance(f, dict) and f.get("interface_id") for f in feats):
            errors.append(f"{label}.invariants.interface_features must be a list of objects with interface_id")
        ref = spec.get("reference_step")
        if ref is not None and not (isinstance(ref, str) and ref.lower().endswith((".step", ".stp"))):
            errors.append(f"{label}.reference_step must be null or a .step/.stp path")
    return errors


def load_suite(path: str | Path = DEFAULT_SUITE_PATH) -> list[BenchmarkSpec]:
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    errors = validate_suite_dict(data)
    if errors:
        raise BenchmarkError("invalid benchmark suite:\n" + "\n".join(errors))
    specs: list[BenchmarkSpec] = []
    for raw in data["specs"]:
        inv = raw["invariants"]
        specs.append(
            BenchmarkSpec(
                id=raw["id"],
                category=raw["category"],
                prompt=raw["prompt"],
                contract=DesignContract.from_dict(raw["contract"]),
                bbox_size_mm=[float(v) for v in inv["bbox_mm"]["size"]],
                bbox_tolerance_mm=float(inv["bbox_mm"]["tolerance_mm"]),
                volume_mm3=float(inv["volume"]["expected_mm3"]),
                volume_rel_tolerance=float(inv["volume"]["rel_tolerance"]),
                volume_formula=inv["volume"]["formula"],
                interface_features=list(inv.get("interface_features", [])),
                reference_step=raw.get("reference_step"),
            )
        )
    return specs


# --------------------------------------------------------------------------- results


@dataclass(slots=True)
class SpecResult:
    spec_id: str
    category: str
    runner: str
    first_shot_compile: bool = False
    first_shot_gate: bool = False
    final_gate: bool = False
    repair_attempts: int = 0
    invariants_ok: bool = False
    invariant_failures: list[str] = field(default_factory=list)
    volume_rel_error: float | None = None
    exact_volume_rel_error: float | None = None
    exact_compare_passed: bool | None = None
    error: str | None = None


def _rate(flags: Sequence[bool]) -> float | None:
    return (sum(1 for f in flags if f) / len(flags)) if flags else None


def aggregate(results: Sequence[SpecResult]) -> dict[str, Any]:
    """Aggregate one runner's results into overall and per-category metrics."""

    def block(items: Sequence[SpecResult]) -> dict[str, Any]:
        failed_first = [r for r in items if not r.first_shot_gate]
        exact = [r.exact_volume_rel_error for r in items if r.exact_volume_rel_error is not None]
        rel = [r.volume_rel_error for r in items if r.volume_rel_error is not None]
        return {
            "n": len(items),
            "first_shot_compile_rate": _rate([r.first_shot_compile for r in items]),
            "first_shot_gate_rate": _rate([r.first_shot_gate for r in items]),
            "repair_pass_rate": _rate([r.final_gate for r in failed_first]),
            "final_gate_rate": _rate([r.final_gate for r in items]),
            "invariant_pass_rate": _rate([r.invariants_ok for r in items]),
            "exact_compared": len(exact),
            "exact_compare_pass_rate": _rate([bool(r.exact_compare_passed) for r in items if r.exact_compare_passed is not None]),
            "mean_exact_volume_rel_error": statistics.fmean(exact) if exact else None,
            "mean_formula_volume_rel_error": statistics.fmean(rel) if rel else None,
            "errors": sum(1 for r in items if r.error),
        }

    categories = sorted({r.category for r in results})
    return {
        "overall": block(results),
        "by_category": {c: block([r for r in results if r.category == c]) for c in categories},
    }


# --------------------------------------------------------------------------- invariants


def check_invariants(
    spec: BenchmarkSpec,
    *,
    bbox_size_mm: Sequence[float] | None,
    volume_mm3: float | None,
) -> tuple[list[str], float | None]:
    """Compare measured bbox/volume against the spec; returns (failures, volume_rel_error)."""
    failures: list[str] = []
    rel_error: float | None = None
    if bbox_size_mm is None:
        failures.append("bbox unavailable")
    else:
        for axis, want, got in zip("XYZ", spec.bbox_size_mm, bbox_size_mm):
            if abs(got - want) > spec.bbox_tolerance_mm:
                failures.append(f"bbox {axis} size {got:.3f} != {want:.3f} (tol {spec.bbox_tolerance_mm})")
    if volume_mm3 is None:
        failures.append("volume unavailable")
    else:
        rel_error = abs(volume_mm3 - spec.volume_mm3) / spec.volume_mm3
        if rel_error > spec.volume_rel_tolerance:
            failures.append(
                f"volume {volume_mm3:.2f} deviates {rel_error:.2%} from {spec.volume_mm3:.2f} "
                f"(tol {spec.volume_rel_tolerance:.2%})"
            )
    return failures, rel_error


# --------------------------------------------------------------------------- runners


class StrictCadCoderAgent(CadCoderAgent):
    """`CadCoderAgent` that can only ever produce code through a real LLM client."""

    def __init__(self, llm_client: LLMClient | None, **kwargs: Any) -> None:
        if llm_client is None:
            raise BenchmarkError("benchmark runs require a real LLM client; template fallback is disabled")
        super().__init__(llm_client, **kwargs)

    def _generate_parametric_template(self, contract: DesignContract) -> str:  # type: ignore[override]
        raise TemplateFallbackError(
            "refusing the deterministic parametric template during a benchmark run"
        )


class GateChecker:
    """Wraps `cad-cli check --json` and `cad-cli compare --json` behind one injectable object."""

    def __init__(self, bridge: CompilerBridge | None = None) -> None:
        self.bridge = bridge or CompilerBridge()

    def check(self, kcl_path: Path) -> CompileResult:
        return self.bridge.check(kcl_path)

    def compare(self, kcl_path: Path, reference_step: str) -> dict[str, Any]:
        """Run `cad-cli compare` and return its JSON report (metrics under `metrics`)."""
        cmd = [
            str(self.bridge.resolve_binary()), "compare", str(kcl_path), reference_step,
            "--json", *self.bridge._workspace_args(),
        ]
        payload, _ = self.bridge._run_json(cmd, subject=kcl_path)
        return payload


def evaluate_gate(contract: DesignContract, result: CompileResult, code: str) -> tuple[bool, bool, list[str]]:
    """Return (compiled, gate_passed, issues) for one `cad-cli check` result."""
    if result.has_errors:
        issues = [d.format_line() for d in result.error_diagnostics] or [f"status={result.status}"]
        return False, False, issues
    issues: list[str] = []
    for ok, message in (check_watertight(result.metrics), check_non_zero_volume(result.metrics)):
        if not ok:
            issues.append(message)
    ok_box, box_issues = check_keep_in_bbox(result.metrics, contract.keep_in_bbox_mm)
    if not ok_box:
        issues.extend(box_issues)
    iface_ok, iface_issues = check_interfaces(contract, result, kcl_source=code)
    if not iface_ok:
        issues.extend(iface_issues)
    return True, not issues, issues


class CadRunner(Protocol):
    """A system under test. One `run` call per spec."""

    name: str

    def run(self, spec: BenchmarkSpec, work_dir: Path) -> SpecResult: ...


class KclRunner:
    """Real-LLM CAD-DSL runner: generate -> `cad-cli check` -> repair loop."""

    def __init__(
        self,
        llm_client: LLMClient | None,
        *,
        gate: GateChecker | None = None,
        coder: CadCoderAgent | None = None,
        max_repairs: int = 2,
        name: str = "kcl-agent",
    ) -> None:
        if coder is not None and not isinstance(coder, StrictCadCoderAgent):
            raise BenchmarkError("coder must be a StrictCadCoderAgent (template fallback is forbidden)")
        self.coder = coder or StrictCadCoderAgent(llm_client)
        if getattr(self.coder, "llm_client", None) is None:
            raise BenchmarkError("benchmark runs require a real LLM client")
        self.gate = gate or GateChecker()
        self.max_repairs = max(0, int(max_repairs))
        self.name = name

    def run(self, spec: BenchmarkSpec, work_dir: Path) -> SpecResult:
        result = SpecResult(spec.id, spec.category, self.name)
        work_dir.mkdir(parents=True, exist_ok=True)
        contract = replace(spec.contract, summary=spec.contract.summary or spec.prompt)
        compile_result: CompileResult | None = None
        code = ""
        try:
            code = self.coder.generate(contract)
            for attempt in range(self.max_repairs + 1):
                kcl_path = work_dir / f"{spec.id}_iter{attempt}.kcl"
                kcl_path.write_text(code, encoding="utf-8")
                compile_result = self.gate.check(kcl_path)
                compiled, gate_ok, issues = evaluate_gate(contract, compile_result, code)
                if attempt == 0:
                    result.first_shot_compile = compiled
                    result.first_shot_gate = gate_ok
                if gate_ok or attempt == self.max_repairs:
                    result.final_gate = gate_ok
                    break
                verdict = CriticVerdict(
                    passed=False, iteration=attempt + 1, issues=issues,
                    repair_instructions="\n".join(issues), metrics=compile_result.metrics,
                )
                code = self.coder.repair(contract, code, compile_result, verdict)
                result.repair_attempts += 1
            if compile_result is not None:
                self._score(spec, contract, compile_result, code, kcl_path, result)
        except TemplateFallbackError:
            raise
        except (CompilerBridgeError, ContractValidationError, OSError) as exc:
            result.error = f"{type(exc).__name__}: {exc}"
        return result

    def _score(
        self, spec: BenchmarkSpec, contract: DesignContract, compile_result: CompileResult,
        code: str, kcl_path: Path, result: SpecResult,
    ) -> None:
        metrics = compile_result.metrics
        failures, rel = check_invariants(
            spec,
            bbox_size_mm=list(metrics.bbox_mm.dimensions) if metrics else None,
            volume_mm3=metrics.volume_mm3 if metrics else None,
        )
        iface_ok, iface_issues = check_interfaces(contract, compile_result, kcl_source=code)
        if spec.interface_features and not iface_ok:
            failures.extend(iface_issues)
        result.invariant_failures = failures
        result.invariants_ok = not failures and result.final_gate
        result.volume_rel_error = rel
        if spec.reference_step:
            report = self.gate.compare(kcl_path, spec.reference_step)
            exact = report.get("metrics")
            if isinstance(exact, dict) and _is_number(exact.get("relative_error")):
                result.exact_volume_rel_error = float(exact["relative_error"])
                result.exact_compare_passed = report.get("status") == "ok"
            else:
                result.exact_compare_passed = False
                result.error = result.error or "cad-cli compare produced no metrics"


@dataclass(slots=True)
class BaselineOutcome:
    """Result of a baseline code generator after executing its model."""

    compiled: bool
    bbox_size_mm: list[float] | None = None
    volume_mm3: float | None = None
    error: str | None = None


class BaselineRunner(Protocol):
    """Pluggable baseline (cadquery / build123d / ...). Returns measured geometry only."""

    name: str

    def build(self, spec: BenchmarkSpec, work_dir: Path) -> BaselineOutcome: ...


class BaselineAdapter:
    """Adapts a `BaselineRunner` to the `CadRunner` protocol (no repair loop for baselines)."""

    def __init__(self, baseline: BaselineRunner) -> None:
        self.baseline = baseline
        self.name = baseline.name

    def run(self, spec: BenchmarkSpec, work_dir: Path) -> SpecResult:
        result = SpecResult(spec.id, spec.category, self.name)
        work_dir.mkdir(parents=True, exist_ok=True)
        try:
            outcome = self.baseline.build(spec, work_dir)
        except (OSError, subprocess.SubprocessError) as exc:
            result.error = f"{type(exc).__name__}: {exc}"
            return result
        result.error = outcome.error
        result.first_shot_compile = outcome.compiled
        if not outcome.compiled:
            return result
        failures, rel = check_invariants(spec, bbox_size_mm=outcome.bbox_size_mm, volume_mm3=outcome.volume_mm3)
        result.first_shot_gate = not failures
        result.final_gate = result.first_shot_gate
        result.invariants_ok = not failures
        result.invariant_failures = failures
        result.volume_rel_error = rel
        return result


_BASELINE_SYSTEM = (
    "You write a complete, self-contained Python script using the `{lib}` library. "
    "The script must build the requested part as one solid in millimetres and export it with "
    "`{export}`. Output only code."
)
_BASELINE_LIBS = {
    "cadquery": ("cadquery", "cadquery.exporters.export(result, OUTPUT_STEP)"),
    "build123d": ("build123d", "build123d.export_step(result, OUTPUT_STEP)"),
}
_MEASURE_SNIPPET = (
    "\nimport json as _j, sys as _s\n"
    "try:\n"
    "    from OCP.GProp import GProp_GProps as _G\n"
    "    from OCP.BRepGProp import BRepGProp as _B\n"
    "    from OCP.Bnd import Bnd_Box as _Bx\n"
    "    from OCP.BRepBndLib import BRepBndLib as _BL\n"
    "    _shape = result.val().wrapped if hasattr(result, 'val') else result.wrapped\n"
    "    _p = _G(); _B.VolumeProperties_s(_shape, _p)\n"
    "    _b = _Bx(); _BL.Add_s(_shape, _b)\n"
    "    _x0, _y0, _z0, _x1, _y1, _z1 = _b.Get()\n"
    "    print('__MEASURE__' + _j.dumps({'volume_mm3': _p.Mass(), 'size': [_x1-_x0, _y1-_y0, _z1-_z0]}))\n"
    "except Exception as _e:\n"
    "    print('__MEASURE_ERROR__' + repr(_e)); _s.exit(3)\n"
)


class CodegenBaselineRunner:
    """LLM code-generation baseline for cadquery / build123d.

    Executes LLM-written Python in a subprocess; this is arbitrary code execution, so it is
    disabled unless `allow_execution=True` (use a sandboxed environment).
    """

    def __init__(
        self,
        library: str,
        llm_client: LLMClient,
        *,
        allow_execution: bool = False,
        timeout_sec: float = 120.0,
        python: str | None = None,
        runner: Callable[..., subprocess.CompletedProcess[str]] | None = None,
    ) -> None:
        if library not in _BASELINE_LIBS:
            raise BenchmarkError(f"unsupported baseline library {library!r}; choose {sorted(_BASELINE_LIBS)}")
        if llm_client is None:
            raise BenchmarkError("baseline requires an LLM client")
        self.library = library
        self.name = f"baseline-{library}"
        self.llm_client = llm_client
        self.allow_execution = allow_execution
        self.timeout_sec = timeout_sec
        self.python = python or sys.executable
        self._run = runner or subprocess.run

    def build(self, spec: BenchmarkSpec, work_dir: Path) -> BaselineOutcome:
        if not self.allow_execution:
            raise BenchmarkError("baseline code execution requires allow_execution=True")
        lib, export = _BASELINE_LIBS[self.library]
        code = self.llm_client.complete(
            _BASELINE_SYSTEM.format(lib=lib, export=export),
            f"{spec.prompt}\nBind the final solid to a variable named `result`; `OUTPUT_STEP` is predefined.",
        ).strip()
        if code.startswith("```"):
            code = "\n".join(line for line in code.splitlines() if not line.startswith("```"))
        step = work_dir / f"{spec.id}_{self.library}.step"
        script = work_dir / f"{spec.id}_{self.library}.py"
        script.write_text(f"OUTPUT_STEP = {str(step)!r}\n{code}\n{_MEASURE_SNIPPET}", encoding="utf-8")
        try:
            proc = self._run(
                [self.python, str(script)], capture_output=True, text=True, timeout=self.timeout_sec,
                check=False, cwd=str(work_dir),
            )
        except subprocess.TimeoutExpired:
            return BaselineOutcome(False, error=f"timeout after {self.timeout_sec}s")
        if proc.returncode != 0:
            return BaselineOutcome(False, error=(proc.stderr or proc.stdout or "")[-300:])
        for line in (proc.stdout or "").splitlines():
            if line.startswith("__MEASURE__"):
                data = json.loads(line[len("__MEASURE__"):])
                return BaselineOutcome(True, bbox_size_mm=data["size"], volume_mm3=data["volume_mm3"])
        return BaselineOutcome(False, error="baseline script produced no measurement")


# --------------------------------------------------------------------------- harness


def render_markdown(report: dict[str, Any]) -> str:
    def pct(v: float | None) -> str:
        return "n/a" if v is None else f"{v * 100:.1f}%"

    def sci(v: float | None) -> str:
        return "n/a" if v is None else f"{v:.3e}"

    lines = [
        f"# CAD evaluation report: {report['suite']}",
        "",
        f"Specs: {report['spec_count']}",
        "",
        "| Runner | N | First-shot compile | First-shot gate | Repair pass | Final gate | Invariants | Exact vol. err | Errors |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for name, data in report["runners"].items():
        o = data["metrics"]["overall"]
        lines.append(
            f"| {name} | {o['n']} | {pct(o['first_shot_compile_rate'])} | {pct(o['first_shot_gate_rate'])} | "
            f"{pct(o['repair_pass_rate'])} | {pct(o['final_gate_rate'])} | {pct(o['invariant_pass_rate'])} | "
            f"{sci(o['mean_exact_volume_rel_error'])} | {o['errors']} |"
        )
    for name, data in report["runners"].items():
        lines += ["", f"## {name} by category", "", "| Category | N | First-shot gate | Final gate | Invariants |", "|---|---|---|---|---|"]
        for cat, b in data["metrics"]["by_category"].items():
            lines.append(
                f"| {cat} | {b['n']} | {pct(b['first_shot_gate_rate'])} | {pct(b['final_gate_rate'])} | {pct(b['invariant_pass_rate'])} |"
            )
    return "\n".join(lines) + "\n"


class EvalHarness:
    """Runs every runner over every spec and produces one unified report."""

    def __init__(self, specs: Sequence[BenchmarkSpec], runners: Sequence[CadRunner], *, suite_name: str = "prismatic_suite") -> None:
        if not runners:
            raise BenchmarkError("at least one runner is required")
        names = [r.name for r in runners]
        if len(set(names)) != len(names):
            raise BenchmarkError(f"runner names must be unique: {names}")
        self.specs = list(specs)
        self.runners = list(runners)
        self.suite_name = suite_name

    def run(
        self,
        *,
        work_dir: str | Path | None = None,
        categories: Sequence[str] | None = None,
        limit: int | None = None,
    ) -> dict[str, Any]:
        specs = [s for s in self.specs if not categories or s.category in categories]
        if limit is not None:
            specs = specs[: max(0, limit)]
        base = Path(work_dir) if work_dir is not None else Path(tempfile.mkdtemp(prefix="cad_eval_"))
        runners: dict[str, Any] = {}
        for runner in self.runners:
            results: list[SpecResult] = []
            for spec in specs:
                try:
                    results.append(runner.run(spec, base / runner.name / spec.id))
                except TemplateFallbackError:
                    raise
                except Exception as exc:  # one broken spec must not sink the benchmark
                    results.append(SpecResult(spec.id, spec.category, runner.name, error=f"{type(exc).__name__}: {exc}"))
            runners[runner.name] = {
                "metrics": aggregate(results),
                "results": [asdict(r) for r in results],
            }
        return {"suite": self.suite_name, "spec_count": len(specs), "runners": runners}

    @staticmethod
    def write_reports(report: dict[str, Any], out_dir: str | Path) -> tuple[Path, Path]:
        out = Path(out_dir)
        out.mkdir(parents=True, exist_ok=True)
        json_path, md_path = out / "report.json", out / "report.md"
        json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        md_path.write_text(render_markdown(report), encoding="utf-8")
        return json_path, md_path
