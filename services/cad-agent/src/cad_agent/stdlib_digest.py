"""On-demand authoritative size digest for the semantic planner.

Numbers are copied from ``crates/cad-stdlib`` (fasteners, bearings, ISO 286)
and from ``schemas/materials.json``. Missing catalog keys are reported as
absent. This module never interpolates a size that is not in those tables.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from pathlib import Path
from typing import Any

DIGEST_LIMIT_BYTES = 1536

_STEP_EDGE_MM: tuple[float, ...] = (3.0, 6.0, 10.0, 18.0, 30.0, 50.0, 80.0, 120.0, 180.0, 250.0)

# size, pitch, tap drill, close, normal, loose, cbore diameter, cbore depth, csink diameter.
# Transcribed from crates/cad-stdlib/src/fasteners.rs. M14 and M16 are not in that table.
_FASTENER_ROWS: tuple[tuple[str, float, float, float, float, float, float, float, float], ...] = (
    ("M2", 0.40, 1.60, 2.2, 2.4, 2.6, 4.4, 2.2, 4.70),
    ("M2.5", 0.45, 2.05, 2.7, 2.9, 3.1, 5.5, 3.0, 5.88),
    ("M3", 0.50, 2.50, 3.2, 3.4, 3.6, 6.5, 3.5, 6.72),
    ("M4", 0.70, 3.30, 4.3, 4.5, 4.8, 8.0, 4.8, 8.96),
    ("M5", 0.80, 4.20, 5.3, 5.5, 5.8, 10.0, 5.8, 11.20),
    ("M6", 1.00, 5.00, 6.4, 6.6, 7.0, 11.0, 6.8, 13.44),
    ("M8", 1.25, 6.80, 8.4, 9.0, 10.0, 15.0, 8.8, 17.92),
    ("M10", 1.50, 8.50, 10.5, 11.0, 12.0, 18.0, 10.8, 22.40),
    ("M12", 1.75, 10.20, 13.0, 13.5, 14.5, 20.0, 12.8, 26.88),
)

# size, pitch, drill, coarse. ISO 262 fine drills are tabulated, not computed here.
_TAP_DRILL_ROWS: tuple[tuple[str, float, float, bool], ...] = (
    ("M2", 0.40, 1.60, True),
    ("M2", 0.25, 1.75, False),
    ("M2.5", 0.45, 2.05, True),
    ("M2.5", 0.35, 2.15, False),
    ("M3", 0.50, 2.50, True),
    ("M3", 0.35, 2.65, False),
    ("M4", 0.70, 3.30, True),
    ("M4", 0.50, 3.50, False),
    ("M5", 0.80, 4.20, True),
    ("M5", 0.50, 4.50, False),
    ("M6", 1.00, 5.00, True),
    ("M6", 0.75, 5.25, False),
    ("M8", 1.25, 6.80, True),
    ("M8", 1.00, 7.00, False),
    ("M8", 0.75, 7.25, False),
    ("M10", 1.50, 8.50, True),
    ("M10", 1.25, 8.75, False),
    ("M10", 1.00, 9.00, False),
    ("M10", 0.75, 9.25, False),
    ("M12", 1.75, 10.20, True),
    ("M12", 1.50, 10.50, False),
    ("M12", 1.25, 10.75, False),
    ("M12", 1.00, 11.00, False),
)

# designation, aliases, bore, outer, width, shaft fit, housing fit, shoulder min, housing shoulder max.
# Transcribed from crates/cad-stdlib/src/bearings.rs.
_BEARING_ROWS: tuple[tuple[str, tuple[str, ...], float, float, float, str, str, float, float], ...] = (
    ("608ZZ", ("608", "608-2Z", "608-ZZ", "6082Z"), 8.0, 22.0, 7.0, "js5", "H7", 10.0, 20.0),
    ("6000", (), 10.0, 26.0, 8.0, "js5", "H7", 12.0, 24.0),
    ("6001", (), 12.0, 28.0, 8.0, "j5", "H7", 14.0, 26.0),
    ("6002", (), 15.0, 32.0, 9.0, "j5", "H7", 17.0, 30.0),
    ("6004", (), 20.0, 42.0, 12.0, "k5", "H7", 25.0, 38.0),
    ("6200", (), 10.0, 30.0, 9.0, "js5", "J7", 14.2, 25.8),
    ("6201", (), 12.0, 32.0, 10.0, "j5", "J7", 16.2, 27.8),
    ("6202", (), 15.0, 35.0, 11.0, "j5", "J7", 19.2, 30.8),
    ("6204", (), 20.0, 47.0, 14.0, "k5", "J7", 25.6, 41.4),
    ("6804", (), 20.0, 32.0, 7.0, "k5", "H7", 22.0, 30.0),
    ("6805", (), 25.0, 37.0, 7.0, "k5", "H7", 27.0, 35.0),
)

# ISO 286-2 limit deviations in micrometres, one value per step in _STEP_EDGE_MM.
# Transcribed from crates/cad-stdlib/src/tolerances.rs. Bands are case-sensitive.
_ISO_BANDS: dict[str, tuple[tuple[float, ...], tuple[float, ...]]] = {
    "H6": ((6, 8, 9, 11, 13, 16, 19, 22, 25, 29), (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
    "H7": ((10, 12, 15, 18, 21, 25, 30, 35, 40, 46), (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
    "H8": ((14, 18, 22, 27, 33, 39, 46, 54, 63, 72), (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
    "H9": ((25, 30, 36, 43, 52, 62, 74, 87, 100, 115), (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
    "H11": ((60, 75, 90, 110, 130, 160, 190, 220, 250, 290), (0, 0, 0, 0, 0, 0, 0, 0, 0, 0)),
    "J7": ((4, 6, 8, 10, 12, 14, 18, 22, 26, 30), (-6, -6, -7, -8, -9, -11, -12, -13, -14, -16)),
    "JS7": ((5, 6, 7.5, 9, 10.5, 12.5, 15, 17.5, 20, 23), (-5, -6, -7.5, -9, -10.5, -12.5, -15, -17.5, -20, -23)),
    "g6": ((-2, -4, -5, -6, -7, -9, -10, -12, -14, -15), (-8, -12, -14, -17, -20, -25, -29, -34, -39, -44)),
    "f7": ((-6, -10, -13, -16, -20, -25, -30, -36, -43, -50), (-16, -22, -28, -34, -41, -50, -60, -71, -83, -96)),
    "h6": ((0, 0, 0, 0, 0, 0, 0, 0, 0, 0), (-6, -8, -9, -11, -13, -16, -19, -22, -25, -29)),
    "h7": ((0, 0, 0, 0, 0, 0, 0, 0, 0, 0), (-10, -12, -15, -18, -21, -25, -30, -35, -40, -46)),
    "js5": ((2, 2.5, 3, 4, 4.5, 5.5, 6.5, 7.5, 9, 10), (-2, -2.5, -3, -4, -4.5, -5.5, -6.5, -7.5, -9, -10)),
    "k5": ((4, 6, 7, 9, 11, 13, 15, 18, 21, 24), (0, 1, 1, 1, 2, 2, 2, 3, 3, 4)),
    "m6": ((8, 12, 15, 18, 21, 25, 30, 35, 40, 46), (2, 4, 6, 7, 8, 9, 11, 13, 15, 17)),
    "p6": ((12, 20, 24, 29, 35, 42, 51, 59, 68, 79), (6, 12, 15, 18, 22, 26, 32, 37, 43, 50)),
}

# Search keys only. Property values still come from schemas/materials.json.
_MATERIAL_SEARCH_EXTRAS: dict[str, tuple[str, ...]] = {
    "Al-6061-T6": ("6061-T6", "6061"),
    "Al-7075-T651": ("7075-T651", "7075"),
    "SUS304": ("SUS304",),
    "Ti-6Al-4V": ("Ti-6Al-4V", "TC4"),
    "45# Steel": ("45#", "45号", "Steel-45"),
}

_CITED_SIZES: frozenset[str] = frozenset(
    {"M2", "M2.5", "M3", "M4", "M5", "M6", "M8", "M10", "M12", "M14", "M16"}
)
_FASTENER_BY_SIZE: dict[str, tuple[str, float, float, float, float, float, float, float, float]] = {
    row[0]: row for row in _FASTENER_ROWS
}
_MATERIAL_CACHE: dict[str, list[dict[str, Any]]] = {}

_THREAD_RE = re.compile(
    r"(?<![A-Za-z0-9.])M\s*(?:(2\.5)|(16|14|12|10|8|6|5|4|3|2)(?!\.\d))(?!\d)"
    r"(?:\s*[x×*]\s*(\d+(?:\.\d+)?))?",
    re.IGNORECASE,
)
_BAND_RE = re.compile(
    r"(?<![A-Za-z0-9])(H11|JS7|H6|H7|H8|H9|J7|g6|f7|h6|h7|js5|k5|m6|p6)(?![A-Za-z0-9])"
)
_NUMBER_RE = re.compile(r"(?<![A-Za-z0-9.])(\d+(?:\.\d+)?)(?:\s*mm)?(?![A-Za-z0-9.])", re.IGNORECASE)
_CSINK_RE = re.compile(r"沉头|埋头|countersink|csink|iso\s*10642", re.IGNORECASE)
_CBORE_RE = re.compile(r"锪平|counterbore|cbore|iso\s*4762", re.IGNORECASE)


class StdlibDigestError(ValueError):
    """A catalog lookup missed, or the material table could not be read."""


def default_materials_path() -> Path:
    """Find ``schemas/materials.json`` by walking up from this module."""
    here = Path(__file__).resolve()
    for parent in here.parents:
        candidate = parent / "schemas" / "materials.json"
        if candidate.is_file():
            return candidate
    raise StdlibDigestError(
        "schemas/materials.json was not found; material numbers cannot be invented"
    )


def load_material_records(path: str | Path | None = None) -> list[dict[str, Any]]:
    """Load material presets from ``schemas/materials.json``."""
    catalog = Path(path) if path is not None else default_materials_path()
    key = str(catalog.resolve())
    cached = _MATERIAL_CACHE.get(key)
    if cached is not None:
        return cached
    try:
        payload = json.loads(catalog.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise StdlibDigestError(f"unable to read material catalog {catalog}: {exc}") from exc
    rows = payload.get("materials") if isinstance(payload, dict) else None
    if not isinstance(rows, list):
        raise StdlibDigestError(f"{catalog} does not contain a materials array")
    copied = [dict(row) for row in rows if isinstance(row, dict) and row.get("id")]
    _MATERIAL_CACHE[key] = copied
    return copied


def lookup_fastener(spec: str) -> dict[str, Any]:
    """Return one fastener row. Unknown sizes and pitches raise ``StdlibDigestError``."""
    size, pitch = _parse_thread_spec(spec)
    row = _FASTENER_BY_SIZE.get(size)
    if row is None:
        raise StdlibDigestError(f"unknown fastener specification {spec.strip()!r}")
    payload = _fastener_payload(row)
    if pitch is None:
        return payload
    drill = _tap_drill(size, pitch)
    if drill is None:
        raise StdlibDigestError(f"unknown fastener specification {spec.strip()!r}")
    payload["requested_pitch_mm"] = pitch
    payload["requested_tap_drill_mm"] = drill
    return payload


def lookup_bearing(designation: str) -> dict[str, Any]:
    """Return one bearing seat. Unknown designations raise ``StdlibDigestError``."""
    row = _bearing_row(designation)
    if row is None:
        raise StdlibDigestError(f"unknown bearing designation {designation.strip()!r}")
    return _bearing_payload(row, with_fit_limits=True)


def lookup_iso286(nominal_mm: float, band: str) -> dict[str, Any]:
    """Return ISO 286 deviations. Out-of-range sizes and unknown bands raise."""
    if isinstance(nominal_mm, bool) or not isinstance(nominal_mm, (int, float)):
        raise StdlibDigestError(f"nominal size {nominal_mm!r} is not a number")
    nominal = float(nominal_mm)
    if nominal != nominal or nominal < 1.0 or nominal > _STEP_EDGE_MM[-1]:
        raise StdlibDigestError(
            f"nominal size {nominal} mm is outside the ISO 286 table (1 mm through 250 mm)"
        )
    canonical = band.strip()
    limits = _ISO_BANDS.get(canonical)
    if limits is None:
        raise StdlibDigestError(f"unknown ISO 286 tolerance band {canonical!r}")
    index = _step_index(nominal)
    upper_um = float(limits[0][index])
    lower_um = float(limits[1][index])
    upper_mm = upper_um * 1.0e-3
    lower_mm = lower_um * 1.0e-3
    return {
        "nominal_mm": nominal,
        "band": canonical,
        "upper_um": upper_um,
        "lower_um": lower_um,
        "upper_mm": upper_mm,
        "lower_mm": lower_mm,
        "max_limit_mm": nominal + upper_mm,
        "min_limit_mm": nominal + lower_mm,
    }


def build_stdlib_digest(
    text: str,
    contract: Any | None = None,
    *,
    materials_path: str | Path | None = None,
) -> str:
    """Build a JSON digest of only the standards cited by ``text`` and ``contract``.

    The UTF-8 encoding is at most ``DIGEST_LIMIT_BYTES`` (1.5 KiB). Cited sizes
    that are not in the catalog are listed under ``absent_*`` with no dimensions.
    """
    haystack = _haystack(text, contract)
    materials = load_material_records(materials_path)
    threads, absent_threads = _cited_threads(haystack)
    bearings = _cited_bearings(haystack)
    iso_rows = _cited_fits(haystack, contract, bearings)
    material_rows = _cited_materials(haystack, materials)
    standards = _cited_standards(haystack)
    payload: dict[str, Any] = {
        "source": "cad-stdlib+schemas/materials.json",
        "rule": "use these values verbatim; do not calculate missing sizes",
        "threads": threads,
        "bearings": bearings,
        "iso286": iso_rows,
        "materials": material_rows,
    }
    if absent_threads:
        payload["absent_threads"] = absent_threads
    if standards:
        payload["standards"] = standards
        if standards and not threads:
            payload["standards_note"] = "standard cited without a catalog thread size; diameters withheld"
    if not threads and not bearings and not iso_rows and not material_rows and not absent_threads:
        payload["note"] = "no catalog citation in the request"
    return _fit_digest(payload)


def _haystack(text: str, contract: Any | None) -> str:
    parts = [text or ""]
    if contract is None:
        return parts[0]
    to_dict = getattr(contract, "to_dict", None)
    if callable(to_dict):
        dumped = to_dict()
        if isinstance(dumped, Mapping):
            parts.append(json.dumps(dumped, ensure_ascii=False, default=str))
            return "\n".join(parts)
    if isinstance(contract, Mapping):
        parts.append(json.dumps(contract, ensure_ascii=False, default=str))
    else:
        parts.append(str(contract))
    return "\n".join(parts)


def _cited_standards(text: str) -> list[str]:
    found: list[str] = []
    if _CBORE_RE.search(text):
        found.append("iso4762_counterbore")
    if _CSINK_RE.search(text):
        found.append("iso10642_countersink")
    return found


def _cited_threads(text: str) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    grouped: dict[str, dict[str, Any]] = {}
    absent: dict[str, dict[str, Any]] = {}
    for match in _THREAD_RE.finditer(text):
        size_text = match.group(1) or match.group(2)
        if size_text is None:
            continue
        size = "M2.5" if size_text == "2.5" else f"M{size_text}"
        if size not in _CITED_SIZES:
            continue
        pitch = float(match.group(3)) if match.group(3) else None
        spec = size if pitch is None else f"{size}x{pitch}"
        row = _FASTENER_BY_SIZE.get(size)
        if row is None:
            absent.setdefault(
                size,
                {"spec": size, "status": "not_in_cad_stdlib_catalog"},
            )
            continue
        entry = grouped.get(size)
        if entry is None:
            entry = _fastener_payload(row)
            entry["requested"] = []
            grouped[size] = entry
        if pitch is None:
            continue
        drill = _tap_drill(size, pitch)
        request: dict[str, Any] = {"spec": spec, "pitch_mm": pitch}
        if drill is None:
            request["status"] = "pitch_not_in_catalog"
        else:
            request["tap_drill_mm"] = drill
        if request not in entry["requested"]:
            entry["requested"].append(request)
    threads = []
    for entry in grouped.values():
        if not entry["requested"]:
            entry.pop("requested")
        threads.append(entry)
    return threads, list(absent.values())


def _cited_bearings(text: str) -> list[dict[str, Any]]:
    hits: list[tuple[int, int, str]] = []
    for row in _BEARING_ROWS:
        designation = row[0]
        tokens = (designation, *row[1])
        for token in tokens:
            for match in _token_pattern(token).finditer(text):
                hits.append((match.start(), match.end(), designation))
    chosen: list[str] = []
    occupied: list[tuple[int, int]] = []
    for start, end, designation in sorted(hits, key=lambda item: (-(item[1] - item[0]), item[0])):
        if any(not (end <= left or start >= right) for left, right in occupied):
            continue
        occupied.append((start, end))
        if designation not in chosen:
            chosen.append(designation)
    payloads = []
    for designation in chosen:
        row = _bearing_row(designation)
        if row is not None:
            payloads.append(_bearing_payload(row, with_fit_limits=True))
    return payloads


def _cited_fits(
    text: str,
    contract: Any | None,
    bearings: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    seen: set[tuple[float, str]] = set()

    def add(nominal: float | None, band: str) -> None:
        if band not in _ISO_BANDS:
            return
        key = (-1.0 if nominal is None else round(float(nominal), 6), band)
        if key in seen:
            return
        seen.add(key)
        if nominal is None:
            found.append({"band": band, "status": "nominal_required"})
            return
        try:
            found.append(lookup_iso286(nominal, band))
        except StdlibDigestError as exc:
            found.append(
                {
                    "nominal_mm": float(nominal),
                    "band": band,
                    "status": "not_in_catalog",
                    "reason": str(exc),
                }
            )

    numbers = [(match.start(), match.end(), float(match.group(1))) for match in _NUMBER_RE.finditer(text)]
    for match in _BAND_RE.finditer(text):
        band = match.group(1)
        nominal = _nearest_nominal(numbers, match.start(), match.end())
        if nominal is None:
            nominal = _nominal_from_bearings(band, bearings)
        add(nominal, band)
    for nominal, band in _interface_fits(contract):
        add(nominal, band)
    return found


def _cited_materials(text: str, materials: list[dict[str, Any]]) -> list[dict[str, Any]]:
    folded = text.casefold()
    found: list[dict[str, Any]] = []
    seen: set[str] = set()
    for row in materials:
        material_id = str(row.get("id") or "")
        if not material_id or material_id in seen:
            continue
        tokens = [material_id]
        aliases = row.get("aliases")
        if isinstance(aliases, list):
            tokens.extend(str(alias) for alias in aliases if str(alias).strip())
        name = row.get("name")
        if isinstance(name, str) and name.strip():
            tokens.append(name)
        tokens.extend(_MATERIAL_SEARCH_EXTRAS.get(material_id, ()))
        if any(_token_in(folded, token) for token in tokens):
            seen.add(material_id)
            found.append(_material_payload(row))
    return found


def _interface_fits(contract: Any | None) -> list[tuple[float, str]]:
    interfaces: list[Any] = []
    if contract is None:
        return []
    raw_interfaces = getattr(contract, "interfaces", None)
    if isinstance(raw_interfaces, list):
        interfaces = raw_interfaces
    elif isinstance(contract, Mapping) and isinstance(contract.get("interfaces"), list):
        interfaces = list(contract["interfaces"])
    fits: list[tuple[float, str]] = []
    for iface in interfaces:
        if isinstance(iface, Mapping):
            tolerance = iface.get("tolerance")
            nominal = iface.get("nominal_mm")
            diameter = iface.get("diameter_mm")
        else:
            tolerance = getattr(iface, "tolerance", None)
            nominal = getattr(iface, "nominal_mm", None)
            diameter = getattr(iface, "diameter_mm", None)
        if not isinstance(tolerance, str) or tolerance.strip() not in _ISO_BANDS:
            continue
        size = diameter if _is_positive_number(diameter) else nominal
        if not _is_positive_number(size):
            continue
        fits.append((float(size), tolerance.strip()))
    return fits


def _nearest_nominal(
    numbers: list[tuple[int, int, float]],
    start: int,
    end: int,
) -> float | None:
    best: tuple[int, float] | None = None
    for num_start, num_end, value in numbers:
        if value < 1.0 or value > _STEP_EDGE_MM[-1]:
            continue
        if num_end < start:
            distance = start - num_end
        elif num_start > end:
            distance = num_start - end
        else:
            distance = 0
        if distance > 32:
            continue
        if best is None or distance < best[0]:
            best = (distance, value)
    return None if best is None else best[1]


def _nominal_from_bearings(band: str, bearings: list[dict[str, Any]]) -> float | None:
    for bearing in bearings:
        if bearing.get("shaft_fit") == band and _is_positive_number(bearing.get("bore_d_mm")):
            return float(bearing["bore_d_mm"])
        if bearing.get("housing_fit") == band and _is_positive_number(bearing.get("outer_d_mm")):
            return float(bearing["outer_d_mm"])
    return None


def _fastener_payload(
    row: tuple[str, float, float, float, float, float, float, float, float],
) -> dict[str, Any]:
    size, pitch, tap, close, normal, loose, cbore_d, cbore_depth, csink_d = row
    return {
        "size": size,
        "pitch_mm": pitch,
        "tap_drill_mm": tap,
        "clearance_close_mm": close,
        "clearance_normal_mm": normal,
        "clearance_loose_mm": loose,
        "cbore_diameter_mm": cbore_d,
        "cbore_depth_mm": cbore_depth,
        "csink_diameter_mm": csink_d,
        "csink_angle_deg": 90.0,
    }


def _bearing_payload(
    row: tuple[str, tuple[str, ...], float, float, float, str, str, float, float],
    *,
    with_fit_limits: bool,
) -> dict[str, Any]:
    designation, _aliases, bore, outer, width, shaft_fit, housing_fit, shoulder, housing_shoulder = row
    payload: dict[str, Any] = {
        "designation": designation,
        "bore_d_mm": bore,
        "outer_d_mm": outer,
        "width_mm": width,
        "shaft_fit": shaft_fit,
        "housing_fit": housing_fit,
        "shoulder_d_min_mm": shoulder,
        "housing_shoulder_d_max_mm": housing_shoulder,
    }
    if with_fit_limits:
        payload["shaft_limits"] = _fit_limits(bore, shaft_fit)
        payload["housing_limits"] = _fit_limits(outer, housing_fit)
    return payload


def _fit_limits(nominal_mm: float, band: str) -> dict[str, Any]:
    try:
        limits = lookup_iso286(nominal_mm, band)
    except StdlibDigestError as exc:
        return {"band": band, "status": "not_in_catalog", "reason": str(exc)}
    return {
        "band": limits["band"],
        "upper_um": limits["upper_um"],
        "lower_um": limits["lower_um"],
        "max_limit_mm": limits["max_limit_mm"],
        "min_limit_mm": limits["min_limit_mm"],
    }


def _material_payload(row: Mapping[str, Any]) -> dict[str, Any]:
    payload: dict[str, Any] = {"id": str(row.get("id"))}
    for key in ("name", "density_g_cm3", "yield_strength_mpa", "machinability_index"):
        if key in row and row.get(key) is not None:
            payload[key] = row[key]
    return payload


def _parse_thread_spec(spec: str) -> tuple[str, float | None]:
    match = _THREAD_RE.fullmatch(spec.strip())
    if match is None:
        raise StdlibDigestError(f"unknown fastener specification {spec.strip()!r}")
    size_text = match.group(1) or match.group(2)
    size = "M2.5" if size_text == "2.5" else f"M{size_text}"
    pitch = float(match.group(3)) if match.group(3) else None
    return size, pitch


def _tap_drill(size: str, pitch_mm: float) -> float | None:
    for row_size, pitch, drill, _coarse in _TAP_DRILL_ROWS:
        if row_size == size and abs(pitch - pitch_mm) <= 1e-6:
            return drill
    return None


def _step_index(nominal_mm: float) -> int:
    for index, edge in enumerate(_STEP_EDGE_MM):
        if nominal_mm <= edge:
            return index
    raise StdlibDigestError(
        f"nominal size {nominal_mm} mm is outside the ISO 286 table (1 mm through 250 mm)"
    )


def _bearing_row(
    designation: str,
) -> tuple[str, tuple[str, ...], float, float, float, str, str, float, float] | None:
    normalized = _normalize_designation(designation)
    if not normalized:
        return None
    for row in _BEARING_ROWS:
        tokens = (row[0], *row[1])
        if any(_normalize_designation(token) == normalized for token in tokens):
            return row
    return None


def _normalize_designation(designation: str) -> str:
    return "".join(
        ch.upper()
        for ch in designation
        if not ch.isspace() and ch not in "-_"
    )


def _token_pattern(token: str) -> re.Pattern[str]:
    body = re.escape(token).replace(r"\-", r"[\s_-]*").replace(r"\_", r"[\s_-]*")
    return re.compile(rf"(?<![A-Za-z0-9]){body}(?![A-Za-z0-9])", re.IGNORECASE)


def _token_in(folded_haystack: str, token: str) -> bool:
    needle = token.casefold()
    if not needle:
        return False
    start = 0
    while True:
        index = folded_haystack.find(needle, start)
        if index < 0:
            return False
        end = index + len(needle)
        before = folded_haystack[index - 1] if index else ""
        after = folded_haystack[end] if end < len(folded_haystack) else ""
        if _is_boundary(before) and _is_boundary(after):
            return True
        start = index + 1


def _is_boundary(char: str) -> bool:
    return not char or not (char.isalnum() or char in "._-")


def _is_positive_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and float(value) > 0.0


def _encode(payload: Mapping[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def _encoded_size(payload: Mapping[str, Any]) -> int:
    return len(_encode(payload).encode("utf-8"))


def _fit_digest(payload: dict[str, Any]) -> str:
    """Drop lower-priority detail until the digest fits. Never invent replacements."""
    if _encoded_size(payload) <= DIGEST_LIMIT_BYTES:
        return _encode(payload)

    for bearing in payload.get("bearings") or []:
        if isinstance(bearing, dict):
            bearing.pop("shaft_limits", None)
            bearing.pop("housing_limits", None)
    payload["truncated"] = True
    if _encoded_size(payload) <= DIGEST_LIMIT_BYTES:
        return _encode(payload)

    for key in ("iso286", "materials", "bearings", "threads"):
        rows = payload.get(key)
        if not isinstance(rows, list):
            continue
        while rows and _encoded_size(payload) > DIGEST_LIMIT_BYTES:
            rows.pop()
        if _encoded_size(payload) <= DIGEST_LIMIT_BYTES:
            return _encode(payload)

    payload.pop("standards_note", None)
    payload.pop("note", None)
    if _encoded_size(payload) <= DIGEST_LIMIT_BYTES:
        return _encode(payload)

    minimal = {
        "source": payload.get("source"),
        "rule": payload.get("rule"),
        "truncated": True,
        "absent_threads": payload.get("absent_threads", []),
    }
    text = _encode(minimal)
    if len(text.encode("utf-8")) <= DIGEST_LIMIT_BYTES:
        return text
    return _encode({"source": "cad-stdlib+schemas/materials.json", "truncated": True})
