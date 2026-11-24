use serde::{Deserialize, Serialize};

/// Static types carried by graph node ports and values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IrType {
    Scalar,
    Int,
    Bool,
    String,
    Length,
    Angle,
    Force,
    Pressure,
    Mass,
    Density,
    Vec2Length,
    Vec3Length,
    Vec3Dir,
    Solid,
    Surface,
    #[serde(rename = "subd")]
    SubD,
    ScalarField,
    VectorField,
    Sketch,
    Assembly,
    Component,
    TopoRef,
    BlobRef,
    ExternalRef,
}
