"""Orchestrate CAD generation, compilation, inspection, and refinement."""

from __future__ import annotations

import os
import re
import shutil
import tempfile
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

from ._agent_utils import _assert_within_base, _sanitize_progress_value
from .spec_parser import SpecParserAgent
from .cad_coder import CadCoderAgent
from .llm import LLMConfigError
from .critic import VisualGeometricCritic
from .compiler_bridge import CompilerBridge, CompilerBridgeError
from .contracts import (
    VALID_MANUFACTURING_METHODS,
    BBox3D,
    CompileResult,
    CriticVerdict,
    DesignContract,
    ExternalInterfaceSummary,
)


@dataclass(slots=True)
class IterationRecord:
    """Trace entry for one iteration of the self-refinement loop."""

    iteration: int
    code: str
    compile_result: CompileResult
    verdict: CriticVerdict


@dataclass(slots=True)
class PipelineResult:
    """Final output of `SelfRefinementPipeline.run`."""

    passed: bool
    contract: DesignContract
    final_code: str
    final_compile_result: CompileResult
    final_verdict: CriticVerdict
    iterations: list[IterationRecord] = field(default_factory=list)
    kcl_path: Path | None = None
    work_dir: Path | None = None

    @property
    def step_path(self) -> Path | None:
        return Path(self.final_compile_result.step_path) if self.final_compile_result.step_path else None

    @property
    def mesh_path(self) -> Path | None:
        return Path(self.final_compile_result.mesh_path) if self.final_compile_result.mesh_path else None

    @property
    def views_dir(self) -> Path | None:
        return Path(self.final_compile_result.views_dir) if self.final_compile_result.views_dir else None

    @property
    def dxf_path(self) -> Path | None:
        return Path(self.final_compile_result.dxf_path) if self.final_compile_result.dxf_path else None

    @property
    def drawing_path(self) -> Path | None:
        return Path(self.final_compile_result.drawing_path) if self.final_compile_result.drawing_path else None

    def cleanup(self) -> None:
        """Explicitly delete `work_dir` when the caller finishes reading artifacts."""
        if self.work_dir is not None and self.work_dir.exists():
            shutil.rmtree(self.work_dir, ignore_errors=True)


def _infer_edit_contract(
    existing_code: str,
    instruction: str,
    *,
    material: str | None = None,
    manufacturing_method: str | None = None,
) -> DesignContract:
    """Build a permissive DesignContract from existing KCL metadata for incremental edits."""
    asm_match = re.search(r"^\s*assembly\s+([A-Za-z_][A-Za-z0-9_]*)\b", existing_code, re.MULTILINE)
    part_matches = re.findall(r"^\s*part\s+([A-Za-z_][A-Za-z0-9_]*)\b", existing_code, re.MULTILINE)
    if asm_match:
        part_name = asm_match.group(1)
        archetype = "assembly"
    elif part_matches:
        part_name = part_matches[-1]
        archetype = "parametric_part"
    else:
        part_name = "EditedPart"
        archetype = "parametric_part"

    mat_match = re.search(r'@material\(\s*"([^"]+)"\s*\)', existing_code)
    resolved_material = material or (mat_match.group(1) if mat_match else "Al-6061-T6")

    mfg_match = re.search(r'@manufacturing\([^)]*method\s*=\s*"([^"]+)"', existing_code)
    detected_mfg = mfg_match.group(1) if mfg_match else "CNC_3AXIS_MILLING"
    resolved_mfg = manufacturing_method or (
        detected_mfg if detected_mfg in VALID_MANUFACTURING_METHODS else "CNC_3AXIS_MILLING"
    )

    contract = DesignContract(
        part_name=part_name,
        units={"length": "mm", "angle": "deg", "force": "N"},
        material=resolved_material,
        manufacturing_method=resolved_mfg,
        keep_in_bbox_mm=BBox3D(min=[-1e6, -1e6, -1e6], max=[1e6, 1e6, 1e6]),
        notes=instruction.strip(),
        summary=instruction.strip(),
        part_archetype=archetype,
    )
    contract.assert_valid()
    return contract


class SelfRefinementPipeline:
    """Drives SpecParser -> CadCoder -> CompilerBridge -> VisualGeometricCritic loop."""

    def __init__(
        self,
        *,
        spec_parser: SpecParserAgent | None = None,
        cad_coder: CadCoderAgent | None = None,
        compiler_bridge: CompilerBridge | None = None,
        critic: VisualGeometricCritic | None = None,
    ) -> None:
        self.spec_parser = spec_parser or SpecParserAgent()
        self.cad_coder = cad_coder or CadCoderAgent()
        self.compiler_bridge = compiler_bridge or CompilerBridge()
        self.critic = critic or VisualGeometricCritic()

    def _check_external_interference(
        self, kcl_path: Path, context: ExternalInterfaceSummary,
    ) -> tuple[bool, str]:
        try:
            result = self.compiler_bridge.check_external_interference(kcl_path, context)
        except CompilerBridgeError:
            return False, "Exact external interference check could not run."
        if result.exit_code == 0 and result.status == "ok" and not result.error_diagnostics:
            return True, "Exact model and reference have no interfering volume within tolerance."
        for error in result.error_diagnostics:
            if error.code == "E506_INTERFERENCE_DETECTED":
                return False, error.message
        codes = ", ".join(error.code for error in result.error_diagnostics)
        return False, f"Exact external interference check failed ({codes or result.status})."

    def _collect_known_secrets(self) -> tuple[str, ...]:
        candidates: list[str] = []
        for env_name in ("OPENAI_API_KEY", "GEMINI_API_KEY"):
            val = os.environ.get(env_name, "").strip()
            if val:
                candidates.append(val)
        for client in (
            getattr(self.spec_parser, "llm_client", None),
            getattr(self.cad_coder, "llm_client", None),
            getattr(getattr(self.critic, "view_inspector", None), "llm_client", None),
        ):
            key = getattr(client, "api_key", None)
            if isinstance(key, str) and key.strip():
                candidates.append(key.strip())
        return tuple(dict.fromkeys(candidates))

    def _emit_progress(
        self,
        progress_callback: Callable[[dict[str, Any]], None] | None,
        event: dict[str, Any],
    ) -> None:
        if progress_callback is None:
            return
        secrets = self._collect_known_secrets()
        safe_event = _sanitize_progress_value(event, secrets)
        progress_callback(safe_event)

    def _execute_loop(
        self,
        contract: DesignContract,
        initial_code: str,
        *,
        max_iters: int,
        work_dir: str | Path | None,
        build_artifacts: bool,
        include_drawings: bool,
        cleanup_temp: bool,
        edit_instruction: str | None = None,
        progress_callback: Callable[[dict[str, Any]], None] | None = None,
    ) -> PipelineResult:
        created_temp = False
        if work_dir is None:
            base_dir = Path(tempfile.mkdtemp(prefix="cad_agent_")).resolve()
            created_temp = True
        else:
            base_dir = Path(work_dir).resolve()
            base_dir.mkdir(parents=True, exist_ok=True)

        history: list[IterationRecord] = []
        code = initial_code
        kcl_path = _assert_within_base(base_dir, base_dir / f"{contract.part_name}.kcl")

        try:
            for iteration in range(1, max_iters + 1):
                if iteration > 1:
                    self._emit_progress(
                        progress_callback,
                        {
                            "event": "progress",
                            "stage": "iteration_start",
                            "iteration": iteration,
                            "max_iters": max_iters,
                            "message": f"Starting iteration {iteration}/{max_iters} (refining KCL code)",
                        },
                    )
                    if edit_instruction is not None:
                        code = self.cad_coder.refine(
                            contract,
                            code,
                            compile_result,
                            verdict,
                            instruction=edit_instruction,
                        )
                    else:
                        code = self.cad_coder.repair(
                            contract, code, compile_result, verdict
                        )
                    self._emit_progress(
                        progress_callback,
                        {
                            "event": "progress",
                            "stage": "code_generated",
                            "iteration": iteration,
                            "retrieved_examples": list(self.cad_coder.last_retrieved_examples),
                            "message": f"Refined KCL code for iteration {iteration}/{max_iters}",
                        },
                    )

                kcl_path.write_text(code, encoding="utf-8")
                iter_dir = _assert_within_base(base_dir, base_dir / f"iter_{iteration}")
                if build_artifacts:
                    step_out = _assert_within_base(iter_dir, iter_dir / f"{contract.part_name}.step")
                    mesh_out = _assert_within_base(iter_dir, iter_dir / f"{contract.part_name}.mesh.json")
                    views_out = _assert_within_base(iter_dir, iter_dir / "views")
                    dxf_out = (
                        _assert_within_base(iter_dir, iter_dir / f"{contract.part_name}.dxf")
                        if include_drawings
                        else None
                    )
                    drawing_out = (
                        _assert_within_base(iter_dir, iter_dir / f"{contract.part_name}_drawing.svg")
                        if include_drawings
                        else None
                    )
                    compile_result = self.compiler_bridge.build(
                        kcl_path,
                        out_step=step_out,
                        out_mesh=mesh_out,
                        out_views=views_out,
                        out_dxf=dxf_out,
                        out_drawing=drawing_out,
                    )
                elif contract.external_context is not None:
                    # Context alignment needs cylindrical mesh evidence even without export artifacts.
                    mesh_out = _assert_within_base(iter_dir, iter_dir / f"{contract.part_name}.mesh.json")
                    compile_result = self.compiler_bridge.build(kcl_path, out_mesh=mesh_out)
                else:
                    compile_result = self.compiler_bridge.check(kcl_path)

                compile_error_count = len(compile_result.error_diagnostics)
                compile_passed = (compile_result.status == "ok") and (compile_error_count == 0)
                self._emit_progress(
                    progress_callback,
                    {
                        "event": "progress",
                        "stage": "compile_done",
                        "iteration": iteration,
                        "passed": compile_passed,
                        "error_count": compile_error_count,
                        "message": (
                            f"Compilation succeeded on iteration {iteration}/{max_iters}"
                            if compile_passed
                            else f"Compilation failed on iteration {iteration}/{max_iters} with {compile_error_count} error(s)"
                        ),
                    },
                )

                context = contract.external_context
                external_interference = (
                    self._check_external_interference(kcl_path, context)
                    if compile_passed and context is not None and context.exact_asset_path is not None
                    else None
                )

                def render_shaded_views() -> dict[str, bytes] | None:
                    if build_artifacts and compile_passed and compile_result.exit_code == 0:
                        return self.compiler_bridge.render_shaded_views(kcl_path)
                    return None

                verdict = self.critic.evaluate(
                    contract, compile_result, iteration=iteration,
                    external_interference=external_interference,
                    render_shaded_views=render_shaded_views,
                )
                issues_count = len(verdict.issues)
                self._emit_progress(
                    progress_callback,
                    {
                        "event": "progress",
                        "stage": "critic_done",
                        "iteration": iteration,
                        "passed": bool(verdict.passed),
                        "issues_count": issues_count,
                        "message": (
                            f"Verification passed on iteration {iteration}/{max_iters}"
                            if verdict.passed
                            else f"Verification failed on iteration {iteration}/{max_iters} with {issues_count} issue(s)"
                        ),
                    },
                )
                history.append(
                    IterationRecord(
                        iteration=iteration,
                        code=code,
                        compile_result=compile_result,
                        verdict=verdict,
                    )
                )
                if verdict.passed:
                    return PipelineResult(
                        passed=True,
                        contract=contract,
                        final_code=code,
                        final_compile_result=compile_result,
                        final_verdict=verdict,
                        iterations=history,
                        kcl_path=kcl_path,
                        work_dir=base_dir,
                    )

            last = history[-1]
            return PipelineResult(
                passed=False,
                contract=contract,
                final_code=last.code,
                final_compile_result=last.compile_result,
                final_verdict=last.verdict,
                iterations=history,
                kcl_path=kcl_path,
                work_dir=base_dir,
            )
        finally:
            if created_temp and cleanup_temp and base_dir.exists():
                shutil.rmtree(base_dir, ignore_errors=True)

    def run(
        self,
        requirement_or_contract: str | DesignContract,
        *,
        external_context: ExternalInterfaceSummary | None = None,
        max_iters: int = 3,
        work_dir: str | Path | None = None,
        build_artifacts: bool = True,
        include_drawings: bool = False,
        cleanup_temp: bool = False,
        progress_callback: Callable[[dict[str, Any]], None] | None = None,
    ) -> PipelineResult:
        if max_iters < 1:
            raise ValueError("max_iters must be >= 1")

        if isinstance(requirement_or_contract, DesignContract):
            contract = requirement_or_contract
            if external_context is not None:
                contract = replace(contract, external_context=external_context)
        elif external_context is not None:
            contract = self.spec_parser.parse(requirement_or_contract, external_context=external_context)
        else:
            contract = self.spec_parser.parse(requirement_or_contract)
        contract.assert_valid()
        if type(self.cad_coder) is CadCoderAgent and self.cad_coder.llm_client is None:
            raise LLMConfigError(
                "SelfRefinementPipeline.run() requires a configured real LLM client. "
                "Hardcoded offline geometry templates have been retired."
            )
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "spec_parsed",
                "part_name": contract.part_name,
                "repair_attempts": int(self.spec_parser.last_repair_attempts),
                "message": f"Design specification ready for {contract.part_name}",
            },
        )
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "iteration_start",
                "iteration": 1,
                "max_iters": max_iters,
                "message": f"Starting iteration 1/{max_iters}",
            },
        )

        initial_code = self.cad_coder.generate(contract)
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "code_generated",
                "iteration": 1,
                "retrieved_examples": list(self.cad_coder.last_retrieved_examples),
                "message": f"Generated KCL code for iteration 1/{max_iters}",
            },
        )
        return self._execute_loop(
            contract,
            initial_code,
            max_iters=max_iters,
            work_dir=work_dir,
            build_artifacts=build_artifacts,
            include_drawings=include_drawings,
            cleanup_temp=cleanup_temp,
            progress_callback=progress_callback,
        )

    def run_edit(
        self,
        existing_code: str,
        instruction: str,
        *,
        contract: DesignContract | None = None,
        external_context: ExternalInterfaceSummary | None = None,
        max_iters: int = 3,
        work_dir: str | Path | None = None,
        build_artifacts: bool = True,
        include_drawings: bool = True,
        cleanup_temp: bool = False,
        progress_callback: Callable[[dict[str, Any]], None] | None = None,
    ) -> PipelineResult:
        """Incrementally modify existing KCL code and run compile + L0-L2 self-refinement."""
        if max_iters < 1:
            raise ValueError("max_iters must be >= 1")

        effective_contract = (
            contract
            if contract is not None
            else _infer_edit_contract(existing_code, instruction)
        )
        if external_context is not None:
            effective_contract = replace(effective_contract, external_context=external_context)
        effective_contract.assert_valid()
        if type(self.cad_coder) is CadCoderAgent and self.cad_coder.llm_client is None:
            raise LLMConfigError(
                "SelfRefinementPipeline.run_edit() requires a configured real LLM client. "
                "Hardcoded offline geometry templates have been retired."
            )
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "spec_parsed",
                "part_name": effective_contract.part_name,
                "repair_attempts": int(self.spec_parser.last_repair_attempts),
                "message": f"Edit specification ready for {effective_contract.part_name}",
            },
        )
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "iteration_start",
                "iteration": 1,
                "max_iters": max_iters,
                "message": f"Starting iteration 1/{max_iters}",
            },
        )

        initial_code = self.cad_coder.edit(
            existing_code,
            instruction,
            contract=effective_contract if external_context is not None else contract,
        )
        self._emit_progress(
            progress_callback,
            {
                "event": "progress",
                "stage": "code_generated",
                "iteration": 1,
                "retrieved_examples": list(self.cad_coder.last_retrieved_examples),
                "message": f"Generated edited KCL code for iteration 1/{max_iters}",
            },
        )
        return self._execute_loop(
            effective_contract,
            initial_code,
            max_iters=max_iters,
            work_dir=work_dir,
            build_artifacts=build_artifacts,
            include_drawings=include_drawings,
            cleanup_temp=cleanup_temp,
            edit_instruction=instruction,
            progress_callback=progress_callback,
        )
