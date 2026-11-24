"""Dimensioned values that serialize to the Graph IR Quantity shape.

Lengths add in millimetres, angles in degrees, forces in newtons, and
pressures in megapascals. The stored ``value`` stays in the unit you asked
for. Adding millimetres to inches is allowed. Adding a length to an angle
raises ``TypeError``.
"""

from __future__ import annotations

import math
from dataclasses import dataclass
from numbers import Real

# Multiply a stored value by this factor to reach the graph's internal unit.
_TO_CANONICAL: dict[tuple[str, str | None], float] = {
    ("length", "mm"): 1.0,
    ("length", "m"): 1_000.0,
    ("length", "in"): 25.4,
    ("length", "um"): 0.001,
    ("length", "cm"): 10.0,
    ("angle", "deg"): 1.0,
    ("angle", "rad"): 180.0 / math.pi,
    ("force", "N"): 1.0,
    ("force", "kN"): 1_000.0,
    ("pressure", "MPa"): 1.0,
    ("pressure", "Pa"): 1e-6,
    ("mass", "g"): 0.001,
    ("mass", "kg"): 1.0,
    ("density", None): 1.0,
}

_UNITS: dict[str, set[str | None]] = {}
for (_dimension, unit) in _TO_CANONICAL:
    _UNITS.setdefault(_dimension, set()).add(unit)


def _finite(value: float, label: str) -> float:
    if not math.isfinite(value):
        raise ValueError(f"{label} must be finite")
    # JSON and the Rust writer both treat -0 and +0 as the same number.
    return 0.0 if value == 0.0 else value


def _real(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, Real):
        raise TypeError(f"{label} must be a real number")
    return _finite(float(value), label)


@dataclass(frozen=True, slots=True)
class Quantity:
    """One physical value: dimension, number, and display unit.

    Density is stored in kilograms per cubic metre and has no unit field.
    """

    dimension: str
    value: float
    unit: str | None = None

    def __post_init__(self) -> None:
        allowed = _UNITS.get(self.dimension)
        if allowed is None or self.unit not in allowed:
            raise ValueError(
                f"unsupported quantity {self.dimension!r} in unit {self.unit!r}"
            )
        object.__setattr__(self, "value", _real(self.value, self.dimension))

    def canonical_value(self) -> float:
        """Return this value in the graph's internal unit for its dimension."""
        return _finite(
            self.value * _TO_CANONICAL[(self.dimension, self.unit)],
            self.dimension,
        )

    def to_json(self) -> dict[str, object]:
        """Return the Quantity object required by ``graph-ir.schema.json``."""
        payload: dict[str, object] = {
            "dimension": self.dimension,
            "value": self.value,
        }
        if self.dimension != "density":
            payload["unit"] = self.unit
        return payload

    def scale(self, factor: object) -> Quantity:
        """Multiply by a plain number. The display unit stays the same."""
        if isinstance(factor, Quantity):
            raise TypeError(f"cannot scale {self.dimension} by {factor.dimension}")
        scaled = self.value * _real(factor, "scale factor")
        return Quantity(self.dimension, scaled, self.unit)

    def __add__(self, other: object) -> Quantity:
        return self._combine(other, 1.0, "add")

    def __sub__(self, other: object) -> Quantity:
        return self._combine(other, -1.0, "subtract")

    def __mul__(self, other: object) -> Quantity:
        return self.scale(other)

    def __rmul__(self, other: object) -> Quantity:
        return self.scale(other)

    def __truediv__(self, other: object) -> Quantity | float:
        if isinstance(other, Quantity):
            if other.dimension != self.dimension:
                raise TypeError(
                    f"cannot divide {self.dimension} by {other.dimension}"
                )
            denominator = other.canonical_value()
            if denominator == 0.0:
                raise ZeroDivisionError("cannot divide by a zero quantity")
            return _finite(self.canonical_value() / denominator, "ratio")
        return self.scale(1.0 / _real(other, "divisor"))

    def _combine(self, other: object, sign: float, verb: str) -> Quantity:
        if not isinstance(other, Quantity):
            raise TypeError(f"expected a Quantity, got {type(other).__name__}")
        if other.dimension != self.dimension:
            raise TypeError(f"cannot {verb} {self.dimension} and {other.dimension}")
        total = self.canonical_value() + sign * other.canonical_value()
        per_unit = _TO_CANONICAL[(self.dimension, self.unit)]
        return Quantity(self.dimension, total / per_unit, self.unit)


def mm(value: float) -> Quantity:
    """Length in millimetres."""
    return Quantity("length", value, "mm")


def inch(value: float) -> Quantity:
    """Length in inches. The JSON unit code is ``in``."""
    return Quantity("length", value, "in")


def meter(value: float) -> Quantity:
    """Length in metres. The JSON unit code is ``m``."""
    return Quantity("length", value, "m")


def deg(value: float) -> Quantity:
    """Angle in degrees."""
    return Quantity("angle", value, "deg")


def rad(value: float) -> Quantity:
    """Angle in radians."""
    return Quantity("angle", value, "rad")


def newton(value: float) -> Quantity:
    """Force in newtons. The JSON unit code is ``N``."""
    return Quantity("force", value, "N")


def mpa(value: float) -> Quantity:
    """Pressure in megapascals. The JSON unit code is ``MPa``."""
    return Quantity("pressure", value, "MPa")


def kg(value: float) -> Quantity:
    """Mass in kilograms. The JSON unit code is ``kg``."""
    return Quantity("mass", value, "kg")


def g(value: float) -> Quantity:
    """Mass in grams. The JSON unit code is ``g``."""
    return Quantity("mass", value, "g")
