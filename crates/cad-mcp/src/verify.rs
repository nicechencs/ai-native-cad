//! Profile names for `cad_verify_and_render`.
//!
//! The report itself is built by `DocumentSession::verify`.

use cad_runtime::VerificationProfile;

use crate::error::GatewayError;

/// `release` is the default. `fast` and `release` are the only accepted names.
pub fn parse_profile(value: Option<&str>) -> Result<VerificationProfile, GatewayError> {
    match value {
        None | Some("release") => Ok(VerificationProfile::Release),
        Some("fast") => Ok(VerificationProfile::Fast),
        Some(_) => Err(GatewayError::invalid("profile must be fast or release")),
    }
}
