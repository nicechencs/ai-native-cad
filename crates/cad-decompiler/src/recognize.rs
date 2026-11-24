//! Modular, pluggable prismatic mechanical feature recognition from an [`AttributedAdjacencyGraph`].
//!
//! Recognizes:
//! - Base stock (`BaseBox` / `BaseCylinder`)
//! - Cylindrical holes (`ThroughHole` / `BlindHole` / `CounterboreHole`)
//! - Cylindrical bosses (`Boss`)
//! - Rectangular pockets / slots (`Pocket`)
//! - Hole patterns (`PolarPattern` / `LinearPattern`, including 2D rectangular grids)
//! - Fillet & chamfer dress-up hints (both smooth/cylindrical and faceted corner rounds / 45° bevels)

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{
    FaceId,
    aag::{
        AagFace, AttributedAdjacencyGraph, EdgeConvexity, Vec3, add, canonical_axis, dot, norm,
        ortho_basis, radial_distance, scale, sub, unit,
    },
};

const DIST_TOL: f64 = 0.05;
const AXIS_DOT_TOL: f64 = 0.998;

/// Tunable tolerances for prismatic feature recognition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognitionOptions {
    pub distance_tol_mm: f64,
    pub angle_dot_tol: f64,
    /// Minimum cylindrical wrap fraction (`0..=1`) to classify a face as a full hole or boss.
    pub min_full_cylinder_fraction: f64,
    /// Enable grouping identical holes into parametric polar/linear arrays.
    pub detect_hole_patterns: bool,
}

impl Default for RecognitionOptions {
    fn default() -> Self {
        Self {
            distance_tol_mm: DIST_TOL,
            angle_dot_tol: AXIS_DOT_TOL,
            min_full_cylinder_fraction: 0.85,
            detect_hole_patterns: true,
        }
    }
}

/// Outer base stock primitive inferred from the AAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseStock {
    BaseBox {
        origin_mm: Vec3,
        size_mm: Vec3,
        face_ids: Vec<usize>,
    },
    BaseCylinder {
        origin_mm: Vec3,
        axis: Vec3,
        radius_mm: f64,
        height_mm: f64,
        face_ids: Vec<usize>,
    },
}

/// Kind of recognized cylindrical hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoleKind {
    ThroughHole,
    BlindHole,
    CounterboreThrough,
    CounterboreBlind,
}

/// Recognized cylindrical hole (simple or stepped counterbore).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoleFeature {
    pub id: String,
    pub kind: HoleKind,
    /// Entry center point on the opening face.
    pub entry_center_mm: Vec3,
    /// Unit drill axis pointing INTO the solid from `entry_center_mm`.
    pub axis: Vec3,
    pub diameter_mm: f64,
    pub depth_mm: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterbore_diameter_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counterbore_depth_mm: Option<f64>,
    pub face_ids: Vec<usize>,
}

/// Recognized outward cylindrical protrusion (boss).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BossFeature {
    pub id: String,
    pub base_center_mm: Vec3,
    pub axis: Vec3,
    pub diameter_mm: f64,
    pub height_mm: f64,
    pub face_ids: Vec<usize>,
}

/// Recognized rectangular pocket or slot depression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PocketFeature {
    pub id: String,
    /// Lower corner of the axis-aligned pocket volume (`[x_min, y_min, z_min]`).
    pub origin_mm: Vec3,
    /// Dimensions `[width_x, depth_y, height_z]` of the cut volume.
    pub size_mm: Vec3,
    /// Outward normal of the opening face where the pocket enters the stock.
    pub opening_normal: Vec3,
    pub is_through: bool,
    pub face_ids: Vec<usize>,
}

/// Pattern family for grouped identical holes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternKind {
    PolarPattern {
        axis_origin_mm: Vec3,
        axis_direction: Vec3,
        count: usize,
        pitch_circle_diameter_mm: f64,
        total_angle_deg: f64,
    },
    LinearPattern {
        direction: Vec3,
        count: usize,
        spacing_mm: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        second_direction: Option<Vec3>,
        #[serde(skip_serializing_if = "Option::is_none")]
        second_count: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        second_spacing_mm: Option<f64>,
    },
}

/// Group of identical holes recognized as a single parametric pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HolePatternFeature {
    pub id: String,
    pub seed_hole: HoleFeature,
    pub pattern: PatternKind,
    pub instance_centers_mm: Vec<Vec3>,
    pub face_ids: Vec<usize>,
}

/// Dress-up transition hint detected on smooth or 45° beveled faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DressUpHintKind {
    Fillet,
    Chamfer,
}

/// Fillet or chamfer hint detected from AAG transition faces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DressUpHint {
    pub id: String,
    pub kind: DressUpHintKind,
    pub radius_or_distance_mm: f64,
    /// `true` when this dress-up applies uniformly to the base stock's vertical/outer edges
    /// and can be directly emitted as a base `FeatureSpec::Fillet` / `FeatureSpec::Chamfer`.
    pub uniform_base_dressup: bool,
    pub face_ids: Vec<usize>,
}

/// High-level tagged recognized feature summary for reporting and reconstruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "feature_kind", rename_all = "snake_case")]
pub enum RecognizedFeature {
    BaseStock(BaseStock),
    Boss(BossFeature),
    Pocket(PocketFeature),
    Hole(HoleFeature),
    HolePattern(HolePatternFeature),
    DressUp(DressUpHint),
}

/// Complete output of the feature recognition pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecognitionResult {
    pub base_stock: Option<BaseStock>,
    pub bosses: Vec<BossFeature>,
    pub pockets: Vec<PocketFeature>,
    pub holes: Vec<HoleFeature>,
    pub hole_patterns: Vec<HolePatternFeature>,
    pub dressup_hints: Vec<DressUpHint>,
    pub explained_face_ids: Vec<usize>,
    pub unrecognized_face_ids: Vec<usize>,
}

impl RecognitionResult {
    /// Flatten recognized features into an ordered summary list.
    pub fn features(&self) -> Vec<RecognizedFeature> {
        let mut out = Vec::new();
        if let Some(base) = &self.base_stock {
            out.push(RecognizedFeature::BaseStock(base.clone()));
        }
        for hint in &self.dressup_hints {
            out.push(RecognizedFeature::DressUp(hint.clone()));
        }
        for boss in &self.bosses {
            out.push(RecognizedFeature::Boss(boss.clone()));
        }
        for pocket in &self.pockets {
            out.push(RecognizedFeature::Pocket(pocket.clone()));
        }
        for hole in &self.holes {
            out.push(RecognizedFeature::Hole(hole.clone()));
        }
        for pat in &self.hole_patterns {
            out.push(RecognizedFeature::HolePattern(pat.clone()));
        }
        out
    }
}

/// Pluggable trait for feature recognizers operating on an AAG.
pub trait FeatureRecognizer {
    fn name(&self) -> &'static str;
    fn recognize(
        &self,
        aag: &AttributedAdjacencyGraph,
        options: &RecognitionOptions,
    ) -> RecognitionResult;
}

/// Default deterministic prismatic mechanical feature recognizer.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultFeatureRecognizer;

impl FeatureRecognizer for DefaultFeatureRecognizer {
    fn name(&self) -> &'static str {
        "prismatic_aag_recognizer_v1"
    }

    fn recognize(
        &self,
        aag: &AttributedAdjacencyGraph,
        options: &RecognitionOptions,
    ) -> RecognitionResult {
        run_prismatic_recognition(aag, options)
    }
}

#[derive(Debug, Clone)]
struct StitchedCylinder {
    axis: Vec3,
    line_point: Vec3,
    axial_min: f64,
    axial_max: f64,
    radius_mm: f64,
    area_mm2: f64,
    is_inward: bool,
    face_ids: Vec<usize>,
}

impl StitchedCylinder {
    fn height(&self) -> f64 {
        (self.axial_max - self.axial_min).max(0.0)
    }

    fn start_point(&self) -> Vec3 {
        add(self.line_point, scale(self.axis, self.axial_min))
    }

    fn end_point(&self) -> Vec3 {
        add(self.line_point, scale(self.axis, self.axial_max))
    }

    fn wrap_fraction(&self) -> f64 {
        let full = std::f64::consts::TAU * self.radius_mm * self.height();
        if full <= 1e-9 {
            0.0
        } else {
            self.area_mm2 / full
        }
    }
}

fn run_prismatic_recognition(
    aag: &AttributedAdjacencyGraph,
    options: &RecognitionOptions,
) -> RecognitionResult {
    let mut explained: BTreeSet<usize> = BTreeSet::new();
    let cylinders = stitch_cylinders(aag, options);

    // Separate full cylinders from partial cylindrical blends (fillets).
    let mut full_outward: Vec<StitchedCylinder> = Vec::new();
    let mut full_inward: Vec<StitchedCylinder> = Vec::new();
    let mut partial_cylinders: Vec<StitchedCylinder> = Vec::new();

    for cyl in cylinders {
        if cyl.wrap_fraction() >= options.min_full_cylinder_fraction {
            if cyl.is_inward {
                full_inward.push(cyl);
            } else {
                full_outward.push(cyl);
            }
        } else {
            partial_cylinders.push(cyl);
        }
    }

    // 1. Identify base stock (BaseCylinder vs BaseBox).
    let (base_stock, used_outward_idx) = recognize_base_stock(aag, &full_outward, options);
    if let Some(base) = &base_stock {
        match base {
            BaseStock::BaseBox { face_ids, .. } | BaseStock::BaseCylinder { face_ids, .. } => {
                explained.extend(face_ids.iter().copied());
            }
        }
    }

    // 2. Recognize bosses from remaining full outward cylinders.
    let mut bosses = Vec::new();
    for (idx, cyl) in full_outward.iter().enumerate() {
        if used_outward_idx == Some(idx) {
            continue;
        }
        let boss = build_boss_feature(bosses.len() + 1, cyl, aag, base_stock.as_ref(), options);
        explained.extend(boss.face_ids.iter().copied());
        bosses.push(boss);
    }

    // 3. Recognize uniform base dress-ups (corner fillets & chamfers) and smooth/bevel hints.
    let dressup_hints =
        recognize_dressups(aag, base_stock.as_ref(), &partial_cylinders, &mut explained);

    // 4. Recognize holes (including stepped counterbores) from full inward cylinders.
    let raw_holes = recognize_holes(aag, &full_inward, base_stock.as_ref(), &bosses, options, &mut explained);

    // 5. Group identical holes into parametric patterns when enabled.
    let (holes, hole_patterns) = if options.detect_hole_patterns {
        group_hole_patterns(raw_holes, base_stock.as_ref(), options)
    } else {
        (raw_holes, Vec::new())
    };

    // 6. Recognize rectangular pockets & slots from recessed planar loops.
    let pockets = recognize_pockets(aag, base_stock.as_ref(), &mut explained, options);

    // Collect explained vs unrecognized faces.
    let all_face_ids: Vec<usize> = (0..aag.faces.len()).collect();
    let unrecognized_face_ids: Vec<usize> = all_face_ids
        .into_iter()
        .filter(|id| !explained.contains(id))
        .collect();
    let explained_face_ids: Vec<usize> = explained.into_iter().collect();

    RecognitionResult {
        base_stock,
        bosses,
        pockets,
        holes,
        hole_patterns,
        dressup_hints,
        explained_face_ids,
        unrecognized_face_ids,
    }
}

fn stitch_cylinders(
    aag: &AttributedAdjacencyGraph,
    options: &RecognitionOptions,
) -> Vec<StitchedCylinder> {
    let mut stitched: Vec<StitchedCylinder> = Vec::new();
    for face in &aag.faces {
        let Some(c) = &face.cylinder else {
            continue;
        };
        let axis = canonical_axis(c.axis);
        let line_point = sub(c.origin, scale(axis, dot(c.origin, axis)));
        let cand = StitchedCylinder {
            axis,
            line_point,
            axial_min: c.axial_min,
            axial_max: c.axial_max,
            radius_mm: c.radius_mm,
            area_mm2: face.area_mm2,
            is_inward: c.is_inward,
            face_ids: vec![face.id.0],
        };
        let mut merged = false;
        for existing in &mut stitched {
            if existing.is_inward == cand.is_inward
                && (existing.radius_mm - cand.radius_mm).abs() <= options.distance_tol_mm
                && dot(existing.axis, cand.axis).abs() >= options.angle_dot_tol
                && radial_distance(cand.line_point, existing.line_point, existing.axis)
                    <= options.distance_tol_mm
                && existing.axial_min <= cand.axial_max + options.distance_tol_mm
                && cand.axial_min <= existing.axial_max + options.distance_tol_mm
            {
                existing.axial_min = existing.axial_min.min(cand.axial_min);
                existing.axial_max = existing.axial_max.max(cand.axial_max);
                existing.area_mm2 += cand.area_mm2;
                existing.face_ids.extend(cand.face_ids.iter().copied());
                merged = true;
                break;
            }
        }
        if !merged {
            stitched.push(cand);
        }
    }
    stitched.sort_by(|a, b| {
        a.radius_mm
            .total_cmp(&b.radius_mm)
            .then_with(|| a.axial_min.total_cmp(&b.axial_min))
            .then_with(|| a.face_ids.first().cmp(&b.face_ids.first()))
    });
    stitched
}

fn recognize_base_stock(
    aag: &AttributedAdjacencyGraph,
    full_outward: &[StitchedCylinder],
    options: &RecognitionOptions,
) -> (Option<BaseStock>, Option<usize>) {
    let bb = aag.solid_bbox;
    if !bb.is_valid() {
        return (None, None);
    }
    let sz = bb.size();
    if sz.iter().any(|&v| v <= 1e-4) {
        return (None, None);
    }

    // Check if an outward cylinder forms the outer radial envelope (BaseCylinder).
    let mut best_cyl: Option<(usize, f64)> = None;
    for (idx, cyl) in full_outward.iter().enumerate() {
        let diam = 2.0 * cyl.radius_mm;
        let radial_match = if cyl.axis[2].abs() > 0.99 {
            (diam - sz[0]).abs() <= options.distance_tol_mm * 4.0
                && (diam - sz[1]).abs() <= options.distance_tol_mm * 4.0
        } else if cyl.axis[0].abs() > 0.99 {
            (diam - sz[1]).abs() <= options.distance_tol_mm * 4.0
                && (diam - sz[2]).abs() <= options.distance_tol_mm * 4.0
        } else if cyl.axis[1].abs() > 0.99 {
            (diam - sz[0]).abs() <= options.distance_tol_mm * 4.0
                && (diam - sz[2]).abs() <= options.distance_tol_mm * 4.0
        } else {
            false
        };
        if radial_match {
            let vol = std::f64::consts::PI * cyl.radius_mm * cyl.radius_mm * cyl.height();
            if best_cyl.is_none_or(|(_, best_v)| vol > best_v) {
                best_cyl = Some((idx, vol));
            }
        }
    }

    // Count how many cardinal box side walls exist on the outer envelope.
    let cardinal_dirs: [Vec3; 6] = [
        [-1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, 1.0],
    ];

    if let Some((cyl_idx, _)) = best_cyl {
        let cyl = &full_outward[cyl_idx];
        // Verify we don't have 4 flat box side walls perpendicular to the cylinder axis.
        let side_planes_count = cardinal_dirs
            .iter()
            .filter(|&&dir| dot(dir, cyl.axis).abs() < 0.1)
            .filter(|&&dir| {
                aag.faces.iter().any(|f| {
                    f.plane
                        .as_ref()
                        .is_some_and(|p| dot(p.normal, dir) >= options.angle_dot_tol)
                })
            })
            .count();

        if side_planes_count < 3 {
            let mut face_ids = cyl.face_ids.clone();
            // Include the cylinder's bottom and top end caps.
            for f in &aag.faces {
                if let Some(p) = &f.plane
                    && dot(p.normal, cyl.axis).abs() >= options.angle_dot_tol
                {
                    let on_lo = (dot(cyl.axis, p.origin) - cyl.axial_min).abs()
                        <= options.distance_tol_mm;
                    let on_hi = (dot(cyl.axis, p.origin) - cyl.axial_max).abs()
                        <= options.distance_tol_mm;
                    if on_lo || on_hi {
                        face_ids.push(f.id.0);
                    }
                }
            }
            face_ids.sort_unstable();
            face_ids.dedup();
            return (
                Some(BaseStock::BaseCylinder {
                    origin_mm: cyl.start_point(),
                    axis: cyl.axis,
                    radius_mm: cyl.radius_mm,
                    height_mm: cyl.height(),
                    face_ids,
                }),
                Some(cyl_idx),
            );
        }
    }

    // Try BaseBox: find outer planar faces along all 6 cardinal directions, excluding boss end caps.
    let boss_cap_ids = find_boss_cap_face_ids(aag, full_outward, options);
    let mut bounds_min = [0.0; 3];
    let mut bounds_max = [0.0; 3];
    let mut base_faces: Vec<usize> = Vec::new();
    let mut covered_dirs = 0usize;

    for (axis_idx, signs) in [(0usize, (-1.0, 1.0)), (1usize, (-1.0, 1.0)), (2usize, (-1.0, 1.0))] {
        let mut neg_dir = [0.0; 3];
        neg_dir[axis_idx] = signs.0;
        let mut pos_dir = [0.0; 3];
        pos_dir[axis_idx] = signs.1;

        let neg_planes = collect_extreme_planar_faces(aag, neg_dir, axis_idx, false, &boss_cap_ids, options);
        let pos_planes = collect_extreme_planar_faces(aag, pos_dir, axis_idx, true, &boss_cap_ids, options);

        if let Some((coord, fids)) = neg_planes {
            bounds_min[axis_idx] = coord;
            base_faces.extend(fids);
            covered_dirs += 1;
        } else {
            bounds_min[axis_idx] = bb.min[axis_idx];
        }

        if let Some((coord, fids)) = pos_planes {
            bounds_max[axis_idx] = coord;
            base_faces.extend(fids);
            covered_dirs += 1;
        } else {
            bounds_max[axis_idx] = bb.max[axis_idx];
        }
    }

    // Require at least 4 cardinal outer faces to accept a BaseBox (rejects spheres/cones/freeform).
    if covered_dirs < 4 {
        return (None, None);
    }

    let size = [
        bounds_max[0] - bounds_min[0],
        bounds_max[1] - bounds_min[1],
        bounds_max[2] - bounds_min[2],
    ];
    if size.iter().any(|&v| v <= 1e-4) {
        return (None, None);
    }

    // Also include all faces coplanar with the 6 base box planes.
    for f in &aag.faces {
        if boss_cap_ids.contains(&f.id.0) {
            continue;
        }
        let Some(p) = &f.plane else { continue };
        for axis_idx in 0..3 {
            let mut neg_dir = [0.0; 3];
            neg_dir[axis_idx] = -1.0;
            let mut pos_dir = [0.0; 3];
            pos_dir[axis_idx] = 1.0;
            if dot(p.normal, neg_dir) >= options.angle_dot_tol
                && (p.origin[axis_idx] - bounds_min[axis_idx]).abs() <= options.distance_tol_mm
            {
                base_faces.push(f.id.0);
            } else if dot(p.normal, pos_dir) >= options.angle_dot_tol
                && (p.origin[axis_idx] - bounds_max[axis_idx]).abs() <= options.distance_tol_mm
            {
                base_faces.push(f.id.0);
            }
        }
    }
    base_faces.sort_unstable();
    base_faces.dedup();

    (
        Some(BaseStock::BaseBox {
            origin_mm: bounds_min,
            size_mm: size,
            face_ids: base_faces,
        }),
        None,
    )
}

/// Identify planar faces that are strictly end caps of smaller protruding cylindrical bosses,
/// so `BaseBox` does not erroneously expand its height to the top of a protruding boss.
fn find_boss_cap_face_ids(
    aag: &AttributedAdjacencyGraph,
    full_outward: &[StitchedCylinder],
    options: &RecognitionOptions,
) -> BTreeSet<usize> {
    let mut caps = BTreeSet::new();
    for cyl in full_outward {
        // Check if this cylinder sits on a larger planar face at one of its ends.
        let end_lo = cyl.start_point();
        let end_hi = cyl.end_point();

        let cap_at_end = |end_pt: Vec3, out_dir: Vec3| -> Vec<usize> {
            let mut ids = Vec::new();
            for f in &aag.faces {
                let Some(p) = &f.plane else { continue };
                if dot(p.normal, out_dir) < options.angle_dot_tol {
                    continue;
                }
                if (dot(out_dir, p.origin) - dot(out_dir, end_pt)).abs() > options.distance_tol_mm {
                    continue;
                }
                // Check if the face's bounding box fits inside the cylinder radius.
                let corners_in_cyl = face_radial_extent_fits_cylinder(f, end_pt, cyl.axis, cyl.radius_mm + options.distance_tol_mm * 2.0);
                if corners_in_cyl {
                    ids.push(f.id.0);
                }
            }
            ids
        };

        let has_larger_base_at = |end_pt: Vec3, out_dir: Vec3| -> bool {
            aag.faces.iter().any(|f| {
                let Some(p) = &f.plane else { return false };
                dot(p.normal, out_dir) >= options.angle_dot_tol
                    && (dot(out_dir, p.origin) - dot(out_dir, end_pt)).abs() <= options.distance_tol_mm
                    && !face_radial_extent_fits_cylinder(f, end_pt, cyl.axis, cyl.radius_mm + options.distance_tol_mm * 2.0)
            })
        };

        if has_larger_base_at(end_lo, cyl.axis) {
            caps.extend(cap_at_end(end_hi, cyl.axis));
        }
        if has_larger_base_at(end_hi, scale(cyl.axis, -1.0)) {
            caps.extend(cap_at_end(end_lo, scale(cyl.axis, -1.0)));
        }
    }
    caps
}

fn face_radial_extent_fits_cylinder(
    face: &AagFace,
    center: Vec3,
    axis: Vec3,
    max_radius: f64,
) -> bool {
    let bb = face.bbox;
    if !bb.is_valid() {
        return false;
    }
    for &x in &[bb.min[0], bb.max[0]] {
        for &y in &[bb.min[1], bb.max[1]] {
            for &z in &[bb.min[2], bb.max[2]] {
                // Check only corners projected perpendicular to axis; for a circle bbox, corners are at sqrt(2)*r.
                let d = sub([x, y, z], center);
                let perp = sub(d, scale(axis, dot(d, axis)));
                if norm(perp) > max_radius * std::f64::consts::SQRT_2 + 0.1 {
                    return false;
                }
            }
        }
    }
    // Also verify every outer_loop point is within `max_radius`.
    if !face.outer_loop.is_empty() {
        return face
            .outer_loop
            .iter()
            .all(|&p| radial_distance(p, center, axis) <= max_radius);
    }
    true
}

fn collect_extreme_planar_faces(
    aag: &AttributedAdjacencyGraph,
    dir: Vec3,
    axis_idx: usize,
    find_max: bool,
    excluded: &BTreeSet<usize>,
    options: &RecognitionOptions,
) -> Option<(f64, Vec<usize>)> {
    let mut best_coord: Option<f64> = None;
    for f in &aag.faces {
        if excluded.contains(&f.id.0) {
            continue;
        }
        let Some(p) = &f.plane else { continue };
        if dot(p.normal, dir) < options.angle_dot_tol {
            continue;
        }
        let c = p.origin[axis_idx];
        best_coord = Some(match best_coord {
            None => c,
            Some(prev) => {
                if find_max {
                    prev.max(c)
                } else {
                    prev.min(c)
                }
            }
        });
    }
    let coord = best_coord?;
    let fids: Vec<usize> = aag
        .faces
        .iter()
        .filter(|f| {
            !excluded.contains(&f.id.0)
                && f.plane.as_ref().is_some_and(|p| {
                    dot(p.normal, dir) >= options.angle_dot_tol
                        && (p.origin[axis_idx] - coord).abs() <= options.distance_tol_mm
                })
        })
        .map(|f| f.id.0)
        .collect();
    Some((coord, fids))
}

fn build_boss_feature(
    index: usize,
    cyl: &StitchedCylinder,
    aag: &AttributedAdjacencyGraph,
    base_stock: Option<&BaseStock>,
    options: &RecognitionOptions,
) -> BossFeature {
    let end_lo = cyl.start_point();
    let end_hi = cyl.end_point();

    // Determine which end rests on the base stock vs which end is the protruding top cap.
    let lo_on_base = point_on_base_surface(end_lo, base_stock, options.distance_tol_mm * 2.0);
    let hi_on_base = point_on_base_surface(end_hi, base_stock, options.distance_tol_mm * 2.0);

    let (base_center_mm, axis, cap_center) = if hi_on_base && !lo_on_base {
        (end_hi, scale(cyl.axis, -1.0), end_lo)
    } else {
        (end_lo, cyl.axis, end_hi)
    };

    let mut face_ids = cyl.face_ids.clone();
    // Mark boss top cap face as explained.
    for f in &aag.faces {
        let Some(p) = &f.plane else { continue };
        if dot(p.normal, axis) >= options.angle_dot_tol
            && (dot(axis, p.origin) - dot(axis, cap_center)).abs() <= options.distance_tol_mm
            && face_radial_extent_fits_cylinder(
                f,
                cap_center,
                axis,
                cyl.radius_mm + options.distance_tol_mm * 2.0,
            )
        {
            face_ids.push(f.id.0);
        }
    }
    face_ids.sort_unstable();
    face_ids.dedup();

    BossFeature {
        id: format!("boss_{index}"),
        base_center_mm,
        axis,
        diameter_mm: 2.0 * cyl.radius_mm,
        height_mm: cyl.height(),
        face_ids,
    }
}

fn point_on_base_surface(pt: Vec3, base: Option<&BaseStock>, tol: f64) -> bool {
    match base {
        Some(BaseStock::BaseBox { origin_mm, size_mm, .. }) => {
            let max = add(*origin_mm, *size_mm);
            let inside_or_on = (0..3).all(|k| pt[k] >= origin_mm[k] - tol && pt[k] <= max[k] + tol);
            let on_boundary = (0..3).any(|k| (pt[k] - origin_mm[k]).abs() <= tol || (pt[k] - max[k]).abs() <= tol);
            inside_or_on && on_boundary
        }
        Some(BaseStock::BaseCylinder {
            origin_mm,
            axis,
            radius_mm,
            height_mm,
            ..
        }) => {
            let t = dot(sub(pt, *origin_mm), *axis);
            let r = radial_distance(pt, *origin_mm, *axis);
            r <= radius_mm + tol && (t.abs() <= tol || (t - height_mm).abs() <= tol)
        }
        None => false,
    }
}

fn recognize_dressups(
    aag: &AttributedAdjacencyGraph,
    base_stock: Option<&BaseStock>,
    partial_cylinders: &[StitchedCylinder],
    explained: &mut BTreeSet<usize>,
) -> Vec<DressUpHint> {
    let mut hints = Vec::new();

    // A. Detect smooth partial cylindrical blends (OCCT exact fillets).
    let mut fillet_groups: Vec<(f64, Vec<usize>)> = Vec::new();
    for cyl in partial_cylinders {
        if cyl.wrap_fraction() >= 0.10 && cyl.wrap_fraction() <= 0.65 {
            // Check if at least one adjacent edge is Smooth or tangent transition.
            let has_smooth = cyl.face_ids.iter().any(|&fid| {
                aag.neighbors(FaceId(fid))
                    .iter()
                    .any(|(_, e)| e.convexity == EdgeConvexity::Smooth)
            });
            if has_smooth || (cyl.wrap_fraction() - 0.25).abs() < 0.08 {
                if let Some((_, fids)) = fillet_groups
                    .iter_mut()
                    .find(|(r, _)| (*r - cyl.radius_mm).abs() <= 0.05)
                {
                    fids.extend(cyl.face_ids.iter().copied());
                } else {
                    fillet_groups.push((cyl.radius_mm, cyl.face_ids.clone()));
                }
            }
        }
    }

    // Check if the partial cylinders form the 4 vertical corner fillets of a BaseBox.
    let mut base_fillet_emitted = false;
    if let Some(BaseStock::BaseBox { origin_mm, size_mm, .. }) = base_stock {
        for (r, fids) in &fillet_groups {
            let all_vertical_z = fids.iter().all(|&fid| {
                aag.face(FaceId(fid))
                    .and_then(|f| f.cylinder.as_ref())
                    .is_some_and(|c| {
                        c.axis[2].abs() > 0.99
                            && (c.height_mm - size_mm[2]).abs() <= 0.1
                            && !c.is_inward
                    })
            });
            let uniform = all_vertical_z && fids.len() >= 4;
            if uniform {
                base_fillet_emitted = true;
            }
            explained.extend(fids.iter().copied());
            hints.push(DressUpHint {
                id: format!("fillet_{}", hints.len() + 1),
                kind: DressUpHintKind::Fillet,
                radius_or_distance_mm: *r,
                uniform_base_dressup: uniform,
                face_ids: fids.clone(),
            });
        }

        // B. Detect faceted vertical corner fillets on Analytic `box |> fillet(r)` solids:
        //    Each of the 4 XY corners has a chain of vertical oblique planar facets spanning [z_min, z_max].
        if !base_fillet_emitted {
            if let Some(hint) = detect_analytic_box_corner_fillet(aag, *origin_mm, *size_mm, explained)
            {
                explained.extend(hint.face_ids.iter().copied());
                hints.push(hint);
            }
        }

        // C. Detect 45-degree chamfer bevel faces on BaseBox.
        if let Some(hint) = detect_box_chamfers(aag, *origin_mm, *size_mm, explained) {
            explained.extend(hint.face_ids.iter().copied());
            hints.push(hint);
        }
    } else {
        for (r, fids) in fillet_groups {
            explained.extend(fids.iter().copied());
            hints.push(DressUpHint {
                id: format!("fillet_{}", hints.len() + 1),
                kind: DressUpHintKind::Fillet,
                radius_or_distance_mm: r,
                uniform_base_dressup: false,
                face_ids: fids,
            });
        }
    }

    hints
}

fn detect_analytic_box_corner_fillet(
    aag: &AttributedAdjacencyGraph,
    origin_mm: Vec3,
    size_mm: Vec3,
    explained: &BTreeSet<usize>,
) -> Option<DressUpHint> {
    let z_min = origin_mm[2];
    let z_max = origin_mm[2] + size_mm[2];
    let x_min = origin_mm[0];
    let x_max = origin_mm[0] + size_mm[0];
    let y_min = origin_mm[1];
    let y_max = origin_mm[1] + size_mm[1];

    // Collect vertical oblique planar faces spanning [z_min, z_max].
    let mut quadrants: [Vec<usize>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    let mut all_fids = Vec::new();
    for f in &aag.faces {
        if explained.contains(&f.id.0) {
            continue;
        }
        let Some(p) = &f.plane else { continue };
        if p.normal[2].abs() > 0.05 {
            continue;
        }
        if p.normal[0].abs() < 0.05 || p.normal[1].abs() < 0.05 {
            continue;
        }
        if (f.bbox.min[2] - z_min).abs() > 0.1 || (f.bbox.max[2] - z_max).abs() > 0.1 {
            continue;
        }
        // Check that outward normal points away from box center (convex corner blend).
        let center_xy = [0.5 * (x_min + x_max), 0.5 * (y_min + y_max)];
        let to_face = [f.centroid[0] - center_xy[0], f.centroid[1] - center_xy[1]];
        if to_face[0] * p.normal[0] + to_face[1] * p.normal[1] <= 0.0 {
            continue;
        }
        let q = match (p.normal[0] > 0.0, p.normal[1] > 0.0) {
            (false, false) => 0,
            (true, false) => 1,
            (true, true) => 2,
            (false, true) => 3,
        };
        quadrants[q].push(f.id.0);
        all_fids.push(f.id.0);
    }

    // Require all 4 quadrants to have at least 2 facets (smoothly segmented corner arcs).
    if quadrants.iter().any(|q| q.len() < 2) {
        return None;
    }

    // Measure fillet radius from the setback of the 4 flat side walls along ±X and ±Y.
    let mut radius_samples = Vec::new();
    for f in &aag.faces {
        let Some(p) = &f.plane else { continue };
        if p.normal[0].abs() > 0.999
            && ((p.origin[0] - x_min).abs() < 0.05 || (p.origin[0] - x_max).abs() < 0.05)
        {
            let flat_y = f.bbox.max[1] - f.bbox.min[1];
            let r = 0.5 * (size_mm[1] - flat_y);
            if r > 0.05 && r < 0.5 * size_mm[0].min(size_mm[1]) {
                radius_samples.push(r);
            }
        } else if p.normal[1].abs() > 0.999
            && ((p.origin[1] - y_min).abs() < 0.05 || (p.origin[1] - y_max).abs() < 0.05)
        {
            let flat_x = f.bbox.max[0] - f.bbox.min[0];
            let r = 0.5 * (size_mm[0] - flat_x);
            if r > 0.05 && r < 0.5 * size_mm[0].min(size_mm[1]) {
                radius_samples.push(r);
            }
        }
    }
    if radius_samples.len() < 4 {
        return None;
    }
    let mean_r = radius_samples.iter().sum::<f64>() / radius_samples.len() as f64;
    if radius_samples
        .iter()
        .any(|&r| (r - mean_r).abs() > (mean_r * 0.03).max(0.05))
    {
        return None;
    }

    all_fids.sort_unstable();
    all_fids.dedup();
    Some(DressUpHint {
        id: "fillet_1".to_owned(),
        kind: DressUpHintKind::Fillet,
        radius_or_distance_mm: mean_r,
        uniform_base_dressup: true,
        face_ids: all_fids,
    })
}

fn detect_box_chamfers(
    aag: &AttributedAdjacencyGraph,
    origin_mm: Vec3,
    size_mm: Vec3,
    explained: &BTreeSet<usize>,
) -> Option<DressUpHint> {
    let mut bevel_fids = Vec::new();
    let mut distances = Vec::new();
    let center = add(origin_mm, scale(size_mm, 0.5));
    let max_pt = add(origin_mm, size_mm);

    for f in &aag.faces {
        if explained.contains(&f.id.0) {
            continue;
        }
        let Some(p) = &f.plane else { continue };
        // 45-degree bevel: two components near ±0.7071 and one near 0.
        let mut abs_c = [p.normal[0].abs(), p.normal[1].abs(), p.normal[2].abs()];
        abs_c.sort_by(f64::total_cmp);
        let is_45 = abs_c[0] < 0.05
            && (abs_c[1] - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.05
            && (abs_c[2] - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.05;
        if !is_45 {
            continue;
        }
        // Outward bevel normal points away from box center.
        if dot(sub(f.centroid, center), p.normal) <= 0.0 {
            continue;
        }
        // Distance from the nearest box edge corner along the two active axes.
        let active_axes: Vec<usize> = (0..3).filter(|&k| p.normal[k].abs() > 0.5).collect();
        if active_axes.len() == 2 {
            let a = active_axes[0];
            let b = active_axes[1];
            let d_a = (f.bbox.max[a] - f.bbox.min[a]).abs();
            let d_b = (f.bbox.max[b] - f.bbox.min[b]).abs();
            let near_a = (f.bbox.min[a] - origin_mm[a]).abs() < 0.1
                || (f.bbox.max[a] - max_pt[a]).abs() < 0.1;
            let near_b = (f.bbox.min[b] - origin_mm[b]).abs() < 0.1
                || (f.bbox.max[b] - max_pt[b]).abs() < 0.1;
            if near_a && near_b && (d_a - d_b).abs() <= (d_a * 0.1).max(0.05) && d_a > 0.05 {
                bevel_fids.push(f.id.0);
                distances.push(0.5 * (d_a + d_b));
            }
        }
    }

    if bevel_fids.is_empty() {
        return None;
    }
    let mean_d = distances.iter().sum::<f64>() / distances.len() as f64;
    let uniform = bevel_fids.len() >= 8
        && distances
            .iter()
            .all(|&d| (d - mean_d).abs() <= (mean_d * 0.05).max(0.05));
    bevel_fids.sort_unstable();
    bevel_fids.dedup();
    Some(DressUpHint {
        id: "chamfer_1".to_owned(),
        kind: DressUpHintKind::Chamfer,
        radius_or_distance_mm: mean_d,
        uniform_base_dressup: uniform,
        face_ids: bevel_fids,
    })
}

fn recognize_holes(
    aag: &AttributedAdjacencyGraph,
    full_inward: &[StitchedCylinder],
    base_stock: Option<&BaseStock>,
    bosses: &[BossFeature],
    options: &RecognitionOptions,
    explained: &mut BTreeSet<usize>,
) -> Vec<HoleFeature> {
    let mut consumed = vec![false; full_inward.len()];
    let mut holes = Vec::new();

    // Pass 1: Detect coaxial stepped counterbore pairs (larger outer cylinder + smaller inner shaft).
    for i in 0..full_inward.len() {
        if consumed[i] {
            continue;
        }
        for j in (i + 1)..full_inward.len() {
            if consumed[j] {
                continue;
            }
            let a = &full_inward[i];
            let b = &full_inward[j];
            if dot(a.axis, b.axis).abs() < options.angle_dot_tol {
                continue;
            }
            if radial_distance(b.line_point, a.line_point, a.axis) > options.distance_tol_mm {
                continue;
            }
            if (a.radius_mm - b.radius_mm).abs() <= options.distance_tol_mm {
                continue;
            }
            // Check end-to-end contact at the counterbore shelf.
            let a_meets_b = (a.axial_max - b.axial_min).abs() <= options.distance_tol_mm * 2.0
                || (b.axial_max - a.axial_min).abs() <= options.distance_tol_mm * 2.0;
            if !a_meets_b {
                continue;
            }

            let (cbore, shaft) = if a.radius_mm > b.radius_mm {
                (a, b)
            } else {
                (b, a)
            };
            // Determine drill direction from outer opening of `cbore` toward `shaft`.
            let (entry_center_mm, drill_axis, shaft_far_end, shelf_pt) =
                if cbore.axial_max > shaft.axial_max {
                    (
                        cbore.end_point(),
                        scale(cbore.axis, -1.0),
                        shaft.start_point(),
                        cbore.start_point(),
                    )
                } else {
                    (
                        cbore.start_point(),
                        cbore.axis,
                        shaft.end_point(),
                        cbore.end_point(),
                    )
                };

            let mut fids = cbore.face_ids.clone();
            fids.extend(shaft.face_ids.iter().copied());

            // Mark annular shelf plane at `shelf_pt` as explained.
            for f in &aag.faces {
                let Some(p) = &f.plane else { continue };
                if dot(p.normal, drill_axis).abs() >= options.angle_dot_tol
                    && (dot(cbore.axis, p.origin) - dot(cbore.axis, shelf_pt)).abs()
                        <= options.distance_tol_mm
                    && face_radial_extent_fits_cylinder(
                        f,
                        shelf_pt,
                        cbore.axis,
                        cbore.radius_mm + options.distance_tol_mm * 2.0,
                    )
                {
                    fids.push(f.id.0);
                }
            }

            let blind_cap = find_hole_cap(aag, shaft_far_end, scale(drill_axis, -1.0), shaft.radius_mm, options);
            let kind = if let Some(cap_id) = blind_cap {
                fids.push(cap_id);
                HoleKind::CounterboreBlind
            } else {
                HoleKind::CounterboreThrough
            };

            fids.sort_unstable();
            fids.dedup();
            explained.extend(fids.iter().copied());
            consumed[i] = true;
            consumed[j] = true;

            holes.push(HoleFeature {
                id: format!("hole_{}", holes.len() + 1),
                kind,
                entry_center_mm,
                axis: drill_axis,
                diameter_mm: 2.0 * shaft.radius_mm,
                depth_mm: cbore.height() + shaft.height(),
                counterbore_diameter_mm: Some(2.0 * cbore.radius_mm),
                counterbore_depth_mm: Some(cbore.height()),
                face_ids: fids,
            });
            break;
        }
    }

    // Pass 2: Classify remaining single inward cylinders as ThroughHole or BlindHole.
    for (i, cyl) in full_inward.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        let end_lo = cyl.start_point();
        let end_hi = cyl.end_point();
        let lo_cap = find_hole_cap(aag, end_lo, cyl.axis, cyl.radius_mm, options);
        let hi_cap = find_hole_cap(aag, end_hi, scale(cyl.axis, -1.0), cyl.radius_mm, options);

        let mut fids = cyl.face_ids.clone();
        let (kind, entry_center_mm, drill_axis) = match (lo_cap, hi_cap) {
            (Some(cap_id), None) => {
                fids.push(cap_id);
                (HoleKind::BlindHole, end_hi, scale(cyl.axis, -1.0))
            }
            (None, Some(cap_id)) => {
                fids.push(cap_id);
                (HoleKind::BlindHole, end_lo, cyl.axis)
            }
            _ => {
                // Through hole: prefer entry at the high end along canonical axis (e.g., top +Z face,
                // drilling toward -Z, matching CAD-DSL convention).
                let _ = (base_stock, bosses);
                (HoleKind::ThroughHole, end_hi, scale(cyl.axis, -1.0))
            }
        };

        fids.sort_unstable();
        fids.dedup();
        explained.extend(fids.iter().copied());
        holes.push(HoleFeature {
            id: format!("hole_{}", holes.len() + 1),
            kind,
            entry_center_mm,
            axis: drill_axis,
            diameter_mm: 2.0 * cyl.radius_mm,
            depth_mm: cyl.height(),
            counterbore_diameter_mm: None,
            counterbore_depth_mm: None,
            face_ids: fids,
        });
    }

    holes
}

fn find_hole_cap(
    aag: &AttributedAdjacencyGraph,
    end_center: Vec3,
    inward_normal: Vec3,
    radius_mm: f64,
    options: &RecognitionOptions,
) -> Option<usize> {
    for f in &aag.faces {
        let Some(p) = &f.plane else { continue };
        if dot(p.normal, inward_normal) < options.angle_dot_tol {
            continue;
        }
        if (dot(inward_normal, p.origin) - dot(inward_normal, end_center)).abs()
            > options.distance_tol_mm
        {
            continue;
        }
        // A blind hole cap is a small planar disk whose radial extent matches `radius_mm`.
        if face_radial_extent_fits_cylinder(
            f,
            end_center,
            inward_normal,
            radius_mm + options.distance_tol_mm * 2.0,
        ) {
            let expected_area = std::f64::consts::PI * radius_mm * radius_mm;
            if f.area_mm2 <= expected_area * 1.25 {
                return Some(f.id.0);
            }
        }
    }
    None
}

fn group_hole_patterns(
    holes: Vec<HoleFeature>,
    base_stock: Option<&BaseStock>,
    options: &RecognitionOptions,
) -> (Vec<HoleFeature>, Vec<HolePatternFeature>) {
    let mut groups: Vec<Vec<HoleFeature>> = Vec::new();
    for hole in holes {
        let matched = groups.iter_mut().find(|g| {
            let s = &g[0];
            s.kind == hole.kind
                && dot(s.axis, hole.axis) >= options.angle_dot_tol
                && (s.diameter_mm - hole.diameter_mm).abs() <= options.distance_tol_mm
                && (s.depth_mm - hole.depth_mm).abs() <= options.distance_tol_mm * 2.0
                && opt_close(s.counterbore_diameter_mm, hole.counterbore_diameter_mm, options.distance_tol_mm)
                && opt_close(s.counterbore_depth_mm, hole.counterbore_depth_mm, options.distance_tol_mm * 2.0)
                && dot(s.axis, sub(hole.entry_center_mm, s.entry_center_mm)).abs()
                    <= options.distance_tol_mm * 2.0
        });
        if let Some(g) = matched {
            g.push(hole);
        } else {
            groups.push(vec![hole]);
        }
    }

    let mut standalone = Vec::new();
    let mut patterns = Vec::new();
    let _ = base_stock;

    for mut group in groups {
        if group.len() < 2 {
            standalone.extend(group);
            continue;
        }
        group.sort_by(|a, b| compare_vec3(a.entry_center_mm, b.entry_center_mm));
        let centers: Vec<Vec3> = group.iter().map(|h| h.entry_center_mm).collect();
        let axis = group[0].axis;

        if let Some((seed_idx, pattern, ordered_centers)) =
            detect_hole_array_pattern(&centers, axis, options.distance_tol_mm)
        {
            let mut face_ids = Vec::new();
            for h in &group {
                face_ids.extend(h.face_ids.iter().copied());
            }
            face_ids.sort_unstable();
            face_ids.dedup();
            let seed = group[seed_idx].clone();
            patterns.push(HolePatternFeature {
                id: format!("hole_pattern_{}", patterns.len() + 1),
                seed_hole: seed,
                pattern,
                instance_centers_mm: ordered_centers,
                face_ids,
            });
        } else {
            standalone.extend(group);
        }
    }

    for (idx, h) in standalone.iter_mut().enumerate() {
        h.id = format!("hole_{}", idx + 1);
    }

    (standalone, patterns)
}

fn detect_hole_array_pattern(
    centers: &[Vec3],
    axis: Vec3,
    tol: f64,
) -> Option<(usize, PatternKind, Vec<Vec3>)> {
    let n = centers.len();
    if n < 2 {
        return None;
    }
    let (u_ax, v_ax) = ortho_basis(axis);
    let origin = centers[0];
    let pts_2d: Vec<[f64; 2]> = centers
        .iter()
        .map(|&p| {
            let d = sub(p, origin);
            [dot(d, u_ax), dot(d, v_ax)]
        })
        .collect();

    // 1. Check 1D linear pattern (collinear and uniformly spaced).
    if let Some((seed_idx, dir_2d, spacing, ordered)) = detect_linear_1d(&pts_2d, tol) {
        let dir_3d = unit(add(scale(u_ax, dir_2d[0]), scale(v_ax, dir_2d[1])))?;
        let ordered_centers = ordered.into_iter().map(|i| centers[i]).collect();
        return Some((
            seed_idx,
            PatternKind::LinearPattern {
                direction: dir_3d,
                count: n,
                spacing_mm: spacing,
                second_direction: None,
                second_count: None,
                second_spacing_mm: None,
            },
            ordered_centers,
        ));
    }

    // 2. Check PolarPattern (n >= 3 evenly spaced around a pitch circle).
    if n >= 3
        && let Some((center_2d, radius, ordered)) = detect_polar_circle(&pts_2d, tol)
    {
        let axis_origin_mm = add(origin, add(scale(u_ax, center_2d[0]), scale(v_ax, center_2d[1])));
        let seed_idx = ordered[0];
        let ordered_centers = ordered.into_iter().map(|i| centers[i]).collect();
        // Ensure canonical axis direction matching right-hand rule around pattern plane.
        let pat_axis = canonical_axis(axis);
        return Some((
            seed_idx,
            PatternKind::PolarPattern {
                axis_origin_mm,
                axis_direction: pat_axis,
                count: n,
                pitch_circle_diameter_mm: 2.0 * radius,
                total_angle_deg: 360.0,
            },
            ordered_centers,
        ));
    }

    // 3. Check 2D orthogonal grid LinearPattern (e.g., 2x2 rectangular mounting bolt holes).
    if n >= 4
        && let Some((seed_idx, d1_2d, c1, s1, d2_2d, c2, s2, ordered)) =
            detect_linear_2d(&pts_2d, tol)
    {
        let dir1 = unit(add(scale(u_ax, d1_2d[0]), scale(v_ax, d1_2d[1])))?;
        let dir2 = unit(add(scale(u_ax, d2_2d[0]), scale(v_ax, d2_2d[1])))?;
        let ordered_centers = ordered.into_iter().map(|i| centers[i]).collect();
        return Some((
            seed_idx,
            PatternKind::LinearPattern {
                direction: dir1,
                count: c1,
                spacing_mm: s1,
                second_direction: Some(dir2),
                second_count: Some(c2),
                second_spacing_mm: Some(s2),
            },
            ordered_centers,
        ));
    }

    None
}

fn detect_linear_1d(pts: &[[f64; 2]], tol: f64) -> Option<(usize, [f64; 2], f64, Vec<usize>)> {
    if pts.len() < 2 {
        return None;
    }
    let a = pts[0];
    let (far_idx, _) = pts
        .iter()
        .enumerate()
        .max_by(|(_, p), (_, q)| {
            let dp = (p[0] - a[0]).hypot(p[1] - a[1]);
            let dq = (q[0] - a[0]).hypot(q[1] - a[1]);
            dp.total_cmp(&dq)
        })?;
    let b = pts[far_idx];
    let len = (b[0] - a[0]).hypot(b[1] - a[1]);
    if len <= tol {
        return None;
    }
    let dir = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
    let mut proj: Vec<(usize, f64)> = Vec::with_capacity(pts.len());
    for (idx, p) in pts.iter().enumerate() {
        let dx = p[0] - a[0];
        let dy = p[1] - a[1];
        let perp = (dx * dir[1] - dy * dir[0]).abs();
        if perp > tol {
            return None;
        }
        proj.push((idx, dx * dir[0] + dy * dir[1]));
    }
    proj.sort_by(|x, y| x.1.total_cmp(&y.1));
    let spacing = (proj.last()?.1 - proj.first()?.1) / (pts.len() - 1) as f64;
    if spacing <= tol {
        return None;
    }
    let start = proj[0].1;
    for (k, &(_, t)) in proj.iter().enumerate() {
        if (t - (start + k as f64 * spacing)).abs() > tol {
            return None;
        }
    }
    let ordered: Vec<usize> = proj.into_iter().map(|(i, _)| i).collect();
    Some((ordered[0], dir, spacing, ordered))
}

fn detect_polar_circle(pts: &[[f64; 2]], tol: f64) -> Option<([f64; 2], f64, Vec<usize>)> {
    let n = pts.len();
    if n < 3 {
        return None;
    }
    // Centroid is exact circle center for any regular polygon (uniform angular spacing).
    let cx = pts.iter().map(|p| p[0]).sum::<f64>() / n as f64;
    let cy = pts.iter().map(|p| p[1]).sum::<f64>() / n as f64;
    let radii: Vec<f64> = pts.iter().map(|p| (p[0] - cx).hypot(p[1] - cy)).collect();
    let mean_r = radii.iter().sum::<f64>() / n as f64;
    if mean_r <= tol {
        return None;
    }
    let r_tol = tol.max(mean_r * 1e-3);
    if radii.iter().any(|&r| (r - mean_r).abs() > r_tol) {
        return None;
    }

    let mut angled: Vec<(usize, f64)> = pts
        .iter()
        .enumerate()
        .map(|(i, p)| (i, (p[1] - cy).atan2(p[0] - cx).rem_euclid(std::f64::consts::TAU)))
        .collect();
    angled.sort_by(|a, b| a.1.total_cmp(&b.1));

    let expected_step = std::f64::consts::TAU / n as f64;
    let angle_tol = 0.015; // ~0.85 deg
    for k in 0..n {
        let a0 = angled[k].1;
        let a1 = angled[(k + 1) % n].1;
        let diff = (a1 - a0).rem_euclid(std::f64::consts::TAU);
        if (diff - expected_step).abs() > angle_tol {
            return None;
        }
    }

    let ordered: Vec<usize> = angled.into_iter().map(|(i, _)| i).collect();
    Some(([cx, cy], mean_r, ordered))
}

#[allow(clippy::type_complexity)]
fn detect_linear_2d(
    pts: &[[f64; 2]],
    tol: f64,
) -> Option<(usize, [f64; 2], usize, f64, [f64; 2], usize, f64, Vec<usize>)> {
    // Try coordinate basis [1,0]/[0,1] first (handles axis-aligned mounting grids cleanly).
    let d1 = [1.0, 0.0];
    let d2 = [0.0, 1.0];

    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    for p in pts {
        if !xs.iter().any(|&x| (x - p[0]).abs() <= tol) {
            xs.push(p[0]);
        }
        if !ys.iter().any(|&y| (y - p[1]).abs() <= tol) {
            ys.push(p[1]);
        }
    }
    xs.sort_by(f64::total_cmp);
    ys.sort_by(f64::total_cmp);

    if xs.len() < 2 || ys.len() < 2 || xs.len() * ys.len() != pts.len() {
        return None;
    }

    let dx = (xs.last()? - xs.first()?) / (xs.len() - 1) as f64;
    let dy = (ys.last()? - ys.first()?) / (ys.len() - 1) as f64;
    if dx <= tol || dy <= tol {
        return None;
    }
    for (i, &x) in xs.iter().enumerate() {
        if (x - (xs[0] + i as f64 * dx)).abs() > tol {
            return None;
        }
    }
    for (j, &y) in ys.iter().enumerate() {
        if (y - (ys[0] + j as f64 * dy)).abs() > tol {
            return None;
        }
    }

    // Verify every (x_i, y_j) grid point maps to a unique hole.
    let mut ordered = Vec::with_capacity(pts.len());
    for &x in &xs {
        for &y in &ys {
            let idx = pts
                .iter()
                .position(|p| (p[0] - x).abs() <= tol && (p[1] - y).abs() <= tol)?;
            ordered.push(idx);
        }
    }
    Some((ordered[0], d1, xs.len(), dx, d2, ys.len(), dy, ordered))
}

fn recognize_pockets(
    aag: &AttributedAdjacencyGraph,
    base_stock: Option<&BaseStock>,
    explained: &mut BTreeSet<usize>,
    options: &RecognitionOptions,
) -> Vec<PocketFeature> {
    let Some(BaseStock::BaseBox {
        origin_mm: base_origin,
        size_mm: base_size,
        ..
    }) = base_stock
    else {
        return Vec::new();
    };
    let base_max = add(*base_origin, *base_size);
    let mut pockets = Vec::new();

    // Candidate recessed planar floors: unexplained planar faces whose normal aligns with one
    // of the 6 cardinal axes and whose plane lies strictly inside the base box along that axis.
    let mut floor_candidates: Vec<(usize, usize, f64, Vec3)> = Vec::new();
    for f in &aag.faces {
        if explained.contains(&f.id.0) {
            continue;
        }
        let Some(p) = &f.plane else { continue };
        for axis_idx in 0..3 {
            let comp = p.normal[axis_idx];
            if comp.abs() < options.angle_dot_tol {
                continue;
            }
            let coord = p.origin[axis_idx];
            let strictly_inside = coord > base_origin[axis_idx] + options.distance_tol_mm * 2.0
                && coord < base_max[axis_idx] - options.distance_tol_mm * 2.0;
            if !strictly_inside {
                continue;
            }
            // Must have at least one concave neighbor wall perpendicular to the floor.
            let has_concave_wall = aag.neighbors(f.id).iter().any(|&(nid, edge)| {
                edge.convexity == EdgeConvexity::Concave
                    && aag
                        .face(nid)
                        .and_then(|nf| nf.plane.as_ref())
                        .is_some_and(|np| dot(np.normal, p.normal).abs() < 0.05)
            });
            if has_concave_wall {
                let outer_coord = if comp > 0.0 {
                    base_max[axis_idx]
                } else {
                    base_origin[axis_idx]
                };
                let depth = (outer_coord - coord).abs();
                floor_candidates.push((f.id.0, axis_idx, depth, p.normal));
            }
        }
    }

    // Process from shallowest to deepest so stepped pockets are ordered cleanly.
    floor_candidates.sort_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.cmp(&b.0)));

    for (floor_fid, axis_idx, depth, opening_normal) in floor_candidates {
        if explained.contains(&floor_fid) {
            continue;
        }
        let floor = &aag.faces[floor_fid];
        let floor_plane = floor.plane.as_ref().unwrap();
        let u_idx = (axis_idx + 1) % 3;
        let v_idx = (axis_idx + 2) % 3;

        // Verify the floor outer boundary is rectangular (accounting for any inner hole loops).
        let u_span = floor.bbox.max[u_idx] - floor.bbox.min[u_idx];
        let v_span = floor.bbox.max[v_idx] - floor.bbox.min[v_idx];
        if u_span <= options.distance_tol_mm * 2.0 || v_span <= options.distance_tol_mm * 2.0 {
            continue;
        }
        let outer_rect_area = u_span * v_span;
        let effective_outer_area = if floor.outer_loop.len() >= 3 {
            polygon_area_projected(&floor.outer_loop, u_idx, v_idx)
        } else {
            floor.area_mm2
        };
        if (effective_outer_area / outer_rect_area) < 0.96 {
            continue;
        }

        // Collect all vertical planar pocket walls surrounding this floor.
        let mut wall_fids = Vec::new();
        for (nid, edge) in aag.neighbors(FaceId(floor_fid)) {
            if edge.convexity != EdgeConvexity::Concave {
                continue;
            }
            let Some(nf) = aag.face(nid) else { continue };
            let Some(np) = &nf.plane else { continue };
            if dot(np.normal, floor_plane.normal).abs() > 0.05 {
                continue;
            }
            // Wall must face inward toward the floor centroid and align with u or v.
            let is_cardinal = np.normal[u_idx].abs() >= options.angle_dot_tol
                || np.normal[v_idx].abs() >= options.angle_dot_tol;
            if is_cardinal && dot(sub(floor.centroid, nf.centroid), np.normal) > 0.0 {
                wall_fids.push(nid.0);
            }
        }
        if wall_fids.is_empty() {
            continue;
        }

        let floor_coord = floor_plane.origin[axis_idx];
        let outer_coord = if opening_normal[axis_idx] > 0.0 {
            base_max[axis_idx]
        } else {
            base_origin[axis_idx]
        };
        let mut pocket_origin = [0.0; 3];
        let mut pocket_size = [0.0; 3];
        pocket_origin[u_idx] = floor.bbox.min[u_idx];
        pocket_origin[v_idx] = floor.bbox.min[v_idx];
        pocket_origin[axis_idx] = floor_coord.min(outer_coord);
        pocket_size[u_idx] = u_span;
        pocket_size[v_idx] = v_span;
        pocket_size[axis_idx] = depth;

        let mut fids = vec![floor_fid];
        fids.extend(wall_fids);
        fids.sort_unstable();
        fids.dedup();
        explained.extend(fids.iter().copied());

        pockets.push(PocketFeature {
            id: format!("pocket_{}", pockets.len() + 1),
            origin_mm: pocket_origin,
            size_mm: pocket_size,
            opening_normal,
            is_through: false,
            face_ids: fids,
        });
    }

    // Also check for rectangular through-slots (4 inward-facing cardinal vertical walls with
    // no floor, spanning from base_origin[axis] to base_max[axis]).
    let mut through_wall_candidates: Vec<usize> = aag
        .faces
        .iter()
        .filter(|f| !explained.contains(&f.id.0))
        .filter(|f| {
            f.plane.as_ref().is_some_and(|p| {
                p.normal[2].abs() < 0.05
                    && (p.normal[0].abs() >= options.angle_dot_tol
                        || p.normal[1].abs() >= options.angle_dot_tol)
                    && (f.bbox.min[2] - base_origin[2]).abs() <= options.distance_tol_mm * 2.0
                    && (f.bbox.max[2] - base_max[2]).abs() <= options.distance_tol_mm * 2.0
            })
        })
        .map(|f| f.id.0)
        .collect();

    let mut visited = BTreeSet::new();
    for &start_fid in &through_wall_candidates {
        if visited.contains(&start_fid) {
            continue;
        }
        // Flood-fill connected concave vertical walls.
        let mut comp = Vec::new();
        let mut stack = vec![start_fid];
        visited.insert(start_fid);
        while let Some(cur) = stack.pop() {
            comp.push(cur);
            for (nid, edge) in aag.neighbors(FaceId(cur)) {
                if edge.convexity == EdgeConvexity::Concave
                    && through_wall_candidates.contains(&nid.0)
                    && visited.insert(nid.0)
                {
                    stack.push(nid.0);
                }
            }
        }
        if comp.len() == 4 {
            let mut px = None;
            let mut nx = None;
            let mut py = None;
            let mut ny = None;
            for &fid in &comp {
                let p = aag.faces[fid].plane.as_ref().unwrap();
                if p.normal[0] > 0.99 {
                    px = Some(p.origin[0]);
                } else if p.normal[0] < -0.99 {
                    nx = Some(p.origin[0]);
                } else if p.normal[1] > 0.99 {
                    py = Some(p.origin[1]);
                } else if p.normal[1] < -0.99 {
                    ny = Some(p.origin[1]);
                }
            }
            if let (Some(x_min), Some(x_max), Some(y_min), Some(y_max)) = (px, nx, py, ny)
                && x_max > x_min + options.distance_tol_mm
                && y_max > y_min + options.distance_tol_mm
            {
                comp.sort_unstable();
                explained.extend(comp.iter().copied());
                pockets.push(PocketFeature {
                    id: format!("pocket_{}", pockets.len() + 1),
                    origin_mm: [x_min, y_min, base_origin[2]],
                    size_mm: [x_max - x_min, y_max - y_min, base_size[2]],
                    opening_normal: [0.0, 0.0, 1.0],
                    is_through: true,
                    face_ids: comp,
                });
            }
        }
    }
    through_wall_candidates.clear();

    pockets
}

fn polygon_area_projected(pts: &[Vec3], u_idx: usize, v_idx: usize) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        sum += pts[i][u_idx] * pts[j][v_idx] - pts[j][u_idx] * pts[i][v_idx];
    }
    0.5 * sum.abs()
}

fn opt_close(a: Option<f64>, b: Option<f64>, tol: f64) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x - y).abs() <= tol,
        _ => false,
    }
}

fn compare_vec3(a: Vec3, b: Vec3) -> std::cmp::Ordering {
    (0..3)
        .map(|k| a[k].total_cmp(&b[k]))
        .find(|c| c.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}
