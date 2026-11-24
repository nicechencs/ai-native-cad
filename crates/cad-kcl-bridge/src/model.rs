//! Shared names for types, operators, units, and literals.
//!
//! The emitter and the parser both go through these tables so a graph survives
//! `emit -> parse -> emit` without renaming operators or units.

use cad_ir::{
    AngleUnit, BlobKind, ForceUnit, IrType, LengthUnit, MassUnit, NodeKind, PayloadPolicy,
    PressureUnit, Quantity, TpmsFamily,
};

pub(crate) fn type_name(ty: IrType) -> &'static str {
    match ty {
        IrType::Scalar => "Scalar",
        IrType::Int => "Int",
        IrType::Bool => "Bool",
        IrType::String => "String",
        IrType::Length => "Length",
        IrType::Angle => "Angle",
        IrType::Force => "Force",
        IrType::Pressure => "Pressure",
        IrType::Mass => "Mass",
        IrType::Density => "Density",
        IrType::Vec2Length => "Vec2Length",
        IrType::Vec3Length => "Vec3Length",
        IrType::Vec3Dir => "Vec3Dir",
        IrType::Solid => "Solid",
        IrType::Surface => "Surface",
        IrType::SubD => "SubD",
        IrType::ScalarField => "ScalarField",
        IrType::VectorField => "VectorField",
        IrType::Sketch => "Sketch",
        IrType::Assembly => "Assembly",
        IrType::Component => "Component",
        IrType::TopoRef => "TopoRef",
        IrType::BlobRef => "BlobRef",
        IrType::ExternalRef => "ExternalRef",
    }
}

pub(crate) fn type_from_name(name: &str) -> Option<IrType> {
    Some(match name {
        "Scalar" => IrType::Scalar,
        "Int" => IrType::Int,
        "Bool" => IrType::Bool,
        "String" => IrType::String,
        "Length" => IrType::Length,
        "Angle" => IrType::Angle,
        "Force" => IrType::Force,
        "Pressure" => IrType::Pressure,
        "Mass" => IrType::Mass,
        "Density" => IrType::Density,
        "Vec2Length" => IrType::Vec2Length,
        "Vec3Length" => IrType::Vec3Length,
        "Vec3Dir" => IrType::Vec3Dir,
        "Solid" => IrType::Solid,
        "Surface" => IrType::Surface,
        "SubD" => IrType::SubD,
        "ScalarField" => IrType::ScalarField,
        "VectorField" => IrType::VectorField,
        "Sketch" => IrType::Sketch,
        "Assembly" => IrType::Assembly,
        "Component" => IrType::Component,
        "TopoRef" => IrType::TopoRef,
        "BlobRef" => IrType::BlobRef,
        "ExternalRef" => IrType::ExternalRef,
        _ => return None,
    })
}

/// Operator spelling for every node kind except the parameter id carried by `ParameterRef`.
pub(crate) fn operator_name(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::ParameterRef { .. } => "parameter_ref",
        NodeKind::Sketch2D => "sketch_2d",
        NodeKind::PrimitiveBox => "box",
        NodeKind::PrimitiveCylinder => "cylinder",
        NodeKind::Extrude => "extrude",
        NodeKind::Revolve => "revolve",
        NodeKind::Sweep => "sweep",
        NodeKind::LoftGuided => "loft_guided",
        NodeKind::Shell => "shell",
        NodeKind::Fillet => "fillet",
        NodeKind::Chamfer => "chamfer",
        NodeKind::Draft => "draft",
        NodeKind::Mirror => "mirror",
        NodeKind::PatternLinear => "pattern_linear",
        NodeKind::PatternPolar => "pattern_polar",
        NodeKind::BooleanUnion => "boolean_union",
        NodeKind::BooleanSubtract => "boolean_subtract",
        NodeKind::BooleanIntersect => "boolean_intersect",
        NodeKind::Hole => "hole",
        NodeKind::DirectFaceEdit => "direct_face_edit",
        NodeKind::SurfaceBoundary => "surface_boundary",
        NodeKind::SurfaceTrim => "surface_trim",
        NodeKind::SurfaceKnitToSolid => "surface_knit_to_solid",
        NodeKind::SubDFromBlob => "subd_from_blob",
        NodeKind::SubDToNurbs => "subd_to_nurbs",
        NodeKind::FieldSdf => "field::sdf",
        NodeKind::FieldTpms { family } => match family {
            TpmsFamily::Gyroid => "field::gyroid",
            TpmsFamily::SchwarzP => "field::schwarz_p",
            TpmsFamily::Diamond => "field::diamond",
            TpmsFamily::Lidinoid => "field::lidinoid",
        },
        NodeKind::FieldRamp => "field::ramp",
        NodeKind::FieldFromFeaBlob => "field::from_fea",
        NodeKind::ImplicitSmoothBlend => "implicit::smooth_blend",
        NodeKind::ImplicitLatticeInfill => "implicit::lattice_infill",
        NodeKind::StdLibCall => "std::call",
        NodeKind::ImportAsset => "import::asset",
        NodeKind::AssemblyComponent => "assembly::component",
        NodeKind::AssemblyMate => "assembly::mate",
        NodeKind::TagFaces => "tag::faces",
        NodeKind::AssertGate => "assert::gate",
    }
}

/// Decode an operator. `ParameterRef` still needs its parameter name from the call.
pub(crate) enum DecodedOp {
    ParameterRef,
    Fixed(NodeKind),
}

pub(crate) fn decode_operator(name: &str) -> Option<DecodedOp> {
    use DecodedOp::{Fixed, ParameterRef};
    Some(match name {
        "parameter_ref" => ParameterRef,
        "sketch_2d" => Fixed(NodeKind::Sketch2D),
        "box" => Fixed(NodeKind::PrimitiveBox),
        "cylinder" => Fixed(NodeKind::PrimitiveCylinder),
        "extrude" => Fixed(NodeKind::Extrude),
        "revolve" => Fixed(NodeKind::Revolve),
        "sweep" => Fixed(NodeKind::Sweep),
        "loft_guided" => Fixed(NodeKind::LoftGuided),
        "shell" => Fixed(NodeKind::Shell),
        "fillet" => Fixed(NodeKind::Fillet),
        "chamfer" => Fixed(NodeKind::Chamfer),
        "draft" => Fixed(NodeKind::Draft),
        "mirror" => Fixed(NodeKind::Mirror),
        "pattern_linear" => Fixed(NodeKind::PatternLinear),
        "pattern_polar" => Fixed(NodeKind::PatternPolar),
        "boolean_union" => Fixed(NodeKind::BooleanUnion),
        "boolean_subtract" => Fixed(NodeKind::BooleanSubtract),
        "boolean_intersect" => Fixed(NodeKind::BooleanIntersect),
        "hole" => Fixed(NodeKind::Hole),
        "direct_face_edit" => Fixed(NodeKind::DirectFaceEdit),
        "surface_boundary" => Fixed(NodeKind::SurfaceBoundary),
        "surface_trim" => Fixed(NodeKind::SurfaceTrim),
        "surface_knit_to_solid" => Fixed(NodeKind::SurfaceKnitToSolid),
        "subd_from_blob" => Fixed(NodeKind::SubDFromBlob),
        "subd_to_nurbs" => Fixed(NodeKind::SubDToNurbs),
        "field::sdf" => Fixed(NodeKind::FieldSdf),
        "field::gyroid" => Fixed(NodeKind::FieldTpms {
            family: TpmsFamily::Gyroid,
        }),
        "field::schwarz_p" => Fixed(NodeKind::FieldTpms {
            family: TpmsFamily::SchwarzP,
        }),
        "field::diamond" => Fixed(NodeKind::FieldTpms {
            family: TpmsFamily::Diamond,
        }),
        "field::lidinoid" => Fixed(NodeKind::FieldTpms {
            family: TpmsFamily::Lidinoid,
        }),
        "field::ramp" => Fixed(NodeKind::FieldRamp),
        "field::from_fea" => Fixed(NodeKind::FieldFromFeaBlob),
        "implicit::smooth_blend" => Fixed(NodeKind::ImplicitSmoothBlend),
        "implicit::lattice_infill" => Fixed(NodeKind::ImplicitLatticeInfill),
        "std::call" => Fixed(NodeKind::StdLibCall),
        "import::asset" => Fixed(NodeKind::ImportAsset),
        "assembly::component" => Fixed(NodeKind::AssemblyComponent),
        "assembly::mate" => Fixed(NodeKind::AssemblyMate),
        "tag::faces" => Fixed(NodeKind::TagFaces),
        "assert::gate" => Fixed(NodeKind::AssertGate),
        _ => return None,
    })
}

pub(crate) fn blob_kind_name(kind: BlobKind) -> &'static str {
    match kind {
        BlobKind::Spline => "spline",
        BlobKind::SubD => "subd",
        BlobKind::Field => "field",
        BlobKind::Import => "import",
    }
}

pub(crate) fn blob_kind_from_name(name: &str) -> Option<BlobKind> {
    Some(match name {
        "spline" => BlobKind::Spline,
        "subd" => BlobKind::SubD,
        "field" => BlobKind::Field,
        "import" => BlobKind::Import,
        _ => return None,
    })
}

pub(crate) fn length_unit_name(unit: LengthUnit) -> &'static str {
    match unit {
        LengthUnit::Mm => "mm",
        LengthUnit::M => "m",
        LengthUnit::In => "in",
        LengthUnit::Um => "um",
        LengthUnit::Cm => "cm",
        LengthUnit::Ft => "ft",
    }
}

pub(crate) fn angle_unit_name(unit: AngleUnit) -> &'static str {
    match unit {
        AngleUnit::Deg => "deg",
        AngleUnit::Rad => "rad",
    }
}

pub(crate) fn force_unit_name(unit: ForceUnit) -> &'static str {
    match unit {
        ForceUnit::N => "N",
        ForceUnit::Kn => "kN",
    }
}

pub(crate) fn pressure_unit_name(unit: PressureUnit) -> &'static str {
    match unit {
        PressureUnit::Mpa => "MPa",
        PressureUnit::Pa => "Pa",
    }
}

pub(crate) fn mass_unit_name(unit: MassUnit) -> &'static str {
    match unit {
        MassUnit::G => "g",
        MassUnit::Kg => "kg",
    }
}

pub(crate) fn length_unit_from_name(name: &str) -> Option<LengthUnit> {
    Some(match name {
        "mm" => LengthUnit::Mm,
        "m" => LengthUnit::M,
        "in" => LengthUnit::In,
        "um" => LengthUnit::Um,
        "cm" => LengthUnit::Cm,
        "ft" => LengthUnit::Ft,
        _ => return None,
    })
}

pub(crate) fn angle_unit_from_name(name: &str) -> Option<AngleUnit> {
    Some(match name {
        "deg" => AngleUnit::Deg,
        "rad" => AngleUnit::Rad,
        _ => return None,
    })
}

pub(crate) fn force_unit_from_name(name: &str) -> Option<ForceUnit> {
    Some(match name {
        "N" => ForceUnit::N,
        "kN" => ForceUnit::Kn,
        _ => return None,
    })
}

pub(crate) fn pressure_unit_from_name(name: &str) -> Option<PressureUnit> {
    Some(match name {
        "MPa" => PressureUnit::Mpa,
        "Pa" => PressureUnit::Pa,
        _ => return None,
    })
}

pub(crate) fn mass_unit_from_name(name: &str) -> Option<MassUnit> {
    Some(match name {
        "g" => MassUnit::G,
        "kg" => MassUnit::Kg,
        _ => return None,
    })
}

pub(crate) enum SuffixParse {
    Quantity(Quantity),
    Unknown,
    NonFinite,
}

pub(crate) fn quantity_from_suffix(value: f64, suffix: &str) -> SuffixParse {
    let built = match suffix {
        "mm" | "m" | "in" | "um" | "cm" => {
            Quantity::length(value, length_unit_from_name(suffix).expect("length suffix"))
        }
        "deg" | "rad" => {
            Quantity::angle(value, angle_unit_from_name(suffix).expect("angle suffix"))
        }
        "N" | "kN" => Quantity::force(value, force_unit_from_name(suffix).expect("force suffix")),
        "MPa" | "Pa" => Quantity::pressure(
            value,
            pressure_unit_from_name(suffix).expect("pressure suffix"),
        ),
        "g" | "kg" => Quantity::mass(value, mass_unit_from_name(suffix).expect("mass suffix")),
        "kg_m3" => Quantity::density(value),
        _ => return SuffixParse::Unknown,
    };
    match built {
        Ok(quantity) => SuffixParse::Quantity(quantity),
        Err(_) => SuffixParse::NonFinite,
    }
}

/// Shortest decimal that reads back as the same finite `f64`.
/// Both signs of zero are written as `0`.
pub(crate) fn format_f64(value: f64) -> String {
    let value = if value == 0.0 { 0.0 } else { value };
    let text = format!("{value}");
    if text == "-0" { "0".to_owned() } else { text }
}

/// Scalar literals keep a decimal or exponent marker so they are not read as integers.
pub(crate) fn format_scalar(value: f64) -> String {
    let mut text = format_f64(value);
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        text.push_str(".0");
    }
    text
}

pub(crate) fn format_quantity(quantity: Quantity) -> String {
    match quantity {
        Quantity::Length { value, unit } => {
            format!("{}{}", format_f64(value), length_unit_name(unit))
        }
        Quantity::Angle { value, unit } => {
            format!("{}{}", format_f64(value), angle_unit_name(unit))
        }
        Quantity::Force { value, unit } => {
            format!("{}{}", format_f64(value), force_unit_name(unit))
        }
        Quantity::Pressure { value, unit } => {
            format!("{}{}", format_f64(value), pressure_unit_name(unit))
        }
        Quantity::Mass { value, unit } => format!("{}{}", format_f64(value), mass_unit_name(unit)),
        Quantity::Density { value } => format!("{}kg_m3", format_f64(value)),
    }
}

pub(crate) fn is_plain_ident(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Words the `.kcl` lexer classifies as keywords, so they cannot be bare names.
pub(crate) fn is_lexer_keyword(text: &str) -> bool {
    matches!(
        text,
        "part" | "assembly" | "let" | "return" | "sketch" | "const" | "true" | "false"
    )
}

/// Names that start a value constructor rather than a node reference.
pub(crate) fn is_value_keyword(text: &str) -> bool {
    matches!(text, "vec3" | "node")
}

pub(crate) fn format_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub(crate) fn format_key(key: &str) -> String {
    if is_plain_ident(key) && !is_lexer_keyword(key) {
        key.to_owned()
    } else {
        format_string(key)
    }
}

pub(crate) fn format_binding(id: &str) -> String {
    if is_plain_ident(id) && !is_lexer_keyword(id) {
        id.to_owned()
    } else {
        format_string(id)
    }
}

pub(crate) fn format_node_ref(id: &str) -> String {
    if is_plain_ident(id) && !is_lexer_keyword(id) && !is_value_keyword(id) {
        id.to_owned()
    } else {
        format!("node({})", format_string(id))
    }
}

pub(crate) fn format_blob_ref(handle: &str) -> String {
    const PREFIXES: &[(&str, &str)] = &[
        ("@spline/", "spline"),
        ("@subd/", "subd"),
        ("@field/", "field"),
        ("@import/", "import"),
    ];
    for (prefix, keyword) in PREFIXES {
        if let Some(rest) = handle.strip_prefix(prefix) {
            return format!("@{keyword}({})", format_string(rest));
        }
    }
    format!("@blob_ref({})", format_string(handle))
}

pub(crate) fn blob_handle_from_keyword(keyword: &str, body: &str) -> Option<String> {
    let prefix = match keyword {
        "spline" => "@spline/",
        "subd" => "@subd/",
        "field" => "@field/",
        "import" => "@import/",
        _ => return None,
    };
    Some(format!("{prefix}{body}"))
}

pub(crate) fn payload_policy_name(policy: PayloadPolicy) -> &'static str {
    match policy {
        PayloadPolicy::Eager => "eager",
        PayloadPolicy::DeferredPayload => "deferred_payload",
    }
}

pub(crate) fn payload_policy_from_name(name: &str) -> Option<PayloadPolicy> {
    Some(match name {
        "eager" => PayloadPolicy::Eager,
        "deferred_payload" => PayloadPolicy::DeferredPayload,
        _ => return None,
    })
}

pub(crate) fn format_external_ref(ref_id: &str) -> String {
    format!("@extref({})", format_string(ref_id))
}
