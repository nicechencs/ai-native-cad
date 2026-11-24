//! Semantic CAD API.
//!
//! Callers describe intent with [`SemanticCommand`]. The crate checks
//! preconditions, allocates stable node ids, and compiles the commands into a
//! [`cad_ir::GraphPatch`]. It does not splice or parse `.kcl` source.
//!
//! [`apply_commands`] clones the graph, applies that patch, evaluates the
//! parameter formula DAG, and validates the result. A failed batch returns
//! [`SemanticApiError`] and does not modify the caller's graph.

#![forbid(unsafe_code)]

mod checks;
mod command;
mod compile;
mod dto;
mod error;
mod inspect;
mod walk;

pub use command::*;
pub use compile::{apply_commands, compile_batch, compile_command, evaluate_parameter_formulas};
pub use dto::*;
pub use error::{SemanticApiError, SemanticErrorCode};
pub use inspect::{explain_entity, inspect_document_summary, slice_document_context};

pub use cad_ir::{
    AngleUnit, BooleanOpKind, CanonicalGraph, ComponentSourceSpec, DatumPlane, DesignAssertionKind,
    DimInput, DirectFaceEditOp, FastenerHoleStandard, FeatureBodyOp, FeatureSpec, GraphPatch,
    HoleDepthSpec, HoleProfileSpec, IntInput, IrType, IrValue, LengthUnit, LinearPatternAxisSpec,
    MateKindSpec, NodeId, NodeState, ParamId, Quantity, SketchAnchor, SketchConstraint2D,
    SketchDimValue, SketchEntity2D, SketchPlaneSpec, SketchPointRef, SketchProfileLoop,
    StructuredSketch2D,
};
