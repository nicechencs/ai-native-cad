"""Backward-compatible re-exports for CAD agent components (no hardcoded geometry templates)."""

from __future__ import annotations

import json
import math
import os
import re
import shutil
import sys as _sys
import tempfile
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from pathlib import Path
from types import ModuleType as _ModuleType
from typing import Any, Protocol

from . import _agent_utils, cad_coder as _cad_coder, critic as _critic, pipeline as _pipeline
from . import spec_parser as _spec_parser

from .compiler_bridge import (
    CompilerBridge,
    CompilerBridgeError,
    check_external_envelope,
    check_keep_in_bbox,
    check_keep_out_zones,
    check_non_zero_volume,
    check_watertight,
    find_workspace_root,
)
from .contracts import (
    VALID_MANUFACTURING_METHODS,
    VALID_TRI_VERDICTS,
    VERIFICATION_LAYERS,
    BBox3D,
    CompileResult,
    ContractValidationError,
    CriticVerdict,
    DesignContract,
    ExternalInterfaceSummary,
    GateCheckResult,
    KernelIdentity,
    LoadCase,
    MechanicalInterface,
    TriVerdict,
    VerificationCheck,
    VerificationLayer,
    VerificationReport,
    aggregate_tri_verdicts,
)
from .llm import (
    GeminiRestClient,
    LLMClient,
    LLMConfig,
    LLMConfigError,
    OpenAICompatibleClient,
    ResolvedLLMInfo,
    create_llm_client,
)
from .prompt_support import check_template_prompt
from .retrieval import ExampleRetriever, LexicalExampleRetriever, format_few_shot_block
from .structured import parse_contract_with_repair

from ._agent_utils import (
    _strip_code_fences,
    _extract_json_payload,
    _to_pascal_case,
    _RepairCountingLLMClient,
    _assert_within_base,
    _PROGRESS_SECRET_PATTERNS,
    _sanitize_progress_text,
    _sanitize_progress_value,
)
from .spec_parser import SpecParserAgent
from .cad_coder import CadCoderAgent
from .critic import (
    OrthogonalViewInspector,
    HeuristicViewInspector,
    _estimate_cylinder_diameter_from_faces,
    _finite_number,
    _whole_number,
    _string_tags,
    _tag_equals,
    _bound_to_interface,
    _evidence_documents,
    _collect_records,
    _is_compiler_geometry_record,
    _handle_matches,
    _handle_diameter_mm,
    _handle_count,
    _record_diameter_mm,
    _pmi_has_tolerance,
    check_interfaces,
    _dot3,
    _external_hole_geometry,
    _projected_distance,
    check_external_holes,
    VisualGeometricCritic,
)
from .pipeline import (
    IterationRecord,
    PipelineResult,
    _infer_edit_contract,
    SelfRefinementPipeline,
)


# A legacy patch must reach every module that uses the original binding.
# Keep the imports above as real attributes for introspection and patch restoration.
_patch_targets = {
    name: tuple(
        module
        for module in (_agent_utils, _spec_parser, _cad_coder, _critic, _pipeline)
        if name in vars(module) and vars(module)[name] is value
    )
    for name, value in list(globals().items())
    if not name.startswith("__")
}


class _AgentsModule(_ModuleType):
    def __setattr__(self, name: str, value: Any) -> None:
        super().__setattr__(name, value)
        for module in _patch_targets.get(name, ()):
            setattr(module, name, value)

    def __delattr__(self, name: str) -> None:
        super().__delattr__(name)
        for module in _patch_targets.get(name, ()):
            delattr(module, name)


_sys.modules[__name__].__class__ = _AgentsModule
