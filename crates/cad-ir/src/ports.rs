//! Built-in v1 port contracts. Literal options can be added by a kernel.
//! Dynamic library calls can also define their own connection ports.

use crate::{IrType, NodeKind};

pub(crate) enum InputType {
    Value(&'static [IrType]),
    List(&'static [IrType]),
    ValueOrList {
        value: &'static [IrType],
        item: &'static [IrType],
    },
}

pub(crate) fn required_inputs(kind: &NodeKind) -> &'static [&'static [&'static str]] {
    use NodeKind::*;

    match kind {
        Extrude | Revolve => &[&["profile", "sketch"]],
        Sweep => &[&["profile", "sketch"], &["path"]],
        SubDFromBlob | FieldFromFeaBlob => &[&["blob"]],
        SubDToNurbs => &[&["subd"]],
        FieldSdf => &[SOLID_INPUT],
        ImplicitLatticeInfill => &[SOLID_INPUT, &["field", "lattice"]],
        _ => &[],
    }
}

const SOLID_INPUT: &[&str] = &["solid", "base", "target", "body"];

pub(crate) fn output_types(kind: &NodeKind) -> Option<&'static [IrType]> {
    use IrType::*;

    Some(match kind {
        NodeKind::ParameterRef { .. } | NodeKind::StdLibCall | NodeKind::ImportAsset => {
            return None;
        }
        NodeKind::Sketch2D => &[Sketch],
        NodeKind::LoftGuided | NodeKind::SubDToNurbs => &[Surface, Solid],
        NodeKind::SurfaceBoundary | NodeKind::SurfaceTrim => &[Surface],
        NodeKind::SubDFromBlob => &[SubD],
        NodeKind::FieldSdf | NodeKind::FieldTpms { .. } | NodeKind::FieldRamp => &[ScalarField],
        NodeKind::FieldFromFeaBlob => &[ScalarField, VectorField],
        NodeKind::AssemblyComponent => &[Component],
        NodeKind::AssemblyMate => &[Assembly],
        NodeKind::AssertGate => &[Bool],
        NodeKind::TagFaces => &[Solid, Surface],
        NodeKind::ImplicitSmoothBlend => &[Solid, ScalarField],
        NodeKind::PrimitiveBox
        | NodeKind::PrimitiveCylinder
        | NodeKind::Extrude
        | NodeKind::Revolve
        | NodeKind::Sweep
        | NodeKind::Shell
        | NodeKind::Fillet
        | NodeKind::Chamfer
        | NodeKind::Draft
        | NodeKind::Mirror
        | NodeKind::PatternLinear
        | NodeKind::PatternPolar
        | NodeKind::BooleanUnion
        | NodeKind::BooleanSubtract
        | NodeKind::BooleanIntersect
        | NodeKind::Hole
        | NodeKind::DirectFaceEdit
        | NodeKind::SurfaceKnitToSolid
        | NodeKind::ImplicitLatticeInfill => &[Solid],
    })
}

pub(crate) fn input_type(kind: &NodeKind, port: &str) -> Option<InputType> {
    use InputType::{List, Value, ValueOrList};
    use IrType::*;
    use NodeKind::*;

    Some(match (kind, port) {
        (PrimitiveBox, "width" | "height" | "depth" | "length")
        | (PrimitiveCylinder, "radius" | "height" | "diameter")
        | (Extrude, "height" | "depth" | "distance" | "thickness")
        | (Shell, "thickness")
        | (Fillet, "radius" | "r")
        | (Chamfer, "distance" | "d" | "distance2")
        | (Hole, "diameter" | "depth" | "radius" | "bore_diameter" | "bore_depth" | "sink_diameter")
        | (PatternLinear, "spacing" | "step" | "second_spacing")
        | (PatternPolar, "radius")
        | (DirectFaceEdit, "offset")
        | (AssemblyMate, "distance")
        | (AssertGate, "min_wall_thickness" | "min_clearance")
        | (FieldTpms { .. }, "cell_size" | "period")
        | (ImplicitSmoothBlend, "radius")
        | (ImplicitLatticeInfill, "wall_thickness") => Value(&[Length]),
        (Revolve | Draft | PatternPolar | Chamfer | AssemblyMate, "angle")
        | (PatternPolar, "start_angle")
        | (Hole, "sink_angle") => Value(&[Angle]),
        (PatternLinear, "count" | "second_count") | (PatternPolar, "count") => Value(&[Int]),
        (Extrude | Revolve | Sweep, "profile" | "sketch") => Value(&[Sketch]),
        (Extrude | Revolve, "target") => Value(&[Solid]),
        (Sweep, "path") => ValueOrList {
            value: &[Sketch],
            item: &[Vec3Length],
        },
        (LoftGuided, "sections" | "guides") => List(&[Sketch]),
        (Extrude | PatternLinear | FieldRamp, "direction")
        | (PatternLinear, "second_direction")
        | (Draft, "pull_direction")
        | (Revolve | Hole | PatternPolar | PrimitiveCylinder, "axis") => Value(&[Vec3Dir]),
        (PrimitiveBox | PrimitiveCylinder | Revolve | Hole | FieldRamp, "origin")
        | (PatternPolar, "axis_origin") => Value(&[Vec3Length]),
        (
            Shell
            | Fillet
            | Chamfer
            | Draft
            | Mirror
            | PatternLinear
            | PatternPolar
            | Hole
            | DirectFaceEdit
            | FieldSdf
            | ImplicitLatticeInfill,
            port,
        ) if SOLID_INPUT.contains(&port) => Value(&[Solid]),
        (
            BooleanUnion | BooleanSubtract | BooleanIntersect,
            "left" | "right" | "base" | "target" | "tool" | "a" | "b",
        ) => Value(&[Solid]),
        (ImplicitSmoothBlend, "left" | "right" | "a" | "b") => Value(&[Solid, ScalarField]),
        (ImplicitSmoothBlend, "solids") => List(&[Solid, ScalarField]),
        (BooleanUnion | BooleanSubtract | BooleanIntersect, "solids" | "tools") => List(&[Solid]),
        (Sketch2D, "anchors") => List(&[TopoRef, Solid, Surface]),
        (Sketch2D, "blobs") => List(&[BlobRef]),
        (Fillet | Chamfer, "edges")
        | (Shell, "open_faces")
        | (TagFaces | Draft, "faces") => List(&[TopoRef]),
        (DirectFaceEdit, "face") => Value(&[TopoRef]),
        (SurfaceTrim | SurfaceKnitToSolid, "surface") => Value(&[Surface]),
        (SurfaceKnitToSolid, "surfaces") => List(&[Surface]),
        (SubDFromBlob | FieldFromFeaBlob | ImportAsset, "blob") => Value(&[BlobRef]),
        (SubDToNurbs, "subd") => Value(&[SubD]),
        (ImplicitLatticeInfill, "field" | "lattice") => Value(&[ScalarField]),
        (AssemblyComponent, "geometry") => Value(&[Solid, Surface, SubD, Assembly]),
        (AssemblyComponent, "external_ref") => Value(&[ExternalRef]),
        (AssemblyMate, "left" | "right") => Value(&[Component]),
        (TagFaces, "solid" | "surface" | "geometry") => Value(&[Solid, Surface]),
        (AssertGate, "condition") => Value(&[Bool]),
        (AssertGate, "target") => Value(&[Solid, Surface, Assembly, Component]),
        (AssertGate, "a" | "b") => Value(&[Solid, Component]),
        (AssertGate, "max_size" | "bbox_min" | "bbox_max") => Value(&[Vec3Length]),
        (AssertGate, "axes") => ValueOrList {
            value: &[Vec3Dir],
            item: &[Vec3Dir],
        },
        (AssertGate, "fixed" | "loads") => List(&[TopoRef, Force]),
        _ => return None,
    })
}
