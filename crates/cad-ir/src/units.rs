use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{IrError, IrType};

/// Length units accepted by the graph IR.  Internally, lengths are represented
/// in millimetres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LengthUnit {
    Mm,
    M,
    In,
    Um,
    Cm,
    Ft,
}

impl LengthUnit {
    /// Conversion factor from this unit to millimetres.
    pub const fn to_mm(self) -> f64 {
        match self {
            Self::Mm => 1.0,
            Self::M => 1_000.0,
            Self::In => 25.4,
            Self::Um => 0.001,
            Self::Cm => 10.0,
            Self::Ft => 304.8,
        }
    }

    /// Convert a value in millimetres into this length unit.
    pub fn from_mm(self, mm: f64) -> f64 {
        mm / self.to_mm()
    }

    /// Canonical wire/display symbol for this length unit.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mm => "mm",
            Self::M => "m",
            Self::In => "in",
            Self::Um => "um",
            Self::Cm => "cm",
            Self::Ft => "ft",
        }
    }
}

impl fmt::Display for LengthUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LengthUnit {
    type Err = IrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mm" => Ok(Self::Mm),
            "m" => Ok(Self::M),
            "in" => Ok(Self::In),
            "um" => Ok(Self::Um),
            "cm" => Ok(Self::Cm),
            "ft" => Ok(Self::Ft),
            _ => Err(IrError::UnitMismatch),
        }
    }
}

/// Angular units accepted by the graph IR.  Internally, angles are represented
/// in degrees to match the canonical graph contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AngleUnit {
    Deg,
    Rad,
}

impl AngleUnit {
    pub const fn to_deg(self) -> f64 {
        match self {
            Self::Deg => 1.0,
            Self::Rad => 180.0 / std::f64::consts::PI,
        }
    }
}

/// Force units accepted by the graph IR.  Internally, forces are represented
/// in newtons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ForceUnit {
    N,
    #[serde(rename = "kN")]
    Kn,
}

impl ForceUnit {
    pub const fn to_newtons(self) -> f64 {
        match self {
            Self::N => 1.0,
            Self::Kn => 1_000.0,
        }
    }
}

/// Pressure units accepted by the graph IR.  Internally, pressures are
/// represented in megapascals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PressureUnit {
    #[serde(rename = "MPa")]
    Mpa,
    Pa,
}

impl PressureUnit {
    pub const fn to_mpa(self) -> f64 {
        match self {
            Self::Mpa => 1.0,
            Self::Pa => 0.000_001,
        }
    }
}

/// Mass units accepted by the graph IR.  Internally, masses are represented
/// in kilograms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MassUnit {
    G,
    Kg,
}

impl MassUnit {
    pub const fn to_kg(self) -> f64 {
        match self {
            Self::G => 0.001,
            Self::Kg => 1.0,
        }
    }
}

/// The unit choices used when displaying or authoring a graph.
///
/// The default is the canonical internal system: millimetres, degrees,
/// newtons, megapascals, and kilograms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitSystem {
    pub length: LengthUnit,
    pub angle: AngleUnit,
    pub force: ForceUnit,
    pub pressure: PressureUnit,
    pub mass: MassUnit,
}

impl Default for UnitSystem {
    fn default() -> Self {
        Self {
            length: LengthUnit::Mm,
            angle: AngleUnit::Deg,
            force: ForceUnit::N,
            pressure: PressureUnit::Mpa,
            mass: MassUnit::Kg,
        }
    }
}

/// A strongly dimensioned numeric value.
///
/// The `dimension` discriminator is intentionally part of the JSON shape so
/// that values remain self-describing when they cross the container boundary.
/// Density is stored in kilograms per cubic metre and therefore has no second
/// unit discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "dimension", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum Quantity {
    Length {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
        unit: LengthUnit,
    },
    Angle {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
        unit: AngleUnit,
    },
    Force {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
        unit: ForceUnit,
    },
    Pressure {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
        unit: PressureUnit,
    },
    Mass {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
        unit: MassUnit,
    },
    Density {
        #[serde(serialize_with = "crate::wire::number")]
        value: f64,
    },
}

impl From<&Quantity> for Quantity {
    fn from(value: &Quantity) -> Self {
        *value
    }
}

impl Quantity {
    pub fn length(value: f64, unit: LengthUnit) -> Result<Self, IrError> {
        Self::checked(Self::Length { value, unit })
    }

    pub fn angle(value: f64, unit: AngleUnit) -> Result<Self, IrError> {
        Self::checked(Self::Angle { value, unit })
    }

    pub fn force(value: f64, unit: ForceUnit) -> Result<Self, IrError> {
        Self::checked(Self::Force { value, unit })
    }

    pub fn pressure(value: f64, unit: PressureUnit) -> Result<Self, IrError> {
        Self::checked(Self::Pressure { value, unit })
    }

    pub fn mass(value: f64, unit: MassUnit) -> Result<Self, IrError> {
        Self::checked(Self::Mass { value, unit })
    }

    pub fn density(value: f64) -> Result<Self, IrError> {
        Self::checked(Self::Density { value })
    }

    /// Return the value in canonical internal units.
    pub fn canonical_value(self) -> f64 {
        let value = match self {
            Self::Length { value, unit } => value * unit.to_mm(),
            Self::Angle { value, unit } => value * unit.to_deg(),
            Self::Force { value, unit } => value * unit.to_newtons(),
            Self::Pressure { value, unit } => value * unit.to_mpa(),
            Self::Mass { value, unit } => value * unit.to_kg(),
            Self::Density { value } => value,
        };
        crate::wire::canonical_zero(value)
    }

    pub const fn ir_type(self) -> IrType {
        match self {
            Self::Length { .. } => IrType::Length,
            Self::Angle { .. } => IrType::Angle,
            Self::Force { .. } => IrType::Force,
            Self::Pressure { .. } => IrType::Pressure,
            Self::Mass { .. } => IrType::Mass,
            Self::Density { .. } => IrType::Density,
        }
    }

    /// Add values with the same physical dimension.  The result uses the
    /// left-hand side's display unit and is therefore stable for round trips.
    #[allow(
        clippy::should_implement_trait,
        reason = "The IR API exposes checked arithmetic as named methods"
    )]
    pub fn add<O>(self, other: O) -> Result<Self, IrError>
    where
        O: Into<Self>,
    {
        self.combine(other.into(), 1.0)
    }

    /// Subtract values with the same physical dimension.  The result uses the
    /// left-hand side's display unit.
    #[allow(
        clippy::should_implement_trait,
        reason = "The IR API exposes checked arithmetic as named methods"
    )]
    pub fn sub<O>(self, other: O) -> Result<Self, IrError>
    where
        O: Into<Self>,
    {
        self.combine(other.into(), -1.0)
    }

    /// Scale a quantity by a finite scalar, preserving its display unit.
    pub fn scale(self, factor: f64) -> Result<Self, IrError> {
        if !factor.is_finite() || !self.canonical_value().is_finite() {
            return Err(IrError::NonFiniteNumber);
        }
        let result = match self {
            Self::Length { value, unit } => Self::Length {
                value: value * factor,
                unit,
            },
            Self::Angle { value, unit } => Self::Angle {
                value: value * factor,
                unit,
            },
            Self::Force { value, unit } => Self::Force {
                value: value * factor,
                unit,
            },
            Self::Pressure { value, unit } => Self::Pressure {
                value: value * factor,
                unit,
            },
            Self::Mass { value, unit } => Self::Mass {
                value: value * factor,
                unit,
            },
            Self::Density { value } => Self::Density {
                value: value * factor,
            },
        };
        Self::checked(result)
    }

    /// Divide two quantities of the same dimension and return their scalar
    /// ratio.  A zero divisor is rejected explicitly.
    pub fn div_quantity<O>(self, other: O) -> Result<f64, IrError>
    where
        O: Into<Self>,
    {
        let other = other.into();
        if self.ir_type() != other.ir_type() {
            return Err(IrError::UnitMismatch);
        }
        let numerator = self.canonical_value();
        let denominator = other.canonical_value();
        if !denominator.is_finite() || !numerator.is_finite() {
            return Err(IrError::NonFiniteNumber);
        }
        if denominator == 0.0 {
            return Err(IrError::DivisionByZero);
        }
        let result = numerator / denominator;
        if result.is_finite() {
            Ok(crate::wire::canonical_zero(result))
        } else {
            Err(IrError::NonFiniteNumber)
        }
    }

    fn checked(value: Self) -> Result<Self, IrError> {
        if value.canonical_value().is_finite() {
            Ok(value)
        } else {
            Err(IrError::NonFiniteNumber)
        }
    }

    fn combine(self, other: Self, sign: f64) -> Result<Self, IrError> {
        if self.ir_type() != other.ir_type() {
            return Err(IrError::UnitMismatch);
        }
        let canonical = self.canonical_value() + sign * other.canonical_value();
        if !canonical.is_finite() {
            return Err(IrError::NonFiniteNumber);
        }
        let result = match self {
            Self::Length { unit, .. } => Self::Length {
                value: canonical / unit.to_mm(),
                unit,
            },
            Self::Angle { unit, .. } => Self::Angle {
                value: canonical / unit.to_deg(),
                unit,
            },
            Self::Force { unit, .. } => Self::Force {
                value: canonical / unit.to_newtons(),
                unit,
            },
            Self::Pressure { unit, .. } => Self::Pressure {
                value: canonical / unit.to_mpa(),
                unit,
            },
            Self::Mass { unit, .. } => Self::Mass {
                value: canonical / unit.to_kg(),
                unit,
            },
            Self::Density { .. } => Self::Density { value: canonical },
        };
        Self::checked(result)
    }
}
