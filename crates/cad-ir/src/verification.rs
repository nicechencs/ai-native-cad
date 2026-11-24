//! Five-layer tri-state engineering verification protocol (`PASS / FAIL / UNKNOWN`).
//!
//! Strictly forbids silently folding `Unknown` (unexecuted checks or unsupported
//! solver capabilities) into `Pass`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Current schema version for [`VerificationReport`].
pub const VERIFICATION_REPORT_SCHEMA_VERSION: u32 = 1;

const fn default_verification_schema_version() -> u32 {
    VERIFICATION_REPORT_SCHEMA_VERSION
}

/// Zero-trust tri-state engineering verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TriVerdict {
    Pass,
    Fail,
    Unknown,
}

impl TriVerdict {
    /// Combine an iterator of verdicts using strict zero-trust aggregation:
    /// - Any `Fail` yields `Fail`.
    /// - Otherwise, any `Unknown` (or an empty slice of verdicts) yields `Unknown`.
    /// - Only a non-empty set of exclusively `Pass` verdicts yields `Pass`.
    pub fn aggregate(verdicts: impl IntoIterator<Item = Self>) -> Self {
        let mut saw_any = false;
        let mut saw_unknown = false;
        for verdict in verdicts {
            saw_any = true;
            match verdict {
                Self::Fail => return Self::Fail,
                Self::Unknown => saw_unknown = true,
                Self::Pass => {}
            }
        }
        if !saw_any || saw_unknown {
            Self::Unknown
        } else {
            Self::Pass
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// Five orthogonal engineering verification layers defined by V2.0 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum VerificationLayer {
    #[serde(rename = "L0_structure_units")]
    L0StructureUnits,
    #[serde(rename = "L1_geometry_topology")]
    L1GeometryTopology,
    #[serde(rename = "L2_interfaces_constraints")]
    L2InterfacesConstraints,
    #[serde(rename = "L3_manufacturability_physics")]
    L3ManufacturabilityPhysics,
    #[serde(rename = "L4_interop_roundtrip")]
    L4InteropRoundtrip,
}

impl VerificationLayer {
    /// Canonical ordered list of all five verification layers.
    pub const ALL: [Self; 5] = [
        Self::L0StructureUnits,
        Self::L1GeometryTopology,
        Self::L2InterfacesConstraints,
        Self::L3ManufacturabilityPhysics,
        Self::L4InteropRoundtrip,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::L0StructureUnits => "L0_structure_units",
            Self::L1GeometryTopology => "L1_geometry_topology",
            Self::L2InterfacesConstraints => "L2_interfaces_constraints",
            Self::L3ManufacturabilityPhysics => "L3_manufacturability_physics",
            Self::L4InteropRoundtrip => "L4_interop_roundtrip",
        }
    }
}

/// Provenance identity of the B-Rep geometry kernel that produced verification evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelIdentity {
    pub provider: String,
    pub version: String,
    pub authoritative: bool,
}

impl KernelIdentity {
    pub fn new(
        provider: impl Into<String>,
        version: impl Into<String>,
        authoritative: bool,
    ) -> Self {
        Self {
            provider: provider.into(),
            version: version.into(),
            authoritative,
        }
    }
}

/// A single deterministic verification check record with structured evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationCheck {
    pub code: String,
    pub layer: VerificationLayer,
    pub status: TriVerdict,
    pub message: String,
    #[serde(default)]
    pub evidence: BTreeMap<String, serde_json::Value>,
}

impl VerificationCheck {
    pub fn new(
        code: impl Into<String>,
        layer: VerificationLayer,
        status: TriVerdict,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            layer,
            status,
            message: message.into(),
            evidence: BTreeMap::new(),
        }
    }

    pub fn with_evidence(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.evidence.insert(key.into(), value.into());
        self
    }
}

/// Layered tri-state verification report persisted alongside an engineering document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReport {
    #[serde(default = "default_verification_schema_version")]
    pub schema_version: u32,
    pub overall_status: TriVerdict,
    pub kernel_identity: KernelIdentity,
    pub layers: BTreeMap<VerificationLayer, TriVerdict>,
    pub checks: Vec<VerificationCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl VerificationReport {
    /// Build a [`VerificationReport`] from individual checks, computing every
    /// layer's verdict and the `overall_status` using strict zero-trust tri-state
    /// aggregation. Any layer without checks defaults to [`TriVerdict::Unknown`].
    pub fn from_checks(
        kernel_identity: KernelIdentity,
        checks: Vec<VerificationCheck>,
        summary: Option<String>,
    ) -> Self {
        let mut report = Self {
            schema_version: VERIFICATION_REPORT_SCHEMA_VERSION,
            overall_status: TriVerdict::Unknown,
            kernel_identity,
            layers: BTreeMap::new(),
            checks,
            summary,
        };
        report.recompute_statuses();
        report
    }

    /// Recompute per-layer verdicts (across all five canonical layers) and
    /// `overall_status` from `self.checks`.
    pub fn recompute_statuses(&mut self) {
        self.layers.clear();
        for layer in VerificationLayer::ALL {
            let layer_verdict = TriVerdict::aggregate(
                self.checks
                    .iter()
                    .filter(|check| check.layer == layer)
                    .map(|check| check.status),
            );
            self.layers.insert(layer, layer_verdict);
        }
        self.overall_status = TriVerdict::aggregate(self.layers.values().copied());
    }

    /// Convenience helper returning `true` strictly when `overall_status == TriVerdict::Pass`,
    /// all five canonical [`VerificationLayer::ALL`] layers are recorded as `Pass`, and
    /// `self.checks` is non-empty with every check `Pass` covering all five layers.
    pub fn is_pass(&self) -> bool {
        self.overall_status == TriVerdict::Pass
            && VerificationLayer::ALL
                .iter()
                .all(|layer| self.layers.get(layer) == Some(&TriVerdict::Pass))
            && !self.checks.is_empty()
            && self
                .checks
                .iter()
                .all(|check| check.status == TriVerdict::Pass)
            && VerificationLayer::ALL
                .iter()
                .all(|layer| self.checks.iter().any(|check| check.layer == *layer))
    }

    /// Return the aggregated verdict for a specific layer (defaults to `Unknown`).
    pub fn layer_status(&self, layer: VerificationLayer) -> TriVerdict {
        self.layers
            .get(&layer)
            .copied()
            .unwrap_or(TriVerdict::Unknown)
    }
}
