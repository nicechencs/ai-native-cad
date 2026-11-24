"""Recognize known gaps before turning a prompt into template geometry."""

from __future__ import annotations

import json
import re
import unicodedata
from pathlib import Path


_RULES = tuple(
    (rule["id"], rule["label"], re.compile(rule["pattern"], re.ASCII))
    for rule in json.loads(Path(__file__).with_name("unsupported_features.json").read_text(encoding="utf-8"))
)


class UnsupportedFeaturesError(ValueError):
    def __init__(self, features: list[str], labels: list[str]) -> None:
        self.features = features
        super().__init__(f"自然语言建模暂不支持：{'、'.join(labels)}。未生成模型。")


def check_template_prompt(prompt: str) -> None:
    normalized = unicodedata.normalize("NFKC", prompt).lower()
    matches = [(feature, label) for feature, label, pattern in _RULES if pattern.search(normalized)]
    if matches:
        raise UnsupportedFeaturesError([feature for feature, _ in matches], [label for _, label in matches])
