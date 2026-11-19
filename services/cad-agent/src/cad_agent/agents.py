"""L4 Multi-Agent closed-loop pipeline: SpecParser -> CadCoder -> VisualGeometricCritic."""

from __future__ import annotations

import json
import math
import os
import re
import shutil
import tempfile
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

from .compiler_bridge import (
    CompilerBridge,
    check_keep_in_bbox,
    check_keep_out_zones,
    check_non_zero_volume,
    check_watertight,
    find_workspace_root,
)
from .contracts import (
    BBox3D,
    CompileResult,
    ContractValidationError,
    CriticVerdict,
    DesignContract,
    GateCheckResult,
    LoadCase,
    MechanicalInterface,
)


class LLMClient(Protocol):
    """Minimal protocol for pluggable LLM backends (OpenAI, Gemini, vLLM, mock)."""

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        """Return model completion text given system and user prompts."""


class OpenAICompatibleClient:
    """Zero-dependency OpenAI / vLLM REST client via `urllib.request`."""

    def __init__(
        self,
        *,
        model: str = "gpt-4o-mini",
        api_key: str | None = None,
        base_url: str = "https://api.openai.com/v1",
        temperature: float = 0.1,
        timeout_sec: float = 60.0,
    ) -> None:
        self.model = model
        self.api_key = api_key or os.environ.get("OPENAI_API_KEY", "")
        self.base_url = base_url.rstrip("/")
        self.temperature = temperature
        self.timeout_sec = timeout_sec

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        url = f"{self.base_url}/chat/completions"
        body = json.dumps(
            {
                "model": self.model,
                "temperature": self.temperature,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
            }
        ).encode("utf-8")
        headers = {"Content-Type": "application/json"}
        if self.api_key:
            headers["Authorization"] = f"Bearer {self.api_key}"
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")
        with urllib.request.urlopen(req, timeout=self.timeout_sec) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
        return str(payload["choices"][0]["message"]["content"])


class GeminiRestClient:
    """Zero-dependency Gemini REST API client via `urllib.request`."""

    def __init__(
        self,
        *,
        model: str = "gemini-2.5-flash",
        api_key: str | None = None,
        temperature: float = 0.1,
        timeout_sec: float = 60.0,
    ) -> None:
        self.model = model
        self.api_key = api_key or os.environ.get("GEMINI_API_KEY", "")
        self.temperature = temperature
        self.timeout_sec = timeout_sec

    def complete(self, system_prompt: str, user_prompt: str) -> str:
        url = (
            f"https://generativelanguage.googleapis.com/v1beta/models/"
            f"{self.model}:generateContent?key={self.api_key}"
        )
        body = json.dumps(
            {
                "systemInstruction": {"parts": [{"text": system_prompt}]},
                "contents": [{"role": "user", "parts": [{"text": user_prompt}]}],
                "generationConfig": {"temperature": self.temperature},
            }
        ).encode("utf-8")
        req = urllib.request.Request(
            url,
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(req, timeout=self.timeout_sec) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
        return str(payload["candidates"][0]["content"]["parts"][0]["text"])


def _strip_code_fences(text: str) -> str:
    """Extract raw code or JSON when an LLM wraps output in markdown fences."""
    stripped = text.strip()
    fence_match = re.search(r"```(?:[a-zA-Z0-9_-]+)?\s*\n(.*?)```", stripped, re.DOTALL)
    if fence_match:
        return fence_match.group(1).strip() + "\n"
    return stripped


def _to_pascal_case(raw: str, fallback: str = "GeneratedPart") -> str:
    tokens = re.findall(r"[A-Za-z0-9]+", raw)
    if not tokens:
        return fallback
    joined = "".join(tok[:1].upper() + tok[1:] for tok in tokens[:4])
    if not joined[0].isalpha():
        return f"Part{joined}"
    return joined


class SpecParserAgent:
    """Parses natural-language CAD requirements into a validated `DesignContract`."""

    SYSTEM_PROMPT = (
        "You are SpecParserAgent for an AI-Native CAD compiler. "
        "Convert the user's mechanical requirement into strict JSON matching "
        "`schemas/design-contract.schema.json` with keys: part_name, units, material, "
        "manufacturing_method, keep_in_bbox_mm ({min:[x,y,z], max:[x,y,z]}), "
        "keep_out_zones, interfaces, loads. Return ONLY JSON."
    )

    def __init__(self, llm_client: LLMClient | None = None) -> None:
        self.llm_client = llm_client

    def parse(self, prompt: str) -> DesignContract:
        if self.llm_client is not None:
            raw = self.llm_client.complete(self.SYSTEM_PROMPT, prompt)
            cleaned = _strip_code_fences(raw)
            data = json.loads(cleaned)
            contract = DesignContract.from_dict(data)
            contract.assert_valid()
            return contract
        return self._parse_heuristic(prompt)

    def _parse_heuristic(self, prompt: str) -> DesignContract:
        text = prompt.strip()
        lower = text.lower()

        material = "Al-6061-T6"
        for pattern, mat_name in (
            (r"7075", "Al-7075-T6"),
            (r"6061", "Al-6061-T6"),
            (r"sus\s*304|304", "SUS304"),
            (r"ti-?6al-?4v|titanium", "Ti-6Al-4V"),
            (r"steel|45#|q235", "AISI-1045"),
        ):
            if re.search(pattern, lower):
                material = mat_name
                break

        manufacturing_method = "CNC_3AXIS_MILLING"
        if any(k in lower for k in ("5-axis", "5 axis", "五轴")):
            manufacturing_method = "CNC_5AXIS_MILLING"
        elif any(k in lower for k in ("3d print", "additive", "slm", "打印", "增材")):
            manufacturing_method = "ADDITIVE_SLM"
        elif any(k in lower for k in ("sheet metal", "钣金")):
            manufacturing_method = "SHEET_METAL_BENDING"

        dims_match = re.search(
            r"(\d+(?:\.\d+)?)\s*(?:mm)?\s*[x×*]\s*(\d+(?:\.\d+)?)\s*(?:mm)?\s*[x×*]\s*(\d+(?:\.\d+)?)\s*mm?",
            lower,
        )
        if dims_match:
            w, d, h = (
                float(dims_match.group(1)),
                float(dims_match.group(2)),
                float(dims_match.group(3)),
            )
            keep_in = BBox3D(min=[-w / 2.0, -d / 2.0, 0.0], max=[w / 2.0, d / 2.0, h])
        else:
            od_match = re.search(r"(?:od|diameter|直径|外径)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower)
            h_match = re.search(r"(?:height|thickness|高|厚)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower)
            od = float(od_match.group(1)) if od_match else 100.0
            h = float(h_match.group(1)) if h_match else 40.0
            keep_in = BBox3D(min=[-od / 2.0, -od / 2.0, 0.0], max=[od / 2.0, od / 2.0, h])

        interfaces: list[MechanicalInterface] = []
        bore_match = re.search(
            r"(?:(?:bore|中心孔|轴承孔)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
            r"|(\d+(?:\.\d+)?)\s*mm(?:\s+[a-z]\d+)?\s*(?:bore|中心孔|轴承孔))",
            lower,
        )
        if bore_match:
            bore_val = float(bore_match.group(1) or bore_match.group(2))
            tol_match = re.search(r"\b([A-Z]\d+)\b", text)
            interfaces.append(
                MechanicalInterface(
                    interface_id="main_bore",
                    kind="bore",
                    diameter_mm=bore_val,
                    tolerance=tol_match.group(1) if tol_match else "H7",
                    preserve_exact_brep=True,
                )
            )
        bolt_match = re.search(r"(\d+)\s*[x×]\s*m(\d+)", lower)
        if bolt_match:
            count = int(bolt_match.group(1))
            m_size = float(bolt_match.group(2))
            interfaces.append(
                MechanicalInterface(
                    interface_id="mounting_bolt_pattern",
                    kind="bolt_pattern",
                    diameter_mm=round(m_size * 1.1, 2),
                    count=count,
                    preserve_exact_brep=True,
                )
            )

        loads: list[LoadCase] = []
        load_match = re.search(r"(\d+(?:\.\d+)?)\s*(k?n)\b", lower)
        if load_match:
            val = float(load_match.group(1))
            if load_match.group(2) == "kn":
                val *= 1000.0
            loads.append(
                LoadCase(
                    load_id="primary_axial_load",
                    kind="force",
                    target_interface=interfaces[0].interface_id if interfaces else "primary_face",
                    magnitude=val,
                    unit="N",
                    vector=[0.0, 0.0, -val],
                    min_safety_factor=2.0,
                )
            )

        name_hint = re.sub(r"[^A-Za-z0-9\s]", " ", text)
        part_name = _to_pascal_case(name_hint, fallback="CustomBracket")
        if len(part_name) < 4:
            part_name = "CustomBracket"

        # Detect Phase 1 v0.2 archetype and feature parameters
        part_archetype = "parametric_part"
        feature_params: dict[str, Any] = {}

        is_assembly = bool(re.search(r"\b(?:assembl(?:y|ies)|multi-part|components|mates?)\b", lower)) or any(
            k in lower for k in ("装配", "组件", "多件")
        ) or ("端盖" in lower and "底座" in lower)
        is_shelled = any(
            k in lower for k in ("shell", "control box", "enclosure", "抽壳", "控制盒", "壳体", "外壳")
        )
        is_swept = any(
            k in lower
            for k in ("sweep", "swept", "loft", "manifold", "pipe", "duct", "扫掠", "放样", "流道", "管道", "歧管")
        )
        is_ribbed = any(
            k in lower
            for k in ("bearing", "rib", "bracket", "flange", "mount", "轴承座", "支架", "加强筋", "法兰")
        )

        if is_assembly:
            part_archetype = "assembly"
        elif is_shelled:
            part_archetype = "shelled_box"
            wall_m = re.search(
                r"(?:wall(?: thickness)?|壁厚)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
                r"|(\d+(?:\.\d+)?)\s*mm\s*(?:walls?\b|壁厚)",
                lower,
            )
            feature_params["wall_thickness_mm"] = float(wall_m.group(1) or wall_m.group(2)) if wall_m else 2.5
            thread_m = re.search(r"\b(m\d+(?:\.\d+)?(?:[x×]\d+(?:\.\d+)?)?(?:-\d+[a-z])?)\b", lower)
            if thread_m:
                raw_spec = thread_m.group(1).upper()
                if "X" not in raw_spec and raw_spec == "M4":
                    raw_spec = "M4x0.7-6H"
                elif "X" not in raw_spec and raw_spec == "M6":
                    raw_spec = "M6x1.0-6H"
                elif "X" not in raw_spec and raw_spec == "M5":
                    raw_spec = "M5x0.8-6H"
                elif "X" not in raw_spec and raw_spec == "M8":
                    raw_spec = "M8x1.25-6H"
                feature_params["thread_spec"] = raw_spec.replace("X", "x").replace("×", "x")
            elif any(k in lower for k in ("tapped", "threaded", "螺纹", "攻丝")):
                feature_params["thread_spec"] = "M4x0.7-6H"
            if "thread_spec" in feature_params:
                interfaces = [iface for iface in interfaces if iface.kind != "bolt_pattern"]
                if bolt_match:
                    feature_params["tap_count"] = int(bolt_match.group(1))
            feature_params["mirror_lugs"] = bool(re.search(r"\b(?:mirror(?:ed)?|lugs?|ears?)\b", lower)) or any(
                k in lower for k in ("对称", "安装耳", "凸耳")
            )
        elif is_swept:
            part_archetype = "swept_manifold"
            pipe_m = re.search(
                r"(?:diameter|直径|管径)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
                r"|(\d+(?:\.\d+)?)\s*mm\s*(?:diameter\b|直径|管径)",
                lower,
            )
            feature_params["pipe_diameter_mm"] = float(pipe_m.group(1) or pipe_m.group(2)) if pipe_m else round(
                min(keep_in.dimensions) * 0.45, 2
            )
        elif is_ribbed:
            part_archetype = "ribbed_bracket"

        contract = DesignContract(
            part_name=part_name,
            units={"length": "mm", "angle": "deg", "force": "N"},
            material=material,
            manufacturing_method=manufacturing_method,
            keep_in_bbox_mm=keep_in,
            interfaces=interfaces,
            loads=loads,
            notes=text,
            summary=text,
            part_archetype=part_archetype,
            feature_params=feature_params,
        )
        contract.assert_valid()
        return contract


class CadCoderAgent:
    """Synthesizes and repairs `.kcl` CAD-DSL code using `dsl-spec/` + `examples/mvp-*`."""

    def __init__(
        self,
        llm_client: LLMClient | None = None,
        *,
        workspace_root: Path | None = None,
        max_few_shot: int = 3,
    ) -> None:
        self.llm_client = llm_client
        self.workspace_root = (
            Path(workspace_root).resolve() if workspace_root is not None else find_workspace_root()
        )
        self.max_few_shot = max_few_shot
        self._cached_spec_max_chars: int | None = None
        self._cached_dsl_spec: str | None = None
        self._cached_few_shot: list[tuple[str, str]] | None = None
        self._cached_system_prompt: str | None = None

    def refresh_prompt_cache(self) -> None:
        """Invalidate cached DSL spec and few-shot prompt material."""
        self._cached_spec_max_chars = None
        self._cached_dsl_spec = None
        self._cached_few_shot = None
        self._cached_system_prompt = None

    def load_dsl_spec_excerpt(self, max_chars: int = 4000, *, force_refresh: bool = False) -> str:
        """Load and cache CAD-DSL specification markdown from `dsl-spec/` up to `max_chars`."""
        if (
            not force_refresh
            and self._cached_dsl_spec is not None
            and self._cached_spec_max_chars == max_chars
        ):
            return self._cached_dsl_spec

        spec_dir = self.workspace_root / "dsl-spec"
        if not spec_dir.is_dir():
            self._cached_dsl_spec = ""
            self._cached_spec_max_chars = max_chars
            return ""

        chunks: list[str] = []
        remaining = max_chars
        for md_file in sorted(spec_dir.glob("*.md")):
            if remaining <= 0:
                break
            header = f"### {md_file.name}\n"
            with md_file.open("r", encoding="utf-8") as fh:
                body = fh.read(max(0, remaining - len(header)))
            chunk = header + body
            chunks.append(chunk)
            remaining -= len(chunk) + 2

        combined = "\n\n".join(chunks)[:max_chars]
        self._cached_dsl_spec = combined
        self._cached_spec_max_chars = max_chars
        return combined

    def load_few_shot_examples(self, *, force_refresh: bool = False) -> list[tuple[str, str]]:
        """Discover, load, and cache few-shot `.kcl` files from `examples/mvp-*` (or `dsl-spec/`)."""
        if not force_refresh and self._cached_few_shot is not None:
            return list(self._cached_few_shot)

        examples: list[tuple[str, str]] = []
        examples_dir = self.workspace_root / "examples"
        if examples_dir.is_dir():
            for mvp_dir in sorted(examples_dir.glob("mvp-*")):
                if not mvp_dir.is_dir():
                    continue
                for kcl_file in sorted(mvp_dir.glob("*.kcl")):
                    examples.append(
                        (
                            kcl_file.relative_to(self.workspace_root).as_posix(),
                            kcl_file.read_text(encoding="utf-8"),
                        )
                    )
                    if len(examples) >= self.max_few_shot:
                        self._cached_few_shot = examples
                        return list(examples)

        dsl_example = self.workspace_root / "dsl-spec" / "bearing_mount_example.kcl"
        if dsl_example.is_file() and len(examples) < self.max_few_shot:
            examples.append(
                (
                    dsl_example.relative_to(self.workspace_root).as_posix(),
                    dsl_example.read_text(encoding="utf-8"),
                )
            )
        self._cached_few_shot = examples
        return list(examples)

    def build_system_prompt(self, *, force_refresh: bool = False) -> str:
        """Construct and cache the CAD-DSL system prompt combining language spec + few-shot examples."""
        if not force_refresh and self._cached_system_prompt is not None:
            return self._cached_system_prompt

        spec_text = self.load_dsl_spec_excerpt(force_refresh=force_refresh)
        few_shots = self.load_few_shot_examples(force_refresh=force_refresh)

        sections = [
            "You are CadCoderAgent. Generate strictly valid, strongly-typed CAD-DSL (.kcl) code.",
            "Rules:",
            "1. Declare @unit(length = \"mm\", angle = \"deg\", force = \"N\"), @material(...), @manufacturing(...).",
            "2. Use explicit dimensional suffixes (mm, deg, N); never mix Length and Angle.",
            "3. Close solid profiles via region::closed([...]); use region::path([...]) for open sweep paths.",
            "4. Return a watertight Solid, or an Assembly containing watertight components with explicit placements. "
            "Mates validate placement; they do not move components.",
            "5. Keep the model inside the contract's keep_in_bbox_mm and implement all required interfaces.",
        ]
        if spec_text:
            sections.append("## Language Specification\n" + spec_text)
        if few_shots:
            sections.append("## Few-Shot Golden Examples")
            for rel_path, content in few_shots:
                sections.append(f"### {rel_path}\n```kcl\n{content.strip()}\n```")
        prompt = "\n\n".join(sections)
        self._cached_system_prompt = prompt
        return prompt

    def generate(self, contract: DesignContract) -> str:
        if self.llm_client is not None:
            user_prompt = (
                "Generate a complete `.kcl` part or assembly implementing this DesignContract:\n"
                f"```json\n{json.dumps(contract.to_dict(), indent=2, ensure_ascii=False)}\n```\n"
                "Return ONLY valid `.kcl` code."
            )
            return _strip_code_fences(self.llm_client.complete(self.build_system_prompt(), user_prompt))
        return self._generate_parametric_template(contract)

    def repair(
        self,
        contract: DesignContract,
        previous_code: str,
        compile_result: CompileResult,
        verdict: CriticVerdict,
    ) -> str:
        if self.llm_client is not None:
            diag_lines = [d.format_line() for d in compile_result.diagnostics]
            user_prompt = (
                "The previous `.kcl` code failed verification. Fix ONLY the failing lines/parameters.\n"
                f"### DesignContract\n```json\n{json.dumps(contract.to_dict(), indent=2, ensure_ascii=False)}\n```\n"
                f"### Compiler Diagnostics\n" + ("\n".join(diag_lines) or "(none)") + "\n"
                f"### Critic Issues\n" + ("\n".join(f"- {i}" for i in verdict.issues) or "(none)") + "\n"
                f"### Repair Instructions\n{verdict.repair_instructions}\n"
                f"### Previous `.kcl`\n```kcl\n{previous_code.strip()}\n```\n"
                "Return ONLY the corrected `.kcl` source."
            )
            return _strip_code_fences(self.llm_client.complete(self.build_system_prompt(), user_prompt))

        return self._repair_heuristic(contract, previous_code, compile_result, verdict)

    def _generate_parametric_template(self, contract: DesignContract) -> str:
        archetype = contract.part_archetype
        if archetype == "shelled_box":
            return self._generate_shelled_box_template(contract)
        if archetype == "swept_manifold":
            return self._generate_swept_manifold_template(contract)
        if archetype == "ribbed_bracket":
            return self._generate_ribbed_bracket_template(contract)
        if archetype == "assembly":
            return self._generate_assembly_template(contract)

        w, d, h = contract.keep_in_bbox_mm.dimensions
        cx, cy, _ = contract.keep_in_bbox_mm.center
        z0 = contract.keep_in_bbox_mm.min[2]
        ox = cx - w / 2.0
        oy = cy - d / 2.0

        bore_iface = next(
            (i for i in contract.interfaces if i.kind in ("bore", "shaft_seat")),
            None,
        )
        bore_d = (
            bore_iface.diameter_mm
            if (bore_iface and bore_iface.diameter_mm)
            else min(w, d) * 0.35
        )
        bore_tag = bore_iface.interface_id if bore_iface else "main_bore"
        bore_tol_annot = (
            f' @pmi::tolerance("{bore_iface.tolerance}")'
            if (bore_iface and bore_iface.tolerance)
            else ""
        )

        bolt_iface = next(
            (
                i
                for i in contract.interfaces
                if i.kind in ("bolt_pattern", "bolt_circle", "counterbore_pattern")
            ),
            None,
        )

        lines = [
            f'@unit(length = "{contract.length_unit}", angle = "{contract.angle_unit}", force = "{contract.force_unit}")',
            f'@material("{contract.material_name}")',
            f'@manufacturing(method = "{contract.manufacturing_method}", min_tool_radius = 1.5mm)',
            "",
            f"part {contract.part_name}(",
            f"    width: Length = {w:.2f}mm,",
            f"    depth: Length = {d:.2f}mm,",
            f"    height: Length = {h:.2f}mm,",
            f"    bore_diameter: Length = {bore_d:.2f}mm{bore_tol_annot}",
            ") -> Solid {",
            f"    let base = box(size = [width, depth, height], origin = [{ox:.2f}mm, {oy:.2f}mm, {z0:.2f}mm])",
            f"    let bore_tool = cylinder(d = bore_diameter, h = height + 2.0mm, origin = [{cx:.2f}mm, {cy:.2f}mm, {z0 - 1.0:.2f}mm], exact_brep = true)",
            f'        |> tag::cylindrical_faces("{bore_tag}")',
        ]
        tools = ["bore_tool"]
        if bolt_iface is not None:
            bolt_d = bolt_iface.diameter_mm or 6.6
            bolt_count = bolt_iface.count or 4
            bolt_pcd = bolt_iface.pitch_circle_diameter_mm or (min(w, d) * 0.7)
            bolt_r = bolt_pcd * 0.5
            lines.extend(
                [
                    f"    let bolt_holes = hole::simple(d = {bolt_d:.2f}mm, depth = height + 2.0mm)",
                    f"        |> array::polar(axis = Z_AXIS, radius = {bolt_r:.2f}mm, count = {bolt_count})",
                    f'        |> tag::cylindrical_faces("{bolt_iface.interface_id}")',
                ]
            )
            tools.append("bolt_holes")

        lines.extend(
            [
                "    let final_part = base",
                f"        |> boolean::subtract(tools = [{', '.join(tools)}], preserve_exact_interfaces = true)",
                "    return final_part",
                "}",
                "",
            ]
        )
        return "\n".join(lines)

    def _generate_shelled_box_template(self, contract: DesignContract) -> str:
        w, d, h = contract.keep_in_bbox_mm.dimensions
        cx, cy, _ = contract.keep_in_bbox_mm.center
        z0 = contract.keep_in_bbox_mm.min[2]
        wall_t = float(contract.feature_params.get("wall_thickness_mm", 2.5))
        thread_spec = contract.feature_params.get("thread_spec")
        mirror_lugs = bool(contract.feature_params.get("mirror_lugs", False))
        tap_count = int(contract.feature_params.get("tap_count", 2))

        lug_w = min(14.0, w * 0.2)
        lug_d = min(12.0, d * 0.2)
        lug_h = min(h, 6.0)
        body_d = d - 2.0 * lug_d if mirror_lugs else d
        ox = cx - w / 2.0
        oy = cy - body_d / 2.0

        lines = [
            f'@unit(length = "{contract.length_unit}", angle = "{contract.angle_unit}", force = "{contract.force_unit}")',
            f'@material("{contract.material_name}")',
            f'@manufacturing(method = "{contract.manufacturing_method}", min_tool_radius = 1.5mm)',
            "",
            f"part {contract.part_name}(",
            f"    outer_w: Length = {w:.2f}mm,",
            f"    outer_d: Length = {body_d:.2f}mm,",
            f"    box_h: Length = {h:.2f}mm,",
            f"    wall_thickness: Length = {wall_t:.2f}mm",
            ") -> Solid {",
            f"    let raw_box = box(size = [outer_w, outer_d, box_h], origin = [{ox:.2f}mm, {oy:.2f}mm, {z0:.2f}mm])",
            "    let top_face = raw_box.faces().planar().filter(normal = +Z).max_by(coord = Z)",
            "    let shelled = raw_box |> shell(thickness = wall_thickness, open_faces = [top_face])",
        ]
        body = "shelled"
        lug_center_offset = (body_d + lug_d) / 2.0
        if mirror_lugs:
            lines.append(
                f"    let single_lug = box(size = [{lug_w:.2f}mm, {lug_d:.2f}mm, {lug_h:.2f}mm], "
                f"origin = [{cx - lug_w / 2.0:.2f}mm, {oy - lug_d:.2f}mm, {z0:.2f}mm])"
            )
            if abs(cy) < 1e-6:
                lines.append("    let lugs = single_lug |> mirror(plane = XZ_PLANE)")
            else:
                lines.append(
                    f"    let lugs = single_lug |> array::linear(axis = Y_AXIS, count = 2, spacing = {2.0 * lug_center_offset:.2f}mm)"
                )
            lines.append("    let body = boolean::union(items = [shelled, lugs])")
            body = "body"

        if thread_spec:
            # Cut the full lug or floor from below, where +Z has a defined tool origin.
            on_lugs = mirror_lugs and tap_count == 2
            hole_span = 2.0 * lug_center_offset if on_lugs else w * 0.5
            hole_x = cx if on_lugs or tap_count == 1 else cx - hole_span / 2.0
            hole_y = cy - lug_center_offset if on_lugs else cy
            depth = f"{lug_h + 2.0:.2f}mm" if on_lugs else "wall_thickness + 2.0mm"
            lines.append(
                f'    let tapped = hole::tapped(spec = {json.dumps(thread_spec)}, depth = {depth}, '
                f"origin = [{hole_x:.2f}mm, {hole_y:.2f}mm, {z0 - 1.0:.2f}mm], axis = +Z)"
            )
            if tap_count > 1:
                axis = "Y_AXIS" if on_lugs else "X_AXIS"
                lines.append(
                    f"        |> array::linear(axis = {axis}, count = {tap_count}, spacing = {hole_span / (tap_count - 1):.2f}mm)"
                )
            lines.append('        |> tag::cylindrical_faces("tapped_mounting_holes")')
            lines.append(f"    return {body} |> boolean::subtract(tools = [tapped], preserve_exact_interfaces = true)")
        else:
            lines.append(f"    return {body}")
        return "\n".join([*lines, "}", ""])

    def _generate_swept_manifold_template(self, contract: DesignContract) -> str:
        w, d, h = contract.keep_in_bbox_mm.dimensions
        cx, cy, _ = contract.keep_in_bbox_mm.center
        z0 = contract.keep_in_bbox_mm.min[2]
        pipe_d = float(contract.feature_params.get("pipe_diameter_mm", 20.0))
        flange_d = max(pipe_d, min(min(w, d) * 0.9, pipe_d * 1.5))
        loft_top = round(z0 + h * 0.45, 2)

        lines = [
            f'@unit(length = "{contract.length_unit}", angle = "{contract.angle_unit}", force = "{contract.force_unit}")',
            f'@material("{contract.material_name}")',
            f'@manufacturing(method = "{contract.manufacturing_method}", min_tool_radius = 1.5mm)',
            "",
            f"part {contract.part_name}(",
            f"    pipe_diameter: Length = {pipe_d:.2f}mm,",
            f"    flange_diameter: Length = {flange_d:.2f}mm,",
            f"    manifold_height: Length = {h:.2f}mm",
            ") -> Solid {",
            "    let sec_bottom = sketch(plane = XY_PLANE) {",
            f"        let c0 = circle(center = [{cx:.2f}mm, {cy:.2f}mm], d = flange_diameter)",
            "        region::closed([c0])",
            "    }",
            "    let sec_top = sketch(plane = XY_PLANE) {",
            f"        let c1 = circle(center = [{cx:.2f}mm, {cy:.2f}mm], d = pipe_diameter)",
            "        region::closed([c1])",
            "    }",
            f"    let transition_body = loft(sections = [sec_bottom, sec_top], offsets = [{z0:.2f}mm, {loft_top:.2f}mm])",
            "    let runner_profile = sketch(plane = XY_PLANE) {",
            "        let section = circle(center = [0.0mm, 0.0mm], d = pipe_diameter)",
            "        region::closed([section])",
            "    }",
        ]
        if abs(cy) < 1e-6:
            lines.extend([
                "    let flow_path = sketch(plane = XZ_PLANE) {",
                f"        let p0 = point(x = {cx:.2f}mm, z = {loft_top:.2f}mm)",
                f"        let p1 = point(x = {cx:.2f}mm, z = {z0:.2f}mm + manifold_height)",
                "        let segment = line(from = p0, to = p1)",
                "        region::path([segment])",
                "    }",
            ])
        else:
            lines.append(
                f"    let flow_path = [[{cx:.2f}mm, {cy:.2f}mm, {loft_top:.2f}mm], "
                f"[{cx:.2f}mm, {cy:.2f}mm, {z0:.2f}mm + manifold_height]]"
            )
        lines.extend([
            "    let runner = sweep(profile = runner_profile, path = flow_path)",
            f"    let inlet_bore = cylinder(d = pipe_diameter * 0.65, h = {loft_top - z0 + 1.0:.2f}mm, origin = [{cx:.2f}mm, {cy:.2f}mm, {z0:.2f}mm], exact_brep = true)",
            '        |> tag::cylindrical_faces("flow_channel")',
            f"    let outlet_bore = cylinder(d = pipe_diameter * 0.65, h = manifold_height + {z0 - loft_top + 1.0:.2f}mm, origin = [{cx:.2f}mm, {cy:.2f}mm, {loft_top:.2f}mm], exact_brep = true)",
            '        |> tag::cylindrical_faces("flow_channel")',
            "    let inlet = transition_body |> boolean::subtract(tools = [inlet_bore], preserve_exact_interfaces = true)",
            "    let outlet = runner |> boolean::subtract(tools = [outlet_bore], preserve_exact_interfaces = true)",
            "    return boolean::union(items = [inlet, outlet])",
            "}",
            "",
        ])
        return "\n".join(lines)

    def _generate_ribbed_bracket_template(self, contract: DesignContract) -> str:
        w, d, h = contract.keep_in_bbox_mm.dimensions
        cx, cy, _ = contract.keep_in_bbox_mm.center
        z0 = contract.keep_in_bbox_mm.min[2]
        base_h = round(min(12.0, h * 0.25), 2)
        hub_d = round(min(w, d) * 0.55, 2)
        bore_iface = next((i for i in contract.interfaces if i.kind in ("bore", "shaft_seat")), None)
        bore_d = (
            min(bore_iface.diameter_mm, hub_d * 0.8)
            if (bore_iface and bore_iface.diameter_mm)
            else round(hub_d * 0.55, 2)
        )
        bore_tag = bore_iface.interface_id if bore_iface else "main_bore"
        bore_tol = (
            f' @pmi::tolerance("{bore_iface.tolerance}")'
            if (bore_iface and bore_iface.tolerance)
            else ""
        )
        ox = cx - w / 2.0
        oy = cy - d / 2.0

        lines = [
            f'@unit(length = "{contract.length_unit}", angle = "{contract.angle_unit}", force = "{contract.force_unit}")',
            f'@material("{contract.material_name}")',
            f'@manufacturing(method = "{contract.manufacturing_method}", min_tool_radius = 1.5mm)',
            "",
            f"part {contract.part_name}(",
            f"    base_w: Length = {w:.2f}mm,",
            f"    base_d: Length = {d:.2f}mm,",
            f"    total_h: Length = {h:.2f}mm,",
            f"    bore_diameter: Length = {bore_d:.2f}mm{bore_tol}",
            ") -> Solid {",
            f"    let base = box(size = [base_w, base_d, {base_h:.2f}mm], origin = [{ox:.2f}mm, {oy:.2f}mm, {z0:.2f}mm])",
            f"    let hub = cylinder(d = {hub_d:.2f}mm, h = total_h, origin = [{cx:.2f}mm, {cy:.2f}mm, {z0:.2f}mm])",
            "    let rib = sketch(plane = XZ_PLANE) {",
            f"        let p0 = point(x = {cx + hub_d * 0.45:.2f}mm, z = {z0 + base_h:.2f}mm)",
            f"        let p1 = point(x = {cx + hub_d * 0.45:.2f}mm, z = {z0 + h * 0.85:.2f}mm)",
            f"        let p2 = point(x = {cx + w * 0.45:.2f}mm, z = {z0 + base_h:.2f}mm)",
            "        let e0 = line(from = p0, to = p1)",
            "        let e1 = line(from = p1, to = p2)",
            "        let e2 = line(from = p2, to = p0)",
            "        region::closed([e0, e1, e2])",
            "    }",
            "    |> extrude_symmetric(thickness = 6.0mm)",
            "    let ribs = rib |> mirror(plane = YZ_PLANE)",
            "    let body = boolean::union(items = [base, hub, ribs])",
            f"    let bore_tool = cylinder(d = bore_diameter, h = total_h + 2.0mm, origin = [{cx:.2f}mm, {cy:.2f}mm, {z0 - 1.0:.2f}mm], exact_brep = true)",
            f'        |> tag::cylindrical_faces("{bore_tag}")',
            "    let final_part = body",
            "        |> boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)",
            "    return final_part",
            "}",
            "",
        ]
        return "\n".join(lines)

    def _generate_assembly_template(self, contract: DesignContract) -> str:
        w, d, h = contract.keep_in_bbox_mm.dimensions
        cx, cy, _ = contract.keep_in_bbox_mm.center
        z0 = contract.keep_in_bbox_mm.min[2]
        base_h = round(h * 0.65, 2)
        cap_h = round(h - base_h, 2)
        bore_iface = next((i for i in contract.interfaces if i.kind in ("bore", "shaft_seat")), None)
        bore_d = (
            min(bore_iface.diameter_mm, min(w, d) * 0.6)
            if (bore_iface and bore_iface.diameter_mm)
            else round(min(w, d) * 0.35, 2)
        )
        bore_tag = bore_iface.interface_id if bore_iface else "main_bore"
        bore_tol = (
            f' @pmi::tolerance("{bore_iface.tolerance}")'
            if (bore_iface and bore_iface.tolerance)
            else ""
        )
        ox = cx - w / 2.0
        oy = cy - d / 2.0
        cap_d = round(min(w, d) * 0.8, 2)

        lines = [
            f'@unit(length = "{contract.length_unit}", angle = "{contract.angle_unit}", force = "{contract.force_unit}")',
            f'@material("{contract.material_name}")',
            f'@manufacturing(method = "{contract.manufacturing_method}", min_tool_radius = 1.5mm)',
            "",
            f"part {contract.part_name}Base(",
            f"    width: Length = {w:.2f}mm,",
            f"    depth: Length = {d:.2f}mm,",
            f"    height: Length = {base_h:.2f}mm,",
            f"    bore_diameter: Length = {bore_d:.2f}mm{bore_tol}",
            ") -> Solid {",
            f"    let base = box(size = [width, depth, height], origin = [{ox:.2f}mm, {oy:.2f}mm, 0.00mm])",
            f"    let bore_tool = cylinder(d = bore_diameter, h = height + 2.0mm, origin = [{cx:.2f}mm, {cy:.2f}mm, -1.00mm], exact_brep = true)",
            f'        |> tag::cylindrical_faces("{bore_tag}")',
            "    return base |> boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)",
            "}",
            "",
            f"part {contract.part_name}Cap(",
            f"    outer_d: Length = {cap_d:.2f}mm,",
            f"    cap_h: Length = {cap_h:.2f}mm,",
            f"    bore_diameter: Length = {bore_d:.2f}mm",
            ") -> Solid {",
            f"    let cap = cylinder(d = outer_d, h = cap_h, origin = [{cx:.2f}mm, {cy:.2f}mm, 0.00mm])",
            f"    let bore_tool = cylinder(d = bore_diameter, h = cap_h + 2.0mm, origin = [{cx:.2f}mm, {cy:.2f}mm, -1.00mm], exact_brep = true)",
            f'        |> tag::cylindrical_faces("{bore_tag}")',
            "    return cap |> boolean::subtract(tools = [bore_tool], preserve_exact_interfaces = true)",
            "}",
            "",
            f"assembly {contract.part_name}() -> Assembly {{",
            f"    let housing_base = component(part = {contract.part_name}Base, origin = [0.00mm, 0.00mm, {z0:.2f}mm])",
            f"    let end_cap = component(part = {contract.part_name}Cap, origin = [0.00mm, 0.00mm, {z0 + base_h:.2f}mm])",
            "    mate::coaxial(housing_base, end_cap)",
            "    mate::coincident(housing_base, end_cap)",
            "    return [housing_base, end_cap]",
            "}",
            "",
        ]
        return "\n".join(lines)

    def _repair_heuristic(
        self,
        contract: DesignContract,
        previous_code: str,
        compile_result: CompileResult,
        verdict: CriticVerdict,
    ) -> str:
        repaired = previous_code
        if any("dim" in d.code.lower() or "dimension" in d.message.lower() for d in compile_result.diagnostics):
            repaired = re.sub(r"(\+\s*\d+(?:\.\d+)?)\s*deg\b", r"\1mm", repaired)
            repaired = re.sub(r"(-\s*\d+(?:\.\d+)?)\s*deg\b", r"\1mm", repaired)

        if any(
            kw in issue.lower()
            for issue in verdict.issues
            for kw in ("keep-in", "bbox", "interface")
        ):
            return self._generate_parametric_template(contract)

        return repaired


class OrthogonalViewInspector(Protocol):
    """Hook for VLM or headless SVG/mesh inspection of rendered views."""

    def inspect(
        self,
        views_dir: Path | None,
        mesh_path: Path | None,
        contract: DesignContract,
        compile_result: CompileResult,
    ) -> tuple[bool, str]:
        """Return (passed, visual_summary)."""


class HeuristicViewInspector:
    """Offline structural check for headless SVG orthogonal views and scene-mesh JSON."""

    EXPECTED_VIEWS: tuple[str, ...] = ("front.svg", "top.svg", "right.svg", "iso.svg")

    def inspect(
        self,
        views_dir: Path | None,
        mesh_path: Path | None,
        contract: DesignContract,
        compile_result: CompileResult,
    ) -> tuple[bool, str]:
        notes: list[str] = []
        if views_dir is not None and views_dir.is_dir():
            missing: list[str] = []
            for view_name in self.EXPECTED_VIEWS:
                svg_file = views_dir / view_name
                if not svg_file.is_file() or "<svg" not in svg_file.read_text(encoding="utf-8", errors="ignore"):
                    missing.append(view_name)
            if missing:
                return (False, f"Missing or invalid orthogonal SVG views: {', '.join(missing)}")
            notes.append("4 orthogonal/iso SVG views verified")

        if mesh_path is not None and mesh_path.is_file():
            try:
                mesh_data = json.loads(mesh_path.read_text(encoding="utf-8"))
            except json.JSONDecodeError as exc:
                return (False, f"Corrupt scene-mesh JSON: {exc}")
            if mesh_data.get("format_version") != "0.1" or not mesh_data.get("faces"):
                return (False, "scene-mesh JSON missing format_version='0.1' or non-empty faces")
            notes.append(f"scene-mesh verified ({len(mesh_data.get('faces', []))} faces)")

        if not notes:
            return (True, "Visual inspection skipped (no views/mesh artifacts requested)")
        return (True, "; ".join(notes))


def _estimate_cylinder_diameter_from_faces(faces: list[dict[str, Any]]) -> float | None:
    """Estimate cylindrical diameter from tessellated face vertices when available."""
    xs: list[float] = []
    ys: list[float] = []
    for face in faces:
        verts = face.get("vertices", [])
        for i in range(0, len(verts) - 2, 3):
            xs.append(float(verts[i]))
            ys.append(float(verts[i + 1]))
    if len(xs) < 3:
        return None
    cx = sum(xs) / len(xs)
    cy = sum(ys) / len(ys)
    radii = [math.hypot(x - cx, y - cy) for x, y in zip(xs, ys)]
    avg_r = sum(radii) / len(radii)
    return 2.0 * avg_r if avg_r > 1e-6 else None


def check_interfaces(
    contract: DesignContract,
    compile_result: CompileResult,
    *,
    mesh_data: dict[str, Any] | None = None,
    kcl_source: str | None = None,
    tolerance_mm: float = 0.05,
) -> tuple[bool, list[str]]:
    """Verify L2 mechanical interfaces (presence, surface type, dimensions, count, PMI)."""
    if not contract.interfaces:
        return (True, [])

    faces: list[dict[str, Any]] = []
    edit_handles: list[dict[str, Any]] = []
    if isinstance(mesh_data, dict):
        faces = [f for f in mesh_data.get("faces", []) if isinstance(f, dict)]
        edit_handles = [h for h in mesh_data.get("edit_handles", []) if isinstance(h, dict)]

    violations: list[str] = []
    cylindrical_kinds = {"bore", "shaft_seat", "bolt_pattern", "bolt_circle", "counterbore_pattern"}
    planar_kinds = {"mounting_face", "datum", "slot"}

    for iface in contract.interfaces:
        iface_id = iface.interface_id
        expected_surface = (
            "cylinder"
            if iface.kind in cylindrical_kinds
            else ("plane" if iface.kind in planar_kinds else None)
        )

        # 1. Check mesh faces tagged with interface_id (or matching surface_type fallback)
        tagged_faces = [
            f
            for f in faces
            if any(iface_id.lower() in str(tag).lower() for tag in f.get("provenance_tags", []))
        ]
        if faces and not tagged_faces:
            # Allow matching by surface type + KCL identifier if tags weren't emitted on all faces
            has_kcl_mention = bool(kcl_source and iface_id.lower() in kcl_source.lower())
            matching_surface_faces = [
                f for f in faces if expected_surface is None or f.get("surface_type") == expected_surface
            ]
            if not (has_kcl_mention and matching_surface_faces):
                violations.append(
                    f"Interface '{iface_id}' ({iface.kind}) missing from compiled geometry/tags"
                )
                continue
            candidate_faces = matching_surface_faces
        else:
            candidate_faces = tagged_faces

        # 2. Check exact analytic surface preservation
        if iface.preserve_exact_brep and candidate_faces and expected_surface is not None:
            bad_surfaces = [
                f.get("surface_type")
                for f in candidate_faces
                if f.get("surface_type") != expected_surface or f.get("exact_interface") is False
            ]
            if bad_surfaces and not any(f.get("surface_type") == expected_surface for f in candidate_faces):
                violations.append(
                    f"Interface '{iface_id}' requires exact '{expected_surface}' B-Rep surface, "
                    f"found {sorted(set(str(s) for s in bad_surfaces))}"
                )

        # 3. Check critical diameter against edit_handles or tagged cylindrical geometry
        if iface.diameter_mm is not None:
            matched_handle = next(
                (
                    h
                    for h in edit_handles
                    if iface_id.lower() in str(h.get("param_name", "")).lower()
                    or (
                        iface.kind in ("bore", "shaft_seat")
                        and "bore" in str(h.get("param_name", "")).lower()
                        and "diameter" in str(h.get("param_name", "")).lower()
                    )
                ),
                None,
            )
            if matched_handle is not None:
                actual_d = float(matched_handle.get("current_value", 0.0))
                if abs(actual_d - iface.diameter_mm) > tolerance_mm:
                    violations.append(
                        f"Interface '{iface_id}' diameter mismatch: expected {iface.diameter_mm:.3f}mm, "
                        f"got {actual_d:.3f}mm in parameter '{matched_handle.get('param_name')}'"
                    )
            elif tagged_faces and expected_surface == "cylinder":
                est_d = _estimate_cylinder_diameter_from_faces(
                    [f for f in tagged_faces if f.get("surface_type") == "cylinder"]
                )
                if est_d is not None and abs(est_d - iface.diameter_mm) > max(tolerance_mm, 0.25):
                    violations.append(
                        f"Interface '{iface_id}' cylindrical mesh diameter ~{est_d:.2f}mm "
                        f"differs from contract {iface.diameter_mm:.2f}mm"
                    )

        # 4. Check pattern count if requested
        if iface.count is not None:
            count_handle = next(
                (
                    h
                    for h in edit_handles
                    if "count" in str(h.get("param_name", "")).lower()
                    and (
                        iface_id.lower() in str(h.get("param_name", "")).lower()
                        or iface.kind in ("bolt_pattern", "bolt_circle", "counterbore_pattern")
                    )
                ),
                None,
            )
            if count_handle is not None:
                actual_count = int(round(float(count_handle.get("current_value", 0))))
                if actual_count != iface.count:
                    violations.append(
                        f"Interface '{iface_id}' count mismatch: expected {iface.count}, got {actual_count}"
                    )
            elif kcl_source is not None:
                array_counts = [
                    int(m.group(1))
                    for m in re.finditer(r"count\s*=\s*(\d+)", kcl_source)
                ]
                if not array_counts and len(tagged_faces) < iface.count:
                    violations.append(
                        f"Interface '{iface_id}' requires {iface.count} pattern instances, none found"
                    )
                elif array_counts and iface.count not in array_counts:
                    violations.append(
                        f"Interface '{iface_id}' pattern count mismatch: expected {iface.count}, "
                        f"found {array_counts}"
                    )

        # 5. Check PMI tolerance declaration when requested
        if iface.tolerance and kcl_source is not None:
            if iface.tolerance not in kcl_source:
                violations.append(
                    f"Interface '{iface_id}' missing PMI tolerance '{iface.tolerance}' in .kcl source"
                )

    return (len(violations) == 0, violations)


class VisualGeometricCritic:
    """Runs L0-L2 hard geometry gates + headless orthogonal view checks."""

    def __init__(
        self,
        view_inspector: OrthogonalViewInspector | None = None,
        *,
        bbox_tolerance_mm: float = 1e-4,
    ) -> None:
        self.view_inspector = view_inspector or HeuristicViewInspector()
        self.bbox_tolerance_mm = bbox_tolerance_mm

    def evaluate(
        self,
        contract: DesignContract,
        compile_result: CompileResult,
        *,
        iteration: int = 1,
    ) -> CriticVerdict:
        gates: list[GateCheckResult] = []
        issues: list[str] = []
        repair_hints: list[str] = []

        mesh_path = Path(compile_result.mesh_path) if compile_result.mesh_path else None
        mesh_data: dict[str, Any] | None = None
        if mesh_path is not None and mesh_path.is_file():
            try:
                mesh_data = json.loads(mesh_path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                mesh_data = None

        kcl_source: str | None = None
        if compile_result.file:
            kcl_file = Path(compile_result.file)
            if kcl_file.is_file():
                try:
                    kcl_source = kcl_file.read_text(encoding="utf-8")
                except OSError:
                    kcl_source = None

        # L0: Syntax, dimensional type checking, deterministic selectors
        errors = compile_result.error_diagnostics
        l0_passed = (compile_result.status == "ok") and (len(errors) == 0)
        if not l0_passed:
            formatted = [d.format_line() for d in errors] or [
                f"Compiler exited with status={compile_result.status}"
            ]
            issues.extend(formatted)
            repair_hints.extend(formatted)
        gates.append(
            GateCheckResult(
                gate="L0",
                name="Syntax & Dimensional Type Check",
                passed=l0_passed,
                details="0 compiler errors" if l0_passed else f"{len(errors)} error(s)",
            )
        )

        # L1: Watertight solid & non-zero volume
        wt_ok, wt_msg = check_watertight(compile_result.metrics)
        vol_ok, vol_msg = check_non_zero_volume(compile_result.metrics)
        l1_passed = wt_ok and vol_ok
        if not wt_ok:
            issues.append(wt_msg)
            repair_hints.append(f"Fix geometry watertightness: {wt_msg}")
        if not vol_ok:
            issues.append(vol_msg)
            repair_hints.append(f"Fix solid volume: {vol_msg}")
        gates.append(
            GateCheckResult(
                gate="L1",
                name="Watertight Manifold & Positive Volume",
                passed=l1_passed,
                details=(
                    f"{wt_msg}; {vol_msg}"
                    if l1_passed
                    else "; ".join(m for ok, m in ((wt_ok, wt_msg), (vol_ok, vol_msg)) if not ok)
                ),
            )
        )

        # L2: Keep-In BBox, Keep-Out clearance (narrow-phase mesh), and Mechanical Interfaces
        keep_in_ok, keep_in_violations = check_keep_in_bbox(
            compile_result.metrics,
            contract.keep_in_bbox_mm,
            tolerance=self.bbox_tolerance_mm,
        )
        keep_out_ok, keep_out_violations = check_keep_out_zones(
            compile_result.metrics,
            contract.keep_out_zones,
            tolerance=self.bbox_tolerance_mm,
            mesh_path=mesh_path,
            mesh_data=mesh_data,
        )
        iface_ok, iface_violations = check_interfaces(
            contract,
            compile_result,
            mesh_data=mesh_data,
            kcl_source=kcl_source,
        )
        l2_passed = keep_in_ok and keep_out_ok and iface_ok
        if not keep_in_ok:
            msg = "Keep-In bbox overflow: " + ", ".join(keep_in_violations)
            issues.append(msg)
            repair_hints.append(
                f"Constrain dimensions inside keep_in_bbox_mm={contract.keep_in_bbox_mm.to_dict()} ({msg})"
            )
        if not keep_out_ok:
            for v in keep_out_violations:
                issues.append(v)
                repair_hints.append(f"Remove geometry from Keep-Out zone: {v}")
        if not iface_ok:
            for v in iface_violations:
                issues.append(v)
                repair_hints.append(f"Implement/fix mechanical interface: {v}")

        l2_details = (
            "Within Keep-In envelope, Keep-Out clear, interfaces verified"
            if l2_passed
            else "; ".join(keep_in_violations + keep_out_violations + iface_violations)
        )
        gates.append(
            GateCheckResult(
                gate="L2",
                name="Keep-In Envelope, Keep-Out Clearance & Interfaces",
                passed=l2_passed,
                details=l2_details,
            )
        )

        # L4: Headless orthogonal views / VLM inspection
        views_dir = Path(compile_result.views_dir) if compile_result.views_dir else None
        vis_ok, vis_summary = self.view_inspector.inspect(
            views_dir, mesh_path, contract, compile_result
        )
        if not vis_ok:
            issues.append(f"Visual/Mesh inspection failed: {vis_summary}")
            repair_hints.append(vis_summary)
        gates.append(
            GateCheckResult(
                gate="L4",
                name="Orthogonal View & Scene-Mesh Inspection",
                passed=vis_ok,
                details=vis_summary,
            )
        )

        overall_passed = all(g.passed for g in gates)
        return CriticVerdict(
            passed=overall_passed,
            iteration=iteration,
            gate_results=gates,
            issues=issues,
            repair_instructions="\n".join(repair_hints),
            visual_summary=vis_summary,
            metrics=compile_result.metrics,
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


def _assert_within_base(base_dir: Path, target: Path) -> Path:
    resolved_base = base_dir.resolve()
    resolved_target = target.resolve()
    if not resolved_target.is_relative_to(resolved_base):
        raise ContractValidationError(
            [f"Refusing path outside work_dir: {resolved_target} not under {resolved_base}"]
        )
    return resolved_target


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

    def run(
        self,
        requirement_or_contract: str | DesignContract,
        *,
        max_iters: int = 3,
        work_dir: str | Path | None = None,
        build_artifacts: bool = True,
        include_drawings: bool = False,
        cleanup_temp: bool = False,
    ) -> PipelineResult:
        if max_iters < 1:
            raise ValueError("max_iters must be >= 1")

        contract = (
            requirement_or_contract
            if isinstance(requirement_or_contract, DesignContract)
            else self.spec_parser.parse(requirement_or_contract)
        )
        contract.assert_valid()

        created_temp = False
        if work_dir is None:
            base_dir = Path(tempfile.mkdtemp(prefix="cad_agent_")).resolve()
            created_temp = True
        else:
            base_dir = Path(work_dir).resolve()
            base_dir.mkdir(parents=True, exist_ok=True)

        history: list[IterationRecord] = []
        code = self.cad_coder.generate(contract)
        kcl_path = _assert_within_base(base_dir, base_dir / f"{contract.part_name}.kcl")

        try:
            for iteration in range(1, max_iters + 1):
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
                else:
                    compile_result = self.compiler_bridge.check(kcl_path)

                verdict = self.critic.evaluate(
                    contract, compile_result, iteration=iteration
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

                if iteration < max_iters:
                    code = self.cad_coder.repair(
                        contract, code, compile_result, verdict
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
