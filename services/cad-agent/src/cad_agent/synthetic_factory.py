"""Parametric Meta-Template Mutation Sampler & Synthetic CAD SFT/GRPO Data Factory (H2-E08).

Generates deterministic, seed-reproducible synthetic CAD design tasks across the 5
industrial part families aligned with ``CAD-Eval V2``:

1. ``mounting_plate`` — base plate + corner bolt hole pattern + optional central pilot bore
2. ``bracket`` — L-shaped / stepped mounting bracket with orthogonal flanges & mounting holes
3. ``motor_mount`` — flanged plate + NEMA-style bolt pattern + central pilot bore
4. ``enclosure`` — walled control box with mounting tabs & wall-thickness contract
5. ``fixture_jig`` — datum base + locating bosses + clamp holes + keep-out clearance zone

Every candidate trajectory is executed against the real ``BranchPatchExplorer`` +
``cad-cli`` SQLite preview branches (``ai/cand-1..N``) + real L0–L4 verification
(``cad-cli verify``). Rewards are never mocked.
"""

from __future__ import annotations

import copy
import json
import math
import random
import subprocess
import tempfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from statistics import mean, pstdev
from typing import Any

from .branch_explorer import (
    BranchExplorationError,
    BranchPatchExplorer,
    SemanticBestOfNResult,
    SemanticCandidateOutcome,
    SemanticRolloutConfig,
    canonical_commands_json,
)
from .compiler_bridge import discover_cad_cli
from .contracts import (
    BBox3D,
    DesignContract,
    KeepOutZone,
    MechanicalInterface,
)
from .grpo_dataset_pipeline import (
    GrpoPromptGroup,
    RewardWeights,
    compute_grpo_group_advantages,
    export_grpo_training_jsonl,
    load_grpo_training_jsonl,
)
from .llm_semantic_planner import _SYSTEM_PROMPT, semantic_command_contract
from .semantic_agent import dim_param, length_mm_value

PART_CATEGORIES: tuple[str, ...] = (
    "mounting_plate",
    "bracket",
    "motor_mount",
    "enclosure",
    "fixture_jig",
)

MUTATION_KINDS: tuple[str, ...] = (
    "negative_dimension",
    "zero_dimension",
    "thin_wall",
    "colliding_body",
    "oversized_hole",
)

_MATERIALS: tuple[str, ...] = (
    "Al-6061-T6",
    "Al-7075-T6",
    "SUS304",
    "C45-Steel",
)

_FASTENERS: tuple[tuple[str, float], ...] = (
    ("M4", 4.5),
    ("M5", 5.5),
    ("M6", 6.6),
    ("M8", 9.0),
)

_NEMA_PROFILES: tuple[tuple[str, float, float, float, str, float], ...] = (
    # (label, plate_size, bolt_pitch, pilot_dia, fastener, clearance_dia)
    ("NEMA17", 46.0, 31.0, 22.0, "M3", 3.4),
    ("NEMA23", 64.0, 47.14, 38.1, "M5", 5.5),
    ("NEMA34", 92.0, 69.6, 73.0, "M6", 6.6),
)


def build_vllm_mounting_recipe(
    *,
    model_name: str = "qwen2.5-coder-7b-cad-grpo",
    base_url: str = "http://127.0.0.1:8000/v1",
) -> dict[str, Any]:
    """Return the zero-code-change local vLLM OpenAI-compatible mounting recipe."""
    return {
        "provider": "openai",
        "env": {
            "CAD_AGENT_PROVIDER": "openai",
            "OPENAI_BASE_URL": base_url,
            "OPENAI_MODEL": model_name,
        },
        "vllm_serve_command": (
            f"vllm serve {model_name} --host 127.0.0.1 --port 8000 "
            f"--served-model-name {model_name}"
        ),
        "notes": (
            "Mount the fine-tuned SFT/GRPO model into LLMSemanticPlanner via "
            "OpenAICompatibleClient with zero code changes by exporting the env vars above."
        ),
    }


@dataclass(frozen=True, slots=True)
class SyntheticTaskSpec:
    """One deterministic parametric CAD design task and its valid baseline trajectory."""

    task_id: str
    prompt_id: str
    category: str
    seed: int
    prompt: str
    contract: DesignContract
    parameters: dict[str, float]
    baseline_commands: list[dict[str, Any]]

    def to_dict(self) -> dict[str, Any]:
        return {
            "task_id": self.task_id,
            "prompt_id": self.prompt_id,
            "category": self.category,
            "seed": self.seed,
            "prompt": self.prompt,
            "contract": self.contract.to_dict(),
            "parameters": dict(self.parameters),
            "baseline_commands": copy.deepcopy(self.baseline_commands),
        }


@dataclass(frozen=True, slots=True)
class CandidateMutation:
    """One sampled candidate trajectory (valid baseline or engineering failure mutation)."""

    candidate_index: int
    mutation_kind: str
    description: str
    commands: list[dict[str, Any]]
    commands_json: str
    is_mutated: bool

    def to_dict(self) -> dict[str, Any]:
        return {
            "candidate_index": self.candidate_index,
            "mutation_kind": self.mutation_kind,
            "description": self.description,
            "commands": copy.deepcopy(self.commands),
            "commands_json": self.commands_json,
            "is_mutated": self.is_mutated,
        }


@dataclass(frozen=True, slots=True)
class SyntheticFactoryConfig:
    """Configuration for running the offline synthetic SFT/GRPO data factory."""

    count: int = 10
    candidates_per_prompt: int = 3
    seed: int = 42
    categories: tuple[str, ...] = PART_CATEGORIES
    out_dir: Path = field(default_factory=lambda: Path("synthetic_grpo_out"))
    cli_bin: Path | None = None
    min_wall_mm: float = 1.5
    reward_weights: RewardWeights | None = None
    require_full_pass: bool = False
    keep_projects: bool = False
    timeout_sec: float = 120.0

    def __post_init__(self) -> None:
        if isinstance(self.count, bool) or not isinstance(self.count, int) or self.count < 1:
            raise ValueError("count must be a positive integer")
        if (
            isinstance(self.candidates_per_prompt, bool)
            or not isinstance(self.candidates_per_prompt, int)
            or self.candidates_per_prompt < 1
        ):
            raise ValueError("candidates_per_prompt must be a positive integer")
        if isinstance(self.seed, bool) or not isinstance(self.seed, int):
            raise ValueError("seed must be an integer")
        cats = tuple(str(c).strip() for c in self.categories if str(c).strip())
        if not cats:
            raise ValueError("categories must contain at least one valid category")
        invalid = [c for c in cats if c not in PART_CATEGORIES]
        if invalid:
            raise ValueError(
                f"unsupported categories {invalid!r}; expected subset of {PART_CATEGORIES}"
            )
        object.__setattr__(self, "categories", cats)
        object.__setattr__(self, "out_dir", Path(self.out_dir))
        if self.cli_bin is not None:
            object.__setattr__(self, "cli_bin", Path(self.cli_bin))
        if not math.isfinite(self.min_wall_mm) or self.min_wall_mm < 0.0:
            raise ValueError("min_wall_mm must be finite and nonnegative")
        if not math.isfinite(self.timeout_sec) or self.timeout_sec <= 0.0:
            raise ValueError("timeout_sec must be finite and positive")


@dataclass(frozen=True, slots=True)
class SyntheticFactoryStats:
    """Deterministic summary metrics emitted by ``run_synthetic_factory``."""

    seed: int
    requested_tasks: int
    completed_tasks: int
    crashed_rollouts: int
    total_candidates_evaluated: int
    unique_candidates_kept: int
    deduplicated_candidates: int
    grpo_groups_written: int
    grpo_groups_with_variance: int
    sft_examples_written: int
    mean_reward: float
    std_reward: float
    category_stats: dict[str, dict[str, Any]]
    grpo_jsonl_path: str
    sft_jsonl_path: str
    summary_json_path: str
    vllm_mounting_recipe: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        return {
            "seed": self.seed,
            "requested_tasks": self.requested_tasks,
            "completed_tasks": self.completed_tasks,
            "crashed_rollouts": self.crashed_rollouts,
            "total_candidates_evaluated": self.total_candidates_evaluated,
            "unique_candidates_kept": self.unique_candidates_kept,
            "deduplicated_candidates": self.deduplicated_candidates,
            "grpo_groups_written": self.grpo_groups_written,
            "grpo_groups_with_variance": self.grpo_groups_with_variance,
            "sft_examples_written": self.sft_examples_written,
            "mean_reward": self.mean_reward,
            "std_reward": self.std_reward,
            "category_stats": copy.deepcopy(self.category_stats),
            "grpo_jsonl_path": self.grpo_jsonl_path,
            "sft_jsonl_path": self.sft_jsonl_path,
            "summary_json_path": self.summary_json_path,
            "vllm_mounting_recipe": copy.deepcopy(self.vllm_mounting_recipe),
        }


def _upsert_length(name: str, value_mm: float) -> dict[str, Any]:
    return {
        "command": "upsert_parameter",
        "args": {
            "name": name,
            "ir_type": "length",
            "value": length_mm_value(round(float(value_mm), 4)),
        },
    }


def _set_contract_cmd(contract: DesignContract, *, category: str) -> dict[str, Any]:
    del category
    return {
        "command": "set_design_contract",
        "args": {
            "contract": {
                "part_name": contract.part_name,
                "summary": contract.summary,
                "material": contract.material_name,
                "manufacturing_method": contract.manufacturing_method,
            }
        },
    }


def _standard_through_hole(
    *,
    node_id: str,
    name: str,
    solid: str,
    origin_mm: list[float],
    designation: str,
) -> dict[str, Any]:
    return {
        "command": "create_hole",
        "args": {
            "node_id": node_id,
            "name": name,
            "solid": solid,
            "origin_mm": [round(float(v), 4) for v in origin_mm],
            "axis": [0.0, 0.0, -1.0],
            "definition": {
                "hole": "standard",
                "standard": "metric_clearance",
                "designation": designation,
                "depth": {"kind": "through_all"},
            },
        },
    }


def _generic_through_hole(
    *,
    node_id: str,
    name: str,
    solid: str,
    origin_mm: list[float],
    diameter_param: str,
) -> dict[str, Any]:
    return {
        "command": "create_hole",
        "args": {
            "node_id": node_id,
            "name": name,
            "solid": solid,
            "origin_mm": [round(float(v), 4) for v in origin_mm],
            "axis": [0.0, 0.0, -1.0],
            "definition": {
                "hole": "generic",
                "diameter": dim_param(diameter_param),
                "depth": {"kind": "through_all"},
                "profile": {"kind": "simple"},
            },
        },
    }


def _sample_mounting_plate(rng: random.Random, *, seed: int, index: int) -> SyntheticTaskSpec:
    width = float(rng.choice([80, 90, 100, 110, 120, 140]))
    depth = float(rng.choice([50, 60, 70, 80, 90]))
    thickness = float(rng.choice([6, 8, 10, 12]))
    edge_margin = float(rng.choice([10, 12, 15]))
    designation, hole_dia = rng.choice(_FASTENERS)
    has_pilot = rng.random() < 0.65
    pilot_dia = float(rng.choice([14, 18, 20, 24])) if has_pilot else 0.0
    material = rng.choice(_MATERIALS)
    min_wall = 2.0

    part_name = f"MountingPlate_{index:04d}"
    task_id = f"syn-mounting_plate-{index:04d}-s{seed}"
    prompt_id = f"syn-plate-{index:04d}-s{seed}"

    pilot_phrase = (
        f" and a central {pilot_dia:.1f} mm through pilot bore" if has_pilot else ""
    )
    prompt = (
        f"Create a rectangular {material} mounting plate {width:.1f}x{depth:.1f}x{thickness:.1f} mm "
        f"with four {designation} clearance through holes inset {edge_margin:.1f} mm from each corner"
        f"{pilot_phrase}. Maintain minimum wall thickness >= {min_wall:.1f} mm."
    )

    params: dict[str, float] = {
        "width": width,
        "depth": depth,
        "thickness": thickness,
        "edge_margin": edge_margin,
    }
    if has_pilot:
        params["pilot_diameter"] = pilot_dia

    interfaces = [
        MechanicalInterface(
            interface_id="corner_bolt_pattern",
            kind="bolt_pattern",
            diameter_mm=hole_dia,
            count=4,
            normal="+Z",
            notes=f"4x {designation} clearance holes",
        )
    ]
    if has_pilot:
        interfaces.append(
            MechanicalInterface(
                interface_id="central_pilot_bore",
                kind="bore",
                diameter_mm=pilot_dia,
                normal="+Z",
                notes="Central locating pilot bore",
            )
        )

    contract = DesignContract(
        part_name=part_name,
        material=material,
        manufacturing_method="CNC_3AXIS_MILLING",
        keep_in_bbox_mm=BBox3D(min=[0.0, 0.0, 0.0], max=[width, depth, thickness]),
        interfaces=interfaces,
        summary=prompt,
        dfm_constraints={"min_wall_thickness_mm": min_wall, "accessible_axes": ["+Z"]},
        part_archetype="mounting_plate",
        feature_params=dict(params),
    )

    commands: list[dict[str, Any]] = [_set_contract_cmd(contract, category="mounting_plate")]
    for pname in sorted(params):
        commands.append(_upsert_length(pname, params[pname]))

    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "base_plate",
                "name": "Base Plate",
                "width": dim_param("width"),
                "depth": dim_param("depth"),
                "height": dim_param("thickness"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )

    corners = [
        ("hole_bl", [edge_margin, edge_margin, thickness]),
        ("hole_br", [width - edge_margin, edge_margin, thickness]),
        ("hole_tr", [width - edge_margin, depth - edge_margin, thickness]),
        ("hole_tl", [edge_margin, depth - edge_margin, thickness]),
    ]
    current_solid = "base_plate"
    for hid, origin in corners:
        commands.append(
            _standard_through_hole(
                node_id=hid,
                name=f"Corner Hole {hid}",
                solid=current_solid,
                origin_mm=origin,
                designation=designation,
            )
        )
        current_solid = hid

    if has_pilot:
        commands.append(
            _generic_through_hole(
                node_id="pilot_bore",
                name="Central Pilot Bore",
                solid=current_solid,
                origin_mm=[width * 0.5, depth * 0.5, thickness],
                diameter_param="pilot_diameter",
            )
        )

    return SyntheticTaskSpec(
        task_id=task_id,
        prompt_id=prompt_id,
        category="mounting_plate",
        seed=seed,
        prompt=prompt,
        contract=contract,
        parameters=params,
        baseline_commands=commands,
    )


def _sample_bracket(rng: random.Random, *, seed: int, index: int) -> SyntheticTaskSpec:
    width = float(rng.choice([60, 75, 80, 90, 100]))
    base_depth = float(rng.choice([45, 50, 60, 70]))
    vertical_height = float(rng.choice([40, 50, 60, 70]))
    thickness = float(rng.choice([6, 8, 10]))
    designation, hole_dia = rng.choice(_FASTENERS)
    material = rng.choice(_MATERIALS)
    min_wall = 2.0

    part_name = f"LBracket_{index:04d}"
    task_id = f"syn-bracket-{index:04d}-s{seed}"
    prompt_id = f"syn-bracket-{index:04d}-s{seed}"
    prompt = (
        f"Create an L-shaped {material} mounting bracket of width {width:.1f} mm, "
        f"base flange depth {base_depth:.1f} mm, upright flange height {vertical_height:.1f} mm, "
        f"and uniform flange thickness {thickness:.1f} mm with two {designation} mounting holes "
        f"on the horizontal flange. Minimum wall thickness >= {min_wall:.1f} mm."
    )

    params: dict[str, float] = {
        "width": width,
        "base_depth": base_depth,
        "vertical_height": vertical_height,
        "thickness": thickness,
    }

    contract = DesignContract(
        part_name=part_name,
        material=material,
        manufacturing_method="CNC_3AXIS_MILLING",
        keep_in_bbox_mm=BBox3D(
            min=[0.0, 0.0, 0.0],
            max=[width, base_depth, thickness + vertical_height],
        ),
        interfaces=[
            MechanicalInterface(
                interface_id="base_mount_holes",
                kind="bolt_pattern",
                diameter_mm=hole_dia,
                count=2,
                normal="+Z",
                notes=f"2x {designation} base mounting holes",
            )
        ],
        summary=prompt,
        dfm_constraints={"min_wall_thickness_mm": min_wall, "accessible_axes": ["+Z", "+Y"]},
        part_archetype="bracket",
        feature_params=dict(params),
    )

    commands: list[dict[str, Any]] = [_set_contract_cmd(contract, category="bracket")]
    for pname in sorted(params):
        commands.append(_upsert_length(pname, params[pname]))

    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "base_flange",
                "name": "Horizontal Base Flange",
                "width": dim_param("width"),
                "depth": dim_param("base_depth"),
                "height": dim_param("thickness"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )
    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "upright_flange",
                "name": "Vertical Upright Flange",
                "width": dim_param("width"),
                "depth": dim_param("thickness"),
                "height": dim_param("vertical_height"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )
    commands.append(
        {
            "command": "boolean_solids",
            "args": {
                "node_id": "bracket_body",
                "name": "Bracket Union Body",
                "op": "union",
                "target": "base_flange",
                "tools": ["upright_flange"],
            },
        }
    )

    hole_y = thickness + (base_depth - thickness) * 0.5
    hole_x_offset = max(12.0, width * 0.22)
    commands.append(
        _standard_through_hole(
            node_id="mount_hole_left",
            name="Left Mount Hole",
            solid="bracket_body",
            origin_mm=[hole_x_offset, hole_y, thickness],
            designation=designation,
        )
    )
    commands.append(
        _standard_through_hole(
            node_id="mount_hole_right",
            name="Right Mount Hole",
            solid="mount_hole_left",
            origin_mm=[width - hole_x_offset, hole_y, thickness],
            designation=designation,
        )
    )

    return SyntheticTaskSpec(
        task_id=task_id,
        prompt_id=prompt_id,
        category="bracket",
        seed=seed,
        prompt=prompt,
        contract=contract,
        parameters=params,
        baseline_commands=commands,
    )


def _sample_motor_mount(rng: random.Random, *, seed: int, index: int) -> SyntheticTaskSpec:
    nema_label, size_mm, bolt_pitch, pilot_dia, designation, hole_dia = rng.choice(
        _NEMA_PROFILES
    )
    flange_pad = float(rng.choice([6, 8, 12]))
    width = size_mm + flange_pad
    depth = size_mm + flange_pad
    thickness = float(rng.choice([8, 10, 12]))
    material = rng.choice(_MATERIALS)
    min_wall = 2.0

    part_name = f"MotorMount_{nema_label}_{index:04d}"
    task_id = f"syn-motor_mount-{index:04d}-s{seed}"
    prompt_id = f"syn-motor-{index:04d}-s{seed}"
    prompt = (
        f"Create a {nema_label} {material} motor mount plate {width:.1f}x{depth:.1f}x{thickness:.1f} mm "
        f"with a central {pilot_dia:.1f} mm through pilot bore and four {designation} mounting holes "
        f"on a {bolt_pitch:.2f} mm square bolt pattern. Minimum wall thickness >= {min_wall:.1f} mm."
    )

    params: dict[str, float] = {
        "width": width,
        "depth": depth,
        "thickness": thickness,
        "pilot_diameter": pilot_dia,
        "bolt_pitch": bolt_pitch,
    }

    contract = DesignContract(
        part_name=part_name,
        material=material,
        manufacturing_method="CNC_3AXIS_MILLING",
        keep_in_bbox_mm=BBox3D(min=[0.0, 0.0, 0.0], max=[width, depth, thickness]),
        interfaces=[
            MechanicalInterface(
                interface_id="nema_pilot_bore",
                kind="bore",
                diameter_mm=pilot_dia,
                normal="+Z",
                notes=f"{nema_label} register pilot bore",
            ),
            MechanicalInterface(
                interface_id="nema_bolt_pattern",
                kind="bolt_pattern",
                diameter_mm=hole_dia,
                count=4,
                normal="+Z",
                notes=f"4x {designation} at {bolt_pitch:.2f} mm square pitch",
            ),
        ],
        summary=prompt,
        dfm_constraints={"min_wall_thickness_mm": min_wall, "accessible_axes": ["+Z"]},
        part_archetype="motor_mount",
        feature_params=dict(params),
    )

    commands: list[dict[str, Any]] = [_set_contract_cmd(contract, category="motor_mount")]
    for pname in sorted(params):
        commands.append(_upsert_length(pname, params[pname]))

    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "mount_plate",
                "name": f"{nema_label} Mount Plate",
                "width": dim_param("width"),
                "depth": dim_param("depth"),
                "height": dim_param("thickness"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )

    cx = width * 0.5
    cy = depth * 0.5
    commands.append(
        _generic_through_hole(
            node_id="pilot_bore",
            name="Motor Pilot Bore",
            solid="mount_plate",
            origin_mm=[cx, cy, thickness],
            diameter_param="pilot_diameter",
        )
    )

    half_p = bolt_pitch * 0.5
    bolt_coords = [
        ("bolt_1", [cx - half_p, cy - half_p, thickness]),
        ("bolt_2", [cx + half_p, cy - half_p, thickness]),
        ("bolt_3", [cx + half_p, cy + half_p, thickness]),
        ("bolt_4", [cx - half_p, cy + half_p, thickness]),
    ]
    current_solid = "pilot_bore"
    for bid, origin in bolt_coords:
        commands.append(
            _standard_through_hole(
                node_id=bid,
                name=f"NEMA Bolt {bid}",
                solid=current_solid,
                origin_mm=origin,
                designation=designation,
            )
        )
        current_solid = bid

    return SyntheticTaskSpec(
        task_id=task_id,
        prompt_id=prompt_id,
        category="motor_mount",
        seed=seed,
        prompt=prompt,
        contract=contract,
        parameters=params,
        baseline_commands=commands,
    )


def _sample_enclosure(rng: random.Random, *, seed: int, index: int) -> SyntheticTaskSpec:
    width = float(rng.choice([80, 100, 120, 140]))
    depth = float(rng.choice([60, 70, 80, 90]))
    height = float(rng.choice([30, 36, 42, 48]))
    wall = float(rng.choice([3.0, 4.0, 5.0]))
    tab_width = float(rng.choice([14.0, 16.0, 18.0]))
    tab_thickness = float(rng.choice([5.0, 6.0]))
    designation, hole_dia = rng.choice(_FASTENERS[:3])  # M4, M5, M6
    material = rng.choice(_MATERIALS)
    min_wall = 2.0

    part_name = f"ControlEnclosure_{index:04d}"
    task_id = f"syn-enclosure-{index:04d}-s{seed}"
    prompt_id = f"syn-enclosure-{index:04d}-s{seed}"
    prompt = (
        f"Create a shelled {material} control enclosure {width:.1f}x{depth:.1f}x{height:.1f} mm "
        f"with wall thickness {wall:.1f} mm, mounting flange tabs ({tab_width:.1f} mm span, "
        f"{tab_thickness:.1f} mm thick), and two {designation} mounting holes. "
        f"Maintain minimum wall thickness >= {min_wall:.1f} mm."
    )

    params: dict[str, float] = {
        "width": width,
        "depth": depth,
        "height": height,
        "wall_thickness": wall,
        "tab_width": tab_width,
        "tab_thickness": tab_thickness,
    }

    contract = DesignContract(
        part_name=part_name,
        material=material,
        manufacturing_method="CNC_3AXIS_MILLING",
        keep_in_bbox_mm=BBox3D(
            min=[0.0, 0.0, 0.0],
            max=[width, depth, height],
        ),
        interfaces=[
            MechanicalInterface(
                interface_id="tab_mount_holes",
                kind="bolt_pattern",
                diameter_mm=hole_dia,
                count=2,
                normal="+Z",
                notes=f"2x {designation} enclosure mounting holes",
            )
        ],
        summary=prompt,
        dfm_constraints={"min_wall_thickness_mm": min_wall, "accessible_axes": ["+Z"]},
        part_archetype="enclosure",
        feature_params=dict(params),
    )

    commands: list[dict[str, Any]] = [_set_contract_cmd(contract, category="enclosure")]
    for pname in sorted(params):
        commands.append(_upsert_length(pname, params[pname]))

    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "outer_shell",
                "name": "Outer Enclosure Block",
                "width": dim_param("width"),
                "depth": dim_param("depth"),
                "height": dim_param("height"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )
    commands.append(
        {
            "command": "shell_solid",
            "args": {
                "node_id": "walled_box",
                "name": "Shelled Enclosure Body",
                "solid": "outer_shell",
                "thickness": dim_param("wall_thickness"),
                "inward": True,
            },
        }
    )
    commands.append(
        _standard_through_hole(
            node_id="left_tab_hole",
            name="Left Mounting Hole",
            solid="walled_box",
            origin_mm=[max(10.0, tab_width * 0.6), depth * 0.5, wall],
            designation=designation,
        )
    )
    commands.append(
        _standard_through_hole(
            node_id="right_tab_hole",
            name="Right Mounting Hole",
            solid="left_tab_hole",
            origin_mm=[width - max(10.0, tab_width * 0.6), depth * 0.5, wall],
            designation=designation,
        )
    )

    return SyntheticTaskSpec(
        task_id=task_id,
        prompt_id=prompt_id,
        category="enclosure",
        seed=seed,
        prompt=prompt,
        contract=contract,
        parameters=params,
        baseline_commands=commands,
    )


def _sample_fixture_jig(rng: random.Random, *, seed: int, index: int) -> SyntheticTaskSpec:
    width = float(rng.choice([120, 140, 160, 180]))
    depth = float(rng.choice([80, 90, 100, 110]))
    base_thickness = float(rng.choice([12, 14, 16]))
    boss_width = float(rng.choice([16.0, 18.0, 20.0]))
    boss_height = float(rng.choice([10.0, 12.0, 15.0]))
    designation, hole_dia = rng.choice(_FASTENERS[1:])  # M5, M6, M8
    material = rng.choice(_MATERIALS)
    min_wall = 2.0

    part_name = f"FixtureJig_{index:04d}"
    task_id = f"syn-fixture_jig-{index:04d}-s{seed}"
    prompt_id = f"syn-fixture-{index:04d}-s{seed}"
    prompt = (
        f"Create a {material} datum fixture jig base {width:.1f}x{depth:.1f}x{base_thickness:.1f} mm "
        f"with a prismatic locating boss ({boss_width:.1f}x{boss_width:.1f} mm, height {boss_height:.1f} mm above base), "
        f"two {designation} clamp holes, and a central keep-out clearance zone above the datum face. "
        f"Minimum wall thickness >= {min_wall:.1f} mm."
    )

    params: dict[str, float] = {
        "width": width,
        "depth": depth,
        "thickness": base_thickness,
        "boss_width": boss_width,
        "boss_total_height": base_thickness + boss_height,
    }

    keep_out = KeepOutZone(
        zone_id="workpiece_clearance",
        kind="box",
        origin_mm=[width * 0.35, depth * 0.30, base_thickness + 2.0],
        size_mm=[width * 0.30, depth * 0.40, 25.0],
        axis="+Z",
        description="Central workpiece loading clearance zone",
    )

    contract = DesignContract(
        part_name=part_name,
        material=material,
        manufacturing_method="CNC_3AXIS_MILLING",
        keep_in_bbox_mm=BBox3D(
            min=[0.0, 0.0, 0.0],
            max=[width, depth, base_thickness + boss_height + 27.0],
        ),
        keep_out_zones=[keep_out],
        interfaces=[
            MechanicalInterface(
                interface_id="clamp_holes",
                kind="bolt_pattern",
                diameter_mm=hole_dia,
                count=2,
                normal="+Z",
                notes=f"2x {designation} toggle clamp mounting holes",
            )
        ],
        summary=prompt,
        dfm_constraints={"min_wall_thickness_mm": min_wall, "accessible_axes": ["+Z"]},
        part_archetype="fixture_jig",
        feature_params=dict(params),
    )

    commands: list[dict[str, Any]] = [_set_contract_cmd(contract, category="fixture_jig")]
    for pname in sorted(params):
        commands.append(_upsert_length(pname, params[pname]))

    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "datum_base",
                "name": "Fixture Datum Base",
                "width": dim_param("width"),
                "depth": dim_param("depth"),
                "height": dim_param("thickness"),
                "origin_mm": [0.0, 0.0, 0.0],
            },
        }
    )
    commands.append(
        {
            "command": "create_primitive_box",
            "args": {
                "node_id": "boss_left",
                "name": "Locating Datum Boss",
                "width": dim_param("boss_width"),
                "depth": dim_param("boss_width"),
                "height": dim_param("boss_total_height"),
                "origin_mm": [14.0, (depth - boss_width) * 0.5, 0.0],
            },
        }
    )
    commands.append(
        {
            "command": "boolean_solids",
            "args": {
                "node_id": "fixture_body",
                "name": "Fixture Body With Locating Boss",
                "op": "union",
                "target": "datum_base",
                "tools": ["boss_left"],
            },
        }
    )
    commands.append(
        _standard_through_hole(
            node_id="clamp_hole_front",
            name="Front Clamp Hole",
            solid="fixture_body",
            origin_mm=[width * 0.55, 14.0, base_thickness],
            designation=designation,
        )
    )
    commands.append(
        _standard_through_hole(
            node_id="clamp_hole_rear",
            name="Rear Clamp Hole",
            solid="clamp_hole_front",
            origin_mm=[width * 0.55, depth - 14.0, base_thickness],
            designation=designation,
        )
    )

    return SyntheticTaskSpec(
        task_id=task_id,
        prompt_id=prompt_id,
        category="fixture_jig",
        seed=seed,
        prompt=prompt,
        contract=contract,
        parameters=params,
        baseline_commands=commands,
    )


_SAMPLERS = {
    "mounting_plate": _sample_mounting_plate,
    "bracket": _sample_bracket,
    "motor_mount": _sample_motor_mount,
    "enclosure": _sample_enclosure,
    "fixture_jig": _sample_fixture_jig,
}


def sample_task_spec(category: str, *, seed: int, index: int = 0) -> SyntheticTaskSpec:
    """Deterministically sample one parametric CAD task spec for ``category``."""
    if category not in _SAMPLERS:
        raise ValueError(f"unknown category {category!r}; expected one of {PART_CATEGORIES}")
    rng = random.Random(seed)
    return _SAMPLERS[category](rng, seed=seed, index=index)


def _apply_mutation(
    spec: SyntheticTaskSpec,
    mutation_kind: str,
    rng: random.Random,
) -> tuple[list[dict[str, Any]], str]:
    """Produce a mutated ``SemanticCommand`` batch exhibiting a realistic CAD/DFM failure."""
    cmds = copy.deepcopy(spec.baseline_commands)

    if mutation_kind == "negative_dimension":
        for cmd in cmds:
            if cmd["command"] == "upsert_parameter" and cmd["args"]["name"] in {
                "width",
                "thickness",
                "wall_thickness",
            }:
                neg_val = -round(rng.uniform(2.0, 12.0), 2)
                cmd["args"]["value"]["value"]["value"] = neg_val
                return cmds, f"Mutated {cmd['args']['name']} to negative value {neg_val} mm"
        cmds.append(
            {
                "command": "create_primitive_box",
                "args": {
                    "node_id": "invalid_neg_box",
                    "name": "Negative Box",
                    "width": {"source": "literal", "dimension": "length", "value": -5.0, "unit": "mm"},
                    "depth": {"source": "literal", "dimension": "length", "value": 20.0, "unit": "mm"},
                    "height": {"source": "literal", "dimension": "length", "value": 5.0, "unit": "mm"},
                },
            }
        )
        return cmds, "Added primitive box with negative width"

    if mutation_kind == "zero_dimension":
        for cmd in cmds:
            if cmd["command"] == "upsert_parameter" and cmd["args"]["name"] in {
                "depth",
                "base_depth",
                "height",
                "thickness",
            }:
                cmd["args"]["value"]["value"]["value"] = 0.0
                return cmds, f"Mutated {cmd['args']['name']} to zero (0.0 mm)"
        return cmds, "Zeroed primary dimension"

    if mutation_kind == "thin_wall":
        thin_val = round(rng.uniform(0.25, 0.65), 2)
        for cmd in cmds:
            if cmd["command"] == "upsert_parameter" and cmd["args"]["name"] in {
                "thickness",
                "wall_thickness",
                "tab_thickness",
            }:
                cmd["args"]["value"]["value"]["value"] = thin_val
                return (
                    cmds,
                    f"Mutated {cmd['args']['name']} to {thin_val} mm (violates min_wall_thickness_mm)",
                )
        cmds.append(
            {
                "command": "create_primitive_box",
                "args": {
                    "node_id": "thin_fin",
                    "name": "Thin Wall Fin",
                    "width": {"source": "literal", "dimension": "length", "value": 25.0, "unit": "mm"},
                    "depth": {"source": "literal", "dimension": "length", "value": 25.0, "unit": "mm"},
                    "height": {"source": "literal", "dimension": "length", "value": thin_val, "unit": "mm"},
                    "origin_mm": [200.0, 0.0, 0.0],
                },
            }
        )
        return cmds, f"Added thin fin with thickness {thin_val} mm"

    if mutation_kind == "colliding_body":
        width = float(spec.parameters.get("width", 60.0))
        depth = float(spec.parameters.get("depth", spec.parameters.get("base_depth", 50.0)))
        cmds.append(
            {
                "command": "create_primitive_box",
                "args": {
                    "node_id": "colliding_interference_block",
                    "name": "Colliding Interference Block",
                    "width": {"source": "literal", "dimension": "length", "value": max(12.0, width * 0.4), "unit": "mm"},
                    "depth": {"source": "literal", "dimension": "length", "value": max(12.0, depth * 0.4), "unit": "mm"},
                    "height": {"source": "literal", "dimension": "length", "value": 12.0, "unit": "mm"},
                    "origin_mm": [width * 0.25, depth * 0.25, 1.0],
                },
            }
        )
        return cmds, "Added un-unioned overlapping solid causing body-body interference"

    if mutation_kind == "oversized_hole":
        width = float(spec.parameters.get("width", 80.0))
        oversized_dia = round(width * 1.35, 2)
        target_solid = None
        for cmd in reversed(cmds):
            node_id = cmd["args"].get("node_id")
            if isinstance(node_id, str) and cmd["command"] in {
                "create_hole",
                "boolean_solids",
                "create_primitive_box",
            }:
                target_solid = node_id
                break
        if target_solid is None:
            target_solid = "base_plate"
        cmds.append(
            {
                "command": "create_hole",
                "args": {
                    "node_id": "oversized_hole_err",
                    "name": "Oversized Destructive Hole",
                    "solid": target_solid,
                    "origin_mm": [width * 0.5, 10.0, 10.0],
                    "axis": [0.0, 0.0, -1.0],
                    "definition": {
                        "hole": "generic",
                        "diameter": {
                            "source": "literal",
                            "dimension": "length",
                            "value": oversized_dia,
                            "unit": "mm",
                        },
                        "depth": {"kind": "through_all"},
                        "profile": {"kind": "simple"},
                    },
                },
            }
        )
        return cmds, f"Added oversized hole diameter {oversized_dia} mm exceeding part envelope"

    raise ValueError(f"unknown mutation_kind {mutation_kind!r}")


def _valid_variant(spec: SyntheticTaskSpec, rng: random.Random) -> tuple[list[dict[str, Any]], str]:
    """Create a second valid parametric variation (slightly thicker/reinforced) for diversity."""
    cmds = copy.deepcopy(spec.baseline_commands)
    delta = float(rng.choice([1.0, 2.0]))
    for cmd in cmds:
        if cmd["command"] == "upsert_parameter" and cmd["args"]["name"] in {
            "thickness",
            "tab_thickness",
        }:
            old = float(cmd["args"]["value"]["value"]["value"])
            new_val = round(old + delta, 4)
            cmd["args"]["value"]["value"]["value"] = new_val
            return cmds, f"Valid reinforced variant ({cmd['args']['name']}={new_val} mm)"
    return cmds, "Valid baseline variant"


def mutate_candidate_commands(
    spec: SyntheticTaskSpec,
    *,
    num_candidates: int = 3,
    rng: random.Random | None = None,
    seed: int | None = None,
) -> list[CandidateMutation]:
    """Generate ``num_candidates`` diverse trajectories (valid + failure mutations) per prompt.

    Guarantees at least one valid baseline candidate and, whenever ``num_candidates >= 2``,
    at least one realistic engineering failure mutation so GRPO groups have real non-zero
    reward variance (``std_reward > 0``).
    """
    if isinstance(num_candidates, bool) or not isinstance(num_candidates, int) or num_candidates < 1:
        raise ValueError("num_candidates must be a positive integer")
    local_rng = rng if rng is not None else random.Random(spec.seed if seed is None else seed)

    candidates: list[CandidateMutation] = []
    seen_json: set[str] = set()

    # Candidate 0: valid parametric baseline.
    base_json = canonical_commands_json(spec.baseline_commands)
    seen_json.add(base_json)
    candidates.append(
        CandidateMutation(
            candidate_index=0,
            mutation_kind="valid",
            description="Valid parametric baseline trajectory",
            commands=copy.deepcopy(spec.baseline_commands),
            commands_json=base_json,
            is_mutated=False,
        )
    )

    # Shuffle mutation kinds deterministically for this prompt.
    # Prioritize hard failures (negative/zero dim) early so even num_candidates=2 gets variance.
    mutation_pool = list(MUTATION_KINDS)
    hard_failures = ["negative_dimension", "zero_dimension"]
    soft_failures = ["thin_wall", "colliding_body", "oversized_hole"]
    local_rng.shuffle(hard_failures)
    local_rng.shuffle(soft_failures)
    ordered_mutations = [hard_failures[0], *soft_failures, hard_failures[1]]

    mut_cursor = 0
    for idx in range(1, num_candidates):
        # For >= 4 candidates, include a second valid reinforced variant at index == num_candidates - 1.
        if num_candidates >= 4 and idx == num_candidates - 1:
            cmds, desc = _valid_variant(spec, local_rng)
            cjson = canonical_commands_json(cmds)
            if cjson not in seen_json:
                seen_json.add(cjson)
                candidates.append(
                    CandidateMutation(
                        candidate_index=idx,
                        mutation_kind="valid_variant",
                        description=desc,
                        commands=cmds,
                        commands_json=cjson,
                        is_mutated=False,
                    )
                )
                continue

        chosen_kind = ordered_mutations[mut_cursor % len(ordered_mutations)]
        mut_cursor += 1
        cmds, desc = _apply_mutation(spec, chosen_kind, local_rng)
        cjson = canonical_commands_json(cmds)
        if cjson in seen_json:
            # Ensure uniqueness within the candidate group.
            fallback_kind = mutation_pool[(mut_cursor + idx) % len(mutation_pool)]
            cmds, desc = _apply_mutation(spec, fallback_kind, local_rng)
            cjson = canonical_commands_json(cmds)
            chosen_kind = fallback_kind
        seen_json.add(cjson)
        candidates.append(
            CandidateMutation(
                candidate_index=idx,
                mutation_kind=chosen_kind,
                description=desc,
                commands=cmds,
                commands_json=cjson,
                is_mutated=True,
            )
        )

    return candidates


class _MutatedCandidatePlanner:
    """``SemanticPlanner`` adapter serving pre-sampled candidate batches to ``BranchPatchExplorer``."""

    def __init__(self, candidates: Sequence[CandidateMutation]) -> None:
        self._candidates = list(candidates)

    def plan_creation(
        self,
        contract: DesignContract,
        intent: str,
        *,
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        del contract, intent, diagnostics
        if attempt < 0 or attempt >= len(self._candidates):
            raise IndexError(f"candidate attempt {attempt} out of range ({len(self._candidates)})")
        return copy.deepcopy(self._candidates[attempt].commands)

    def plan_local_edit(
        self,
        *,
        target: str,
        instruction: str,
        context: Mapping[str, Any],
        diagnostics: Sequence[Mapping[str, Any]],
        attempt: int,
    ) -> list[dict[str, Any]]:
        raise NotImplementedError("_MutatedCandidatePlanner only supports 0->1 creation rollouts")


def deduplicate_candidates(
    prompt_id: str,
    candidates: Sequence[CandidateMutation],
    seen: set[tuple[str, str]],
) -> tuple[list[CandidateMutation], int]:
    """Filter out candidates whose ``(prompt_id, canonical_commands_json)`` was already seen."""
    kept: list[CandidateMutation] = []
    skipped = 0
    for cand in candidates:
        canonical = canonical_commands_json(cand.commands)
        key = (prompt_id, canonical)
        if key in seen:
            skipped += 1
            continue
        seen.add(key)
        kept.append(cand)
    return kept, skipped


def is_sft_eligible(outcome: SemanticCandidateOutcome) -> bool:
    """Strict SFT admission gate: ``gate_passed == True`` and ``L0=PASS``, ``L1=PASS``, ``L3!=FAIL``."""
    if not outcome.applied_ok or not outcome.gate_passed:
        return False
    ver = outcome.verification
    if not isinstance(ver, Mapping):
        return False
    layers = ver.get("layers")
    if not isinstance(layers, Mapping):
        return False
    l0 = str(layers.get("L0_structure_units", "")).upper()
    l1 = str(layers.get("L1_geometry_topology", "")).upper()
    l3 = str(layers.get("L3_manufacturability_physics", "")).upper()
    return l0 == "PASS" and l1 == "PASS" and l3 != "FAIL"


def format_sft_chatml_record(
    spec: SyntheticTaskSpec,
    outcome: SemanticCandidateOutcome,
) -> dict[str, Any]:
    """Format a verified winning candidate as a ChatML/OpenAI-compatible SFT JSONL record."""
    if not is_sft_eligible(outcome):
        raise ValueError(
            f"candidate {outcome.candidate_id!r} failed strict SFT eligibility "
            f"(gate_passed={outcome.gate_passed}, overall={outcome.overall_status})"
        )
    user_payload = {
        "mode": "create",
        "intent": spec.prompt,
        "design_contract": spec.contract.to_dict(),
    }
    user_prompt = "\n".join(
        [
            "mode: create",
            "request:",
            json.dumps(user_payload, ensure_ascii=False, sort_keys=True),
            "semantic_command_contract:",
            semantic_command_contract(),
            "Return the JSON array only.",
        ]
    )
    layers = (
        dict(outcome.verification.get("layers", {}))
        if isinstance(outcome.verification, Mapping)
        else {}
    )
    return {
        "messages": [
            {"role": "system", "content": _SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
            {"role": "assistant", "content": outcome.commands_json},
        ],
        "metadata": {
            "task_id": spec.task_id,
            "prompt_id": spec.prompt_id,
            "category": spec.category,
            "seed": spec.seed,
            "candidate_id": outcome.candidate_id,
            "branch_name": outcome.branch_name,
            "reward": outcome.reward,
            "group_advantage": outcome.group_advantage,
            "overall_status": outcome.overall_status,
            "layers": layers,
        },
    }


def _init_sqlite_project(cli_bin: Path, project_path: Path, part_name: str, timeout_sec: float) -> None:
    completed = subprocess.run(
        [
            str(cli_bin),
            "project",
            "init",
            str(project_path),
            "--name",
            part_name,
            "--format",
            "sqlite",
            "--json",
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout_sec,
        check=False,
    )
    if completed.returncode != 0:
        detail = (completed.stderr or "").strip() or (completed.stdout or "").strip()
        raise BranchExplorationError(f"cad-cli project init failed: {detail}")
    try:
        payload = json.loads(completed.stdout or "{}")
    except json.JSONDecodeError as exc:
        raise BranchExplorationError(f"cad-cli project init returned non-JSON: {exc}") from exc
    if payload.get("status") != "ok":
        raise BranchExplorationError(f"cad-cli project init error: {payload}")

    # Remove the default starter box on main so 0->1 candidate branches start with a clean canvas.
    tmp_cmd: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            suffix=".json",
            prefix="aicad_clean_init_",
            delete=False,
        ) as fh:
            fh.write(json.dumps([{"command": "delete_node", "args": {"node_id": "box"}}]))
            tmp_cmd = Path(fh.name)
        subprocess.run(
            [
                str(cli_bin),
                "api",
                "apply",
                "--project",
                str(project_path),
                "--workspace",
                str(project_path.parent),
                "--branch",
                "main",
                "--commands",
                f"@{tmp_cmd}",
                "--json",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout_sec,
            check=False,
        )
    finally:
        if tmp_cmd is not None:
            tmp_cmd.unlink(missing_ok=True)


def _write_jsonl_atomic(path: Path, records: Sequence[Mapping[str, Any]]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        json.dumps(
            dict(rec),
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
        + "\n"
        for rec in records
    ]
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        newline="\n",
        dir=path.parent,
        delete=False,
    ) as handle:
        tmp_path = Path(handle.name)
        for line in lines:
            handle.write(line)
    try:
        tmp_path.replace(path)
    finally:
        tmp_path.unlink(missing_ok=True)
    return len(lines)


def _write_json_atomic(path: Path, payload: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(dict(payload), ensure_ascii=False, indent=2, sort_keys=True, allow_nan=False) + "\n"
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        newline="\n",
        dir=path.parent,
        delete=False,
    ) as handle:
        tmp_path = Path(handle.name)
        handle.write(text)
    try:
        tmp_path.replace(path)
    finally:
        tmp_path.unlink(missing_ok=True)


def run_synthetic_factory(config: SyntheticFactoryConfig) -> SyntheticFactoryStats:
    """Execute the end-to-end offline synthetic CAD SFT/GRPO data factory."""
    cli_bin = config.cli_bin if config.cli_bin is not None else discover_cad_cli()
    if cli_bin is None or not Path(cli_bin).is_file():
        raise FileNotFoundError(
            "cad-cli binary was not found (pass cli_bin, set CAD_CLI_BIN, or build target/debug/cad-cli)"
        )
    cli_bin = Path(cli_bin).resolve()

    out_dir = config.out_dir
    out_dir.mkdir(parents=True, exist_ok=True)
    grpo_path = out_dir / "grpo_train.jsonl"
    sft_path = out_dir / "sft_train.jsonl"
    summary_path = out_dir / "factory_summary.json"

    explorer = BranchPatchExplorer(
        cli_bin=cli_bin,
        min_wall_mm=config.min_wall_mm,
        timeout_sec=config.timeout_sec,
    )

    seen_trajectories: set[tuple[str, str]] = set()
    grpo_groups: list[GrpoPromptGroup] = []
    sft_records: list[dict[str, Any]] = []
    all_rewards: list[float] = []

    category_accum: dict[str, dict[str, Any]] = {
        cat: {
            "tasks": 0,
            "passing_tasks": 0,
            "candidates": 0,
            "passing_candidates": 0,
            "sft_examples": 0,
            "rewards": [],
        }
        for cat in config.categories
    }

    completed_tasks = 0
    crashed_rollouts = 0
    total_candidates_evaluated = 0
    unique_candidates_kept = 0
    deduplicated_candidates = 0

    def _run_in_workspace(ws_dir: Path) -> None:
        nonlocal completed_tasks, crashed_rollouts
        nonlocal total_candidates_evaluated, unique_candidates_kept, deduplicated_candidates

        for idx in range(config.count):
            category = config.categories[idx % len(config.categories)]
            task_seed = config.seed + idx * 997 + 17
            spec = sample_task_spec(category, seed=task_seed, index=idx)
            raw_candidates = mutate_candidate_commands(
                spec,
                num_candidates=config.candidates_per_prompt,
                seed=task_seed + 1,
            )

            # Deduplicate by (prompt_id, canonical_commands_json).
            unique_candidates, dup_count = deduplicate_candidates(
                spec.prompt_id, raw_candidates, seen_trajectories
            )
            deduplicated_candidates += dup_count

            if not unique_candidates:
                continue

            project_path = ws_dir / f"{spec.task_id}.aicad"
            try:
                _init_sqlite_project(cli_bin, project_path, spec.contract.part_name, config.timeout_sec)
                planner = _MutatedCandidatePlanner(unique_candidates)
                result: SemanticBestOfNResult = explorer.explore_semantic_candidates(
                    project_path,
                    planner,
                    workspace_root=ws_dir,
                    contract=spec.contract,
                    intent=spec.prompt,
                    config=SemanticRolloutConfig(
                        num_candidates=len(unique_candidates),
                        base_branch="main",
                        branch_prefix="ai/cand",
                        require_full_pass=config.require_full_pass,
                        reward_weights=config.reward_weights,
                        grpo_out_path=None,
                        prompt_id=spec.prompt_id,
                    ),
                )
            except Exception:
                crashed_rollouts += 1
                continue

            completed_tasks += 1
            total_candidates_evaluated += len(result.outcomes)
            unique_candidates_kept += len(result.outcomes)
            grpo_groups.append(result.grpo_group)

            cat_bucket = category_accum[category]
            cat_bucket["tasks"] += 1
            cat_bucket["candidates"] += len(result.outcomes)

            for outcome in result.outcomes:
                all_rewards.append(outcome.reward)
                cat_bucket["rewards"].append(outcome.reward)
                if outcome.applied_ok and outcome.gate_passed:
                    cat_bucket["passing_candidates"] += 1

            # Pick the winning candidate if it satisfies strict SFT admission.
            winner_outcome: SemanticCandidateOutcome | None = None
            if result.winner_candidate_id is not None:
                for outcome in result.outcomes:
                    if outcome.candidate_id == result.winner_candidate_id:
                        winner_outcome = outcome
                        break
            if winner_outcome is not None and is_sft_eligible(winner_outcome):
                cat_bucket["passing_tasks"] += 1
                cat_bucket["sft_examples"] += 1
                sft_records.append(format_sft_chatml_record(spec, winner_outcome))

    if config.keep_projects:
        projects_dir = out_dir / "projects"
        projects_dir.mkdir(parents=True, exist_ok=True)
        _run_in_workspace(projects_dir)
    else:
        with tempfile.TemporaryDirectory(prefix="aicad_syn_factory_") as tmp_dir:
            _run_in_workspace(Path(tmp_dir))

    # Write GRPO dataset and verify it loads cleanly via load_grpo_training_jsonl.
    grpo_written = export_grpo_training_jsonl(grpo_groups, grpo_path, append=False)
    loaded_grpo = load_grpo_training_jsonl(grpo_path)
    if len(loaded_grpo) != grpo_written:
        raise RuntimeError(
            f"GRPO validation mismatch: wrote {grpo_written}, loaded {len(loaded_grpo)}"
        )

    # Write SFT ChatML JSONL.
    sft_written = _write_jsonl_atomic(sft_path, sft_records)

    grpo_groups_with_variance = sum(1 for g in grpo_groups if g.std_reward > 0.0)
    overall_mean = mean(all_rewards) if all_rewards else 0.0
    overall_std = pstdev(all_rewards) if all_rewards else 0.0

    category_stats: dict[str, dict[str, Any]] = {}
    for cat, bucket in category_accum.items():
        tasks_n = bucket["tasks"]
        rewards_list = bucket["rewards"]
        category_stats[cat] = {
            "tasks": tasks_n,
            "passing_tasks": bucket["passing_tasks"],
            "pass_rate": (bucket["passing_tasks"] / tasks_n) if tasks_n else 0.0,
            "candidates": bucket["candidates"],
            "passing_candidates": bucket["passing_candidates"],
            "sft_examples": bucket["sft_examples"],
            "mean_reward": mean(rewards_list) if rewards_list else 0.0,
            "std_reward": pstdev(rewards_list) if rewards_list else 0.0,
        }

    stats = SyntheticFactoryStats(
        seed=config.seed,
        requested_tasks=config.count,
        completed_tasks=completed_tasks,
        crashed_rollouts=crashed_rollouts,
        total_candidates_evaluated=total_candidates_evaluated,
        unique_candidates_kept=unique_candidates_kept,
        deduplicated_candidates=deduplicated_candidates,
        grpo_groups_written=grpo_written,
        grpo_groups_with_variance=grpo_groups_with_variance,
        sft_examples_written=sft_written,
        mean_reward=overall_mean,
        std_reward=overall_std,
        category_stats=category_stats,
        grpo_jsonl_path=str(grpo_path),
        sft_jsonl_path=str(sft_path),
        summary_json_path=str(summary_path),
        vllm_mounting_recipe=build_vllm_mounting_recipe(),
    )
    _write_json_atomic(summary_path, stats.to_dict())
    return stats


__all__ = [
    "MUTATION_KINDS",
    "PART_CATEGORIES",
    "CandidateMutation",
    "SyntheticFactoryConfig",
    "SyntheticFactoryStats",
    "SyntheticTaskSpec",
    "build_vllm_mounting_recipe",
    "deduplicate_candidates",
    "format_sft_chatml_record",
    "is_sft_eligible",
    "mutate_candidate_commands",
    "run_synthetic_factory",
    "sample_task_spec",
]
