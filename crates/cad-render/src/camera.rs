//! 双精度相机。大装配的平移先在 CPU 上用 f64 算完，再转成 f32 交给 GPU。

use cad_kernel_occt::Mat4;

/// 正交看工程图，透视看外形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionMode {
    Orthographic,
    Perspective,
}

/// 与现有工程图一致的四个观察方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewPreset {
    Front,
    Top,
    Right,
    Iso,
}

/// 观察目标用毫米，角度用度。视口交互以后只改这些数，不改零件坐标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    pub target_mm: [f64; 3],
    pub distance_mm: f64,
    pub yaw_deg: f64,
    pub pitch_deg: f64,
    pub projection: ProjectionMode,
    pub ortho_height_mm: f64,
    pub fov_y_deg: f64,
}

impl Camera {
    pub fn preset(preset: ViewPreset, target_mm: [f64; 3], distance_mm: f64) -> Self {
        let (yaw_deg, pitch_deg) = match preset {
            ViewPreset::Front => (0.0, 0.0),
            ViewPreset::Right => (90.0, 0.0),
            ViewPreset::Top => (0.0, 90.0),
            ViewPreset::Iso => (45.0, (1.0_f64 / 3.0_f64.sqrt()).asin().to_degrees()),
        };
        Self {
            target_mm,
            distance_mm,
            yaw_deg,
            pitch_deg,
            projection: ProjectionMode::Orthographic,
            ortho_height_mm: 100.0,
            fov_y_deg: 45.0,
        }
    }

    /// 眼睛位置。偏航 0、俯仰 0 时从目标的 -Y 侧看向 XZ 平面。
    pub fn eye_mm(self) -> [f64; 3] {
        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        let flat = pitch.cos();
        let offset = [yaw.sin() * flat, -yaw.cos() * flat, pitch.sin()];
        let distance = if self.distance_mm.is_finite() {
            self.distance_mm
        } else {
            0.0
        };
        [
            self.target_mm[0] + offset[0] * distance,
            self.target_mm[1] + offset[1] * distance,
            self.target_mm[2] + offset[2] * distance,
        ]
    }

    /// 预设视角切换时的直线插值。`t` 超出 0 到 1 会被夹住。
    pub fn lerp(self, other: Self, t: f64) -> Self {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Self {
            target_mm: [
                mix(self.target_mm[0], other.target_mm[0]),
                mix(self.target_mm[1], other.target_mm[1]),
                mix(self.target_mm[2], other.target_mm[2]),
            ],
            distance_mm: mix(self.distance_mm, other.distance_mm),
            yaw_deg: mix(self.yaw_deg, other.yaw_deg),
            pitch_deg: mix(self.pitch_deg, other.pitch_deg),
            projection: if t < 1.0 {
                self.projection
            } else {
                other.projection
            },
            ortho_height_mm: mix(self.ortho_height_mm, other.ortho_height_mm),
            fov_y_deg: mix(self.fov_y_deg, other.fov_y_deg),
        }
    }
}

/// GPU 平移：`(原点 - 相机焦点) + 旋转 × 原型基准 + 爆炸偏移`，最后才转成 f32。
pub fn camera_relative_translation(
    origin_mm: [f64; 3],
    rotation_deg: [f64; 3],
    anchor_mm: [f64; 3],
    camera_target_mm: [f64; 3],
    explode_offset_mm: [f64; 3],
) -> [f32; 3] {
    let rotated_anchor = rotate_f64(rotation_deg, anchor_mm);
    let exact = [
        origin_mm[0] - camera_target_mm[0] + rotated_anchor[0] + explode_offset_mm[0],
        origin_mm[1] - camera_target_mm[1] + rotated_anchor[1] + explode_offset_mm[1],
        origin_mm[2] - camera_target_mm[2] + rotated_anchor[2] + explode_offset_mm[2],
    ];
    [
        finite_f32(exact[0]),
        finite_f32(exact[1]),
        finite_f32(exact[2]),
    ]
}

/// 着色器里的相机相对坐标：先用 f32 旋转局部顶点，再加上 f64 算好的平移。
pub fn camera_relative_position_mm(
    local_position: [f32; 3],
    origin_mm: [f64; 3],
    rotation_deg: [f64; 3],
    anchor_mm: [f64; 3],
    camera_target_mm: [f64; 3],
    explode_offset_mm: [f64; 3],
) -> [f32; 3] {
    let translation = camera_relative_translation(
        origin_mm,
        rotation_deg,
        anchor_mm,
        camera_target_mm,
        explode_offset_mm,
    );
    let rotated = rotate_f32(rotation_deg, local_position);
    [
        rotated[0] + translation[0],
        rotated[1] + translation[1],
        rotated[2] + translation[2],
    ]
}

fn rotate_f64(rotation_deg: [f64; 3], point: [f64; 3]) -> [f64; 3] {
    let matrix = Mat4::rotate_euler_deg(rotation_deg[0], rotation_deg[1], rotation_deg[2]);
    apply_linear(matrix, point, |value| value)
}

fn rotate_f32(rotation_deg: [f64; 3], point: [f32; 3]) -> [f32; 3] {
    let matrix = Mat4::rotate_euler_deg(rotation_deg[0], rotation_deg[1], rotation_deg[2]);
    apply_linear(matrix, point, |value| value as f32)
}

fn apply_linear<T>(matrix: Mat4, point: [T; 3], cast: impl Fn(f64) -> T) -> [T; 3]
where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<Output = T>,
{
    let row = |index: usize| {
        cast(matrix.m[index][0]) * point[0]
            + cast(matrix.m[index][1]) * point[1]
            + cast(matrix.m[index][2]) * point[2]
    };
    [row(0), row(1), row(2)]
}

fn finite_f32(value: f64) -> f32 {
    if value.is_finite() { value as f32 } else { 0.0 }
}
