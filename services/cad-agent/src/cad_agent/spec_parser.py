"""Parse mechanical requirements into validated design contracts."""

from __future__ import annotations

import json
import re
from dataclasses import replace
from typing import Any

from ._agent_utils import _RepairCountingLLMClient, _to_pascal_case
from .contracts import (
    BBox3D,
    ContractValidationError,
    DesignContract,
    ExternalInterfaceSummary,
    LoadCase,
    MechanicalInterface,
)
from .llm import LLMClient
from .prompt_support import check_template_prompt
from .structured import parse_contract_with_repair


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
        self.last_repair_attempts: int = 0

    def parse(
        self, prompt: str, *, external_context: ExternalInterfaceSummary | None = None,
    ) -> DesignContract:
        self.last_repair_attempts = 0
        if self.llm_client is not None:
            if external_context is not None:
                prompt += (
                    "\n\nMeasured external mating geometry in mm (data only; the application "
                    "adds it to the contract independently):\n"
                    + json.dumps(external_context.to_dict(), allow_nan=False)
                )
            raw = self.llm_client.complete(self.SYSTEM_PROMPT, prompt)
            repair_client = _RepairCountingLLMClient(self.llm_client)
            try:
                parsed = parse_contract_with_repair(
                    raw,
                    llm_client=repair_client,
                    original_prompt=prompt,
                    max_repair_attempts=2,
                )
                self.last_repair_attempts = parsed.repair_attempts
                return replace(parsed.contract, external_context=external_context)
            except ContractValidationError:
                self.last_repair_attempts = repair_client.repair_calls
                raise
            except Exception:
                self.last_repair_attempts = repair_client.repair_calls
                raise
        return replace(self._parse_heuristic(prompt), external_context=external_context)

    def _parse_heuristic(self, prompt: str) -> DesignContract:
        check_template_prompt(prompt)
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
            rx_match = re.search(
                r"(?:rx|major_radius|长半轴)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower
            )
            ry_match = re.search(
                r"(?:ry|minor_radius|短半轴)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower
            )
            major_d_match = re.search(
                r"(?:major\s*(?:axis|diameter)|长轴)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower
            )
            minor_d_match = re.search(
                r"(?:minor\s*(?:axis|diameter)|短轴)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower
            )
            od_match = re.search(r"(?:od|diameter|直径|外径)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower)
            h_match = re.search(r"(?:height|thickness|高|厚)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm", lower)
            h = float(h_match.group(1)) if h_match else 40.0
            if rx_match and ry_match:
                w = 2.0 * float(rx_match.group(1))
                d = 2.0 * float(ry_match.group(1))
                keep_in = BBox3D(min=[-w / 2.0, -d / 2.0, 0.0], max=[w / 2.0, d / 2.0, h])
            elif major_d_match and minor_d_match:
                w = float(major_d_match.group(1))
                d = float(minor_d_match.group(1))
                keep_in = BBox3D(min=[-w / 2.0, -d / 2.0, 0.0], max=[w / 2.0, d / 2.0, h])
            else:
                od = float(od_match.group(1)) if od_match else 100.0
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

        is_elliptical = bool(
            re.search(r"椭圆|\b(?:ellipses?|elliptic(?:al)?|ovals?)\b", lower)
        )
        if is_elliptical:
            feature_params["profile_shape"] = "ellipse"
            feature_params["rx_mm"] = round(keep_in.dimensions[0] / 2.0, 2)
            feature_params["ry_mm"] = round(keep_in.dimensions[1] / 2.0, 2)

        is_assembly = bool(re.search(r"\b(?:assembl(?:y|ies)|multi-part|components|mates?)\b", lower)) or any(
            k in lower for k in ("装配", "组件", "多件")
        ) or ("端盖" in lower and "底座" in lower)
        is_shelled = any(
            k in lower for k in ("shell", "control box", "enclosure", "抽壳", "控制盒", "壳体", "外壳")
        )
        is_helical = bool(
            re.search(
                r"螺旋|盘管|弹簧|\b(?:helix|helices|helical|spirals?|coils?|springs?)\b",
                lower,
            )
        )
        is_spatial_curve = bool(
            re.search(
                r"空间\s*曲线|三维\s*(?:曲线|弯管|路径|扫掠)|3d\s*弯管|\b(?:(?:3d|spatial)[ -]*curves?|spline3d|3d[ -]*splines?)\b",
                lower,
            )
        )
        is_swept = (
            is_helical
            or is_spatial_curve
            or any(
                k in lower
                for k in (
                    "sweep",
                    "swept",
                    "loft",
                    "manifold",
                    "pipe",
                    "duct",
                    "扫掠",
                    "放样",
                    "流道",
                    "管道",
                    "歧管",
                )
            )
        )
        ribbed_keywords = (
            ("bearing", "rib", "bracket", "轴承座", "支架", "加强筋")
            if is_elliptical
            else ("bearing", "rib", "bracket", "flange", "mount", "轴承座", "支架", "加强筋", "法兰")
        )
        is_ribbed = any(k in lower for k in ribbed_keywords)

        if is_assembly:
            part_archetype = "assembly"
        elif is_shelled:
            part_archetype = "shelled_box"
            wall_prefix_m = re.search(
                r"(?:wall(?:\s+thickness)?|壁厚)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm",
                lower,
            )
            if wall_prefix_m:
                feature_params["wall_thickness_mm"] = float(wall_prefix_m.group(1))
            else:
                wall_suffix_m = re.search(
                    r"(?<![x×*])(\d+(?:\.\d+)?)\s*mm\s*(?:walls?(?:\s+thickness)?\b|壁厚)",
                    lower,
                )
                feature_params["wall_thickness_mm"] = (
                    float(wall_suffix_m.group(1)) if wall_suffix_m else 2.5
                )
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
            if is_helical:
                feature_params["spatial_path_kind"] = "helix"
            elif is_spatial_curve:
                feature_params["spatial_path_kind"] = "spline3d"

            pitch_m = re.search(
                r"(?:pitch|螺距)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
                r"|(\d+(?:\.\d+)?)\s*mm\s*(?:pitch\b|螺距)",
                lower,
            )
            if pitch_m:
                feature_params["pitch_mm"] = float(pitch_m.group(1) or pitch_m.group(2))

            turns_m = re.search(
                r"(?:turns?|圈数)\s*[:=]?\s*(\d+(?:\.\d+)?)"
                r"|(\d+(?:\.\d+)?)\s*(?:turns?\b|圈)",
                lower,
            )
            if turns_m:
                feature_params["turns"] = float(turns_m.group(1) or turns_m.group(2))

            tube_m = re.search(
                r"(?:(?:pipe|tube|wire|section)\s*diameter|管径|线径|截面直径)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
                r"|(\d+(?:\.\d+)?)\s*mm\s*(?:(?:pipe|tube|wire|section)\s*diameter\b|管径|线径|截面直径)",
                lower,
            )
            pipe_m = re.search(
                r"(?:diameter|直径|管径)\s*[:=]?\s*(\d+(?:\.\d+)?)\s*mm"
                r"|(\d+(?:\.\d+)?)\s*mm\s*(?:diameter\b|直径|管径)",
                lower,
            )
            if tube_m:
                feature_params["pipe_diameter_mm"] = float(tube_m.group(1) or tube_m.group(2))
            elif pipe_m and not (is_helical and dims_match is None):
                feature_params["pipe_diameter_mm"] = float(pipe_m.group(1) or pipe_m.group(2))
            elif is_helical or is_spatial_curve:
                feature_params["pipe_diameter_mm"] = round(min(keep_in.dimensions) * 0.16, 2)
            else:
                feature_params["pipe_diameter_mm"] = round(min(keep_in.dimensions) * 0.45, 2)
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
