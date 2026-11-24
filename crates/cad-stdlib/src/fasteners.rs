//! Metric fastener holes.
//!
//! Clearance diameters are ISO 273 (fine = close, medium = normal, coarse = loose).
//! Coarse tap drills are the published workshop sizes. Fine-pitch drills are
//! nominal minus pitch for the ISO 262 fine series of the same diameters.
//! Counterbore diameter and depth are the usual ISO 4762 socket-head recess
//! (clear of `dk`, about 0.5 mm to 0.8 mm deeper than the head height).
//! Countersink diameter is the ISO 10642 theoretical head diameter, the 90°
//! cone that seats a maximum head flush.

use serde::{Deserialize, Serialize};

use crate::StdLibError;

/// ISO 273 clearance-hole class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClearanceClass {
    /// Fine series (H12).
    Close,
    /// Medium series (H13). Default for ordinary fabrication.
    Normal,
    /// Coarse series (H14).
    Loose,
}

/// ISO 4762 socket-head counterbore and the matching tapped-hole drill.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CounterboreSpec {
    /// Nominal thread, for example `M6`.
    pub thread_size: String,
    /// Coarse pitch, millimetres.
    pub pitch_mm: f64,
    /// Coarse-pitch tap drill, millimetres.
    pub tap_drill_mm: f64,
    /// ISO 273 close (fine) clearance, millimetres.
    pub clearance_close_mm: f64,
    /// ISO 273 normal (medium) clearance, millimetres.
    pub clearance_normal_mm: f64,
    /// ISO 273 loose (coarse) clearance, millimetres.
    pub clearance_loose_mm: f64,
    /// Counterbore diameter, millimetres.
    pub cbore_diameter_mm: f64,
    /// Counterbore depth, millimetres.
    pub cbore_depth_mm: f64,
}

impl CounterboreSpec {
    /// Clearance hole for `class`.
    pub fn clearance_mm(&self, class: ClearanceClass) -> f64 {
        match class {
            ClearanceClass::Close => self.clearance_close_mm,
            ClearanceClass::Normal => self.clearance_normal_mm,
            ClearanceClass::Loose => self.clearance_loose_mm,
        }
    }
}

/// ISO 10642 90° flat-head countersink.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CountersinkSpec {
    /// Nominal thread, for example `M6`.
    pub thread_size: String,
    /// Theoretical head / countersink diameter, millimetres.
    pub csink_diameter_mm: f64,
    /// Included countersink angle, degrees. Always 90 for this series.
    pub csink_angle_deg: f64,
    /// ISO 273 normal clearance through-hole, millimetres.
    pub clearance_normal_mm: f64,
}

struct Fastener {
    size: &'static str,
    pitch_mm: f64,
    tap_drill_mm: f64,
    clearance_close_mm: f64,
    clearance_normal_mm: f64,
    clearance_loose_mm: f64,
    cbore_diameter_mm: f64,
    cbore_depth_mm: f64,
    /// ISO 10642 theoretical `dk`.
    csink_diameter_mm: f64,
}

struct TapDrill {
    size: &'static str,
    pitch_mm: f64,
    drill_mm: f64,
    coarse: bool,
}

const FASTENERS: &[Fastener] = &[
    Fastener {
        size: "M2",
        pitch_mm: 0.40,
        tap_drill_mm: 1.60,
        clearance_close_mm: 2.2,
        clearance_normal_mm: 2.4,
        clearance_loose_mm: 2.6,
        cbore_diameter_mm: 4.4,
        cbore_depth_mm: 2.2,
        csink_diameter_mm: 4.70,
    },
    Fastener {
        size: "M2.5",
        pitch_mm: 0.45,
        tap_drill_mm: 2.05,
        clearance_close_mm: 2.7,
        clearance_normal_mm: 2.9,
        clearance_loose_mm: 3.1,
        cbore_diameter_mm: 5.5,
        cbore_depth_mm: 3.0,
        csink_diameter_mm: 5.88,
    },
    Fastener {
        size: "M3",
        pitch_mm: 0.50,
        tap_drill_mm: 2.5,
        clearance_close_mm: 3.2,
        clearance_normal_mm: 3.4,
        clearance_loose_mm: 3.6,
        cbore_diameter_mm: 6.5,
        cbore_depth_mm: 3.5,
        csink_diameter_mm: 6.72,
    },
    Fastener {
        size: "M4",
        pitch_mm: 0.70,
        tap_drill_mm: 3.3,
        clearance_close_mm: 4.3,
        clearance_normal_mm: 4.5,
        clearance_loose_mm: 4.8,
        cbore_diameter_mm: 8.0,
        cbore_depth_mm: 4.8,
        csink_diameter_mm: 8.96,
    },
    Fastener {
        size: "M5",
        pitch_mm: 0.80,
        tap_drill_mm: 4.2,
        clearance_close_mm: 5.3,
        clearance_normal_mm: 5.5,
        clearance_loose_mm: 5.8,
        cbore_diameter_mm: 10.0,
        cbore_depth_mm: 5.8,
        csink_diameter_mm: 11.20,
    },
    Fastener {
        size: "M6",
        pitch_mm: 1.0,
        tap_drill_mm: 5.0,
        clearance_close_mm: 6.4,
        clearance_normal_mm: 6.6,
        clearance_loose_mm: 7.0,
        cbore_diameter_mm: 11.0,
        cbore_depth_mm: 6.8,
        csink_diameter_mm: 13.44,
    },
    Fastener {
        size: "M8",
        pitch_mm: 1.25,
        tap_drill_mm: 6.8,
        clearance_close_mm: 8.4,
        clearance_normal_mm: 9.0,
        clearance_loose_mm: 10.0,
        cbore_diameter_mm: 15.0,
        cbore_depth_mm: 8.8,
        csink_diameter_mm: 17.92,
    },
    Fastener {
        size: "M10",
        pitch_mm: 1.50,
        tap_drill_mm: 8.5,
        clearance_close_mm: 10.5,
        clearance_normal_mm: 11.0,
        clearance_loose_mm: 12.0,
        cbore_diameter_mm: 18.0,
        cbore_depth_mm: 10.8,
        csink_diameter_mm: 22.40,
    },
    Fastener {
        size: "M12",
        pitch_mm: 1.75,
        tap_drill_mm: 10.2,
        clearance_close_mm: 13.0,
        clearance_normal_mm: 13.5,
        clearance_loose_mm: 14.5,
        cbore_diameter_mm: 20.0,
        cbore_depth_mm: 12.8,
        csink_diameter_mm: 26.88,
    },
];

/// Coarse workshop drills plus ISO 262 fine pitches (`drill = nominal - pitch`).
const TAP_DRILLS: &[TapDrill] = &[
    TapDrill {
        size: "M2",
        pitch_mm: 0.40,
        drill_mm: 1.60,
        coarse: true,
    },
    TapDrill {
        size: "M2",
        pitch_mm: 0.25,
        drill_mm: 1.75,
        coarse: false,
    },
    TapDrill {
        size: "M2.5",
        pitch_mm: 0.45,
        drill_mm: 2.05,
        coarse: true,
    },
    TapDrill {
        size: "M2.5",
        pitch_mm: 0.35,
        drill_mm: 2.15,
        coarse: false,
    },
    TapDrill {
        size: "M3",
        pitch_mm: 0.50,
        drill_mm: 2.50,
        coarse: true,
    },
    TapDrill {
        size: "M3",
        pitch_mm: 0.35,
        drill_mm: 2.65,
        coarse: false,
    },
    TapDrill {
        size: "M4",
        pitch_mm: 0.70,
        drill_mm: 3.30,
        coarse: true,
    },
    TapDrill {
        size: "M4",
        pitch_mm: 0.50,
        drill_mm: 3.50,
        coarse: false,
    },
    TapDrill {
        size: "M5",
        pitch_mm: 0.80,
        drill_mm: 4.20,
        coarse: true,
    },
    TapDrill {
        size: "M5",
        pitch_mm: 0.50,
        drill_mm: 4.50,
        coarse: false,
    },
    TapDrill {
        size: "M6",
        pitch_mm: 1.00,
        drill_mm: 5.00,
        coarse: true,
    },
    TapDrill {
        size: "M6",
        pitch_mm: 0.75,
        drill_mm: 5.25,
        coarse: false,
    },
    TapDrill {
        size: "M8",
        pitch_mm: 1.25,
        drill_mm: 6.80,
        coarse: true,
    },
    TapDrill {
        size: "M8",
        pitch_mm: 1.00,
        drill_mm: 7.00,
        coarse: false,
    },
    TapDrill {
        size: "M8",
        pitch_mm: 0.75,
        drill_mm: 7.25,
        coarse: false,
    },
    TapDrill {
        size: "M10",
        pitch_mm: 1.50,
        drill_mm: 8.50,
        coarse: true,
    },
    TapDrill {
        size: "M10",
        pitch_mm: 1.25,
        drill_mm: 8.75,
        coarse: false,
    },
    TapDrill {
        size: "M10",
        pitch_mm: 1.00,
        drill_mm: 9.00,
        coarse: false,
    },
    TapDrill {
        size: "M10",
        pitch_mm: 0.75,
        drill_mm: 9.25,
        coarse: false,
    },
    TapDrill {
        size: "M12",
        pitch_mm: 1.75,
        drill_mm: 10.20,
        coarse: true,
    },
    TapDrill {
        size: "M12",
        pitch_mm: 1.50,
        drill_mm: 10.50,
        coarse: false,
    },
    TapDrill {
        size: "M12",
        pitch_mm: 1.25,
        drill_mm: 10.75,
        coarse: false,
    },
    TapDrill {
        size: "M12",
        pitch_mm: 1.00,
        drill_mm: 11.00,
        coarse: false,
    },
];

fn unknown(spec: &str) -> StdLibError {
    StdLibError::UnknownFastener(spec.trim().to_string())
}

fn normalize_spec(spec: &str) -> String {
    let mut out = String::with_capacity(spec.len());
    for ch in spec.chars() {
        match ch {
            ' ' | '\t' => {}
            '×' | '*' | 'X' | 'x' => out.push('x'),
            other => out.push(other.to_ascii_uppercase()),
        }
    }
    out
}

/// `(nominal size, optional pitch)`.
fn parse_spec(spec: &str) -> Result<(&'static str, Option<f64>), StdLibError> {
    let normalized = normalize_spec(spec);
    if normalized.is_empty() {
        return Err(unknown(spec));
    }
    let (size_text, pitch) = if let Some((size_text, pitch_text)) = normalized.split_once('x') {
        if pitch_text.is_empty() || pitch_text.contains('x') {
            return Err(unknown(spec));
        }
        let pitch = pitch_text.parse::<f64>().map_err(|_| unknown(spec))?;
        if !pitch.is_finite() || pitch <= 0.0 {
            return Err(unknown(spec));
        }
        (size_text, Some(pitch))
    } else {
        (normalized.as_str(), None)
    };
    let size = FASTENERS
        .iter()
        .find(|row| row.size == size_text)
        .map(|row| row.size)
        .ok_or_else(|| unknown(spec))?;
    Ok((size, pitch))
}

fn fastener(size: &str) -> Result<&'static Fastener, StdLibError> {
    FASTENERS
        .iter()
        .find(|row| row.size == size)
        .ok_or_else(|| unknown(size))
}

fn pitches_match(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-6
}

fn tap_drill(size: &str, pitch_mm: f64) -> Option<f64> {
    TAP_DRILLS
        .iter()
        .find(|row| row.size == size && pitches_match(row.pitch_mm, pitch_mm))
        .map(|row| row.drill_mm)
}

fn counterbore_from(row: &'static Fastener) -> CounterboreSpec {
    CounterboreSpec {
        thread_size: row.size.to_string(),
        pitch_mm: row.pitch_mm,
        tap_drill_mm: row.tap_drill_mm,
        clearance_close_mm: row.clearance_close_mm,
        clearance_normal_mm: row.clearance_normal_mm,
        clearance_loose_mm: row.clearance_loose_mm,
        cbore_diameter_mm: row.cbore_diameter_mm,
        cbore_depth_mm: row.cbore_depth_mm,
    }
}

/// ISO 4762 socket-head counterbore for `size` (`"M6"` or `"M6x1.0"`).
///
/// An explicit pitch must be a published coarse or fine pitch of that diameter.
/// Head geometry does not change with pitch.
pub fn lookup_iso4762_cbore(size: &str) -> Result<CounterboreSpec, StdLibError> {
    let (nominal, pitch) = parse_spec(size)?;
    if let Some(pitch) = pitch {
        if tap_drill(nominal, pitch).is_none() {
            return Err(unknown(size));
        }
    }
    Ok(counterbore_from(fastener(nominal)?))
}

/// ISO 10642 90° countersink for `size`.
pub fn lookup_iso10642_csink(size: &str) -> Result<CountersinkSpec, StdLibError> {
    let (nominal, pitch) = parse_spec(size)?;
    if let Some(pitch) = pitch {
        if tap_drill(nominal, pitch).is_none() {
            return Err(unknown(size));
        }
    }
    let row = fastener(nominal)?;
    Ok(CountersinkSpec {
        thread_size: row.size.to_string(),
        csink_diameter_mm: row.csink_diameter_mm,
        csink_angle_deg: 90.0,
        clearance_normal_mm: row.clearance_normal_mm,
    })
}

/// Tap drill for `"M6"` (coarse) or `"M6x1.0"` / `"M6x0.75"`.
///
/// Pitches that are not in the coarse or ISO 262 fine table return an error.
pub fn lookup_metric_tap_drill(spec: &str) -> Result<f64, StdLibError> {
    let (nominal, pitch) = parse_spec(spec)?;
    let pitch = match pitch {
        Some(pitch) => pitch,
        None => TAP_DRILLS
            .iter()
            .find(|row| row.size == nominal && row.coarse)
            .map(|row| row.pitch_mm)
            .ok_or_else(|| unknown(spec))?,
    };
    tap_drill(nominal, pitch).ok_or_else(|| unknown(spec))
}
