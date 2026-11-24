"""Pluggable VLM multimodal 4-view visual inspection module for `cad_agent`."""

from __future__ import annotations

import base64
import json
import math
import os
import re
import struct
import xml.etree.ElementTree as ET
import zlib
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from collections.abc import Mapping
from typing import Any, Callable, Protocol, Sequence

from . import observe

from .critic import HeuristicViewInspector, OrthogonalViewInspector
from .compiler_bridge import (
    ORTHOGONAL_VIEW_NAMES,
    ShadedViewsBundle,
    _extract_view_legend_payload,
    _normalize_view_stem,
    load_semantic_legend,
    load_semantic_mask,
    load_shaded_views,
)
from .contracts import (
    VERIFICATION_LAYERS,
    CompileResult,
    DesignContract,
    VerificationCheck,
    VerificationReport,
)
from .llm import LLMClient

EXPECTED_VIEWS: tuple[str, ...] = tuple(f"{name}.svg" for name in ORTHOGONAL_VIEW_NAMES)

_SVG_GEOMETRY_TAGS: frozenset[str] = frozenset(
    {"polygon", "polyline", "path", "rect", "circle", "ellipse", "line"}
)
_SECRET_PATTERNS: tuple[re.Pattern[str], ...] = (
    re.compile(r"sk-[A-Za-z0-9_\-]{8,}"),
    re.compile(r"AIza[0-9A-Za-z_\-]{16,}"),
    re.compile(r"(Bearer\s+)[A-Za-z0-9._\-]{8,}", re.IGNORECASE),
    re.compile(r"(api[_-]?key\s*[=:]\s*)[^\s,;\"']+", re.IGNORECASE),
)


class MultimodalLLMClient(Protocol):
    """Optional extension protocol for LLM clients that accept multimodal image blocks."""

    def complete_multimodal(
        self,
        system_prompt: str,
        user_prompt: str,
        images: list[Any],
    ) -> str:
        """Return model completion text given prompts and base64 image blocks."""


@dataclass(slots=True)
class MultimodalImageBlock:
    """Base64-encoded image attachment for multimodal VLM clients."""

    name: str
    mime_type: str
    data_base64: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "mime_type": self.mime_type,
            "data_base64": self.data_base64,
        }

    def __getitem__(self, key: str) -> Any:
        return self.to_dict()[key]

    def get(self, key: str, default: Any = None) -> Any:
        return self.to_dict().get(key, default)


@dataclass(slots=True)
class ViewProjectionSummary:
    """Compact structural & geometric summary extracted from a single SVG view."""

    view_name: str
    width: float | None = None
    height: float | None = None
    view_box: str | None = None
    element_counts: dict[str, int] = field(default_factory=dict)
    total_geometry_elements: int = 0
    projected_bbox: tuple[float, float, float, float] | None = None
    labels: list[str] = field(default_factory=list)
    size_bytes: int = 0

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "view_name": self.view_name,
            "width": self.width,
            "height": self.height,
            "view_box": self.view_box,
            "element_counts": dict(self.element_counts),
            "total_geometry_elements": self.total_geometry_elements,
            "size_bytes": self.size_bytes,
        }
        if self.projected_bbox is not None:
            payload["projected_bbox"] = [round(v, 2) for v in self.projected_bbox]
        if self.labels:
            payload["labels"] = list(self.labels)
        return payload


@dataclass(slots=True)
class VisualReviewFinding:
    """Single structured finding produced by baseline or VLM visual inspection."""

    view: str
    message: str
    severity: str = "error"
    category: str = "geometry"
    point_norm: list[float] | None = None
    bbox_norm: list[float] | None = None
    component_id: str | None = None
    face_id: str | None = None
    ast_span: dict[str, int] | None = None
    tags: list[str] = field(default_factory=list)

    def format_location(self) -> str:
        """Render non-empty semantic localization context for downstream repair prompts."""
        parts: list[str] = []
        if self.component_id:
            parts.append(f"component_id={self.component_id}")
        if self.face_id:
            parts.append(f"face_id={self.face_id}")
        if self.ast_span:
            sl = self.ast_span.get("start_line")
            sc = self.ast_span.get("start_col")
            el = self.ast_span.get("end_line")
            ec = self.ast_span.get("end_col")
            if sl is not None and el is not None:
                if sc is not None and ec is not None:
                    parts.append(f"ast_span=L{sl}:C{sc}-L{el}:C{ec}")
                else:
                    parts.append(f"ast_span=L{sl}-L{el}")
        if self.tags:
            parts.append(f"tags={','.join(self.tags)}")
        return ", ".join(parts)

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "view": self.view,
            "severity": self.severity,
            "category": self.category,
            "message": self.message,
        }
        if self.point_norm is not None:
            payload["point_norm"] = list(self.point_norm)
        if self.bbox_norm is not None:
            payload["bbox_norm"] = list(self.bbox_norm)
        if self.component_id is not None:
            payload["component_id"] = self.component_id
        if self.face_id is not None:
            payload["face_id"] = self.face_id
        if self.ast_span is not None:
            payload["ast_span"] = dict(self.ast_span)
        if self.tags:
            payload["tags"] = list(self.tags)
        return payload


@dataclass(slots=True)
class SemanticLookupHit:
    """Deterministic semantic target resolved from a mask PNG and legend JSON."""

    view: str
    color: str
    face_id: str
    component_id: str | None = None
    instance_index: int = 0
    is_edge: bool = False
    ast_span: dict[str, int] | None = None
    tags: list[str] = field(default_factory=list)
    pixel_count: int = 1
    coverage: float = 1.0

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "view": self.view,
            "color": self.color,
            "face_id": self.face_id,
            "instance_index": self.instance_index,
            "is_edge": self.is_edge,
            "pixel_count": self.pixel_count,
            "coverage": round(self.coverage, 4),
        }
        if self.component_id is not None:
            payload["component_id"] = self.component_id
        if self.ast_span is not None:
            payload["ast_span"] = dict(self.ast_span)
        if self.tags:
            payload["tags"] = list(self.tags)
        return payload


@dataclass(slots=True)
class VisualReviewResult:
    """Detailed outcome of 4-view orthogonal + optional VLM visual inspection."""

    passed: bool
    summary: str
    issues: list[str] = field(default_factory=list)
    used_vlm: bool = False
    views_checked: list[str] = field(default_factory=list)
    findings: list[VisualReviewFinding] = field(default_factory=list)
    fallback_reason: str | None = None

    def to_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "passed": self.passed,
            "summary": self.summary,
            "issues": list(self.issues),
            "used_vlm": self.used_vlm,
            "views_checked": list(self.views_checked),
            "findings": [f.to_dict() for f in self.findings],
        }
        if self.fallback_reason is not None:
            payload["fallback_reason"] = self.fallback_reason
        return payload


def _redact_secrets(text: str, llm_client: Any = None) -> str:
    """Strip API keys or bearer tokens from error messages before surfacing them."""
    sanitized = text
    candidate_secrets: list[str] = []
    if llm_client is not None:
        client_key = getattr(llm_client, "api_key", None)
        if isinstance(client_key, str) and client_key.strip():
            candidate_secrets.append(client_key.strip())
    for env_name in ("OPENAI_API_KEY", "GEMINI_API_KEY"):
        env_val = os.environ.get(env_name, "").strip()
        if env_val:
            candidate_secrets.append(env_val)

    for secret in candidate_secrets:
        sanitized = sanitized.replace(secret, "***")

    for pattern in _SECRET_PATTERNS:
        if pattern.groups:
            sanitized = pattern.sub(r"\1***", sanitized)
        else:
            sanitized = pattern.sub("***", sanitized)
    return sanitized


def _parse_float_attr(raw: str | None) -> float | None:
    if not raw:
        return None
    match = re.match(r"^\s*([+-]?\d+(?:\.\d+)?)", raw)
    if not match:
        return None
    try:
        return float(match.group(1))
    except ValueError:
        return None


def _local_tag(tag: str) -> str:
    if "}" in tag:
        return tag.rsplit("}", 1)[-1].lower()
    return tag.lower()


def _is_valid_svg_document(svg_text: str) -> bool:
    """Return True only when `svg_text` is a valid SVG document with an `<svg>` root."""
    if "<svg" not in svg_text.lower():
        return False
    try:
        root = ET.fromstring(svg_text)
        return _local_tag(root.tag) == "svg"
    except ET.ParseError:
        return bool(re.search(r"<(?:[A-Za-z_][\w.-]*:)?svg\b", svg_text, re.IGNORECASE))


def summarize_svg_view(view_name: str, svg_text: str) -> ViewProjectionSummary:
    """Extract compact geometric and structural metadata from an orthogonal SVG view."""
    size_bytes = len(svg_text.encode("utf-8", errors="ignore"))
    counts: dict[str, int] = {}
    labels: list[str] = []
    xs: list[float] = []
    ys: list[float] = []
    width: float | None = None
    height: float | None = None
    view_box: str | None = None

    try:
        root = ET.fromstring(svg_text)
        if _local_tag(root.tag) != "svg":
            return ViewProjectionSummary(
                view_name=view_name,
                size_bytes=size_bytes,
            )
        width = _parse_float_attr(root.attrib.get("width"))
        height = _parse_float_attr(root.attrib.get("height"))
        view_box = root.attrib.get("viewBox")

        for elem in root.iter():
            tag = _local_tag(elem.tag)
            if tag in _SVG_GEOMETRY_TAGS:
                counts[tag] = counts.get(tag, 0) + 1
                if tag in ("polygon", "polyline"):
                    pts_attr = elem.attrib.get("points", "")
                    for mx, my in re.findall(
                        r"([+-]?\d+(?:\.\d+)?)\s*,\s*([+-]?\d+(?:\.\d+)?)", pts_attr
                    ):
                        xs.append(float(mx))
                        ys.append(float(my))
                elif tag == "rect":
                    # Ignore full-canvas background rect (width="100%")
                    w_raw = elem.attrib.get("width", "")
                    h_raw = elem.attrib.get("height", "")
                    if "%" not in w_raw and "%" not in h_raw:
                        rx = _parse_float_attr(elem.attrib.get("x")) or 0.0
                        ry = _parse_float_attr(elem.attrib.get("y")) or 0.0
                        rw = _parse_float_attr(w_raw)
                        rh = _parse_float_attr(h_raw)
                        if rw is not None and rh is not None:
                            xs.extend((rx, rx + rw))
                            ys.extend((ry, ry + rh))
                elif tag == "circle":
                    cx = _parse_float_attr(elem.attrib.get("cx"))
                    cy = _parse_float_attr(elem.attrib.get("cy"))
                    r = _parse_float_attr(elem.attrib.get("r"))
                    if cx is not None and cy is not None and r is not None:
                        xs.extend((cx - r, cx + r))
                        ys.extend((cy - r, cy + r))
                elif tag == "line":
                    for attr_x in ("x1", "x2"):
                        val = _parse_float_attr(elem.attrib.get(attr_x))
                        if val is not None:
                            xs.append(val)
                    for attr_y in ("y1", "y2"):
                        val = _parse_float_attr(elem.attrib.get(attr_y))
                        if val is not None:
                            ys.append(val)
            elif tag == "text":
                counts["text"] = counts.get("text", 0) + 1
                text_content = "".join(elem.itertext()).strip()
                if text_content and len(labels) < 6:
                    labels.append(text_content[:120])
    except ET.ParseError:
        # Fallback regex summary if SVG contains non-strict XML entities
        vb_match = re.search(r'viewBox\s*=\s*"([^"]+)"', svg_text)
        if vb_match:
            view_box = vb_match.group(1)
        w_match = re.search(r'\bwidth\s*=\s*"([^"]+)"', svg_text)
        h_match = re.search(r'\bheight\s*=\s*"([^"]+)"', svg_text)
        width = _parse_float_attr(w_match.group(1)) if w_match else None
        height = _parse_float_attr(h_match.group(1)) if h_match else None
        for tag in sorted(_SVG_GEOMETRY_TAGS):
            c = len(re.findall(rf"<{tag}\b", svg_text, re.IGNORECASE))
            if c:
                counts[tag] = c

    total_geom = sum(v for k, v in counts.items() if k in _SVG_GEOMETRY_TAGS)
    projected_bbox = (min(xs), min(ys), max(xs), max(ys)) if xs and ys else None
    return ViewProjectionSummary(
        view_name=view_name,
        width=width,
        height=height,
        view_box=view_box,
        element_counts=counts,
        total_geometry_elements=total_geom,
        projected_bbox=projected_bbox,
        labels=labels,
        size_bytes=size_bytes,
    )


def _parse_svg_path_polylines(d_attr: str) -> list[list[tuple[float, float]]]:
    """Parse an SVG `<path d="...">` attribute into coordinate polylines."""
    tokens = re.findall(r"[A-Za-z]|[+-]?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?", d_attr)
    polylines: list[list[tuple[float, float]]] = []
    current: list[tuple[float, float]] = []
    cx, cy = 0.0, 0.0
    start_x, start_y = 0.0, 0.0
    cmd = ""
    idx = 0

    def _next_float() -> float | None:
        nonlocal idx
        if idx >= len(tokens) or tokens[idx].isalpha():
            return None
        val = float(tokens[idx])
        idx += 1
        return val

    while idx < len(tokens):
        tok = tokens[idx]
        if tok.isalpha():
            cmd = tok
            idx += 1
            if cmd in ("Z", "z"):
                if current:
                    current.append((start_x, start_y))
                    polylines.append(current)
                    current = []
                cx, cy = start_x, start_y
                continue

        if cmd in ("M", "m"):
            x = _next_float()
            y = _next_float()
            if x is None or y is None:
                break
            if cmd == "m":
                cx += x
                cy += y
            else:
                cx, cy = x, y
            if len(current) >= 2:
                polylines.append(current)
            start_x, start_y = cx, cy
            current = [(cx, cy)]
            cmd = "l" if cmd == "m" else "L"
        elif cmd in ("L", "l"):
            x = _next_float()
            y = _next_float()
            if x is None or y is None:
                break
            if cmd == "l":
                cx += x
                cy += y
            else:
                cx, cy = x, y
            if not current:
                current = [(start_x, start_y)]
            current.append((cx, cy))
        elif cmd in ("H", "h"):
            x = _next_float()
            if x is None:
                break
            cx = (cx + x) if cmd == "h" else x
            if not current:
                current = [(start_x, start_y)]
            current.append((cx, cy))
        elif cmd in ("V", "v"):
            y = _next_float()
            if y is None:
                break
            cy = (cy + y) if cmd == "v" else y
            if not current:
                current = [(start_x, start_y)]
            current.append((cx, cy))
        elif cmd in ("C", "c"):
            vals = [_next_float() for _ in range(6)]
            if any(v is None for v in vals):
                break
            x1, y1, x2, y2, x, y = (float(v) for v in vals if v is not None)
            if cmd == "c":
                x1, y1 = cx + x1, cy + y1
                x2, y2 = cx + x2, cy + y2
                x, y = cx + x, cy + y
            if not current:
                current = [(cx, cy)]
            p0x, p0y = cx, cy
            for step in range(1, 9):
                t = step / 8.0
                mt = 1.0 - t
                bx = mt**3 * p0x + 3 * mt**2 * t * x1 + 3 * mt * t**2 * x2 + t**3 * x
                by = mt**3 * p0y + 3 * mt**2 * t * y1 + 3 * mt * t**2 * y2 + t**3 * y
                current.append((bx, by))
            cx, cy = x, y
        elif cmd in ("Q", "q"):
            vals = [_next_float() for _ in range(4)]
            if any(v is None for v in vals):
                break
            x1, y1, x, y = (float(v) for v in vals if v is not None)
            if cmd == "q":
                x1, y1 = cx + x1, cy + y1
                x, y = cx + x, cy + y
            if not current:
                current = [(cx, cy)]
            p0x, p0y = cx, cy
            for step in range(1, 9):
                t = step / 8.0
                mt = 1.0 - t
                bx = mt**2 * p0x + 2 * mt * t * x1 + t**2 * x
                by = mt**2 * p0y + 2 * mt * t * y1 + t**2 * y
                current.append((bx, by))
            cx, cy = x, y
        elif cmd in ("A", "a"):
            vals = [_next_float() for _ in range(7)]
            if any(v is None for v in vals):
                break
            x = float(vals[5])  # type: ignore[arg-type]
            y = float(vals[6])  # type: ignore[arg-type]
            if cmd == "a":
                cx += x
                cy += y
            else:
                cx, cy = x, y
            if not current:
                current = [(start_x, start_y)]
            current.append((cx, cy))
        else:
            idx += 1

    if len(current) >= 2:
        polylines.append(current)
    return polylines


def _png_chunk(chunk_type: bytes, data: bytes) -> bytes:
    """Pack a single PNG chunk with length, type, payload, and CRC32."""
    crc = zlib.crc32(chunk_type + data) & 0xFFFFFFFF
    return struct.pack(">I", len(data)) + chunk_type + data + struct.pack(">I", crc)


def render_svg_to_png_base64(svg_text: str, width: int = 240, height: int = 240) -> str:
    """Zero-dependency pure-Python SVG wireframe rasterizer producing a base64 PNG image."""
    w = max(8, int(width))
    h = max(8, int(height))

    vb_min_x = 0.0
    vb_min_y = 0.0
    vb_w: float | None = None
    vb_h: float | None = None
    polylines: list[list[tuple[float, float]]] = []

    try:
        root = ET.fromstring(svg_text)
        if _local_tag(root.tag) == "svg":
            vb_raw = root.attrib.get("viewBox", "")
            vb_nums = [
                float(v)
                for v in re.findall(r"[+-]?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?", vb_raw)
            ]
            if len(vb_nums) == 4 and vb_nums[2] > 0 and vb_nums[3] > 0:
                vb_min_x, vb_min_y, vb_w, vb_h = vb_nums
            else:
                vb_w = _parse_float_attr(root.attrib.get("width"))
                vb_h = _parse_float_attr(root.attrib.get("height"))

            for elem in root.iter():
                tag = _local_tag(elem.tag)
                if tag == "line":
                    x1 = _parse_float_attr(elem.attrib.get("x1"))
                    y1 = _parse_float_attr(elem.attrib.get("y1"))
                    x2 = _parse_float_attr(elem.attrib.get("x2"))
                    y2 = _parse_float_attr(elem.attrib.get("y2"))
                    if None not in (x1, y1, x2, y2):
                        polylines.append([(float(x1), float(y1)), (float(x2), float(y2))])  # type: ignore[arg-type]
                elif tag in ("polyline", "polygon"):
                    nums = [
                        float(v)
                        for v in re.findall(
                            r"[+-]?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?",
                            elem.attrib.get("points", ""),
                        )
                    ]
                    pts = [(nums[i], nums[i + 1]) for i in range(0, len(nums) - 1, 2)]
                    if len(pts) >= 2:
                        if tag == "polygon" and pts[0] != pts[-1]:
                            pts.append(pts[0])
                        polylines.append(pts)
                elif tag == "rect":
                    w_raw = elem.attrib.get("width", "")
                    h_raw = elem.attrib.get("height", "")
                    if "%" not in w_raw and "%" not in h_raw:
                        rx = _parse_float_attr(elem.attrib.get("x")) or 0.0
                        ry = _parse_float_attr(elem.attrib.get("y")) or 0.0
                        rw = _parse_float_attr(w_raw)
                        rh = _parse_float_attr(h_raw)
                        if rw is not None and rh is not None and rw > 0 and rh > 0:
                            polylines.append(
                                [
                                    (rx, ry),
                                    (rx + rw, ry),
                                    (rx + rw, ry + rh),
                                    (rx, ry + rh),
                                    (rx, ry),
                                ]
                            )
                elif tag == "circle":
                    cx = _parse_float_attr(elem.attrib.get("cx")) or 0.0
                    cy = _parse_float_attr(elem.attrib.get("cy")) or 0.0
                    r = _parse_float_attr(elem.attrib.get("r"))
                    if r is not None and r > 0:
                        steps = 28
                        polylines.append(
                            [
                                (
                                    cx + r * math.cos(2.0 * math.pi * i / steps),
                                    cy + r * math.sin(2.0 * math.pi * i / steps),
                                )
                                for i in range(steps + 1)
                            ]
                        )
                elif tag == "ellipse":
                    cx = _parse_float_attr(elem.attrib.get("cx")) or 0.0
                    cy = _parse_float_attr(elem.attrib.get("cy")) or 0.0
                    rx = _parse_float_attr(elem.attrib.get("rx"))
                    ry = _parse_float_attr(elem.attrib.get("ry"))
                    if rx is not None and ry is not None and rx > 0 and ry > 0:
                        steps = 28
                        polylines.append(
                            [
                                (
                                    cx + rx * math.cos(2.0 * math.pi * i / steps),
                                    cy + ry * math.sin(2.0 * math.pi * i / steps),
                                )
                                for i in range(steps + 1)
                            ]
                        )
                elif tag == "path":
                    d_attr = elem.attrib.get("d", "")
                    if d_attr:
                        polylines.extend(_parse_svg_path_polylines(d_attr))
    except ET.ParseError:
        pass

    if vb_w is None or vb_h is None or vb_w <= 0 or vb_h <= 0:
        all_pts = [pt for poly in polylines for pt in poly]
        if all_pts:
            min_x = min(p[0] for p in all_pts)
            min_y = min(p[1] for p in all_pts)
            max_x = max(p[0] for p in all_pts)
            max_y = max(p[1] for p in all_pts)
            vb_min_x = min_x
            vb_min_y = min_y
            vb_w = max(max_x - min_x, 1.0)
            vb_h = max(max_y - min_y, 1.0)
        else:
            vb_w = float(w)
            vb_h = float(h)

    # Dark blueprint background RGB(15, 23, 42) + bright cyan-white wireframe RGB(226, 232, 240)
    bg_rgb = bytes((15, 23, 42))
    fg_rgb = (226, 232, 240)
    canvas = bytearray(bg_rgb * (w * h))

    def _set_pixel(px: int, py: int) -> None:
        if 0 <= px < w and 0 <= py < h:
            offset = (py * w + px) * 3
            canvas[offset] = fg_rgb[0]
            canvas[offset + 1] = fg_rgb[1]
            canvas[offset + 2] = fg_rgb[2]

    def _draw_line(x0: int, y0: int, x1: int, y1: int) -> None:
        dx = abs(x1 - x0)
        dy = -abs(y1 - y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        cx_i, cy_i = x0, y0
        while True:
            _set_pixel(cx_i, cy_i)
            if cx_i == x1 and cy_i == y1:
                break
            e2 = 2 * err
            if e2 >= dy:
                err += dy
                cx_i += sx
            if e2 <= dx:
                err += dx
                cy_i += sy

    def _to_px(x: float, y: float) -> tuple[int, int]:
        px = int(round((x - vb_min_x) / float(vb_w) * (w - 1)))
        py = int(round((y - vb_min_y) / float(vb_h) * (h - 1)))
        return (max(0, min(w - 1, px)), max(0, min(h - 1, py)))

    for poly in polylines:
        for i in range(len(poly) - 1):
            x0, y0 = _to_px(poly[i][0], poly[i][1])
            x1, y1 = _to_px(poly[i + 1][0], poly[i + 1][1])
            _draw_line(x0, y0, x1, y1)

    row_stride = w * 3
    raw_scanlines = bytearray((row_stride + 1) * h)
    for row in range(h):
        dst_off = row * (row_stride + 1)
        src_off = row * row_stride
        raw_scanlines[dst_off] = 0  # PNG filter type 0 (None)
        raw_scanlines[dst_off + 1 : dst_off + 1 + row_stride] = canvas[
            src_off : src_off + row_stride
        ]

    ihdr_data = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    idat_data = zlib.compress(bytes(raw_scanlines), level=6)
    png_bytes = (
        b"\x89PNG\r\n\x1a\n"
        + _png_chunk(b"IHDR", ihdr_data)
        + _png_chunk(b"IDAT", idat_data)
        + _png_chunk(b"IEND", b"")
    )
    return base64.b64encode(png_bytes).decode("ascii")


def summarize_mesh_payload(mesh_payload: dict[str, Any] | None) -> dict[str, Any] | None:
    """Build a compact, token-efficient summary of a scene-mesh JSON payload."""
    if not isinstance(mesh_payload, dict):
        return None
    faces = [f for f in mesh_payload.get("faces", []) if isinstance(f, dict)]
    edges = [e for e in mesh_payload.get("edges", []) if isinstance(e, dict)]
    handles = [h for h in mesh_payload.get("edit_handles", []) if isinstance(h, dict)]
    components = [c for c in mesh_payload.get("components", []) if isinstance(c, dict)]

    surface_types: dict[str, int] = {}
    tags: set[str] = set()
    exact_interface_faces = 0
    for face in faces:
        st = str(face.get("surface_type", "unknown"))
        surface_types[st] = surface_types.get(st, 0) + 1
        if face.get("exact_interface"):
            exact_interface_faces += 1
        for tag in face.get("provenance_tags", []):
            tags.add(str(tag))

    return {
        "format_version": mesh_payload.get("format_version"),
        "part_name": mesh_payload.get("part_name"),
        "kind": mesh_payload.get("kind"),
        "units": mesh_payload.get("units"),
        "face_count": len(faces),
        "edge_count": len(edges),
        "edit_handle_count": len(handles),
        "component_count": len(components),
        "exact_interface_faces": exact_interface_faces,
        "surface_types": surface_types,
        "provenance_tags": sorted(tags)[:20],
    }


_PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
MAX_PNG_DIMENSION = 8192
MAX_PNG_PIXELS = 16_777_216
MAX_PNG_INPUT_BYTES = 32 * 1024 * 1024


def _paeth_predictor(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa = abs(p - a)
    pb = abs(p - b)
    pc = abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


@dataclass(slots=True)
class DecodedPngImage:
    """Decoded 8-bit RGB or RGBA PNG pixel buffer."""

    width: int
    height: int
    channels: int
    pixels: bytes

    def pixel_rgb(self, x: int, y: int) -> tuple[int, int, int]:
        if not (0 <= x < self.width and 0 <= y < self.height):
            raise IndexError(f"Pixel ({x}, {y}) out of bounds for {self.width}x{self.height}")
        offset = (y * self.width + x) * self.channels
        return (self.pixels[offset], self.pixels[offset + 1], self.pixels[offset + 2])

    def pixel_rgba(self, x: int, y: int) -> tuple[int, int, int, int]:
        r, g, b = self.pixel_rgb(x, y)
        if self.channels == 4:
            offset = (y * self.width + x) * 4
            return (r, g, b, self.pixels[offset + 3])
        return (r, g, b, 255)

    def rgb_distribution(
        self,
        *,
        point_norm: Sequence[float] | None = None,
        bbox_norm: Sequence[float] | None = None,
    ) -> dict[tuple[int, int, int], int]:
        """Sample RGB counts at a normalized `[x, y]` point or `[x1, y1, x2, y2]` box."""
        if point_norm is not None:
            nx, ny = _validate_norm_point(point_norm)
            px = min(self.width - 1, max(0, int(math.floor(nx * self.width))))
            py = min(self.height - 1, max(0, int(math.floor(ny * self.height))))
            return {self.pixel_rgb(px, py): 1}

        if bbox_norm is not None:
            x1, y1, x2, y2 = _validate_norm_bbox(bbox_norm)
            x_start = min(self.width - 1, max(0, int(math.floor(x1 * self.width))))
            y_start = min(self.height - 1, max(0, int(math.floor(y1 * self.height))))
            x_end = min(self.width - 1, max(x_start, int(math.ceil(x2 * self.width)) - 1))
            y_end = min(self.height - 1, max(y_start, int(math.ceil(y2 * self.height)) - 1))
            counts: Counter[tuple[int, int, int]] = Counter()
            for py in range(y_start, y_end + 1):
                row_base = py * self.width * self.channels
                for px in range(x_start, x_end + 1):
                    off = row_base + px * self.channels
                    counts[(self.pixels[off], self.pixels[off + 1], self.pixels[off + 2])] += 1
            return dict(counts)

        raise ValueError("Either point_norm or bbox_norm must be provided")


def decode_png(data: bytes) -> DecodedPngImage:
    """Zero-dependency pure-Python PNG decoder supporting 8-bit RGB/RGBA and filters 0..4."""
    if len(data) > MAX_PNG_INPUT_BYTES:
        raise ValueError(
            f"PNG input size {len(data)} exceeds limit {MAX_PNG_INPUT_BYTES} bytes"
        )
    if not data.startswith(_PNG_SIGNATURE):
        raise ValueError("Invalid PNG signature")

    offset = len(_PNG_SIGNATURE)
    width = 0
    height = 0
    channels = 0
    saw_ihdr = False
    saw_iend = False
    idat_chunks: list[bytes] = []
    total_idat_bytes = 0

    while offset + 12 <= len(data):
        length, chunk_type = struct.unpack_from(">I4s", data, offset)
        payload_start = offset + 8
        payload_end = payload_start + length
        if payload_end + 4 > len(data):
            raise ValueError("Truncated PNG chunk")
        (expected_crc,) = struct.unpack_from(">I", data, payload_end)
        actual_crc = zlib.crc32(memoryview(data)[offset + 4 : payload_end]) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise ValueError(f"PNG CRC mismatch in chunk {chunk_type!r}")
        payload = data[payload_start:payload_end]

        if not saw_ihdr:
            if chunk_type != b"IHDR" or length != 13:
                raise ValueError("PNG must start with valid 13-byte IHDR chunk")
            width, height, bit_depth, color_type, compression, filter_method, interlace = (
                struct.unpack(">IIBBBBB", payload)
            )
            if width == 0 or height == 0:
                raise ValueError("PNG dimensions must be positive")
            if width > MAX_PNG_DIMENSION or height > MAX_PNG_DIMENSION:
                raise ValueError(
                    f"PNG dimensions {width}x{height} exceed max {MAX_PNG_DIMENSION}"
                )
            if width * height > MAX_PNG_PIXELS:
                raise ValueError(
                    f"PNG pixel count {width * height} exceeds max {MAX_PNG_PIXELS}"
                )
            if bit_depth != 8 or color_type not in (2, 6):
                raise ValueError(
                    f"Unsupported PNG format (bit_depth={bit_depth}, color_type={color_type}); "
                    "only 8-bit RGB/RGBA supported"
                )
            if compression != 0 or filter_method != 0 or interlace != 0:
                raise ValueError("Unsupported PNG compression, filter method, or interlacing")
            channels = 3 if color_type == 2 else 4
            saw_ihdr = True
        elif chunk_type == b"IHDR":
            raise ValueError("Duplicate PNG IHDR chunk")
        elif chunk_type == b"IDAT":
            total_idat_bytes += length
            if total_idat_bytes > MAX_PNG_INPUT_BYTES:
                raise ValueError("PNG IDAT payload exceeds maximum input size")
            idat_chunks.append(payload)
        elif chunk_type == b"IEND":
            if length != 0:
                raise ValueError("Non-empty PNG IEND chunk")
            saw_iend = True
            offset = payload_end + 4
            break

        offset = payload_end + 4

    if not saw_ihdr or not saw_iend or not idat_chunks:
        raise ValueError("Incomplete PNG stream (missing IHDR, IDAT, or IEND)")

    row_bytes = width * channels
    expected_len = height * (1 + row_bytes)
    decompressor = zlib.decompressobj()
    try:
        raw = decompressor.decompress(b"".join(idat_chunks), expected_len + 1)
        if len(raw) > expected_len or decompressor.unconsumed_tail:
            raise ValueError(
                f"PNG IDAT stream exceeds expected scanline buffer size ({expected_len})"
            )
        if not decompressor.eof:
            tail = decompressor.flush()
            if tail:
                raw += tail
    except zlib.error as exc:
        raise ValueError(f"Corrupt PNG IDAT zlib stream: {exc}") from exc

    if not decompressor.eof or len(raw) != expected_len:
        raise ValueError(f"Unexpected PNG scanline buffer size: {len(raw)} != {expected_len}")

    out = bytearray(width * height * channels)
    bpp = channels
    prev_row = bytes(row_bytes)

    for y in range(height):
        src_start = y * (1 + row_bytes)
        ftype = raw[src_start]
        filt = raw[src_start + 1 : src_start + 1 + row_bytes]
        recon = bytearray(row_bytes)

        if ftype == 0:
            recon[:] = filt
        elif ftype == 1:
            for x in range(row_bytes):
                left = recon[x - bpp] if x >= bpp else 0
                recon[x] = (filt[x] + left) & 0xFF
        elif ftype == 2:
            for x in range(row_bytes):
                recon[x] = (filt[x] + prev_row[x]) & 0xFF
        elif ftype == 3:
            for x in range(row_bytes):
                left = recon[x - bpp] if x >= bpp else 0
                up = prev_row[x]
                recon[x] = (filt[x] + ((left + up) >> 1)) & 0xFF
        elif ftype == 4:
            for x in range(row_bytes):
                left = recon[x - bpp] if x >= bpp else 0
                up = prev_row[x]
                up_left = prev_row[x - bpp] if x >= bpp else 0
                recon[x] = (filt[x] + _paeth_predictor(left, up, up_left)) & 0xFF
        else:
            raise ValueError(f"Unsupported PNG scanline filter type: {ftype}")

        out[y * row_bytes : (y + 1) * row_bytes] = recon
        prev_row = bytes(recon)

    return DecodedPngImage(width=width, height=height, channels=channels, pixels=bytes(out))


def _validate_norm_point(point: Sequence[float]) -> tuple[float, float]:
    if isinstance(point, (str, bytes)) or len(point) != 2:
        raise ValueError("point_norm must be a 2-element [x, y] coordinate")
    x = float(point[0])
    y = float(point[1])
    if not (math.isfinite(x) and math.isfinite(y)):
        raise ValueError("point_norm coordinates must be finite")
    if x < 0.0 or x > 1.0 or y < 0.0 or y > 1.0:
        raise ValueError("point_norm coordinates must lie in [0.0, 1.0]")
    return (x, y)


def _validate_norm_bbox(bbox: Sequence[float]) -> tuple[float, float, float, float]:
    if isinstance(bbox, (str, bytes)) or len(bbox) != 4:
        raise ValueError("bbox_norm must be a 4-element [x1, y1, x2, y2] box")
    coords = [float(v) for v in bbox]
    if not all(math.isfinite(v) for v in coords):
        raise ValueError("bbox_norm coordinates must be finite")
    if any(v < 0.0 or v > 1.0 for v in coords):
        raise ValueError("bbox_norm coordinates must lie in [0.0, 1.0]")
    x1, y1, x2, y2 = coords
    return (min(x1, x2), min(y1, y2), max(x1, x2), max(y1, y2))


def _coerce_norm_point(raw: Any) -> list[float] | None:
    if not isinstance(raw, (list, tuple)) or len(raw) != 2:
        return None
    try:
        x, y = _validate_norm_point(raw)
        return [x, y]
    except (TypeError, ValueError):
        return None


def _coerce_norm_bbox(raw: Any) -> list[float] | None:
    if not isinstance(raw, (list, tuple)) or len(raw) != 4:
        return None
    try:
        x1, y1, x2, y2 = _validate_norm_bbox(raw)
        return [x1, y1, x2, y2]
    except (TypeError, ValueError):
        return None


def sample_png_rgb_distribution(
    mask_png: bytes | Path,
    *,
    point_norm: Sequence[float] | None = None,
    bbox_norm: Sequence[float] | None = None,
) -> dict[tuple[int, int, int], int]:
    """Decode a PNG from bytes/path and return its RGB frequency distribution in a point or box."""
    raw_bytes = Path(mask_png).read_bytes() if isinstance(mask_png, (str, Path)) else mask_png
    image = decode_png(raw_bytes)
    return image.rgb_distribution(point_norm=point_norm, bbox_norm=bbox_norm)


def _parse_color_key(raw: Any) -> tuple[int, int, int] | None:
    if isinstance(raw, str):
        text = raw.strip().lstrip("#")
        if len(text) == 6:
            try:
                return (int(text[0:2], 16), int(text[2:4], 16), int(text[4:6], 16))
            except ValueError:
                return None
    elif isinstance(raw, (list, tuple)) and len(raw) == 3:
        try:
            rgb = tuple(int(v) for v in raw)
            if all(0 <= c <= 255 for c in rgb):
                return rgb  # type: ignore[return-value]
        except (TypeError, ValueError):
            return None
    return None


def _normalize_ast_span(raw: Any) -> dict[str, int] | None:
    if not isinstance(raw, dict):
        return None
    out: dict[str, int] = {}
    for key in ("start_line", "start_col", "end_line", "end_col"):
        val = raw.get(key)
        if isinstance(val, int) and not isinstance(val, bool) and val > 0:
            out[key] = val
    return out or None


def _extract_legend_color_map(
    legend: Any,
    view_stem: str,
) -> dict[tuple[int, int, int], dict[str, Any]]:
    """Build `(r, g, b) -> SemanticEntry` lookup dictionary for a view."""
    entries: list[dict[str, Any]] = []
    if isinstance(legend, (str, Path)):
        loaded = load_semantic_legend(Path(legend), view_stem or None)
        if loaded is None:
            return {}
        legend = loaded

    if isinstance(legend, list):
        entries = [e for e in legend if isinstance(e, dict)]
    elif isinstance(legend, dict):
        if "views" in legend:
            extracted = _extract_view_legend_payload(legend, view_stem or None)
            if isinstance(extracted, dict):
                if isinstance(extracted.get("entries"), list):
                    entries = [e for e in extracted["entries"] if isinstance(e, dict)]
                elif isinstance(extracted.get("views"), list):
                    for v in extracted["views"]:
                        if isinstance(v, dict) and isinstance(v.get("entries"), list):
                            entries.extend(e for e in v["entries"] if isinstance(e, dict))
        elif isinstance(legend.get("entries"), list):
            entries = [e for e in legend["entries"] if isinstance(e, dict)]
        elif isinstance(legend.get("legend"), list):
            entries = [e for e in legend["legend"] if isinstance(e, dict)]
        else:
            # Direct mapping {"#rrggbb": {...}}
            direct_map: dict[tuple[int, int, int], dict[str, Any]] = {}
            for color_str, entry in legend.items():
                rgb = _parse_color_key(color_str)
                if rgb is not None and isinstance(entry, dict):
                    direct_map[rgb] = entry
            return direct_map

    color_map: dict[tuple[int, int, int], dict[str, Any]] = {}
    for entry in entries:
        rgb = _parse_color_key(entry.get("color"))
        if rgb is not None:
            color_map[rgb] = entry
    return color_map


def _enrich_from_mesh(
    entry: dict[str, Any],
    mesh_payload: dict[str, Any] | None,
) -> tuple[str | None, list[str]]:
    """Collect `component_id` and `tags` from legend entry and optional `scene-mesh` JSON."""
    component_id = entry.get("component_id")
    if not isinstance(component_id, str) or not component_id.strip():
        component_id = None
    else:
        component_id = component_id.strip()

    tags: list[str] = []
    seen_tags: set[str] = set()

    def _add_tags(raw_tags: Any) -> None:
        if isinstance(raw_tags, str) and raw_tags.strip():
            val = raw_tags.strip()
            if val not in seen_tags:
                seen_tags.add(val)
                tags.append(val)
        elif isinstance(raw_tags, list):
            for item in raw_tags:
                if isinstance(item, str) and item.strip():
                    val = item.strip()
                    if val not in seen_tags:
                        seen_tags.add(val)
                        tags.append(val)

    _add_tags(entry.get("tags"))
    _add_tags(entry.get("provenance_tags"))

    face_id = str(entry.get("face_id", "")).strip()
    is_edge = bool(entry.get("is_edge", False))
    if isinstance(mesh_payload, dict) and face_id:
        records_key = "edges" if is_edge else "faces"
        id_key = "edge_id" if is_edge else "face_id"
        for rec in mesh_payload.get(records_key, []):
            if not isinstance(rec, dict):
                continue
            if str(rec.get(id_key, "")).strip() != face_id:
                continue
            _add_tags(rec.get("provenance_tags"))
            _add_tags(rec.get("tags"))
            if component_id is None:
                rec_comp = rec.get("component_id")
                if isinstance(rec_comp, str) and rec_comp.strip():
                    component_id = rec_comp.strip()
            break

    return (component_id, tags)


def lookup_semantic_region(
    mask_png: bytes | Path | None,
    legend: Any,
    *,
    view: str = "",
    point_norm: Sequence[float] | None = None,
    bbox_norm: Sequence[float] | None = None,
    mesh_payload: dict[str, Any] | None = None,
) -> SemanticLookupHit | None:
    """Resolve normalized `(x, y)` or `[x1, y1, x2, y2]` via `.mask.png` + `.semantic.json`.

    Returns `SemanticLookupHit` with `component_id`, `face_id`, `ast_span`, and `tags`,
    or `None` when the mask/legend is unavailable, invalid, or unmatched.
    """
    if mask_png is None or legend is None:
        return None
    view_stem = _normalize_view_stem(view) if view else ""
    color_map = _extract_legend_color_map(legend, view_stem)
    if not color_map:
        return None

    try:
        raw_bytes = Path(mask_png).read_bytes() if isinstance(mask_png, (str, Path)) else mask_png
        image = decode_png(raw_bytes)
    except (OSError, ValueError, MemoryError):
        return None

    def _match_distribution(dist: dict[tuple[int, int, int], int]) -> SemanticLookupHit | None:
        total = sum(dist.values())
        if total <= 0:
            return None
        for rgb, count in sorted(dist.items(), key=lambda kv: (-kv[1], kv[0])):
            entry = color_map.get(rgb)
            if entry is None:
                continue
            face_id = str(entry.get("face_id", "")).strip()
            if not face_id:
                continue
            component_id, tags = _enrich_from_mesh(entry, mesh_payload)
            inst_raw = entry.get("instance_index", 0)
            instance_index = int(inst_raw) if isinstance(inst_raw, int) and not isinstance(inst_raw, bool) else 0
            return SemanticLookupHit(
                view=view_stem or view,
                color=f"#{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}",
                face_id=face_id,
                component_id=component_id,
                instance_index=instance_index,
                is_edge=bool(entry.get("is_edge", False)),
                ast_span=_normalize_ast_span(entry.get("ast_span")),
                tags=tags,
                pixel_count=count,
                coverage=count / float(total),
            )
        return None

    if point_norm is not None:
        try:
            hit = _match_distribution(image.rgb_distribution(point_norm=point_norm))
            if hit is not None:
                return hit
        except ValueError:
            pass

    if bbox_norm is not None:
        try:
            hit = _match_distribution(image.rgb_distribution(bbox_norm=bbox_norm))
            if hit is not None:
                return hit
        except ValueError:
            pass

    return None


def _extract_vlm_json(raw_text: str) -> dict[str, Any]:
    """Strictly extract a JSON object from VLM output, supporting ```json fences."""
    text = raw_text.strip()
    if not text:
        raise ValueError("VLM returned empty response")

    # 1. Direct JSON parse
    try:
        parsed = json.loads(text)
        if isinstance(parsed, dict):
            return parsed
    except json.JSONDecodeError:
        pass

    # 2. Markdown ```json ... ``` or ``` ... ``` fence
    fence_match = re.search(r"```(?:json)?\s*\n(.*?)```", text, re.DOTALL | re.IGNORECASE)
    if fence_match:
        fenced = fence_match.group(1).strip()
        try:
            parsed = json.loads(fenced)
            if isinstance(parsed, dict):
                return parsed
        except json.JSONDecodeError as exc:
            raise ValueError(f"Invalid JSON inside markdown fence: {exc}") from exc

    # 3. Embedded JSON object in prose
    decoder = json.JSONDecoder()
    for idx, ch in enumerate(text):
        if ch != "{":
            continue
        try:
            parsed, _ = decoder.raw_decode(text, idx)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed

    raise ValueError("VLM response did not contain a valid JSON object")


def _parse_vlm_review_payload(
    raw_text: str,
    *,
    views_checked: list[str],
    semantic_masks: dict[str, bytes] | None = None,
    semantic_legend: dict[str, Any] | list[dict[str, Any]] | None = None,
    mesh_payload: dict[str, Any] | None = None,
) -> VisualReviewResult:
    """Validate and convert raw VLM JSON text into a `VisualReviewResult`."""
    data = _extract_vlm_json(raw_text)

    passed = data.get("passed")
    if not isinstance(passed, bool):
        raise ValueError("VLM JSON field 'passed' must be a boolean")

    summary = data.get("summary")
    if not isinstance(summary, str) or not summary.strip():
        raise ValueError("VLM JSON field 'summary' must be a non-empty string")
    summary = summary.strip()

    raw_issues = data.get("issues", [])
    if not isinstance(raw_issues, list) or not all(isinstance(item, str) for item in raw_issues):
        raise ValueError("VLM JSON field 'issues' must be a list of strings")
    issues = [item.strip() for item in raw_issues if item.strip()]

    findings: list[VisualReviewFinding] = []
    raw_findings = data.get("findings")
    if isinstance(raw_findings, list):
        for entry in raw_findings:
            if not isinstance(entry, dict):
                continue
            msg = str(entry.get("message", "")).strip()
            if not msg:
                continue
            view_raw = str(entry.get("view", "overall"))
            point_norm = _coerce_norm_point(entry.get("point_norm"))
            bbox_norm = _coerce_norm_bbox(entry.get("bbox_norm"))
            component_id = (
                str(entry["component_id"]).strip()
                if isinstance(entry.get("component_id"), str) and str(entry["component_id"]).strip()
                else None
            )
            face_id = (
                str(entry["face_id"]).strip()
                if isinstance(entry.get("face_id"), str) and str(entry["face_id"]).strip()
                else None
            )
            ast_span = _normalize_ast_span(entry.get("ast_span"))
            tags = [
                str(t).strip()
                for t in entry.get("tags", [])
                if isinstance(t, str) and str(t).strip()
            ] if isinstance(entry.get("tags"), list) else []

            view_stem = _normalize_view_stem(view_raw)
            mask_bytes = semantic_masks.get(view_stem) if semantic_masks else None
            if (
                mask_bytes is not None
                and semantic_legend is not None
                and (point_norm is not None or bbox_norm is not None)
            ):
                hit = lookup_semantic_region(
                    mask_bytes,
                    semantic_legend,
                    view=view_stem,
                    point_norm=point_norm,
                    bbox_norm=bbox_norm,
                    mesh_payload=mesh_payload,
                )
                if hit is not None:
                    component_id = hit.component_id or component_id
                    face_id = hit.face_id or face_id
                    ast_span = hit.ast_span or ast_span
                    merged_tags = list(hit.tags)
                    for tag in tags:
                        if tag not in merged_tags:
                            merged_tags.append(tag)
                    tags = merged_tags

            findings.append(
                VisualReviewFinding(
                    view=view_raw,
                    message=msg,
                    severity=str(entry.get("severity", "error" if not passed else "info")),
                    category=str(entry.get("category", "vlm")),
                    point_norm=point_norm,
                    bbox_norm=bbox_norm,
                    component_id=component_id,
                    face_id=face_id,
                    ast_span=ast_span,
                    tags=tags,
                )
            )
    if not passed and not issues:
        issues = [f.message for f in findings] if findings else [summary]
    if not findings and issues:
        for issue in issues:
            findings.append(
                VisualReviewFinding(
                    view="overall",
                    message=issue,
                    severity="error" if not passed else "warning",
                    category="vlm",
                )
            )

    return VisualReviewResult(
        passed=passed,
        summary=summary,
        issues=issues,
        used_vlm=True,
        views_checked=list(views_checked),
        findings=findings,
    )


class VLMViewInspector:
    """Pluggable 4-view orthogonal + VLM visual/geometric inspector.

    Compatible with `OrthogonalViewInspector.inspect(...)` as well as direct
    `(views_dir, mesh_payload, contract)` calls. Always runs deterministic
    structural checks first so missing/corrupt views never waste VLM calls.
    """

    EXPECTED_VIEWS: tuple[str, ...] = EXPECTED_VIEWS

    SYSTEM_PROMPT: str = (
        "You are a senior mechanical CAD visual-geometric reviewer inspecting 4 orthogonal/isometric "
        "views (front, top, right, iso), their SVG projection summaries, the DesignContract, "
        "and scene-mesh metadata. "
        "Verify that the rendered geometry aligns with the contract's keep-in envelope, interfaces "
        "(bores, bolt patterns, mounting faces), symmetry, and manufacturability without obvious "
        "degenerate projections or missing features. "
        "When you spot a localized defect in a specific view, include optional `findings` items with "
        "normalized coordinates `point_norm: [x, y]` or `bbox_norm: [x1, y1, x2, y2]` in [0, 1] "
        "(top-left is [0, 0], bottom-right is [1, 1]) so the compiler semantic mask can map the defect "
        "back to its exact component, face_id, and AST source span. "
        'Return ONLY strict JSON matching: {"passed": boolean, "summary": string, "issues": [string], '
        '"findings": [{"view": "front|top|right|iso", "severity": "error|warning", '
        '"category": "geometry|interface|manufacturability", "message": string, '
        '"point_norm": [x, y], "bbox_norm": [x1, y1, x2, y2]}]}.'
    )

    def __init__(
        self,
        llm_client: LLMClient | None = None,
        *,
        enable_vlm: bool = True,
        strict_vlm: bool = False,
        include_base64_images: bool = False,
    ) -> None:
        self.llm_client = llm_client
        self.enable_vlm = enable_vlm
        self.strict_vlm = strict_vlm
        self.include_base64_images = include_base64_images

    def _resolve_mesh_payload(
        self,
        mesh_input: Path | str | dict[str, Any] | None,
        mesh_payload: dict[str, Any] | None,
    ) -> tuple[dict[str, Any] | None, str | None]:
        """Load and validate mesh payload from either a dict or file path."""
        if isinstance(mesh_payload, dict):
            data = mesh_payload
        elif isinstance(mesh_input, dict):
            data = mesh_input
        elif isinstance(mesh_input, (str, Path)):
            mesh_file = Path(mesh_input)
            if not mesh_file.is_file():
                return (None, None)
            try:
                data = json.loads(mesh_file.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as exc:
                return (None, f"Corrupt scene-mesh JSON: {exc}")
        else:
            return (None, None)

        if not isinstance(data, dict):
            return (None, "scene-mesh payload must be a JSON object")
        if data.get("format_version") != "0.1" or not data.get("faces"):
            return (None, "scene-mesh JSON missing format_version='0.1' or non-empty faces")
        return (data, None)

    def _image_blocks_from_shaded(
        self,
        views_dir: Path,
        shaded: dict[str, bytes] | None,
    ) -> list[MultimodalImageBlock]:
        """Convert preloaded shaded PNGs to image blocks, or rasterize all SVGs."""
        if shaded is not None:
            return [
                MultimodalImageBlock(name, "image/png", base64.b64encode(data).decode("ascii"))
                for name, data in shaded.items()
            ]
        return [
            MultimodalImageBlock(
                name,
                "image/png",
                render_svg_to_png_base64((views_dir / name).read_text(encoding="utf-8", errors="ignore")),
            )
            for name in self.EXPECTED_VIEWS
        ]

    def _build_image_blocks(
        self,
        views_dir: Path,
        render_shaded_views: Callable[[], dict[str, bytes] | None] | None = None,
    ) -> list[MultimodalImageBlock]:
        """Use a complete shaded set when available, otherwise rasterize all SVGs."""
        shaded = render_shaded_views() if render_shaded_views is not None else load_shaded_views(views_dir)
        return self._image_blocks_from_shaded(views_dir, shaded)

    def _build_user_prompt(
        self,
        *,
        view_summaries: list[ViewProjectionSummary],
        mesh_summary: dict[str, Any] | None,
        contract: DesignContract | None,
        compile_result: CompileResult | None,
    ) -> str:
        payload: dict[str, Any] = {
            "views": [s.to_dict() for s in view_summaries],
        }
        if contract is not None:
            payload["design_contract"] = contract.to_dict()
        if mesh_summary is not None:
            payload["scene_mesh_summary"] = mesh_summary
        if compile_result is not None and compile_result.metrics is not None:
            payload["compile_metrics"] = compile_result.metrics.to_dict()

        return (
            "Review the following 4-view orthogonal CAD projections against the DesignContract "
            "and scene-mesh summary.\n"
            f"```json\n{json.dumps(payload, indent=2, ensure_ascii=False)}\n```\n"
            'Respond ONLY with strict JSON: {"passed": true/false, "summary": "...", "issues": ["..."], '
            '"findings": [{"view": "...", "severity": "...", "category": "...", "message": "...", '
            '"point_norm": [x, y], "bbox_norm": [x1, y1, x2, y2]}]} (omit findings if none).'
        )

    def inspect_detailed(
        self,
        views_dir: Path | None,
        mesh_path: Path | str | dict[str, Any] | None = None,
        contract: DesignContract | None = None,
        compile_result: CompileResult | None = None,
        *,
        mesh_payload: dict[str, Any] | None = None,
        image_blocks: list[Any] | None = None,
        render_shaded_views: Callable[[], dict[str, bytes] | None] | None = None,
        semantic_masks: dict[str, bytes] | None = None,
        semantic_legend: dict[str, Any] | list[dict[str, Any]] | None = None,
    ) -> VisualReviewResult:
        """Run baseline 4-view/mesh verification followed by optional VLM review."""
        baseline_notes: list[str] = []
        views_checked: list[str] = []
        view_summaries: list[ViewProjectionSummary] = []

        # 1. Baseline structural check on 4 orthogonal/iso SVG views
        if views_dir is not None:
            views_path = Path(views_dir)
            if not views_path.is_dir():
                msg = f"Orthogonal views directory does not exist: {views_path}"
                return VisualReviewResult(
                    passed=False,
                    summary=msg,
                    issues=[msg],
                    used_vlm=False,
                    views_checked=[],
                    findings=[VisualReviewFinding(view="overall", message=msg, category="structure")],
                )

            missing: list[str] = []
            for view_name in self.EXPECTED_VIEWS:
                svg_file = views_path / view_name
                if not svg_file.is_file():
                    missing.append(view_name)
                    continue
                svg_text = svg_file.read_text(encoding="utf-8", errors="ignore")
                if not _is_valid_svg_document(svg_text):
                    missing.append(view_name)
                    continue
                views_checked.append(view_name)
                view_summaries.append(summarize_svg_view(view_name, svg_text))

            if missing:
                msg = f"Missing or invalid orthogonal SVG views: {', '.join(missing)}"
                return VisualReviewResult(
                    passed=False,
                    summary=msg,
                    issues=[msg],
                    used_vlm=False,
                    views_checked=views_checked,
                    findings=[
                        VisualReviewFinding(
                            view=name,
                            message=f"View '{name}' is missing or not a valid SVG document",
                            category="structure",
                        )
                        for name in missing
                    ],
                )
            baseline_notes.append("4 orthogonal/iso SVG views verified")

        # 2. Baseline structural check on scene-mesh JSON (if provided)
        resolved_mesh, mesh_error = self._resolve_mesh_payload(mesh_path, mesh_payload)
        if mesh_error is not None:
            return VisualReviewResult(
                passed=False,
                summary=mesh_error,
                issues=[mesh_error],
                used_vlm=False,
                views_checked=views_checked,
                findings=[VisualReviewFinding(view="mesh", message=mesh_error, category="structure")],
            )
        mesh_summary = summarize_mesh_payload(resolved_mesh)
        if mesh_summary is not None:
            baseline_notes.append(f"scene-mesh verified ({mesh_summary['face_count']} faces)")

        baseline_summary = (
            "; ".join(baseline_notes)
            if baseline_notes
            else "Visual inspection skipped (no views/mesh artifacts requested)"
        )

        # 3. Early return if VLM is disabled or no views were provided
        if not self.enable_vlm or not view_summaries:
            return VisualReviewResult(
                passed=True,
                summary=baseline_summary,
                issues=[],
                used_vlm=False,
                views_checked=views_checked,
            )

        if self.llm_client is None:
            reason = "VLM client not configured"
            observe.warn(
                "vision", "vlm_review", f"VLM degraded: {reason}",
                code=observe.CODE_VLM_DEGRADED,
            )
            if self.strict_vlm:
                msg = f"VLM visual review required (strict_vlm=True) but {reason}"
                return VisualReviewResult(
                    passed=False,
                    summary=msg,
                    issues=[msg],
                    used_vlm=False,
                    views_checked=views_checked,
                    findings=[VisualReviewFinding(view="overall", message=msg, category="vlm")],
                    fallback_reason=reason,
                )
            return VisualReviewResult(
                passed=True,
                summary=f"{baseline_summary} (VLM skipped: {reason})",
                issues=[],
                used_vlm=False,
                views_checked=views_checked,
                fallback_reason=reason,
            )

        # 4. Invoke VLM (preferring multimodal hook when supported by client)
        resolved_masks: dict[str, bytes] = {}
        resolved_legend: dict[str, Any] | list[dict[str, Any]] | None = None
        if views_dir is not None:
            views_path = Path(views_dir)
            for view_name in ORTHOGONAL_VIEW_NAMES:
                disk_mask = load_semantic_mask(views_path, view_name)
                if disk_mask is not None:
                    resolved_masks[view_name] = disk_mask
            resolved_legend = load_semantic_legend(views_path)

        user_prompt = self._build_user_prompt(
            view_summaries=view_summaries,
            mesh_summary=mesh_summary,
            contract=contract,
            compile_result=compile_result,
        )
        try:
            multimodal_fn = getattr(self.llm_client, "complete_multimodal", None)
            if callable(multimodal_fn) and (image_blocks is not None or views_dir is not None):
                if image_blocks is not None:
                    resolved_blocks = list(image_blocks)
                else:
                    shaded = (
                        render_shaded_views()
                        if render_shaded_views is not None
                        else load_shaded_views(Path(views_dir))  # type: ignore[arg-type]
                    )
                    if isinstance(shaded, ShadedViewsBundle):
                        if shaded.semantic_masks:
                            resolved_masks.update(shaded.semantic_masks)
                        if shaded.semantic_legend is not None:
                            resolved_legend = shaded.semantic_legend
                    resolved_blocks = self._image_blocks_from_shaded(Path(views_dir), shaded)  # type: ignore[arg-type]
                raw_response = multimodal_fn(self.SYSTEM_PROMPT, user_prompt, resolved_blocks)
            else:
                if self.include_base64_images and views_dir is not None:
                    # Extension point: image blocks are ready for multimodal-capable clients
                    _ = self._build_image_blocks(Path(views_dir))
                raw_response = self.llm_client.complete(self.SYSTEM_PROMPT, user_prompt)

            if semantic_masks:
                for key, val in semantic_masks.items():
                    resolved_masks[_normalize_view_stem(key)] = val
            if semantic_legend is not None:
                resolved_legend = semantic_legend

            if not isinstance(raw_response, str):
                raise ValueError("VLM client returned non-string response")

            return _parse_vlm_review_payload(
                raw_response,
                views_checked=views_checked,
                semantic_masks=resolved_masks or None,
                semantic_legend=resolved_legend,
                mesh_payload=resolved_mesh,
            )
        except Exception as exc:
            safe_error = _redact_secrets(f"{type(exc).__name__}: {exc}", self.llm_client)
            observe.warn(
                "vision", "vlm_review", "VLM review failed; degrading to baseline",
                code=observe.CODE_VLM_DEGRADED,
                error_type=type(exc).__name__,
            )
            if self.strict_vlm:
                msg = f"VLM visual review failed (strict_vlm=True): {safe_error}"
                return VisualReviewResult(
                    passed=False,
                    summary=msg,
                    issues=[msg],
                    used_vlm=False,
                    views_checked=views_checked,
                    findings=[VisualReviewFinding(view="overall", message=msg, category="vlm")],
                    fallback_reason=safe_error,
                )
            return VisualReviewResult(
                passed=True,
                summary=f"{baseline_summary} (VLM degraded fallback: {safe_error})",
                issues=[],
                used_vlm=False,
                views_checked=views_checked,
                findings=[
                    VisualReviewFinding(
                        view="overall",
                        message=f"VLM review degraded to baseline check: {safe_error}",
                        severity="warning",
                        category="vlm",
                    )
                ],
                fallback_reason=safe_error,
            )

    def inspect(
        self,
        views_dir: Path | None,
        mesh_path: Path | str | dict[str, Any] | None = None,
        contract: DesignContract | None = None,
        compile_result: CompileResult | None = None,
        *,
        mesh_payload: dict[str, Any] | None = None,
        image_blocks: list[Any] | None = None,
        render_shaded_views: Callable[[], dict[str, bytes] | None] | None = None,
        semantic_masks: dict[str, bytes] | None = None,
        semantic_legend: dict[str, Any] | list[dict[str, Any]] | None = None,
    ) -> tuple[bool, str]:
        """OrthogonalViewInspector-compatible entrypoint returning `(passed, summary)`."""
        result = self.inspect_detailed(
            views_dir,
            mesh_path,
            contract,
            compile_result,
            mesh_payload=mesh_payload,
            image_blocks=image_blocks,
            render_shaded_views=render_shaded_views,
            semantic_masks=semantic_masks,
            semantic_legend=semantic_legend,
        )
        if result.passed:
            return (True, result.summary)

        extra: list[str] = [
            issue for issue in result.issues if issue not in result.summary
        ]
        for finding in result.findings:
            loc = finding.format_location()
            if loc:
                localized = f"[{finding.view}] {finding.message} ({loc})"
                if localized not in result.summary and localized not in extra:
                    extra.append(localized)

        if extra:
            return (False, f"{result.summary} | {'; '.join(extra)}")
        return (False, result.summary)


def attach_visual_assist(
    report: VerificationReport,
    visual: VisualReviewResult,
) -> VerificationReport:
    """Record an offscreen visual review without promoting geometric FAIL or UNKNOWN.

    The assist check copies the host layer's current status, so a visual PASS
    cannot turn a geometric FAIL or UNKNOWN into PASS, and a visual FAIL cannot
    overturn a geometric PASS.
    """
    before_layers = {layer: report.layers[layer] for layer in VERIFICATION_LAYERS}
    before_overall = report.overall_status or "UNKNOWN"
    host_layer = "L4_interop_roundtrip"
    host_status = before_layers.get(host_layer, "UNKNOWN")
    if host_status not in {"PASS", "FAIL", "UNKNOWN"}:
        host_status = "UNKNOWN"
    summary = (visual.summary or "Visual assist does not change the geometric verdict.").strip()
    evidence: dict[str, Any] = {
        "advisory_only": True,
        "visual_passed": bool(visual.passed),
        "used_vlm": bool(visual.used_vlm),
        "cannot_promote_fail_or_unknown": True,
        "geometric_overall": before_overall,
        "findings": [finding.to_dict() for finding in visual.findings],
    }
    if visual.fallback_reason:
        evidence["fallback_reason"] = visual.fallback_reason
    check = VerificationCheck(
        code="L4_VISUAL_ASSIST",
        layer=host_layer,
        status=host_status,
        message=summary,
        evidence=evidence,
    )
    rebuilt = VerificationReport(
        kernel_identity=report.kernel_identity,
        layers=dict(before_layers),
        checks=[*report.checks, check],
        overall_status=None,
        schema_version=report.schema_version,
        summary=report.summary,
    )
    if before_overall in {"FAIL", "UNKNOWN"} and rebuilt.overall_status == "PASS":
        return report
    for layer, status in before_layers.items():
        if status in {"FAIL", "UNKNOWN"} and rebuilt.layers.get(layer) == "PASS":
            return report
    if before_overall == "PASS" and rebuilt.overall_status != "PASS":
        return report
    return rebuilt


def review_offscreen_semantic_masks(
    inspector: VLMViewInspector,
    images: Mapping[str, bytes] | ShadedViewsBundle | None,
    *,
    semantic_masks: Mapping[str, bytes] | None = None,
    semantic_legend: dict[str, Any] | list[dict[str, Any]] | None = None,
    contract: DesignContract | None = None,
    mesh_payload: dict[str, Any] | None = None,
) -> VisualReviewResult:
    """Review offscreen multi-view PNGs plus semantic masks.

    The returned review is advisory. Callers must pass it through
    ``attach_visual_assist`` so it cannot change a geometric FAIL or UNKNOWN
    into PASS.
    """
    views = _offscreen_views(images)
    masks: dict[str, bytes] = {}
    legend = semantic_legend
    if isinstance(images, ShadedViewsBundle):
        masks.update(images.semantic_masks)
        if legend is None:
            legend = images.semantic_legend
    if semantic_masks:
        for key, value in semantic_masks.items():
            masks[_normalize_view_stem(key)] = value
    if len(views) < len(ORTHOGONAL_VIEW_NAMES):
        return VisualReviewResult(
            passed=False,
            summary=(
                "Offscreen multi-view semantic masks are unavailable. "
                "Visual assist stays UNKNOWN and does not change the geometric verdict."
            ),
            issues=[],
            used_vlm=False,
            views_checked=[name for name, _data in views],
            fallback_reason="offscreen_unavailable",
        )
    if not inspector.enable_vlm or inspector.llm_client is None:
        return VisualReviewResult(
            passed=False,
            summary=(
                "VLM client is not configured. Offscreen masks were not allowed "
                "to change the geometric verdict."
            ),
            issues=[],
            used_vlm=False,
            views_checked=[name for name, _data in views],
            fallback_reason="vlm_unconfigured",
        )
    summaries = [
        ViewProjectionSummary(view_name=f"{name}.svg", size_bytes=len(data))
        for name, data in views
    ]
    blocks = [
        MultimodalImageBlock(f"{name}.png", "image/png", base64.b64encode(data).decode("ascii"))
        for name, data in views
    ]
    user_prompt = inspector._build_user_prompt(
        view_summaries=summaries,
        mesh_summary=summarize_mesh_payload(mesh_payload) if isinstance(mesh_payload, dict) else None,
        contract=contract,
        compile_result=None,
    )
    try:
        multimodal = getattr(inspector.llm_client, "complete_multimodal", None)
        if callable(multimodal):
            raw_response = multimodal(inspector.SYSTEM_PROMPT, user_prompt, blocks)
        else:
            raw_response = inspector.llm_client.complete(inspector.SYSTEM_PROMPT, user_prompt)
        if not isinstance(raw_response, str):
            raise ValueError("VLM client returned non-string response")
        return _parse_vlm_review_payload(
            raw_response,
            views_checked=[f"{name}.svg" for name, _data in views],
            semantic_masks=masks or None,
            semantic_legend=legend,
            mesh_payload=mesh_payload,
        )
    except Exception as exc:
        safe_error = _redact_secrets(f"{type(exc).__name__}: {exc}", inspector.llm_client)
        return VisualReviewResult(
            passed=False,
            summary=f"Offscreen VLM review failed: {safe_error}",
            issues=[safe_error] if inspector.strict_vlm else [],
            used_vlm=False,
            views_checked=[name for name, _data in views],
            fallback_reason=safe_error,
        )


def _offscreen_views(
    images: Mapping[str, bytes] | ShadedViewsBundle | None,
) -> list[tuple[str, bytes]]:
    if not isinstance(images, Mapping):
        return []
    found: dict[str, bytes] = {}
    for key, value in images.items():
        if not isinstance(value, (bytes, bytearray)) or isinstance(key, int):
            continue
        stem = _normalize_view_stem(str(key))
        if stem in ORTHOGONAL_VIEW_NAMES and stem not in found:
            found[stem] = bytes(value)
    return [(name, found[name]) for name in ORTHOGONAL_VIEW_NAMES if name in found]


def create_view_inspector(
    llm_client: LLMClient | None = None,
    *,
    enable_vlm: bool = False,
    strict_vlm: bool = False,
) -> OrthogonalViewInspector:
    """Factory for pluggable view inspection (`HeuristicViewInspector` vs `VLMViewInspector`)."""
    if not enable_vlm:
        return HeuristicViewInspector()
    return VLMViewInspector(
        llm_client=llm_client,
        enable_vlm=True,
        strict_vlm=strict_vlm,
    )


__all__ = [
    "DecodedPngImage",
    "EXPECTED_VIEWS",
    "MultimodalImageBlock",
    "MultimodalLLMClient",
    "SemanticLookupHit",
    "VLMViewInspector",
    "ViewProjectionSummary",
    "VisualReviewFinding",
    "VisualReviewResult",
    "attach_visual_assist",
    "create_view_inspector",
    "decode_png",
    "review_offscreen_semantic_masks",
    "lookup_semantic_region",
    "render_svg_to_png_base64",
    "sample_png_rgb_distribution",
    "summarize_mesh_payload",
    "summarize_svg_view",
]
