"""Pluggable Few-shot golden `.kcl` example retrieval for `cad_agent`."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol

from .compiler_bridge import find_workspace_root

DEFAULT_SKIP_DIRS: tuple[str, ...] = ("phase2-hybrid",)

# Canonical mechanical concept -> English & Chinese synonyms/keywords.
# Multi-word phrases and Chinese substrings are matched directly against lowercased text.
SYNONYM_GROUPS: dict[str, tuple[str, ...]] = {
    "enclosure_box": (
        "box",
        "enclosure",
        "shell",
        "shelled",
        "control_box",
        "control",
        "case",
        "casing",
        "thin_wall",
        "thin",
        "wall",
        "盒",
        "控制盒",
        "薄壁",
        "抽壳",
        "壳体",
        "外壳",
        "箱体",
        "机箱",
    ),
    "flange_ring": (
        "flange",
        "circular",
        "ring",
        "hub",
        "revolve",
        "turning",
        "cylinder",
        "法兰",
        "圆形法兰",
        "圆盘",
        "轮毂",
        "回转",
        "旋转",
        "车削",
    ),
    "bracket_support": (
        "bracket",
        "rib",
        "ribbed",
        "gusset",
        "mount",
        "support",
        "stand",
        "支架",
        "托架",
        "加强筋",
        "筋板",
        "安装座",
        "支撑座",
    ),
    "l_sensor_bracket": (
        "l_sensor",
        "l_bracket",
        "sensor",
        "perpendicular",
        "l型",
        "l形",
        "直角支架",
        "传感器",
        "传感器支架",
    ),
    "u_clevis": (
        "clevis",
        "u_clevis",
        "yoke",
        "pin",
        "u型",
        "u形",
        "耳座",
        "耳片",
        "接头",
        "销轴",
    ),
    "motor_mount": (
        "motor",
        "nema",
        "nema17",
        "stepper",
        "电机",
        "步进电机",
        "电机座",
    ),
    "link_arm": (
        "link",
        "arm",
        "gripper",
        "boss",
        "lever",
        "连杆",
        "连杆臂",
        "夹爪",
        "机械臂",
        "铰接",
    ),
    "pipe_manifold": (
        "pipe",
        "manifold",
        "sweep",
        "swept",
        "loft",
        "lofted",
        "duct",
        "tube",
        "runner",
        "flow",
        "routing",
        "inlet",
        "管",
        "管路",
        "管道",
        "歧管",
        "流道",
        "扫掠",
        "放样",
        "弯管",
    ),
    "assembly_mate": (
        "assembly",
        "component",
        "mate",
        "mates",
        "multipart",
        "multi_part",
        "cover",
        "cap",
        "base",
        "housing",
        "装配",
        "装配体",
        "组件",
        "配合",
        "多件",
        "底座",
        "端盖",
        "上盖",
    ),
    "bearing": (
        "bearing",
        "shaft",
        "seat",
        "end_cap",
        "spigot",
        "轴承",
        "轴承座",
        "轴孔",
        "轴承端盖",
        "止口",
    ),
    "plate_hole": (
        "plate",
        "baseplate",
        "mounting",
        "spacer",
        "slot",
        "slotted",
        "rail",
        "hole",
        "bore",
        "bolt",
        "pattern",
        "counterbore",
        "板",
        "底板",
        "安装板",
        "垫块",
        "垫片",
        "槽",
        "长圆孔",
        "腰形孔",
        "导轨",
        "孔",
        "螺栓",
    ),
    "thread_mirror": (
        "tap",
        "tapped",
        "thread",
        "threaded",
        "mirror",
        "symmetric",
        "螺纹",
        "攻丝",
        "镜像",
        "对称",
    ),
}

# Operator / KCL syntax patterns -> normalized operator tag
OPERATOR_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("op:shell", re.compile(r"\bshell\s*\(")),
    ("op:sweep", re.compile(r"\bsweep\s*\(")),
    ("op:loft", re.compile(r"\bloft\s*\(")),
    ("op:revolve", re.compile(r"\brevolve\s*\(")),
    ("op:extrude", re.compile(r"\bextrude\s*\(")),
    ("op:mirror", re.compile(r"\bmirror\s*\(")),
    ("op:box", re.compile(r"\bbox\s*\(")),
    ("op:cylinder", re.compile(r"\bcylinder\s*\(")),
    ("op:sketch", re.compile(r"\bsketch\s*\(")),
    ("op:fillet", re.compile(r"\bfillet\s*\(")),
    ("op:chamfer", re.compile(r"\bchamfer\s*\(")),
    ("op:tapped_hole", re.compile(r"\bhole::tapped\s*\(")),
    ("op:counterbore", re.compile(r"\bhole::counterbore\s*\(")),
    ("op:circular_pattern", re.compile(r"\bpattern::circular\s*\(")),
    ("op:linear_pattern", re.compile(r"\bpattern::linear\s*\(")),
    ("op:boolean_union", re.compile(r"\bboolean::union\s*\(")),
    ("op:boolean_subtract", re.compile(r"\bboolean::subtract\s*\(")),
    ("op:assembly", re.compile(r"\bassembly\s+[A-Za-z_]")),
    ("op:component", re.compile(r"\bcomponent\s*\(")),
    ("op:mate", re.compile(r"\bmate::[a-z_]+\s*\(|\bmate\s*\(")),
    ("op:pmi", re.compile(r"@pmi::[a-z_]+")),
)

# Archetype -> (preferred concepts, preferred operator tags, preferred tier substrings)
ARCHETYPE_HINTS: dict[str, tuple[tuple[str, ...], tuple[str, ...], tuple[str, ...]]] = {
    "shelled_box": (
        ("enclosure_box", "thread_mirror"),
        ("op:shell", "op:tapped_hole", "op:mirror"),
        ("tier3",),
    ),
    "enclosure": (
        ("enclosure_box",),
        ("op:shell",),
        ("tier3",),
    ),
    "swept_manifold": (
        ("pipe_manifold",),
        ("op:sweep", "op:loft"),
        ("tier3",),
    ),
    "manifold": (
        ("pipe_manifold",),
        ("op:sweep", "op:loft"),
        ("tier3",),
    ),
    "assembly": (
        ("assembly_mate", "bearing"),
        ("op:assembly", "op:component", "op:mate"),
        ("tier3",),
    ),
    "ribbed_bracket": (
        ("bracket_support", "l_sensor_bracket", "u_clevis", "motor_mount", "link_arm"),
        ("op:extrude", "op:sketch"),
        ("tier2",),
    ),
    "bracket": (
        ("bracket_support", "l_sensor_bracket", "u_clevis", "motor_mount"),
        ("op:extrude", "op:sketch"),
        ("tier2",),
    ),
    "flange": (
        ("flange_ring", "bearing"),
        ("op:revolve", "op:circular_pattern"),
        ("tier1",),
    ),
    "plate": (
        ("plate_hole",),
        ("op:box", "op:sketch"),
        ("tier1",),
    ),
    "parametric_part": (
        ("plate_hole", "flange_ring"),
        ("op:box", "op:cylinder"),
        ("tier1",),
    ),
}

_CAMEL_SPLIT_RE = re.compile(r"([a-z0-9])([A-Z])")
_EN_TOKEN_RE = re.compile(r"[a-z0-9]+")
_CJK_RUN_RE = re.compile(r"[\u4e00-\u9fff]+")
_STOPWORDS: frozenset[str] = frozenset(
    {
        "the",
        "and",
        "for",
        "with",
        "from",
        "into",
        "that",
        "this",
        "part",
        "solid",
        "length",
        "mm",
        "deg",
        "let",
        "return",
        "true",
        "false",
        "mvp",
        "tier",
        "golden",
        "一个",
        "设计",
        "生成",
        "制作",
        "需要",
        "使用",
        "带有",
    }
)


@dataclass(slots=True)
class RetrievedExample:
    """A single retrieved `.kcl` golden example with relevance score and metadata."""

    name: str
    path: Path
    tier: str
    score: float
    source: str
    tags: tuple[str, ...]


class ExampleRetriever(Protocol):
    """Pluggable protocol for retrieving few-shot `.kcl` golden examples."""

    def retrieve(
        self,
        query: str,
        *,
        archetype: str | None = None,
        top_k: int = 2,
        max_chars: int = 4500,
    ) -> list[RetrievedExample]:
        """Return up to `top_k` relevant golden examples respecting `max_chars`."""


@dataclass(slots=True)
class _IndexedExample:
    name: str
    path: Path
    tier: str
    source: str
    tags: tuple[str, ...]
    title_tokens: frozenset[str]
    body_tokens: frozenset[str]
    concepts: frozenset[str]
    op_tags: frozenset[str]


def _tokenize_text(text: str) -> set[str]:
    """Tokenize mixed Chinese/English text into lowercase lexical units."""
    split_camel = _CAMEL_SPLIT_RE.sub(r"\1 \2", text)
    lower = split_camel.lower()
    tokens: set[str] = set()

    for tok in _EN_TOKEN_RE.findall(lower):
        if len(tok) >= 2 and not tok.isdigit() and tok not in _STOPWORDS:
            tokens.add(tok)
        elif tok in {"l", "u", "t"}:
            # Preserve single-letter mechanical geometry prefixes (L-bracket, U-clevis, T-slot)
            tokens.add(tok)

    # Extract Chinese character bigrams and trigrams from CJK runs
    for run in _CJK_RUN_RE.findall(lower):
        n = len(run)
        if n == 1:
            tokens.add(run)
            continue
        for i in range(n - 1):
            bg = run[i : i + 2]
            if bg not in _STOPWORDS:
                tokens.add(bg)
        for i in range(n - 2):
            tokens.add(run[i : i + 3])

    return tokens


def _extract_concepts(text: str) -> set[str]:
    """Map mixed Chinese/English text to canonical mechanical concepts."""
    split_camel = _CAMEL_SPLIT_RE.sub(r"\1 \2", text)
    lower = split_camel.lower()
    en_words = set(_EN_TOKEN_RE.findall(lower))
    matched: set[str] = set()

    for concept, keywords in SYNONYM_GROUPS.items():
        for kw in keywords:
            if any("\u4e00" <= ch <= "\u9fff" for ch in kw):
                if kw in lower:
                    matched.add(concept)
                    break
            elif "_" in kw:
                if kw in lower or kw.replace("_", " ") in lower:
                    matched.add(concept)
                    break
            else:
                if kw in en_words:
                    matched.add(concept)
                    break
    return matched


def _extract_operator_tags(source: str) -> tuple[str, ...]:
    """Detect KCL modeling operators and annotations used in `.kcl` source."""
    found: list[str] = []
    for tag, pattern in OPERATOR_PATTERNS:
        if pattern.search(source):
            found.append(tag)
    return tuple(found)


def _extract_header_comments(source: str) -> str:
    """Extract leading `//` comments and `part`/`assembly` declaration names."""
    lines: list[str] = []
    for line in source.splitlines():
        stripped = line.strip()
        if stripped.startswith("//"):
            lines.append(stripped[2:].strip())
        elif stripped.startswith(("part ", "assembly ")):
            lines.append(stripped)
    return "\n".join(lines)


def _truncate_source(source: str, budget: int) -> str:
    """Truncate `.kcl` source cleanly on a line boundary to fit `budget` chars."""
    if budget <= 0:
        return ""
    if len(source) <= budget:
        return source
    marker = "\n// ... [truncated]\n"
    if budget <= len(marker):
        return source[:budget]
    usable = budget - len(marker)
    clipped = source[:usable]
    last_nl = clipped.rfind("\n")
    if last_nl > usable // 2:
        clipped = clipped[:last_nl]
    return clipped.rstrip() + marker


class LexicalExampleRetriever:
    """Zero-dependency lexical + bilingual synonym retriever over `examples/**/*.kcl`."""

    def __init__(
        self,
        workspace_root: Path | str | None = None,
        *,
        examples_dir: Path | str | None = None,
        include_phase2_hybrid: bool = False,
        skip_dirs: tuple[str, ...] | None = None,
    ) -> None:
        if examples_dir is not None:
            self.examples_dir = Path(examples_dir)
        else:
            root = Path(workspace_root) if workspace_root is not None else find_workspace_root()
            self.examples_dir = root / "examples"

        if skip_dirs is not None:
            self.skip_dirs = tuple(skip_dirs)
        elif include_phase2_hybrid:
            self.skip_dirs = ()
        else:
            self.skip_dirs = DEFAULT_SKIP_DIRS

        self._index: list[_IndexedExample] = self._build_index()

    @property
    def indexed_count(self) -> int:
        """Number of `.kcl` golden examples indexed."""
        return len(self._index)

    def _should_skip(self, rel_path: Path) -> bool:
        parts_lower = {p.lower() for p in rel_path.parts[:-1]}
        return any(skip.lower() in parts_lower for skip in self.skip_dirs)

    def _build_index(self) -> list[_IndexedExample]:
        if not self.examples_dir.is_dir():
            return []

        items: list[_IndexedExample] = []
        for path in sorted(self.examples_dir.rglob("*.kcl")):
            try:
                rel = path.relative_to(self.examples_dir)
            except ValueError:
                rel = Path(path.name)
            if self._should_skip(rel):
                continue

            try:
                source = path.read_text(encoding="utf-8")
            except OSError:
                continue

            tier = rel.parts[0] if len(rel.parts) > 1 else "root"
            name = path.stem
            header_text = _extract_header_comments(source)
            title_blob = f"{name} {tier} {header_text}"

            title_tokens = frozenset(_tokenize_text(title_blob))
            body_tokens = frozenset(_tokenize_text(source))
            concepts = frozenset(_extract_concepts(f"{title_blob}\n{source}"))
            op_tags = frozenset(_extract_operator_tags(source))

            # Build concise, deduplicated metadata tags
            ordered_tags: list[str] = [f"tier:{tier}"]
            ordered_tags.extend(sorted(concepts))
            ordered_tags.extend(sorted(op_tags))

            items.append(
                _IndexedExample(
                    name=name,
                    path=path,
                    tier=tier,
                    source=source,
                    tags=tuple(ordered_tags),
                    title_tokens=title_tokens,
                    body_tokens=body_tokens,
                    concepts=concepts,
                    op_tags=op_tags,
                )
            )
        return items

    def _score_example(
        self,
        item: _IndexedExample,
        *,
        query_tokens: set[str],
        query_concepts: set[str],
        archetype: str | None,
    ) -> float:
        score = 0.0

        # 1. Direct lexical token overlap (title/comments weighted higher than code body)
        title_hits = query_tokens & item.title_tokens
        body_hits = (query_tokens & item.body_tokens) - title_hits
        score += 3.0 * len(title_hits)
        score += 1.0 * len(body_hits)

        # 2. Canonical bilingual concept overlap
        concept_hits = query_concepts & item.concepts
        score += 4.5 * len(concept_hits)

        # 3. Specific operator alignment when query implies advanced operations
        if "enclosure_box" in query_concepts and "op:shell" in item.op_tags:
            score += 6.0
        if "pipe_manifold" in query_concepts and (
            "op:sweep" in item.op_tags or "op:loft" in item.op_tags
        ):
            score += 6.0
        if "assembly_mate" in query_concepts and "op:assembly" in item.op_tags:
            score += 8.0
        if "l_sensor_bracket" in query_concepts and "l_sensor_bracket" in item.concepts:
            score += 4.0
        if "u_clevis" in query_concepts and "u_clevis" in item.concepts:
            score += 4.0
        if "motor_mount" in query_concepts and "motor_mount" in item.concepts:
            score += 4.0
        if "link_arm" in query_concepts and "link_arm" in item.concepts:
            score += 4.0

        # 4. Archetype weighting
        if archetype:
            norm_arch = archetype.strip().lower()
            hints = ARCHETYPE_HINTS.get(norm_arch)
            if hints is not None:
                pref_concepts, pref_ops, pref_tiers = hints
                if any(c in item.concepts for c in pref_concepts):
                    score += 4.0
                op_matches = sum(1 for op in pref_ops if op in item.op_tags)
                score += 2.5 * op_matches
                if any(t in item.tier.lower() for t in pref_tiers):
                    score += 1.5
            else:
                # Fallback lexical match on custom archetype string
                arch_tokens = _tokenize_text(norm_arch)
                score += 2.0 * len(arch_tokens & item.title_tokens)

        return round(score, 4)

    def retrieve(
        self,
        query: str,
        *,
        archetype: str | None = None,
        top_k: int = 2,
        max_chars: int = 4500,
    ) -> list[RetrievedExample]:
        if top_k <= 0 or max_chars <= 0 or not self._index:
            return []

        query_tokens = _tokenize_text(query)
        query_concepts = _extract_concepts(query)

        scored: list[tuple[float, _IndexedExample]] = []
        for item in self._index:
            s = self._score_example(
                item,
                query_tokens=query_tokens,
                query_concepts=query_concepts,
                archetype=archetype,
            )
            scored.append((s, item))

        # Sort by descending score, then deterministic path order
        scored.sort(key=lambda pair: (-pair[0], str(pair[1].path)))

        results: list[RetrievedExample] = []
        remaining = max_chars

        for score_val, item in scored[:top_k]:
            src = item.source
            if remaining <= 0:
                break
            if len(src) > remaining:
                if not results:
                    src = _truncate_source(src, remaining)
                elif remaining >= 200:
                    src = _truncate_source(src, remaining)
                else:
                    break
            remaining -= len(src)

            results.append(
                RetrievedExample(
                    name=item.name,
                    path=item.path,
                    tier=item.tier,
                    score=score_val,
                    source=src,
                    tags=item.tags,
                )
            )

        return results


def format_few_shot_block(
    examples: list[RetrievedExample],
    *,
    max_chars: int = 4500,
) -> str:
    """Format retrieved `.kcl` examples into a compact few-shot prompt block within `max_chars`."""
    if not examples or max_chars <= 0:
        return ""

    header = "### Golden `.kcl` Reference Examples\n"
    if len(header) >= max_chars:
        return header[:max_chars]

    parts: list[str] = [header]
    used = len(header)

    for idx, ex in enumerate(examples, start=1):
        tags_preview = ", ".join(ex.tags[:6])
        section_prefix = (
            f"\n#### Example {idx}: {ex.name} (tier={ex.tier}, score={ex.score:.2f}"
            + (f", tags={tags_preview}" if tags_preview else "")
            + ")\n```kcl\n"
        )
        section_suffix = "\n```\n"
        overhead = len(section_prefix) + len(section_suffix)
        remaining = max_chars - used

        if remaining <= overhead + 20:
            break

        code_budget = remaining - overhead
        code = ex.source.strip()
        if len(code) > code_budget:
            code = _truncate_source(code, code_budget).rstrip()

        block = f"{section_prefix}{code}{section_suffix}"
        if used + len(block) > max_chars:
            # Final safety clamp
            allowed_code = max_chars - used - overhead
            if allowed_code <= 0:
                break
            code = _truncate_source(code, allowed_code).rstrip()
            block = f"{section_prefix}{code}{section_suffix}"
            if used + len(block) > max_chars:
                break

        parts.append(block)
        used += len(block)

    result = "".join(parts)
    if len(result) > max_chars:
        return result[:max_chars]
    return result
