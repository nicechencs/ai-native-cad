"""Little-endian ``SUBD`` v1 blobs for ``blobs/subd/`` payloads.

The byte layout matches ``crates/cad-subd/src/codec.rs``:

``SUBD`` + version ``u32`` + vertex count ``u32`` + face count ``u32``
+ crease count ``u32`` + ``xyz`` vertices as ``f64`` + each face as a
``u32`` corner count and ``u32`` indices + each crease as ``u32`` ``u32``
``f32``. Counts and coordinates are little-endian. Crease keys are
undirected edges stored with the smaller index first, and the crease
table is written in that sorted order.
"""

from __future__ import annotations

import math
import struct
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from numbers import Real

SUBD_MAGIC = b"SUBD"
SUBD_VERSION = 1
CREASE_SMOOTH = 0.0
CREASE_INFINITE = 10.0

_HEADER = struct.Struct("<4sIIII")
_VERTEX = struct.Struct("<3d")
_U32 = struct.Struct("<I")
_CREASE = struct.Struct("<IIf")
_HEADER_LEN = _HEADER.size
_VERTEX_STRIDE = _VERTEX.size
_CREASE_STRIDE = _CREASE.size
_MIN_FACE_STRIDE = 16


@dataclass(slots=True)
class SubDCage:
    """Polygon control cage: vertices, n-gon faces, and crease weights.

    ``edge_creases`` uses an undirected key ``(min(a, b), max(a, b))``.
    Sharpness ``0.0`` is smooth and sharpness ``10.0`` is infinitely sharp.
    """

    vertices: list[tuple[float, float, float]]
    faces: list[list[int]]
    edge_creases: dict[tuple[int, int], float] = field(default_factory=dict)

    @classmethod
    def unit_cube(cls, size_mm: float) -> SubDCage:
        """Axis-aligned cube centered at the origin.

        ``size_mm`` is the edge length. Corners sit at ±``size_mm / 2``.
        Faces wind so their normals point outward.
        """
        half = _real(size_mm, "size_mm") * 0.5
        return cls(
            vertices=[
                (-half, -half, -half),
                (half, -half, -half),
                (half, half, -half),
                (-half, half, -half),
                (-half, -half, half),
                (half, -half, half),
                (half, half, half),
                (-half, half, half),
            ],
            faces=[
                [0, 3, 2, 1],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [3, 7, 6, 2],
                [0, 4, 7, 3],
                [1, 2, 6, 5],
            ],
        )

    def set_crease(self, a: int, b: int, sharpness: float) -> None:
        """Store a crease on the undirected edge ``(a, b)``.

        The key is stored as ``(min, max)``. The edge must already exist
        on a face, and ``sharpness`` must lie in ``0.0..=10.0``.
        """
        key = _normalized_key(a, b)
        weight = _sharpness(sharpness)
        vertex_count = len(self.vertices)
        if key[0] >= vertex_count or key[1] >= vertex_count:
            raise ValueError(
                f"crease ({key[0]}, {key[1]}) references a vertex outside 0..{vertex_count}"
            )
        if key not in _face_edges(self.faces):
            raise ValueError(f"crease ({key[0]}, {key[1]}) is not an edge of any face")
        self.edge_creases.pop((key[1], key[0]), None)
        self.edge_creases[key] = weight


def encode_subd_blob(cage: SubDCage) -> bytes:
    """Encode a validated cage. The same cage always produces the same bytes."""
    if not isinstance(cage, SubDCage):
        raise TypeError("cage must be a SubDCage")
    vertices = [_vertex(vertex, index) for index, vertex in enumerate(_as_sequence(cage.vertices, "vertices"))]
    faces = [_face(face, index) for index, face in enumerate(_as_sequence(cage.faces, "faces"))]
    creases = _normalize_creases(cage.edge_creases)
    _validate(vertices, faces, creases)

    out = bytearray()
    out += _HEADER.pack(SUBD_MAGIC, SUBD_VERSION, len(vertices), len(faces), len(creases))
    for vertex in vertices:
        out += _VERTEX.pack(*vertex)
    for face in faces:
        out += _U32.pack(len(face))
        for index in face:
            out += _U32.pack(index)
    for (start, end), weight in sorted(creases.items()):
        out += _CREASE.pack(start, end, weight)
    return bytes(out)


def decode_subd_blob(data: bytes) -> SubDCage:
    """Decode a version-1 blob. Truncation, bad magic, and non-finite numbers fail."""
    blob = _as_bytes(data)
    if len(blob) < 4:
        raise ValueError("SubD blob is truncated")
    if blob[:4] != SUBD_MAGIC:
        raise ValueError("SubD blob magic is not SUBD")
    if len(blob) < _HEADER_LEN:
        raise ValueError("SubD blob is truncated")

    reader = _Reader(blob)
    reader.take(4)
    version = reader.u32()
    if version != SUBD_VERSION:
        raise ValueError(f"SubD blob version {version} is not supported")
    vertex_count = reader.u32()
    face_count = reader.u32()
    crease_count = reader.u32()

    vertex_bytes = _checked_mul(vertex_count, _VERTEX_STRIDE)
    if reader.remaining() < vertex_bytes:
        raise ValueError("SubD blob is truncated")
    vertices = [reader.vertex() for _ in range(vertex_count)]

    if reader.remaining() < _checked_mul(face_count, _MIN_FACE_STRIDE):
        raise ValueError("SubD blob is truncated")
    faces: list[list[int]] = []
    for face_index in range(face_count):
        corner_count = reader.u32()
        if corner_count < 3:
            raise ValueError(
                f"face {face_index} has {corner_count} vertices; at least 3 are required"
            )
        index_bytes = _checked_mul(corner_count, 4)
        if reader.remaining() < index_bytes:
            raise ValueError("SubD blob is truncated")
        faces.append([reader.u32() for _ in range(corner_count)])

    crease_bytes = _checked_mul(crease_count, _CREASE_STRIDE)
    if reader.remaining() < crease_bytes:
        raise ValueError("SubD blob is truncated")
    if reader.remaining() != crease_bytes:
        raise ValueError(f"SubD blob has {reader.remaining() - crease_bytes} trailing bytes")

    creases: dict[tuple[int, int], float] = {}
    for _ in range(crease_count):
        start = reader.u32()
        end = reader.u32()
        weight = reader.f32()
        if start >= end:
            raise ValueError(
                f"crease ({start}, {end}) must be stored as an ordered pair with a < b"
            )
        if (start, end) in creases:
            raise ValueError(f"duplicate crease ({start}, {end})")
        creases[(start, end)] = weight
    if reader.remaining() != 0:
        raise ValueError(f"SubD blob has {reader.remaining()} trailing bytes")

    _validate(vertices, faces, creases)
    return SubDCage(vertices=vertices, faces=faces, edge_creases=creases)


class _Reader:
    def __init__(self, blob: bytes) -> None:
        self._blob = blob
        self._pos = 0

    def remaining(self) -> int:
        return len(self._blob) - self._pos

    def take(self, size: int) -> bytes:
        end = self._pos + size
        if size < 0 or end > len(self._blob):
            raise ValueError("SubD blob is truncated")
        chunk = self._blob[self._pos : end]
        self._pos = end
        return chunk

    def u32(self) -> int:
        return _U32.unpack(self.take(4))[0]

    def f32(self) -> float:
        return struct.unpack("<f", self.take(4))[0]

    def f64(self) -> float:
        return struct.unpack("<d", self.take(8))[0]

    def vertex(self) -> tuple[float, float, float]:
        return _VERTEX.unpack(self.take(_VERTEX_STRIDE))


def _validate(
    vertices: Sequence[tuple[float, float, float]],
    faces: Sequence[Sequence[int]],
    creases: Mapping[tuple[int, int], float],
) -> None:
    if not vertices:
        raise ValueError("control cage has no vertices")
    for index, vertex in enumerate(vertices):
        if not all(math.isfinite(component) for component in vertex):
            raise ValueError(f"vertex {index} has a non-finite coordinate")

    vertex_count = len(vertices)
    for face_index, face in enumerate(faces):
        if len(face) < 3:
            raise ValueError(
                f"face {face_index} has {len(face)} vertices; at least 3 are required"
            )
        for vertex in face:
            if vertex >= vertex_count:
                raise ValueError(
                    f"face {face_index} references vertex {vertex} "
                    f"but the cage has {vertex_count} vertices"
                )
        for left in range(len(face)):
            for right in range(left + 1, len(face)):
                if face[left] == face[right]:
                    raise ValueError(f"face {face_index} repeats vertex {face[left]}")

    edges = _face_edges(faces)
    for (start, end), weight in creases.items():
        if start >= end:
            raise ValueError(
                f"crease ({start}, {end}) must be stored as an ordered pair with a < b"
            )
        _sharpness(weight)
        if start >= vertex_count or end >= vertex_count:
            raise ValueError(
                f"crease ({start}, {end}) references a vertex outside 0..{vertex_count}"
            )
        if (start, end) not in edges:
            raise ValueError(f"crease ({start}, {end}) is not an edge of any face")


def _normalize_creases(creases: object) -> dict[tuple[int, int], float]:
    if not isinstance(creases, Mapping):
        raise TypeError("edge_creases must be a mapping")
    normalized: dict[tuple[int, int], float] = {}
    for raw_key, raw_weight in creases.items():
        key = _normalized_key(*_key_pair(raw_key))
        if key in normalized:
            raise ValueError(f"duplicate crease ({key[0]}, {key[1]})")
        normalized[key] = _sharpness(raw_weight)
    return normalized


def _face_edges(faces: Sequence[Sequence[int]]) -> set[tuple[int, int]]:
    edges: set[tuple[int, int]] = set()
    for face in faces:
        corner_count = len(face)
        if corner_count < 2:
            continue
        for corner in range(corner_count):
            start = face[corner]
            end = face[(corner + 1) % corner_count]
            if start == end:
                continue
            edges.add((start, end) if start < end else (end, start))
    return edges


def _normalized_key(a: object, b: object) -> tuple[int, int]:
    start = _u32(a, "crease endpoint")
    end = _u32(b, "crease endpoint")
    if start == end:
        raise ValueError(f"crease ({start}, {end}) must be stored as an ordered pair with a < b")
    return (start, end) if start < end else (end, start)


def _key_pair(key: object) -> tuple[object, object]:
    if isinstance(key, (str, bytes)) or not isinstance(key, Sequence) or len(key) != 2:
        raise TypeError("crease key must be a pair of vertex indices")
    return key[0], key[1]


def _sharpness(value: object) -> float:
    number = _real(value, "crease sharpness")
    if not math.isfinite(number):
        raise ValueError("crease sharpness is not finite")
    if not CREASE_SMOOTH <= number <= CREASE_INFINITE:
        raise ValueError(f"crease sharpness {number} is outside 0.0..=10.0")
    return number


def _vertex(vertex: object, index: int) -> tuple[float, float, float]:
    if isinstance(vertex, (str, bytes)) or not isinstance(vertex, Sequence) or len(vertex) != 3:
        raise ValueError(f"vertex {index} must have 3 coordinates")
    return (
        _coordinate(vertex[0], index),
        _coordinate(vertex[1], index),
        _coordinate(vertex[2], index),
    )


def _face(face: object, index: int) -> list[int]:
    if isinstance(face, (str, bytes)) or not isinstance(face, Sequence):
        raise TypeError(f"face {index} must be a sequence of vertex indices")
    return [_u32(vertex, f"face {index} vertex") for vertex in face]


def _coordinate(value: object, index: int) -> float:
    number = _real(value, f"vertex {index} coordinate")
    if not math.isfinite(number):
        raise ValueError(f"vertex {index} has a non-finite coordinate")
    return number


def _u32(value: object, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{label} must be an integer")
    if not 0 <= value <= 0xFFFFFFFF:
        raise ValueError(f"{label} does not fit in u32")
    return value


def _real(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(f"{label} must be a real number")
    return float(value)


def _as_sequence(value: object, label: str) -> Sequence[object]:
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise TypeError(f"{label} must be a sequence")
    return value


def _as_bytes(value: object) -> bytes:
    if not isinstance(value, (bytes, bytearray, memoryview)):
        raise TypeError("SubD blob must be bytes")
    return bytes(value)


def _checked_mul(count: int, stride: int) -> int:
    if count > (2**64 - 1) // stride:
        raise ValueError("SubD blob count does not fit the payload")
    return count * stride
