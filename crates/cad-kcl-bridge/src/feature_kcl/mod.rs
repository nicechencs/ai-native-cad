//! Human-readable `.kcl 2.0` blocks for V2 [`cad_ir::FeatureSpec`] nodes.
//!
//! A node built by [`cad_ir::IrNode::from_feature_spec`] emits a `let` block.
//! Parsing that block calls `from_feature_spec` again, so the canonical ports
//! and the `feature_spec` JSON payload come back unchanged. Legacy nodes that
//! only have loose ports keep the original `operator(...)` call syntax.

mod emit;
mod parse;

pub(crate) use emit::try_emit_feature;
pub(crate) use parse::parse_feature_block;
