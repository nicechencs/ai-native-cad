//! Develop rectangular sheet-metal parts with one upward bend per selected edge.

use cad_mesh_io::Mesh;
use cad_stdlib::{BendAllowanceResult, compute_bend_allowance, default_k_factor, lookup_material};

/// An edge of the base rectangle in the XY plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SheetMetalEdgeSide {
    NegY,
    PosX,
    PosY,
    NegX,
}

impl SheetMetalEdgeSide {
    const COUNTERCLOCKWISE: [Self; 4] = [Self::NegY, Self::PosX, Self::PosY, Self::NegX];

    fn outward(self) -> [f64; 2] {
        match self {
            Self::NegY => [0.0, -1.0],
            Self::PosX => [1.0, 0.0],
            Self::PosY => [0.0, 1.0],
            Self::NegX => [-1.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SheetMetalFlangeSpec {
    pub edge: SheetMetalEdgeSide,
    /// The turn from the base plane towards +Z, strictly between 0 and 180 degrees.
    pub angle_deg: f64,
    /// Outside leg length, measured from the virtual sharp bend to the flange tip.
    pub leg_length_mm: f64,
    /// Defaults to the material's minimum inside bend radius.
    pub inner_radius_mm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SheetMetalPartSpec {
    pub material_code: String,
    pub thickness_mm: f64,
    /// X span between outside mold lines at bent edges and cut lines at free edges.
    pub base_width_mm: f64,
    /// Y span measured the same way as the width.
    pub base_depth_mm: f64,
    pub flanges: Vec<SheetMetalFlangeSpec>,
}

/// Centerline of a developed bend strip, with its bend parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct BendLine2D {
    pub edge: SheetMetalEdgeSide,
    pub start_mm: [f64; 2],
    pub end_mm: [f64; 2],
    pub angle_deg: f64,
    pub inner_radius_mm: f64,
    pub k_factor: f64,
    pub bend_allowance_mm: f64,
    pub bend_deduction_mm: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnfoldedSheetPattern {
    pub outer_bbox_min_mm: [f64; 2],
    pub outer_bbox_max_mm: [f64; 2],
    pub flat_width_mm: f64,
    pub flat_depth_mm: f64,
    pub flat_area_mm2: f64,
    pub bend_lines: Vec<BendLine2D>,
    /// Counterclockwise outline with an implicit closing edge.
    pub outline_polygon_mm: Vec<[f64; 2]>,
    pub folded_mesh: Mesh,
}

#[derive(Clone, Copy)]
struct Bend {
    angle_deg: f64,
    inner_radius_mm: f64,
    allowance: BendAllowanceResult,
    outside_setback_mm: f64,
    straight_leg_mm: f64,
}

impl Bend {
    fn flat_extension_mm(self) -> f64 {
        self.straight_leg_mm + self.allowance.bend_allowance_mm
    }
}

fn positive(value: f64, name: &str) -> Result<(), String> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(format!("{name} must be finite and positive"))
    }
}

fn minimum_radius_mm(material: &str, thickness_mm: f64) -> f64 {
    // Nominal press-brake design minima. The 5052-H32 thickness bands follow the
    // Atlas Steels bend table at 3.2 and 12 mm, with a 1T floor for thinner sheet.
    let ratio = match material {
        "AL-5052-H32" if thickness_mm >= 12.0 => 2.0,
        "AL-5052-H32" if thickness_mm >= 3.2 => 1.5,
        "SS-304" | "SS-316L" => 1.5,
        "AL-6061-T6" => 3.0,
        _ => 1.0,
    };
    thickness_mm * ratio
}

fn resolve_bend(
    flange: &SheetMetalFlangeSpec,
    thickness_mm: f64,
    min_radius_mm: f64,
    k_material: Option<&str>,
) -> Result<Bend, String> {
    positive(flange.leg_length_mm, "flange leg length")?;
    let radius = flange.inner_radius_mm.unwrap_or(min_radius_mm);
    if !radius.is_finite() || radius < min_radius_mm {
        return Err(format!(
            "{:?} inner bend radius must be finite and at least {min_radius_mm} mm",
            flange.edge
        ));
    }
    let k_factor = k_material.map_or(0.40, |material| {
        default_k_factor(material, radius / thickness_mm)
    });
    let allowance = compute_bend_allowance(thickness_mm, radius, flange.angle_deg, k_factor)
        .map_err(|error| format!("{:?}: {error}", flange.edge))?;
    let outside_setback_mm = (radius + thickness_mm) * (flange.angle_deg * 0.5).to_radians().tan();
    let straight_leg_mm = flange.leg_length_mm - outside_setback_mm;
    let rounding_tolerance = f64::EPSILON * flange.leg_length_mm;
    if straight_leg_mm < -rounding_tolerance {
        return Err(format!(
            "{:?} flange leg length must reach the bend tangent ({outside_setback_mm} mm)",
            flange.edge
        ));
    }
    Ok(Bend {
        angle_deg: flange.angle_deg,
        inner_radius_mm: radius,
        allowance,
        outside_setback_mm,
        straight_leg_mm: if straight_leg_mm > rounding_tolerance {
            straight_leg_mm
        } else {
            0.0
        },
    })
}

fn offset(point: [f64; 2], direction: [f64; 2], distance: f64) -> [f64; 2] {
    std::array::from_fn(|i| point[i] + direction[i] * distance)
}

/// Unfold a rectangular base and its edge flanges using neutral-axis bend lengths.
///
/// The virtual base rectangle starts at (0, 0). Bends trim back the base to its
/// tangent lines, and flanges span those trimmed edges, leaving open corners.
/// The folded underside of the base is at Z = 0. The folded mesh represents the
/// prescribed bends and may self-intersect when long flanges turn past 90 degrees.
pub fn unfold_sheet_metal_part(spec: &SheetMetalPartSpec) -> Result<UnfoldedSheetPattern, String> {
    positive(spec.thickness_mm, "thickness")?;
    positive(spec.base_width_mm, "base width")?;
    positive(spec.base_depth_mm, "base depth")?;

    let material = spec.material_code.trim().to_ascii_uppercase();
    let min_radius_mm = minimum_radius_mm(&material, spec.thickness_mm);
    // Map sheet grades outside the catalog to the corresponding air-bend rows.
    let k_material = match material.as_str() {
        "AL-5052-H32" | "AL-6061-T6" => Some("Al-6061-T6"),
        "AL-7075-T6" => Some("Al-7075-T6"),
        "SS-304" | "SS-316L" => Some(material.as_str()),
        "SPCC" => Some("Steel-45"),
        _ => {
            let code = spec.material_code.trim();
            lookup_material(code).is_ok().then_some(code)
        }
    };
    let mut bends = [None; 4];
    for flange in &spec.flanges {
        let bend = &mut bends[flange.edge as usize];
        if bend.is_some() {
            return Err(format!("duplicate flange on {:?} edge", flange.edge));
        }
        *bend = Some(resolve_bend(
            flange,
            spec.thickness_mm,
            min_radius_mm,
            k_material,
        )?);
    }

    let [neg_y, pos_x, pos_y, neg_x] = bends.map(|bend| bend.map_or(0.0, |b| b.outside_setback_mm));
    let [x_min, y_min] = [neg_x, neg_y];
    let [x_max, y_max] = [spec.base_width_mm - pos_x, spec.base_depth_mm - pos_y];
    let tangent_width = x_max - x_min;
    let tangent_depth = y_max - y_min;
    if tangent_width <= 0.0 || tangent_depth <= 0.0 {
        return Err("bend setbacks leave no flat base between tangents".to_string());
    }
    let base_corners = [
        [x_min, y_min],
        [x_max, y_min],
        [x_max, y_max],
        [x_min, y_max],
    ];
    let mut outline_polygon_mm = Vec::with_capacity(4 + 2 * spec.flanges.len());
    let mut bend_lines = Vec::with_capacity(spec.flanges.len());
    let mut flat_area_mm2 = tangent_width * tangent_depth;
    for edge in SheetMetalEdgeSide::COUNTERCLOCKWISE {
        let i = edge as usize;
        let start = base_corners[i];
        let end = base_corners[(i + 1) % 4];
        outline_polygon_mm.push(start);
        if let Some(bend) = bends[i] {
            let direction = edge.outward();
            let extension = bend.flat_extension_mm();
            outline_polygon_mm.push(offset(start, direction, extension));
            outline_polygon_mm.push(offset(end, direction, extension));
            let bend_center = bend.allowance.bend_allowance_mm * 0.5;
            bend_lines.push(BendLine2D {
                edge,
                start_mm: offset(start, direction, bend_center),
                end_mm: offset(end, direction, bend_center),
                angle_deg: bend.angle_deg,
                inner_radius_mm: bend.inner_radius_mm,
                k_factor: bend.allowance.k_factor,
                bend_allowance_mm: bend.allowance.bend_allowance_mm,
                bend_deduction_mm: bend.allowance.bend_deduction_mm,
            });
            flat_area_mm2 += extension * (end[0] - start[0]).hypot(end[1] - start[1]);
        }
    }

    let [neg_y, pos_x, pos_y, neg_x] = bends.map(|bend| bend.map_or(0.0, Bend::flat_extension_mm));
    let outer_bbox_min_mm = [x_min - neg_x, y_min - neg_y];
    let outer_bbox_max_mm = [x_max + pos_x, y_max + pos_y];
    let flat_width_mm = outer_bbox_max_mm[0] - outer_bbox_min_mm[0];
    let flat_depth_mm = outer_bbox_max_mm[1] - outer_bbox_min_mm[1];
    positive(flat_width_mm, "flat width")?;
    positive(flat_depth_mm, "flat depth")?;
    positive(flat_area_mm2, "flat area")?;

    let folded_mesh = build_folded_mesh(base_corners, spec.thickness_mm, bends);
    folded_mesh
        .validate_closed_manifold()
        .map_err(|error| format!("invalid folded sheet metal mesh: {error}"))?;
    Ok(UnfoldedSheetPattern {
        outer_bbox_min_mm,
        outer_bbox_max_mm,
        flat_width_mm,
        flat_depth_mm,
        flat_area_mm2,
        bend_lines,
        outline_polygon_mm,
        folded_mesh,
    })
}

fn quad(mesh: &mut Mesh, [a, b, c, d]: [u32; 4]) {
    mesh.triangles.extend([[a, b, c], [a, c, d]]);
}

fn build_folded_mesh(corners: [[f64; 2]; 4], thickness: f64, bends: [Option<Bend>; 4]) -> Mesh {
    let mut mesh = Mesh::default();
    for z in [0.0, thickness] {
        mesh.vertices.extend(corners.map(|[x, y]| [x, y, z]));
    }
    quad(&mut mesh, [0, 3, 2, 1]);
    quad(&mut mesh, [4, 5, 6, 7]);
    for edge in SheetMetalEdgeSide::COUNTERCLOCKWISE {
        let i = edge as usize;
        let next = (i + 1) % 4;
        let start = [i as u32, next as u32, next as u32 + 4, i as u32 + 4];
        if let Some(bend) = bends[i] {
            append_flange(&mut mesh, start, edge.outward(), thickness, bend);
        } else {
            quad(&mut mesh, start);
        }
    }
    mesh
}

fn append_flange(
    mesh: &mut Mesh,
    mut section: [u32; 4],
    outward: [f64; 2],
    thickness: f64,
    bend: Bend,
) {
    let seam = [section[0], section[1]].map(|index| {
        let [x, y, _] = mesh.vertices[index as usize];
        [x, y]
    });
    let angle_rad = bend.angle_deg.to_radians();
    let offsets_at = |angle: f64, leg: f64| {
        let (sin, cos) = angle.sin_cos();
        // The half-angle form retains the rise even for shallow bends.
        let rise = 2.0 * (angle * 0.5).sin().powi(2);
        let inner = bend.inner_radius_mm;
        let outer = inner + thickness;
        [
            [outer * sin + leg * cos, outer * rise + leg * sin],
            [
                inner * sin + leg * cos,
                thickness + inner * rise + leg * sin,
            ],
        ]
    };
    // Bound the angular step and include the bend's exact tangent endpoints.
    let segments = (bend.angle_deg / 5.0).ceil().max(1.0) as u32;
    for segment in 1..=segments {
        let angle = angle_rad * (f64::from(segment) / f64::from(segments));
        section = extend_section(mesh, section, seam, outward, offsets_at(angle, 0.0));
    }
    if bend.straight_leg_mm > 0.0 {
        section = extend_section(
            mesh,
            section,
            seam,
            outward,
            offsets_at(angle_rad, bend.straight_leg_mm),
        );
    }
    quad(mesh, section);
}

fn extend_section(
    mesh: &mut Mesh,
    previous: [u32; 4],
    [a, b]: [[f64; 2]; 2],
    outward: [f64; 2],
    [outer, inner]: [[f64; 2]; 2],
) -> [u32; 4] {
    let first = mesh.vertices.len() as u32;
    for (origin, [distance, z]) in [(a, outer), (b, outer), (b, inner), (a, inner)] {
        let [x, y] = offset(origin, outward, distance);
        mesh.vertices.push([x, y, z]);
    }
    let section = [first, first + 1, first + 2, first + 3];
    for i in 0..4 {
        let next = (i + 1) % 4;
        quad(
            mesh,
            [previous[i], previous[next], section[next], section[i]],
        );
    }
    section
}
