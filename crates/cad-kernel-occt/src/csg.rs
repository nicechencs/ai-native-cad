use crate::types::{BBox3, BRepEdge, BRepFace, ComponentMeta, MateRecord, PmiRecord, SketchPlane};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4 {
    pub m: [[f64; 4]; 4],
}

impl Mat4 {
    pub fn identity() -> Self {
        Self {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn translate(dx: f64, dy: f64, dz: f64) -> Self {
        let mut r = Self::identity();
        r.m[0][3] = dx;
        r.m[1][3] = dy;
        r.m[2][3] = dz;
        r
    }

    pub fn rotate_axis(axis: [f64; 3], angle_rad: f64) -> Self {
        let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2])
            .sqrt()
            .max(1e-12);
        let x = axis[0] / len;
        let y = axis[1] / len;
        let z = axis[2] / len;
        let c = angle_rad.cos();
        let s = angle_rad.sin();
        let t = 1.0 - c;
        Self {
            m: [
                [t * x * x + c, t * x * y - s * z, t * x * z + s * y, 0.0],
                [t * x * y + s * z, t * y * y + c, t * y * z - s * x, 0.0],
                [t * x * z - s * y, t * y * z + s * x, t * z * z + c, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub fn rotate_euler_deg(rx_deg: f64, ry_deg: f64, rz_deg: f64) -> Self {
        let rx = Self::rotate_axis([1.0, 0.0, 0.0], rx_deg.to_radians());
        let ry = Self::rotate_axis([0.0, 1.0, 0.0], ry_deg.to_radians());
        let rz = Self::rotate_axis([0.0, 0.0, 1.0], rz_deg.to_radians());
        rz.mul_mat(ry.mul_mat(rx))
    }

    pub fn mul_mat(self, rhs: Mat4) -> Mat4 {
        let mut out = [[0.0; 4]; 4];
        for i in 0..4 {
            for j in 0..4 {
                out[i][j] = (0..4).map(|k| self.m[i][k] * rhs.m[k][j]).sum();
            }
        }
        Mat4 { m: out }
    }

    pub fn transform_point(&self, p: [f64; 3]) -> [f64; 3] {
        [
            self.m[0][0] * p[0] + self.m[0][1] * p[1] + self.m[0][2] * p[2] + self.m[0][3],
            self.m[1][0] * p[0] + self.m[1][1] * p[1] + self.m[1][2] * p[2] + self.m[1][3],
            self.m[2][0] * p[0] + self.m[2][1] * p[1] + self.m[2][2] * p[2] + self.m[2][3],
        ]
    }

    pub fn transform_dir(&self, d: [f64; 3]) -> [f64; 3] {
        let r = [
            self.m[0][0] * d[0] + self.m[0][1] * d[1] + self.m[0][2] * d[2],
            self.m[1][0] * d[0] + self.m[1][1] * d[1] + self.m[1][2] * d[2],
            self.m[2][0] * d[0] + self.m[2][1] * d[1] + self.m[2][2] * d[2],
        ];
        let len = (r[0] * r[0] + r[1] * r[1] + r[2] * r[2]).sqrt().max(1e-12);
        [r[0] / len, r[1] / len, r[2] / len]
    }

    pub fn inverse_rigid(&self) -> Mat4 {
        // Transpose of upper-left 3x3 rotation, rotated negated translation
        let t = [self.m[0][3], self.m[1][3], self.m[2][3]];
        let rx = -(self.m[0][0] * t[0] + self.m[1][0] * t[1] + self.m[2][0] * t[2]);
        let ry = -(self.m[0][1] * t[0] + self.m[1][1] * t[1] + self.m[2][1] * t[2]);
        let rz = -(self.m[0][2] * t[0] + self.m[1][2] * t[1] + self.m[2][2] * t[2]);
        Mat4 {
            m: [
                [self.m[0][0], self.m[1][0], self.m[2][0], rx],
                [self.m[0][1], self.m[1][1], self.m[2][1], ry],
                [self.m[0][2], self.m[1][2], self.m[2][2], rz],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }
}

#[derive(Debug, Clone)]
pub enum CsgNode {
    Empty,
    Box {
        min: [f64; 3],
        max: [f64; 3],
    },
    ChamferedBox {
        min: [f64; 3],
        max: [f64; 3],
        chamfer_d: f64,
    },
    Cylinder {
        origin: [f64; 3],
        axis: [f64; 3],
        radius: f64,
        height: f64,
    },
    Counterbore {
        origin: [f64; 3],
        r_through: f64,
        r_cbore: f64,
        depth_through: f64,
        depth_cbore: f64,
    },
    Prism {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
        offset_min: f64,
        offset_max: f64,
    },
    Revolve {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
        axis: [f64; 3],
        angle_deg: f64,
    },
    Union(Vec<CsgNode>),
    Subtract {
        base: Box<CsgNode>,
        tools: Vec<CsgNode>,
    },
    Transform {
        child: Box<CsgNode>,
        fwd: Mat4,
        inv: Mat4,
    },
    Reflect {
        child: Box<CsgNode>,
        plane: SketchPlane,
    },
    Mesh {
        bbox: BBox3,
        triangles: Vec<[[f64; 3]; 3]>,
    },
}

impl CsgNode {
    pub fn transformed(self, fwd: Mat4) -> Self {
        let inv = fwd.inverse_rigid();
        CsgNode::Transform {
            child: Box::new(self),
            fwd,
            inv,
        }
    }

    pub fn bbox(&self) -> BBox3 {
        match self {
            CsgNode::Empty => BBox3::default(),
            CsgNode::Box { min, max } | CsgNode::ChamferedBox { min, max, .. } => BBox3 {
                min: *min,
                max: *max,
            },
            CsgNode::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
            } => {
                let r = r_through.max(*r_cbore);
                let z_min = origin[2] - depth_through;
                let z_max = origin[2] + 1.0;
                let _ = depth_cbore;
                BBox3 {
                    min: [origin[0] - r, origin[1] - r, z_min],
                    max: [origin[0] + r, origin[1] + r, z_max],
                }
            }
            CsgNode::Cylinder {
                origin,
                axis,
                radius,
                height,
            } => {
                let end = [
                    origin[0] + axis[0] * height,
                    origin[1] + axis[1] * height,
                    origin[2] + axis[2] * height,
                ];
                let mut bb = BBox3::empty();
                // Tight radial extents perpendicular to axis
                let rx = radius * (1.0 - axis[0] * axis[0]).max(0.0).sqrt();
                let ry = radius * (1.0 - axis[1] * axis[1]).max(0.0).sqrt();
                let rz = radius * (1.0 - axis[2] * axis[2]).max(0.0).sqrt();
                for c in [origin, &end] {
                    bb.expand_point([c[0] - rx, c[1] - ry, c[2] - rz]);
                    bb.expand_point([c[0] + rx, c[1] + ry, c[2] + rz]);
                }
                bb
            }
            CsgNode::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
            } => {
                let mut bb = BBox3::empty();
                for &[u, v] in polygon_2d {
                    bb.expand_point(plane.to_3d(u, v, *offset_min));
                    bb.expand_point(plane.to_3d(u, v, *offset_max));
                }
                bb
            }
            CsgNode::Revolve {
                plane,
                polygon_2d,
                axis,
                ..
            } => {
                let mut bb = BBox3::empty();
                let mut max_r = 0.0f64;
                let mut z_min = f64::INFINITY;
                let mut z_max = f64::NEG_INFINITY;
                for &[u, v] in polygon_2d {
                    let p3 = plane.to_3d(u, v, 0.0);
                    let axial = p3[0] * axis[0] + p3[1] * axis[1] + p3[2] * axis[2];
                    let perp2 = (p3[0] - axial * axis[0]).powi(2)
                        + (p3[1] - axial * axis[1]).powi(2)
                        + (p3[2] - axial * axis[2]).powi(2);
                    max_r = max_r.max(perp2.sqrt());
                    z_min = z_min.min(axial);
                    z_max = z_max.max(axial);
                }
                if axis[2].abs() > 0.9 {
                    bb.expand_point([-max_r, -max_r, z_min]);
                    bb.expand_point([max_r, max_r, z_max]);
                } else if axis[0].abs() > 0.9 {
                    bb.expand_point([z_min, -max_r, -max_r]);
                    bb.expand_point([z_max, max_r, max_r]);
                } else {
                    bb.expand_point([-max_r, z_min, -max_r]);
                    bb.expand_point([max_r, z_max, max_r]);
                }
                bb
            }
            CsgNode::Union(items) => {
                let mut bb = BBox3::empty();
                for it in items {
                    bb = bb.union(it.bbox());
                }
                if bb.is_valid() {
                    bb
                } else {
                    BBox3::default()
                }
            }
            CsgNode::Subtract { base, .. } => base.bbox(),
            CsgNode::Transform { child, fwd, .. } => {
                let cbb = child.bbox();
                if !cbb.is_valid() {
                    return cbb;
                }
                let mut bb = BBox3::empty();
                for sx in [cbb.min[0], cbb.max[0]] {
                    for sy in [cbb.min[1], cbb.max[1]] {
                        for sz in [cbb.min[2], cbb.max[2]] {
                            bb.expand_point(fwd.transform_point([sx, sy, sz]));
                        }
                    }
                }
                bb
            }
            CsgNode::Reflect { child, plane } => {
                let cbb = child.bbox();
                if !cbb.is_valid() {
                    return cbb;
                }
                match plane {
                    SketchPlane::Yz => BBox3 {
                        min: [-cbb.max[0], cbb.min[1], cbb.min[2]],
                        max: [-cbb.min[0], cbb.max[1], cbb.max[2]],
                    },
                    SketchPlane::Xz => BBox3 {
                        min: [cbb.min[0], -cbb.max[1], cbb.min[2]],
                        max: [cbb.max[0], -cbb.min[1], cbb.max[2]],
                    },
                    SketchPlane::Xy => BBox3 {
                        min: [cbb.min[0], cbb.min[1], -cbb.max[2]],
                        max: [cbb.max[0], cbb.max[1], -cbb.min[2]],
                    },
                }
            }
            CsgNode::Mesh { bbox, .. } => *bbox,
        }
    }

    pub fn contains(&self, p: [f64; 3]) -> bool {
        match self {
            CsgNode::Empty => false,
            CsgNode::Box { min, max } => {
                p[0] >= min[0]
                    && p[0] <= max[0]
                    && p[1] >= min[1]
                    && p[1] <= max[1]
                    && p[2] >= min[2]
                    && p[2] <= max[2]
            }
            CsgNode::ChamferedBox { min, max, chamfer_d } => {
                if p[0] < min[0]
                    || p[0] > max[0]
                    || p[1] < min[1]
                    || p[1] > max[1]
                    || p[2] < min[2]
                    || p[2] > max[2]
                {
                    return false;
                }
                let d = *chamfer_d;
                let dx_out = (min[0] + d - p[0]).max(0.0).max((p[0] - (max[0] - d)).max(0.0));
                let dy_out = (min[1] + d - p[1]).max(0.0).max((p[1] - (max[1] - d)).max(0.0));
                let dz_out = (min[2] + d - p[2]).max(0.0).max((p[2] - (max[2] - d)).max(0.0));
                dx_out.max(dy_out) + dz_out <= d + 1e-9
            }
            CsgNode::Counterbore {
                origin,
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
            } => {
                let dx = p[0] - origin[0];
                let dy = p[1] - origin[1];
                let r2 = dx * dx + dy * dy;
                let z_bot = origin[2] - depth_through;
                let z_shelf = origin[2] - depth_cbore;
                let z_top = origin[2] + 1.0;
                if p[2] < z_bot || p[2] > z_top {
                    false
                } else if p[2] >= z_shelf {
                    r2 <= r_cbore * r_cbore
                } else {
                    r2 <= r_through * r_through
                }
            }
            CsgNode::Cylinder {
                origin,
                axis,
                radius,
                height,
            } => {
                let d = [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]];
                let t = d[0] * axis[0] + d[1] * axis[1] + d[2] * axis[2];
                if t < 0.0 || t > *height {
                    return false;
                }
                let px = d[0] - t * axis[0];
                let py = d[1] - t * axis[1];
                let pz = d[2] - t * axis[2];
                px * px + py * py + pz * pz <= radius * radius
            }
            CsgNode::Prism {
                plane,
                polygon_2d,
                offset_min,
                offset_max,
            } => {
                let (u, v, w) = match plane {
                    SketchPlane::Xy => (p[0], p[1], p[2]),
                    SketchPlane::Xz => (p[0], p[2], p[1]),
                    SketchPlane::Yz => (p[1], p[2], p[0]),
                };
                if w < *offset_min || w > *offset_max {
                    return false;
                }
                point_in_polygon_2d(u, v, polygon_2d)
            }
            CsgNode::Revolve {
                plane,
                polygon_2d,
                axis,
                angle_deg,
            } => {
                // Project p into (r, axial) around `axis`
                let axial = p[0] * axis[0] + p[1] * axis[1] + p[2] * axis[2];
                let rx = p[0] - axial * axis[0];
                let ry = p[1] - axial * axis[1];
                let rz = p[2] - axial * axis[2];
                let r = (rx * rx + ry * ry + rz * rz).sqrt();
                if *angle_deg < 359.99 {
                    let theta = if axis[2].abs() > 0.9 {
                        p[1].atan2(p[0]).to_degrees()
                    } else if axis[0].abs() > 0.9 {
                        p[2].atan2(p[1]).to_degrees()
                    } else {
                        p[0].atan2(p[2]).to_degrees()
                    };
                    let t = ((theta % 360.0) + 360.0) % 360.0;
                    if t > *angle_deg {
                        return false;
                    }
                }
                // Check in sketch plane coordinates (u = r, v = axial)
                let (u, v) = match plane {
                    SketchPlane::Xz | SketchPlane::Yz => (r, axial),
                    SketchPlane::Xy => (r, axial),
                };
                point_in_polygon_2d(u, v, polygon_2d) || point_in_polygon_2d(-u, v, polygon_2d)
            }
            CsgNode::Union(items) => items.iter().any(|c| c.contains(p)),
            CsgNode::Subtract { base, tools } => {
                base.contains(p) && !tools.iter().any(|t| t.contains(p))
            }
            CsgNode::Transform { child, inv, .. } => child.contains(inv.transform_point(p)),
            CsgNode::Reflect { child, plane } => {
                let rp = match plane {
                    SketchPlane::Yz => [-p[0], p[1], p[2]],
                    SketchPlane::Xz => [p[0], -p[1], p[2]],
                    SketchPlane::Xy => [p[0], p[1], -p[2]],
                };
                child.contains(rp)
            }
            CsgNode::Mesh { bbox, triangles } => {
                if p[0] < bbox.min[0]
                    || p[0] > bbox.max[0]
                    || p[1] < bbox.min[1]
                    || p[1] > bbox.max[1]
                    || p[2] < bbox.min[2]
                    || p[2] > bbox.max[2]
                {
                    return false;
                }
                point_in_closed_mesh_ray_x(p, triangles)
            }
        }
    }

    pub fn compute_volume(&self) -> f64 {
        match self {
            CsgNode::Empty => 0.0,
            CsgNode::Box { min, max } => {
                (max[0] - min[0]).max(0.0)
                    * (max[1] - min[1]).max(0.0)
                    * (max[2] - min[2]).max(0.0)
            }
            CsgNode::ChamferedBox { min, max, chamfer_d } => {
                let w = (max[0] - min[0]).max(0.0);
                let d_len = (max[1] - min[1]).max(0.0);
                let h = (max[2] - min[2]).max(0.0);
                let c = *chamfer_d;
                // Middle prism [z_min+c .. z_max-c] + 2 octagonal-frustum caps of height c
                let mid = w * d_len * (h - 2.0 * c).max(0.0);
                let a_base = w * d_len;
                let a_top = (w - 2.0 * c).max(0.0) * (d_len - 2.0 * c).max(0.0);
                let cap = (c / 3.0) * (a_base + a_top + (a_base * a_top).sqrt());
                mid + 2.0 * cap
            }
            CsgNode::Counterbore {
                r_through,
                r_cbore,
                depth_through,
                depth_cbore,
                ..
            } => {
                let h_low = (depth_through - depth_cbore).max(0.0);
                std::f64::consts::PI * r_through * r_through * h_low
                    + std::f64::consts::PI * r_cbore * r_cbore * (depth_cbore + 1.0)
            }
            CsgNode::Cylinder { radius, height, .. } => {
                std::f64::consts::PI * radius * radius * height.max(0.0)
            }
            CsgNode::Prism {
                polygon_2d,
                offset_min,
                offset_max,
                ..
            } => polygon_area_2d(polygon_2d) * (offset_max - offset_min).max(0.0),
            CsgNode::Revolve {
                polygon_2d,
                angle_deg,
                ..
            } => {
                let (area, cx) = polygon_area_and_centroid_x(polygon_2d);
                2.0 * std::f64::consts::PI * cx.abs() * area * (angle_deg / 360.0).clamp(0.0, 1.0)
            }
            CsgNode::Transform { child, .. } | CsgNode::Reflect { child, .. } => {
                child.compute_volume()
            }
            CsgNode::Mesh { triangles, .. } => closed_mesh_volume(triangles),
            CsgNode::Union(_) | CsgNode::Subtract { .. } => {
                let bb = self.bbox();
                let sz = bb.size();
                let box_vol = sz[0] * sz[1] * sz[2];
                if box_vol <= 1e-9 {
                    return 0.0;
                }
                let nx = 64usize;
                let ny = 64usize;
                let nz = 64usize;
                let total = (nx * ny * nz) as f64;
                let mut inside = 0usize;
                let dx = sz[0] / nx as f64;
                let dy = sz[1] / ny as f64;
                let dz = sz[2] / nz as f64;
                for ix in 0..nx {
                    let x = bb.min[0] + (ix as f64 + 0.5) * dx;
                    for iy in 0..ny {
                        let y = bb.min[1] + (iy as f64 + 0.5) * dy;
                        for iz in 0..nz {
                            let z = bb.min[2] + (iz as f64 + 0.5) * dz;
                            if self.contains([x, y, z]) {
                                inside += 1;
                            }
                        }
                    }
                }
                box_vol * (inside as f64 / total)
            }
        }
    }
}

pub fn polygon_area_2d(pts: &[[f64; 2]]) -> f64 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        sum += pts[i][0] * pts[j][1] - pts[j][0] * pts[i][1];
    }
    0.5 * sum.abs()
}

fn polygon_area_and_centroid_x(pts: &[[f64; 2]]) -> (f64, f64) {
    if pts.len() < 3 {
        return (0.0, 0.0);
    }
    let mut a2 = 0.0;
    let mut cx_acc = 0.0;
    for i in 0..pts.len() {
        let j = (i + 1) % pts.len();
        let cross = pts[i][0] * pts[j][1] - pts[j][0] * pts[i][1];
        a2 += cross;
        cx_acc += (pts[i][0] + pts[j][0]) * cross;
    }
    let area = 0.5 * a2.abs();
    if area < 1e-12 {
        (0.0, 0.0)
    } else {
        (area, (cx_acc / (3.0 * a2)).abs())
    }
}

pub fn point_in_polygon_2d(px: f64, py: f64, poly: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let xi = poly[i][0];
        let yi = poly[i][1];
        let xj = poly[j][0];
        let yj = poly[j][1];
        let intersect = ((yi > py) != (yj > py))
            && (px < (xj - xi) * (py - yi) / (yj - yi + 1e-30) + xi);
        if intersect {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[derive(Debug, Clone)]
pub struct SolidShape {
    pub part_name: String,
    pub csg: CsgNode,
    pub faces: Vec<BRepFace>,
    pub edges: Vec<BRepEdge>,
    pub pmi: Vec<PmiRecord>,
    pub exact_brep: bool,
    pub components: Vec<ComponentMeta>,
    pub mates: Vec<MateRecord>,
}

impl SolidShape {
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            part_name: name.into(),
            csg: CsgNode::Empty,
            faces: Vec::new(),
            edges: Vec::new(),
            pmi: Vec::new(),
            exact_brep: true,
            components: Vec::new(),
            mates: Vec::new(),
        }
    }
}

fn closed_mesh_volume(triangles: &[[[f64; 3]; 3]]) -> f64 {
    let mut sum6 = 0.0;
    for [a, b, c] in triangles {
        let cx = b[1] * c[2] - b[2] * c[1];
        let cy = b[2] * c[0] - b[0] * c[2];
        let cz = b[0] * c[1] - b[1] * c[0];
        sum6 += a[0] * cx + a[1] * cy + a[2] * cz;
    }
    (sum6 / 6.0).abs()
}

fn point_in_closed_mesh_ray_x(p: [f64; 3], triangles: &[[[f64; 3]; 3]]) -> bool {
    let mut crossings = 0usize;
    let dir = [1.0, 0.000137, 0.000211];
    for &[v0, v1, v2] in triangles {
        let e1 = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
        let e2 = [v2[0] - v0[0], v2[1] - v0[1], v2[2] - v0[2]];
        let h = [
            dir[1] * e2[2] - dir[2] * e2[1],
            dir[2] * e2[0] - dir[0] * e2[2],
            dir[0] * e2[1] - dir[1] * e2[0],
        ];
        let a = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
        if a.abs() < 1e-12 {
            continue;
        }
        let f = 1.0 / a;
        let s = [p[0] - v0[0], p[1] - v0[1], p[2] - v0[2]];
        let u = f * (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]);
        if u < 0.0 || u > 1.0 {
            continue;
        }
        let q = [
            s[1] * e1[2] - s[2] * e1[1],
            s[2] * e1[0] - s[0] * e1[2],
            s[0] * e1[1] - s[1] * e1[0],
        ];
        let v = f * (dir[0] * q[0] + dir[1] * q[1] + dir[2] * q[2]);
        if v < 0.0 || u + v > 1.0 {
            continue;
        }
        let t = f * (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]);
        if t > 1e-7 {
            crossings += 1;
        }
    }
    crossings % 2 == 1
}
