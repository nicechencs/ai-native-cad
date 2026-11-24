//! Direct implicit-field additive manufacturing layer slicer.
//!
//! [`slice_field`] samples the zero contour on each XY layer. Use
//! [`export_cli_ascii`] for manufacturing output or [`export_layer_svg`] to
//! inspect a layer visually.

#![forbid(unsafe_code)]

pub mod cli_format;
pub mod contour_verify;
pub mod direct_pipeline;
pub mod field;
pub mod gpu_slicer;
pub mod marching_squares;
pub mod sli_format;
pub mod svg_export;

pub use cli_format::{CliSummary, export_cli_ascii, parse_cli_ascii, parse_cli_ascii_summary};
pub use contour_verify::{
    ContourViolation, ContourViolationKind, LayerVerificationReport, StackVerificationReport,
    verify_layer_contours, verify_stack_contours,
};
pub use direct_pipeline::{
    DirectSliceBundle, HybridTpmsSliceField, ProtectedZone, slice_hybrid_tpms_to_stack,
};
pub use field::{FnSliceField, SliceField};
pub use gpu_slicer::{
    LayerSlice, SliceError, StreamingSliceStats, TpmsLayerFieldSpec, TpmsVariant,
    WGSL_TPMS_LAYER_SLICE_SHADER, stream_slice_to_cli,
};
pub use marching_squares::{ContourLoop, SliceConfig, SliceLayer, SliceStack, slice_field};
pub use sli_format::{
    SLI_MAGIC, SLI_VERSION, SliDocument, SliHatchSegment, SliHeader, SliLayerHatches,
    export_sli_binary, export_sli_binary_with_hatches, parse_sli_binary, parse_sli_binary_summary,
};
pub use svg_export::export_layer_svg;

/// An error while sampling a field or reading or writing a CLI/SLI stack.
#[derive(Debug, thiserror::Error)]
pub enum SlicerError {
    #[error("invalid slice configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("non-finite field sample at ({x_mm}, {y_mm}, {z_mm}) mm")]
    NonFiniteSample { x_mm: f64, y_mm: f64, z_mm: f64 },
    #[error("CLI units must be finite, positive, and representable at CLI precision")]
    InvalidUnits,
    #[error("invalid contour in layer {layer_index}: {reason}")]
    InvalidContour {
        layer_index: usize,
        reason: &'static str,
    },
    #[error("invalid CLI at line {line}: {reason}")]
    InvalidCli { line: usize, reason: String },
    #[error("invalid SLI at byte offset {offset}: {reason}")]
    InvalidSli { offset: usize, reason: String },
}
