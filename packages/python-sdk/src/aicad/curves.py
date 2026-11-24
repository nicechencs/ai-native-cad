"""Exact curve generators and the compact spline blob they write to disk.

The blob layout is little-endian and has no padding:

``SPLN`` + version ``u32`` + dimension ``u32`` + degree ``u32`` + closed ``u8``
+ point count ``u32`` + one ``f64`` per coordinate.
"""

from __future__ import annotations

import hashlib
import math
import struct
from collections.abc import Sequence
from numbers import Real

SPLINE_BLOB_MAGIC = b"SPLN"
SPLINE_BLOB_VERSION = 1
_HEADER = struct.Struct("<4sIIIBI")

# Closed trailing edge. The open-edge NACA polynomial uses -0.1015 here and
# leaves a small gap at the trailing edge.
_THICKNESS_A4 = -0.1036


def _real(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(f"{label} must be a real number")
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"{label} must be finite")
    return 0.0 if number == 0.0 else number


def _naca_thickness(x: float, thickness: float) -> float:
    return 5.0 * thickness * (
        0.2969 * math.sqrt(max(x, 0.0))
        - 0.1260 * x
        - 0.3516 * x * x
        + 0.2843 * x * x * x
        + _THICKNESS_A4 * x * x * x * x
    )


def _naca_camber(x: float, camber: float, position: float) -> tuple[float, float]:
    """Return the mean-line height and its slope angle at chord fraction ``x``."""
    if camber == 0.0 or position in (0.0, 1.0):
        return 0.0, 0.0
    if x < position:
        height = camber / (position * position) * (2.0 * position * x - x * x)
        slope = 2.0 * camber / (position * position) * (position - x)
    else:
        remainder = (1.0 - position) ** 2
        height = (
            camber
            / remainder
            * ((1.0 - 2.0 * position) + 2.0 * position * x - x * x)
        )
        slope = 2.0 * camber / remainder * (position - x)
    return height, math.atan(slope)


def generate_naca4_airfoil(
    code: str = "2412",
    chord_mm: float = 100.0,
    num_points: int = 40,
) -> list[tuple[float, float]]:
    """Return a closed NACA 4-digit airfoil outline, in millimetres.

    ``num_points`` is the number of cosine-spaced stations along the chord,
    including the leading and trailing edges. The outline starts at the
    trailing edge, follows the upper surface through the leading edge, then
    the lower surface back toward the trailing edge. The repeated closing
    point is omitted so a closed spline does not contain a zero-length span.
    """
    digits = code.strip()
    if len(digits) != 4 or not digits.isdigit():
        raise ValueError(f"NACA 4-digit code required, got {code!r}")
    if isinstance(num_points, bool) or not isinstance(num_points, int) or num_points < 2:
        raise ValueError("num_points must be an integer >= 2")
    chord = _real(chord_mm, "chord_mm")
    if chord <= 0.0:
        raise ValueError("chord_mm must be positive")

    camber = int(digits[0]) / 100.0
    position = int(digits[1]) / 10.0
    thickness = int(digits[2:]) / 100.0
    upper: list[tuple[float, float]] = []
    lower: list[tuple[float, float]] = []
    for index in range(num_points):
        beta = math.pi * index / (num_points - 1)
        x = 0.5 * (1.0 - math.cos(beta))
        half_thickness = _naca_thickness(x, thickness)
        height, theta = _naca_camber(x, camber, position)
        sine = math.sin(theta)
        cosine = math.cos(theta)
        upper.append((x - half_thickness * sine, height + half_thickness * cosine))
        lower.append((x + half_thickness * sine, height - half_thickness * cosine))

    outline = list(reversed(upper)) + lower[1:]
    if len(outline) >= 2 and _near(outline[0], outline[-1]):
        outline.pop()
    return [(x * chord, y * chord) for x, y in outline]


def generate_involute_gear_tooth(
    module_mm: float = 2.0,
    teeth: int = 20,
    pressure_angle_deg: float = 20.0,
    samples_per_flank: int = 12,
) -> list[tuple[float, float]]:
    """Return one full-depth involute tooth outline, centred on +X.

    Addendum is one module and dedendum is 1.25 modules. Each flank is the
    involute of the base circle, sampled uniformly in roll angle from the
    base circle to the tip. When the root sits inside the base circle, a
    radial line joins the root circle to the start of the involute.
    """
    module = _real(module_mm, "module_mm")
    pressure = _real(pressure_angle_deg, "pressure_angle_deg")
    if module <= 0.0:
        raise ValueError("module_mm must be positive")
    if isinstance(teeth, bool) or not isinstance(teeth, int) or teeth < 3:
        raise ValueError("teeth must be an integer >= 3")
    if (
        isinstance(samples_per_flank, bool)
        or not isinstance(samples_per_flank, int)
        or samples_per_flank < 2
    ):
        raise ValueError("samples_per_flank must be an integer >= 2")
    if not 0.0 <= pressure < 90.0:
        raise ValueError("pressure_angle_deg must be in [0, 90)")

    phi = math.radians(pressure)
    pitch_radius = module * teeth / 2.0
    base_radius = pitch_radius * math.cos(phi)
    tip_radius = pitch_radius + module
    root_radius = pitch_radius - 1.25 * module
    if base_radius <= 0.0 or root_radius <= 0.0:
        raise ValueError("tooth geometry is not positive; increase the tooth count")

    tip_roll = math.sqrt((tip_radius / base_radius) ** 2 - 1.0)
    pitch_roll = math.tan(phi)
    pitch_x, pitch_y = _involute_point(base_radius, pitch_roll)
    pitch_polar = math.atan2(pitch_y, pitch_x)
    # Place the pitch point half a tooth-thickness angle off the +X centerline.
    rotation = math.pi / (2.0 * teeth) - pitch_polar

    right: list[tuple[float, float]] = []
    if root_radius < base_radius:
        right.append(_rotate(root_radius, 0.0, rotation))
    for index in range(samples_per_flank):
        roll = tip_roll * index / (samples_per_flank - 1)
        point_x, point_y = _involute_point(base_radius, roll)
        right.append(_rotate(point_x, point_y, rotation))
    left = [(x, -y) for x, y in reversed(right)]
    if right and left and _near(right[-1], left[0]):
        left = left[1:]
    return right + left


def generate_helix_3d(
    radius_mm: float,
    pitch_mm: float,
    turns: float,
    samples_per_turn: int = 24,
) -> list[tuple[float, float, float]]:
    """Return a right-handed helix. ``pitch_mm`` is the rise of one full turn."""
    radius = _real(radius_mm, "radius_mm")
    pitch = _real(pitch_mm, "pitch_mm")
    turn_count = _real(turns, "turns")
    if radius < 0.0:
        raise ValueError("radius_mm must be >= 0")
    if turn_count < 0.0:
        raise ValueError("turns must be >= 0")
    if (
        isinstance(samples_per_turn, bool)
        or not isinstance(samples_per_turn, int)
        or samples_per_turn < 1
    ):
        raise ValueError("samples_per_turn must be an integer >= 1")
    if turn_count == 0.0:
        return [(radius, 0.0, 0.0)]

    steps = max(1, int(round(samples_per_turn * turn_count)))
    points: list[tuple[float, float, float]] = []
    for index in range(steps + 1):
        fraction = index / steps
        theta = 2.0 * math.pi * turn_count * fraction
        points.append(
            (
                radius * math.cos(theta),
                radius * math.sin(theta),
                pitch * turn_count * fraction,
            )
        )
    return points


def encode_spline_blob(
    points: Sequence[Sequence[float]],
    degree: int = 3,
    closed: bool = False,
) -> bytes:
    """Encode control points as a deterministic ``SPLN`` blob."""
    if isinstance(degree, bool) or not isinstance(degree, int) or not 1 <= degree <= 16:
        raise ValueError("degree must be an integer from 1 to 16")
    if not isinstance(closed, bool):
        raise TypeError("closed must be a bool")
    if len(points) < 2:
        raise ValueError("a spline blob needs at least two points")

    dimension = len(points[0])
    if dimension < 1:
        raise ValueError("points need at least one coordinate")
    flat: list[float] = []
    for index, point in enumerate(points):
        if len(point) != dimension:
            raise ValueError(f"point {index} has {len(point)} coordinates, expected {dimension}")
        flat.extend(_real(coordinate, f"point {index}") for coordinate in point)

    header = _HEADER.pack(
        SPLINE_BLOB_MAGIC,
        SPLINE_BLOB_VERSION,
        dimension,
        degree,
        int(closed),
        len(points),
    )
    coordinates = struct.pack("<" + "d" * len(flat), *flat)
    return header + coordinates


def decode_spline_blob(data: bytes) -> dict[str, object]:
    """Read a blob written by :func:`encode_spline_blob`."""
    if len(data) < _HEADER.size:
        raise ValueError("spline blob is truncated")
    magic, version, dimension, degree, closed_byte, count = _HEADER.unpack_from(data)
    if magic != SPLINE_BLOB_MAGIC:
        raise ValueError("spline blob magic is not SPLN")
    if version != SPLINE_BLOB_VERSION:
        raise ValueError(f"unsupported spline blob version {version}")
    if dimension < 1 or closed_byte not in (0, 1):
        raise ValueError("spline blob header is invalid")
    expected = _HEADER.size + count * dimension * 8
    if len(data) != expected:
        raise ValueError("spline blob length does not match its header")
    if count == 0:
        coordinates: tuple[float, ...] = ()
    else:
        coordinates = struct.unpack_from("<" + "d" * (count * dimension), data, _HEADER.size)
    points = [
        coordinates[start : start + dimension]
        for start in range(0, len(coordinates), dimension)
    ]
    return {
        "version": version,
        "degree": degree,
        "closed": bool(closed_byte),
        "points": [tuple(point) for point in points],
    }


def sha256_hex(data: bytes) -> str:
    """Return the lowercase SHA-256 hex digest used as a blob address."""
    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("sha256_hex expects bytes")
    return hashlib.sha256(bytes(data)).hexdigest()


def _involute_point(base_radius: float, roll: float) -> tuple[float, float]:
    """Classic circle involute. ``roll`` is the unwrapped angle in radians."""
    return (
        base_radius * (math.cos(roll) + roll * math.sin(roll)),
        base_radius * (math.sin(roll) - roll * math.cos(roll)),
    )


def _rotate(x: float, y: float, angle: float) -> tuple[float, float]:
    cosine = math.cos(angle)
    sine = math.sin(angle)
    return (x * cosine - y * sine, x * sine + y * cosine)


def _near(left: tuple[float, ...], right: tuple[float, ...], tolerance: float = 1e-12) -> bool:
    return len(left) == len(right) and all(
        abs(a - b) <= tolerance for a, b in zip(left, right, strict=True)
    )
