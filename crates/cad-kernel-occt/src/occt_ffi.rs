//! C++ OpenCASCADE (OCCT) dynamic FFI bridge declarations.
//! Enabled only with `--features occt-ffi` when linking against `cpp/occt-bridge`.

use crate::{
    BBox3, BRepEdge, BRepFace, CheckReport, ComponentMeta, CsgNode, CurveType, KernelError,
    KernelOperationError, KernelPayload, Mat4, OcctImportFormat, OcctImportMetadata,
    OcctNativeError, SketchPlane, SolidShape, SurfaceType,
    brep_gen::{transform_preview, transform_preview_geometry},
    freeform::{BsplineLoftSpec, DraftFacesSpec, QuadGridSurfaceSpec, VariableFilletSpec},
    sketch_plane_to_occt_code,
};
use cad_syntax::Span;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, c_char, c_double, c_int};
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub use crate::OcctImportOptions;
pub use crate::brep_kernel::{EXPECTED_OCCT_ABI_VERSION, validate_occt_abi_version};

pub const OCCT_OK: i32 = 0;
pub const OCCT_ERR_BUFFER_TOO_SMALL: i32 = -10;

/// Resolve and invoke `aicad_kernel_abi_version` from the loaded `occt_bridge`
/// dynamic library, verifying it matches [`EXPECTED_OCCT_ABI_VERSION`].
pub fn verify_abi_handshake() -> Result<u32, KernelError> {
    use std::sync::OnceLock;
    static HANDSHAKE: OnceLock<Result<u32, String>> = OnceLock::new();
    match HANDSHAKE.get_or_init(|| {
        validate_occt_abi_version(resolve_kernel_abi_version()).map_err(|err| err.to_string())
    }) {
        Ok(version) => Ok(*version),
        Err(message) => Err(KernelError::OperationFailed(message.clone())),
    }
}

#[cfg(windows)]
fn resolve_kernel_abi_version() -> Option<u32> {
    use std::ffi::c_void;
    use std::ptr;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, proc_name: *const c_char) -> *const c_void;
    }

    let dll_name: Vec<u16> = "occt_bridge.dll".encode_utf16().chain([0]).collect();
    // SAFETY: `dll_name` is NUL-terminated UTF-16; `GetModuleHandleW` does not increment refcount.
    let module = unsafe {
        let m = GetModuleHandleW(dll_name.as_ptr());
        if !m.is_null() {
            m
        } else {
            GetModuleHandleW(ptr::null())
        }
    };
    if module.is_null() {
        return None;
    }
    let symbol = c"aicad_kernel_abi_version";
    // SAFETY: `module` is a valid loaded module handle and `symbol` is NUL-terminated ASCII.
    let proc = unsafe { GetProcAddress(module, symbol.as_ptr()) };
    if proc.is_null() {
        return None;
    }
    // SAFETY: `aicad_kernel_abi_version` has C signature `uint32_t aicad_kernel_abi_version(void)`.
    let func: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(proc) };
    Some(unsafe { func() })
}

#[cfg(unix)]
fn resolve_kernel_abi_version() -> Option<u32> {
    use std::ffi::c_void;

    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[cfg(target_os = "macos")]
    const RTLD_DEFAULT: *mut c_void = (-2isize) as *mut c_void;
    #[cfg(not(target_os = "macos"))]
    const RTLD_DEFAULT: *mut c_void = std::ptr::null_mut();

    let symbol = c"aicad_kernel_abi_version";
    // SAFETY: `RTLD_DEFAULT` searches loaded shared libraries and `symbol` is NUL-terminated.
    let proc = unsafe { dlsym(RTLD_DEFAULT, symbol.as_ptr()) };
    if proc.is_null() {
        return None;
    }
    // SAFETY: `aicad_kernel_abi_version` has C signature `uint32_t aicad_kernel_abi_version(void)`.
    let func: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(proc) };
    Some(unsafe { func() })
}

#[cfg(not(any(windows, unix)))]
fn resolve_kernel_abi_version() -> Option<u32> {
    None
}

#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct OcctBridgeBBox {
    pub min: [c_double; 3],
    pub max: [c_double; 3],
}

#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct OcctBridgeMetrics {
    pub watertight: c_int,
    pub volume_mm3: c_double,
    pub bbox: OcctBridgeBBox,
    pub face_count: c_int,
    pub edge_count: c_int,
}

#[repr(C)]
pub struct OcctImportMetadataRaw {
    pub unit_detected: [c_char; 16],
    pub is_watertight_solid: i32,
    pub open_shell_count: i32,
    pub component_count: i32,
}

#[derive(Default)]
#[repr(C)]
pub struct OcctTessellationCounts {
    pub face_count: i32,
    pub total_vertices: u32,
    pub total_indices: u32,
}

#[derive(Clone, Default)]
#[repr(C)]
pub struct OcctFaceDescriptor {
    pub face_index: i32,
    pub surface_kind: i32,
    pub normal_or_axis: [f64; 3],
    pub origin_or_center: [f64; 3],
    pub radius_mm: f64,
    pub area_mm2: f64,
    pub tri_vertex_offset: u32,
    pub tri_vertex_count: u32,
    pub tri_index_offset: u32,
    pub tri_index_count: u32,
}

#[derive(Clone)]
#[repr(C)]
pub struct OcctComponentDescriptor {
    pub component_id: [c_char; 128],
    pub part_name: [c_char; 128],
    pub transform_col_major: [f64; 16],
    pub shape_handle: u64,
}

#[derive(Default)]
#[repr(C)]
struct OcctBridgeSymmetricDifference {
    candidate_only_mm3: f64,
    reference_only_mm3: f64,
    reference_volume_mm3: f64,
}

#[repr(C)]
struct OcctBridgeLastError {
    status_code: i32,
    operation: [c_char; 64],
    message: [c_char; 512],
}

unsafe extern "C" {
    fn occt_bridge_take_last_error(out: *mut OcctBridgeLastError) -> i32;
    pub fn occt_bridge_version() -> *const c_char;
    pub fn occt_bridge_make_box(
        dx: c_double,
        dy: c_double,
        dz: c_double,
        ox: c_double,
        oy: c_double,
        oz: c_double,
    ) -> u64;
    pub fn occt_bridge_make_cylinder(
        diameter: c_double,
        height: c_double,
        ox: c_double,
        oy: c_double,
        oz: c_double,
    ) -> u64;
    pub fn occt_bridge_boolean_cut(base_handle: u64, tool_handle: u64) -> u64;
    fn occt_bridge_make_cylinder_axis(
        diameter: c_double,
        height: c_double,
        origin: *const c_double,
        axis: *const c_double,
    ) -> u64;
    fn occt_bridge_sweep_polygon(
        profile_xy: *const c_double,
        profile_count: i32,
        path_xyz: *const c_double,
        path_count: i32,
    ) -> u64;
    fn occt_bridge_loft_polygons(
        section_count: i32,
        section_point_counts: *const i32,
        plane_codes: *const i32,
        offsets: *const c_double,
        xy_pairs: *const c_double,
        ruled: i32,
    ) -> u64;
    pub fn occt_bridge_boolean_union(base_handle: u64, tool_handle: u64) -> u64;
    pub fn occt_bridge_boolean_intersect(base_handle: u64, tool_handle: u64) -> u64;
    fn occt_bridge_has_exact_interference() -> i32;
    fn occt_bridge_check_interference(
        a_handle: u64,
        b_handle: u64,
        min_clearance_mm: c_double,
        out_measurement: *mut c_double,
    ) -> i32;
    fn occt_bridge_measure_symmetric_difference(
        candidate_handle: u64,
        reference_handle: u64,
        out: *mut OcctBridgeSymmetricDifference,
    ) -> i32;
    fn occt_bridge_read_through_holes(
        handle: u64,
        count: i32,
        face_indices: *const i32,
        out_radii: *mut c_double,
    ) -> i32;
    fn occt_bridge_offset_through_hole(
        handle: u64,
        face_index: i32,
        delta_mm: f64,
        capacity: i32,
        out_source_indices: *mut i32,
        out_handle: *mut u64,
    ) -> i32;
    fn occt_bridge_move_through_hole(
        handle: u64,
        face_index: i32,
        offset_mm: *const f64,
        capacity: i32,
        out_source_indices: *mut i32,
        out_handle: *mut u64,
    ) -> i32;
    fn occt_bridge_delete_through_hole(
        handle: u64,
        face_index: i32,
        capacity: i32,
        out_source_indices: *mut i32,
        out_handle: *mut u64,
    ) -> i32;
    pub fn occt_bridge_fillet_all(handle: u64, radius: c_double) -> u64;
    pub fn occt_bridge_chamfer_all(handle: u64, distance: c_double) -> u64;
    pub fn occt_bridge_fillet_edges(
        handle: u64,
        radius: c_double,
        edge_indices: *const i32,
        edge_count: i32,
    ) -> u64;
    pub fn occt_bridge_chamfer_edges(
        handle: u64,
        distance: c_double,
        edge_indices: *const i32,
        edge_count: i32,
    ) -> u64;
    pub fn occt_bridge_shell(
        handle: u64,
        thickness: c_double,
        open_nx: c_double,
        open_ny: c_double,
        open_nz: c_double,
    ) -> u64;
    pub fn occt_bridge_extrude_polygon(
        plane_code: c_int,
        xy_pairs: *const c_double,
        point_count: c_int,
        height: c_double,
        symmetric: c_int,
    ) -> u64;
    pub fn occt_bridge_check(handle: u64, out_metrics: *mut OcctBridgeMetrics) -> c_int;
    pub fn occt_bridge_volume_centroid(handle: u64, out_mm: *mut f64) -> c_int;
    pub fn occt_bridge_export_step_named(
        handle: u64,
        path_utf8: *const c_char,
        name_utf8: *const c_char,
    ) -> c_int;
    fn occt_bridge_export_step_assembly(
        count: i32,
        handles: *const u64,
        names_utf8: *const *const c_char,
        assembly_name_utf8: *const c_char,
        path_utf8: *const c_char,
    ) -> c_int;
    pub fn occt_bridge_free(handle: u64);
    pub fn occt_bridge_import_file(
        path_utf8: *const c_char,
        format: i32,
        opts: *const OcctImportOptions,
        out_meta: *mut OcctImportMetadataRaw,
        err_buf: *mut c_char,
        err_buf_len: i32,
    ) -> u64;
    pub fn occt_bridge_get_components(
        root_handle: u64,
        out_items: *mut OcctComponentDescriptor,
        max_items: i32,
    ) -> i32;
    fn occt_bridge_get_component_prototypes(
        root_handle: u64,
        out_indices: *mut i32,
        capacity: i32,
    ) -> i32;
    pub fn occt_bridge_mesh_and_query_counts(
        handle: u64,
        opts: *const OcctImportOptions,
        out_counts: *mut OcctTessellationCounts,
    ) -> i32;
    pub fn occt_bridge_extract_tessellation(
        handle: u64,
        max_faces: i32,
        max_vertices: u32,
        max_indices: u32,
        out_faces: *mut OcctFaceDescriptor,
        out_positions_mm: *mut f64,
        out_normals: *mut f64,
        out_indices: *mut u32,
    ) -> i32;
    pub fn occt_bridge_transform(handle: u64, mat_col_major: *const f64) -> u64;
    pub fn occt_bridge_revolve_polygon(
        plane_code: c_int,
        xy_pairs: *const f64,
        point_count: c_int,
        axis_origin: *const f64,
        axis_dir: *const f64,
        angle_deg: f64,
    ) -> u64;
    fn occt_bridge_make_bspline_loft(
        section_count: i32,
        section_point_counts: *const i32,
        xyz_points: *const f64,
        make_solid: i32,
        ruled: i32,
    ) -> u64;
    fn occt_bridge_draft_faces(
        handle: u64,
        draft_angle_deg: f64,
        pull_dir: *const f64,
        neutral_origin: *const f64,
        neutral_normal: *const f64,
    ) -> u64;
    fn occt_bridge_variable_fillet(handle: u64, r_start_mm: f64, r_end_mm: f64) -> u64;
    fn occt_bridge_quad_grid_to_bspline_surface(
        nu: i32,
        nv: i32,
        xyz_grid: *const f64,
        thickness_mm: f64,
    ) -> u64;
}

#[derive(Debug)]
struct ShapeId(NonZeroU64);

impl ShapeId {
    fn new(raw: u64, op: &str) -> Result<Self, KernelError> {
        verify_abi_handshake()?;
        Self::from_ffi(raw, op).map_err(Into::into)
    }

    fn from_ffi(raw: u64, op: &str) -> Result<Self, KernelOperationError> {
        verify_abi_handshake()?;
        NonZeroU64::new(raw)
            .map(Self)
            .ok_or_else(|| ffi_failure(op))
    }

    fn raw(&self) -> u64 {
        self.0.get()
    }

    fn metrics(&self) -> Result<OcctBridgeMetrics, KernelError> {
        let mut metrics = OcctBridgeMetrics::default();
        status(
            unsafe { occt_bridge_check(self.raw(), &mut metrics) },
            "check",
        )?;
        let bbox = BBox3 {
            min: metrics.bbox.min,
            max: metrics.bbox.max,
        };
        if !bbox.is_valid()
            || !bbox.min.into_iter().chain(bbox.max).all(f64::is_finite)
            || !metrics.volume_mm3.is_finite()
            || metrics.face_count < 0
            || metrics.edge_count < 0
        {
            return Err(failure("invalid native shape metrics"));
        }
        Ok(metrics)
    }

    fn transformed(&self, matrix: Mat4) -> Result<Self, KernelError> {
        if !is_rigid(matrix) {
            return Err(failure("placement is not a finite rigid transform"));
        }
        let data: [f64; 16] = std::array::from_fn(|i| matrix.m[i % 4][i / 4]);
        Self::new(
            unsafe { occt_bridge_transform(self.raw(), data.as_ptr()) },
            "transform",
        )
    }
}

impl Drop for ShapeId {
    fn drop(&mut self) {
        // All handles returned by the ABI refer to independent registry entries.
        unsafe { occt_bridge_free(self.raw()) };
    }
}

#[derive(Debug, Clone)]
struct NativeComponent {
    shape: Arc<ShapeId>,
    matrix: Mat4,
    prototype: usize,
    id: String,
    name: String,
}

impl NativeComponent {
    fn placed_shape(&self) -> Result<ShapeId, KernelError> {
        self.shape.transformed(self.matrix)
    }

    fn metadata(&self, metrics: &OcctBridgeMetrics) -> ComponentMeta {
        let m = self.matrix.m;
        let pitch = (-m[2][0]).clamp(-1.0, 1.0).asin();
        let (roll, yaw) = if pitch.cos().abs() > 1e-10 {
            (m[2][1].atan2(m[2][2]), m[1][0].atan2(m[0][0]))
        } else {
            (0.0, (-m[0][1]).atan2(m[1][1]))
        };
        ComponentMeta {
            component_id: self.id.clone(),
            part_name: self.name.clone(),
            origin_mm: self.matrix.transform_point([0.0; 3]),
            rotation_deg: [roll.to_degrees(), pitch.to_degrees(), yaw.to_degrees()],
            volume_mm3: metrics.volume_mm3,
            bbox_mm: BBox3 {
                min: metrics.bbox.min,
                max: metrics.bbox.max,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct OcctShapeGuard {
    id: Arc<ShapeId>,
    metrics: OcctBridgeMetrics,
    options: OcctImportOptions,
    components: Vec<NativeComponent>,
}

impl OcctShapeGuard {
    pub(crate) fn raw_handle(&self) -> u64 {
        self.id.raw()
    }

    pub(crate) fn is_open_shell(&self) -> bool {
        self.metrics.watertight != 1
            && (self.metrics.face_count > 0
                || self.metrics.edge_count > 0
                || !self.components.is_empty())
    }

    pub(crate) fn report(&self, shape: &SolidShape) -> CheckReport {
        let m = &self.metrics;
        CheckReport {
            watertight: m.watertight == 1,
            volume_mm3: m.volume_mm3,
            surface_area_mm2: shape.faces.iter().map(|f| f.area_mm2).sum(),
            bbox_mm: BBox3 {
                min: m.bbox.min,
                max: m.bbox.max,
            },
            face_count: m.face_count as usize,
            edge_count: m.edge_count as usize,
            // The ABI reports topology counts, not individual open edges.
            open_edge_count: 0,
            issues: if self.is_open_shell() {
                vec![KernelError::OpenShell.to_string()]
            } else if m.face_count == 0 {
                vec!["solid has zero B-Rep faces".into()]
            } else {
                vec![]
            },
        }
    }

    /// Returns the volume centroid of a closed native solid in millimeters.
    pub fn volume_centroid_mm(&self) -> Result<[f64; 3], KernelError> {
        let mut centroid = [0.0; 3];
        status(
            unsafe { occt_bridge_volume_centroid(self.id.raw(), centroid.as_mut_ptr()) },
            "measure volume centroid",
        )?;
        Ok(centroid)
    }

    pub(crate) fn export_step(&self, name: &str) -> Result<String, KernelOperationError> {
        let dir = PrivateDir::new()?;
        let path = dir.0.join("export.step");
        let path_c = path_string(&path)?;
        let name_c = CString::new(name).map_err(|_| failure("STEP product name contains NUL"))?;
        ffi_status(
            unsafe { occt_bridge_export_step_named(self.id.raw(), path_c.as_ptr(), name_c.as_ptr()) },
            "export STEP",
        )?;
        fs::read_to_string(path)
            .map(|text| compact_step_entities(&text))
            .map_err(io_failure)
            .map_err(Into::into)
    }
}

/// Write `#N = ENTITY(` instance headers as `#N=ENTITY(`. Part 21 ignores this
/// whitespace; only line-leading instance names are touched, never strings.
fn compact_step_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let digits = line
            .strip_prefix('#')
            .map(|rest| rest.bytes().take_while(u8::is_ascii_digit).count())
            .unwrap_or(0);
        match line.get(1 + digits..).and_then(|rest| rest.strip_prefix(" = ")) {
            Some(body) if digits > 0 => {
                out.push_str(&line[..1 + digits]);
                out.push('=');
                out.push_str(body);
            }
            _ => out.push_str(line),
        }
    }
    out
}

fn failure(op: &str) -> KernelError {
    KernelError::OperationFailed(format!("OCCT: {op}"))
}

/// Call only on the same thread and before the next bridge operation.
pub(crate) fn take_last_error() -> Option<OcctNativeError> {
    let mut raw = std::mem::MaybeUninit::<OcctBridgeLastError>::uninit();
    if unsafe { occt_bridge_take_last_error(raw.as_mut_ptr()) } != 1 {
        return None;
    }
    // The bridge initializes the record and NUL-terminates both arrays on success.
    let raw = unsafe { raw.assume_init() };
    let err = OcctNativeError {
        operation: text(&raw.operation),
        status_code: raw.status_code,
        message: text(&raw.message),
    };
    crate::record_native_error(&err);
    Some(err)
}

fn ffi_failure(op: &str) -> KernelOperationError {
    match take_last_error() {
        Some(error) => error.into(),
        None => failure(op).into(),
    }
}

fn ffi_status(status: i32, op: &str) -> Result<(), KernelOperationError> {
    if status == OCCT_OK {
        Ok(())
    } else {
        Err(match take_last_error() {
            Some(error) => error.into(),
            None => failure(&format!("{op} failed ({status})")).into(),
        })
    }
}

fn status(status: i32, op: &str) -> Result<(), KernelError> {
    ffi_status(status, op).map_err(Into::into)
}

fn io_failure(err: io::Error) -> KernelError {
    failure(&format!("temporary file I/O failed: {err}"))
}

fn path_string(path: &Path) -> Result<CString, KernelError> {
    path.to_str()
        .and_then(|s| CString::new(s).ok())
        .ok_or_else(|| failure("temporary path cannot be encoded as UTF-8"))
}

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> Result<Self, KernelError> {
        // Create the directory atomically: never accept a pre-existing path or link.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let seed = RandomState::new();
        let temp = std::env::temp_dir();
        for _ in 0..32 {
            let nonce = seed.hash_one(NEXT.fetch_add(1, Ordering::Relaxed));
            let path = temp.join(format!("cad-occt-{:016x}", nonce));
            match create_private_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(io_failure(err)),
            }
        }
        Err(failure("could not create a private temporary directory"))
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt, ptr};

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: *const u16,
            revision: u32,
            out: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateDirectoryW(path: *const u16, attributes: *const SecurityAttributes) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    // Protected DACL: grant only Owner Rights and LocalSystem, inherited by
    // files in the directory. Do not rely on permissions inherited from TEMP.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
        .encode_utf16()
        .chain([0])
        .collect();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut descriptor = ptr::null_mut();
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        let attributes = SecurityAttributes {
            length: size_of::<SecurityAttributes>() as u32,
            descriptor,
            inherit_handle: 0,
        };
        let success = CreateDirectoryW(wide.as_ptr(), &attributes) != 0;
        let result = if success {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
        LocalFree(descriptor);
        result
    }
}

#[cfg(not(any(unix, windows)))]
fn create_private_dir(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private temporary directories unavailable",
    ))
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn text<const N: usize>(value: &[c_char; N]) -> String {
    let bytes: Vec<u8> = value
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn is_rigid(matrix: Mat4) -> bool {
    let m = matrix.m;
    m.iter().flatten().all(|val| val.is_finite())
        && m[3] == [0.0, 0.0, 0.0, 1.0]
        && (0..3).all(|a| {
            (0..3).all(|b| {
                let dot: f64 = (0..3).map(|i| m[i][a] * m[i][b]).sum();
                (dot - f64::from(a == b)).abs() <= 1e-9
            })
        })
}

fn component_descriptors(root: &ShapeId) -> Result<Vec<NativeComponent>, KernelError> {
    let count = unsafe { occt_bridge_get_components(root.raw(), std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(ffi_failure("component count query failed").into());
    }
    if count == 0 {
        return Ok(vec![]);
    }
    let zero = OcctComponentDescriptor {
        component_id: [0; 128],
        part_name: [0; 128],
        transform_col_major: [0.0; 16],
        shape_handle: 0,
    };
    let mut buf = buffer(count as usize, zero)?;
    let mut prototypes = buffer(count as usize, 0)?;
    if count > 1
        && unsafe { occt_bridge_get_component_prototypes(root.raw(), prototypes.as_mut_ptr(), count) }
            != count
    {
        // This query is only for an optimization; fall back to distinct prototypes.
        for (index, prototype) in prototypes.iter_mut().enumerate() {
            *prototype = index as i32;
        }
    }
    let got = unsafe { occt_bridge_get_components(root.raw(), buf.as_mut_ptr(), count) };
    // Take ownership of every returned handle before interpreting any descriptors.
    let mut items = Vec::new();
    let mut missing = false;
    for (index, item) in buf.into_iter().enumerate() {
        let Some(shape) = NonZeroU64::new(item.shape_handle).map(ShapeId) else {
            missing = true;
            continue;
        };
        items.push(NativeComponent {
            shape: Arc::new(shape),
            matrix: Mat4 {
                m: std::array::from_fn(|i| {
                    std::array::from_fn(|j| item.transform_col_major[4 * j + i])
                }),
            },
            prototype: usize::try_from(prototypes[index])
                .ok()
                .filter(|&first| first <= index && prototypes[first] == first as i32)
                .unwrap_or(index),
            id: text(&item.component_id),
            name: text(&item.part_name),
        });
    }
    if got != count || missing {
        return Err(failure("component extraction failed"));
    }
    Ok(items)
}

pub(crate) fn import_bytes(
    bytes: &[u8],
    format: OcctImportFormat,
    options: &OcctImportOptions,
    component: Option<&str>,
    allow_open_shell: bool,
    span: Span,
) -> Result<(SolidShape, OcctImportMetadata), KernelError> {
    verify_abi_handshake()?;
    if bytes.is_empty() {
        return Err(failure("empty import"));
    }
    if ![
        options.sewing_tol_mm,
        options.linear_deflection_mm,
        options.angular_deflection_rad,
    ]
    .into_iter()
    .all(|val| val.is_finite() && val > 0.0)
    {
        return Err(failure("import tolerances must be positive and finite"));
    }
    let mut raw = OcctImportMetadataRaw {
        unit_detected: [0; 16],
        is_watertight_solid: 0,
        open_shell_count: 0,
        component_count: 0,
    };
    let mut error = [0; 1024];
    let dir = PrivateDir::new()?;
    let name = match format {
        OcctImportFormat::Step => "import.step",
        OcctImportFormat::Brep => "import.brep",
        OcctImportFormat::Iges => "import.iges",
    };
    let path = dir.0.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(io_failure)?;
    file.write_all(bytes).map_err(io_failure)?;
    drop(file);
    let path = path_string(&path)?;
    let root_raw = unsafe {
        occt_bridge_import_file(
            path.as_ptr(),
            format as i32,
            options,
            &mut raw,
            error.as_mut_ptr(),
            error.len() as i32,
        )
    };
    let root = ShapeId::new(root_raw, &format!("import failed: {}", text(&error)))?;
    drop(dir);
    if component.is_none() && !allow_open_shell && raw.is_watertight_solid != 1 {
        return Err(KernelError::OpenShell);
    }
    let mut parts = component_descriptors(&root)?;
    let unit_detected = text(&raw.unit_detected);
    let count = parts.len();
    if usize::try_from(raw.component_count).ok() != Some(count) {
        return Err(failure("inconsistent import component count"));
    }
    let (id, name, metadata) = if let Some(query) = component {
        let index = if let Some(index) = parts.iter().position(|part| part.id == query) {
            index
        } else {
            let mut matches = parts
                .iter()
                .enumerate()
                .filter(|(_, part)| part.name == query);
            let index = matches
                .next()
                .map(|(i, _)| i)
                .ok_or_else(|| failure(&format!("component {query:?} was not found")))?;
            if matches.next().is_some() {
                return Err(failure(&format!(
                    "component name {query:?} is ambiguous; select by ID"
                )));
            }
            index
        };
        let part = parts.swap_remove(index);
        let id = part.placed_shape()?;
        let metrics = id.metrics()?;
        if !allow_open_shell && metrics.watertight != 1 {
            return Err(KernelError::OpenShell);
        }
        let name = part.name.clone();
        parts = vec![part];
        let metadata = OcctImportMetadata {
            unit_detected,
            is_watertight_solid: metrics.watertight == 1,
            // `check` does not expose the number of open shells for a leaf.
            open_shell_count: usize::from(metrics.watertight != 1),
            component_count: 1,
        };
        (id, name, metadata)
    } else {
        let name = if let [part] = parts.as_slice() {
            part.name.clone()
        } else {
            "imported_assembly".into()
        };
        let metadata = OcctImportMetadata {
            unit_detected,
            is_watertight_solid: raw.is_watertight_solid == 1,
            open_shell_count: raw.open_shell_count.max(0) as usize,
            component_count: count,
        };
        (root, name, metadata)
    };
    let solid = build_shape(Arc::new(id), &name, span, *options, parts)?;
    if !allow_open_shell
        && let KernelPayload::Occt(handle) = &solid.payload
        && handle.is_open_shell()
    {
        return Err(KernelError::OpenShell);
    }
    Ok((solid, metadata))
}

fn buffer<T: Clone>(len: usize, value: T) -> Result<Vec<T>, KernelError> {
    let mut out = Vec::new();
    out.try_reserve_exact(len)
        .map_err(|_| failure("tessellation allocation failed"))?;
    out.resize(len, value);
    Ok(out)
}

fn slice_range(
    offset: u32,
    count: u32,
    stride: usize,
) -> Result<std::ops::Range<usize>, KernelError> {
    let start = (offset as usize).checked_mul(stride);
    let end = offset
        .checked_add(count)
        .and_then(|end| (end as usize).checked_mul(stride));
    start
        .zip(end)
        .map(|(start, end)| start..end)
        .ok_or_else(|| failure("invalid tessellation range"))
}

fn vertices_and_faces(
    id: &ShapeId,
    options: &OcctImportOptions,
    span: Span,
) -> Result<(Vec<BRepFace>, Vec<BRepEdge>), KernelError> {
    let mut counts = OcctTessellationCounts::default();
    status(
        unsafe { occt_bridge_mesh_and_query_counts(id.raw(), options, &mut counts) },
        "mesh counts",
    )?;
    let n_faces = usize::try_from(counts.face_count).map_err(|_| failure("invalid face count"))?;
    let n_coords = (counts.total_vertices as usize)
        .checked_mul(3)
        .ok_or_else(|| failure("invalid vertex count"))?;
    let mut descriptors = buffer(n_faces, OcctFaceDescriptor::default())?;
    let mut positions = buffer(n_coords, 0.0)?;
    let mut normals = buffer(n_coords, 0.0)?;
    let mut indices = buffer(counts.total_indices as usize, 0u32)?;
    status(
        unsafe {
            occt_bridge_extract_tessellation(
                id.raw(),
                counts.face_count,
                counts.total_vertices,
                counts.total_indices,
                descriptors.as_mut_ptr(),
                positions.as_mut_ptr(),
                normals.as_mut_ptr(),
                indices.as_mut_ptr(),
            )
        },
        "mesh extraction",
    )?;
    if positions.iter().chain(&normals).any(|val| !val.is_finite()) {
        return Err(failure("non-finite tessellation coordinate"));
    }
    let mut faces = Vec::new();
    let mut mesh_segments = Vec::new();
    let mut next_vert = 0;
    let mut next_index = 0;
    let mut face_indexes = BTreeSet::new();
    for desc in descriptors {
        let vr = slice_range(desc.tri_vertex_offset, desc.tri_vertex_count, 3)?;
        let ir = slice_range(desc.tri_index_offset, desc.tri_index_count, 1)?;
        if vr.start != next_vert
            || ir.start != next_index
            || ir.len() % 3 != 0
            || !face_indexes.insert(desc.face_index)
            || !desc.area_mm2.is_finite()
            || desc.area_mm2 < 0.0
            || !desc
                .origin_or_center
                .iter()
                .chain(&desc.normal_or_axis)
                .all(|x| x.is_finite())
        {
            return Err(failure("invalid face descriptor"));
        }
        next_vert = vr.end;
        next_index = ir.end;
        let vertices = positions
            .get(vr.clone())
            .ok_or_else(|| failure("face vertices out of range"))?;
        let face_normals = normals
            .get(vr)
            .ok_or_else(|| failure("face normals out of range"))?;
        let face_indices = indices
            .get(ir)
            .ok_or_else(|| failure("face indices out of range"))?;
        if face_indices.iter().any(|&idx| idx >= desc.tri_vertex_count) {
            return Err(failure("face-local triangle index out of range"));
        }
        let (outer_loop, inner_loops) = face_boundaries(vertices, face_indices, |p, q| {
            // Collect unique mesh-boundary segments; they are later grouped onto
            // exact native B-Rep edges so a hole rim stays one Circle, not dozens
            // of Other polylines (TES-1-C3).
            if p != q {
                mesh_segments.push((p, q));
            }
        });
        let surface_type = match desc.surface_kind {
            0 => SurfaceType::Plane,
            1 => SurfaceType::Cylinder,
            2 => SurfaceType::Cone,
            3 => SurfaceType::Sphere,
            4 => SurfaceType::Torus,
            5 => SurfaceType::Bspline,
            _ => SurfaceType::Mesh,
        };
        faces.push(BRepFace {
            face_id: format!("occt_face_{}", desc.face_index),
            surface_type,
            normal_hint: desc.normal_or_axis,
            center_hint: desc.origin_or_center,
            radius_hint: (desc.radius_mm.is_finite() && desc.radius_mm > 0.0)
                .then_some(desc.radius_mm),
            area_mm2: desc.area_mm2,
            vertices: vertices.to_vec(),
            normals: face_normals.to_vec(),
            indices: face_indices.to_vec(),
            outer_loop,
            top_loop: None,
            inner_loops,
            provenance_tags: Vec::new(),
            exact_interface: matches!(surface_type, SurfaceType::Plane | SurfaceType::Cylinder),
            component_id: None,
            ast_span: span,
        });
    }
    if next_vert != positions.len() || next_index != indices.len() {
        return Err(failure("incomplete tessellation ranges"));
    }
    let edges = preview_edges_from_native_topology(id, &faces, &mesh_segments, span)?;
    Ok((faces, edges))
}

fn curve_type_from_native(kind: crate::topology::CurveKind) -> CurveType {
    match kind {
        crate::topology::CurveKind::Line => CurveType::Line,
        crate::topology::CurveKind::Circle => CurveType::Circle,
        crate::topology::CurveKind::Ellipse => CurveType::Ellipse,
        crate::topology::CurveKind::BSpline => CurveType::Bspline,
        crate::topology::CurveKind::Other => CurveType::Polyline,
    }
}

fn quantize_point(p: [f64; 3]) -> [i64; 3] {
    p.map(|x| (x * 1e7).round() as i64)
}

fn point_on_face(face: &BRepFace, p: [f64; 3]) -> bool {
    const TOL_SQ: f64 = 1e-8;
    face.vertices.chunks_exact(3).any(|chunk| {
        let d0 = chunk[0] - p[0];
        let d1 = chunk[1] - p[1];
        let d2 = chunk[2] - p[2];
        d0 * d0 + d1 * d1 + d2 * d2 <= TOL_SQ
    })
}

fn face_index_of(face: &BRepFace) -> Option<u32> {
    face.face_id
        .strip_prefix("occt_face_")
        .and_then(|s| s.parse().ok())
}

fn face_bbox_expanded(face: &BRepFace, tol: f64) -> Option<BBox3> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for chunk in face.vertices.chunks_exact(3) {
        any = true;
        for axis in 0..3 {
            min[axis] = min[axis].min(chunk[axis]);
            max[axis] = max[axis].max(chunk[axis]);
        }
    }
    any.then(|| BBox3 {
        min: [min[0] - tol, min[1] - tol, min[2] - tol],
        max: [max[0] + tol, max[1] + tol, max[2] + tol],
    })
}

fn bbox_contains_point(bbox: &BBox3, p: [f64; 3]) -> bool {
    (0..3).all(|axis| p[axis] >= bbox.min[axis] && p[axis] <= bbox.max[axis])
}

fn incident_face_indices(
    face_entries: &[(u32, BBox3, &BRepFace)],
    p: [f64; 3],
    q: [f64; 3],
) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for &(idx, bbox, face) in face_entries {
        if bbox_contains_point(&bbox, p)
            && bbox_contains_point(&bbox, q)
            && point_on_face(face, p)
            && point_on_face(face, q)
        {
            out.insert(idx);
        }
    }
    out
}

fn chain_segment_polylines(segments: &[([f64; 3], [f64; 3])]) -> Vec<Vec<f64>> {
    if segments.is_empty() {
        return Vec::new();
    }
    let mut adjacency: BTreeMap<[i64; 3], Vec<(usize, bool)>> = BTreeMap::new();
    for (i, (a, b)) in segments.iter().enumerate() {
        adjacency.entry(quantize_point(*a)).or_default().push((i, false));
        adjacency.entry(quantize_point(*b)).or_default().push((i, true));
    }
    let mut used = vec![false; segments.len()];
    let mut chains = Vec::new();

    loop {
        // Prefer open endpoints (degree == 1 among unused segments) so open edges
        // trace end-to-end rather than splitting in the middle; otherwise pick
        // the first remaining vertex to trace a closed loop.
        let mut open_start = None;
        let mut fallback_start = None;
        for (key, entries) in &adjacency {
            let remaining = entries.iter().filter(|(idx, _)| !used[*idx]).count();
            if remaining == 1 {
                open_start = Some(*key);
                break;
            }
            if remaining > 0 && fallback_start.is_none() {
                fallback_start = Some(*key);
            }
        }
        let Some(start_key) = open_start.or(fallback_start) else {
            break;
        };

        let mut points = Vec::new();
        let mut key = start_key;
        let Some((first, reversed)) = adjacency
            .get(&key)
            .and_then(|v| v.iter().copied().find(|(i, _)| !used[*i]))
        else {
            break;
        };
        used[first] = true;
        let (a, b) = segments[first];
        let (start, end) = if reversed { (b, a) } else { (a, b) };
        points.extend(start);
        points.extend(end);
        key = quantize_point(end);

        if key != start_key {
            loop {
                let Some(candidates) = adjacency.get(&key) else {
                    break;
                };
                let Some((idx, reversed)) =
                    candidates.iter().copied().find(|(i, _)| !used[*i])
                else {
                    break;
                };
                used[idx] = true;
                let (a, b) = segments[idx];
                let end = if reversed { a } else { b };
                points.extend(end);
                key = quantize_point(end);
                if points.len() >= 6 && key == start_key {
                    break;
                }
            }
        }
        if points.len() >= 6 {
            chains.push(points);
        }
    }
    chains
}

fn preview_edges_from_native_topology(
    id: &ShapeId,
    faces: &[BRepFace],
    mesh_segments: &[([f64; 3], [f64; 3])],
    span: Span,
) -> Result<Vec<BRepEdge>, KernelError> {
    const FACE_TOL: f64 = 1e-4;
    let topo = crate::topology::extract_from_handle(id.raw())?;
    let face_entries: Vec<(u32, BBox3, &BRepFace)> = faces
        .iter()
        .filter_map(|face| {
            let idx = face_index_of(face)?;
            let bbox = face_bbox_expanded(face, FACE_TOL)?;
            Some((idx, bbox, face))
        })
        .collect();

    let native_meta: Vec<(usize, crate::topology::CurveKind, BTreeSet<u32>, BTreeSet<u32>)> = topo
        .edges
        .iter()
        .enumerate()
        .filter(|(_, edge)| !edge.is_degenerate)
        .map(|(native_idx, edge)| {
            let face_set: BTreeSet<u32> = edge
                .face_uses
                .iter()
                .filter(|u| !u.is_seam)
                .map(|u| u.face_index)
                .collect();
            let all_faces: BTreeSet<u32> = edge.face_uses.iter().map(|u| u.face_index).collect();
            (native_idx, edge.curve_kind, face_set, all_faces)
        })
        .collect();

    let mut unique = BTreeSet::new();
    let mut buckets: BTreeMap<Vec<usize>, Vec<([f64; 3], [f64; 3])>> = BTreeMap::new();
    for &(p, q) in mesh_segments {
        let mut key = [quantize_point(p), quantize_point(q)];
        key.sort_unstable();
        if key[0] == key[1] || !unique.insert(key) {
            continue;
        }
        let incident = incident_face_indices(&face_entries, p, q);
        if incident.is_empty() {
            continue;
        }
        let matching: Vec<usize> = native_meta
            .iter()
            .filter_map(|(native_idx, _, face_set, all_faces)| {
                let hits = if !face_set.is_empty()
                    && incident.is_subset(face_set)
                    && incident.len() >= face_set.len().min(2)
                {
                    true
                } else {
                    !all_faces.is_empty() && incident == *all_faces
                };
                hits.then_some(*native_idx)
            })
            .collect();
        if !matching.is_empty() {
            buckets.entry(matching).or_default().push((p, q));
        }
    }

    let mut keyed_edges: Vec<((usize, usize), BRepEdge)> = Vec::new();
    for (matching, segs) in buckets {
        let polylines = chain_segment_polylines(&segs);
        for (chain_idx, polyline) in polylines.into_iter().enumerate() {
            let native_idx = matching
                .get(chain_idx)
                .copied()
                .unwrap_or(matching[0]);
            let curve_kind = topo.edges[native_idx].curve_kind;
            let edge_id = if chain_idx < matching.len() {
                format!("occt_edge_{native_idx}")
            } else {
                format!("occt_edge_{native_idx}_{chain_idx}")
            };
            keyed_edges.push((
                (native_idx, chain_idx),
                BRepEdge {
                    edge_id,
                    curve_type: curve_type_from_native(curve_kind),
                    polyline,
                    provenance_tags: Vec::new(),
                    component_id: None,
                    ast_span: span,
                },
            ));
        }
    }
    keyed_edges.sort_by_key(|(sort_key, _)| *sort_key);
    let edges: Vec<BRepEdge> = keyed_edges.into_iter().map(|(_, edge)| edge).collect();
    if edges.is_empty() {
        // Do not fail solid construction if grouping cannot publish edges.
        // The pre-fix preview (each mesh chord as an Other polyline) stays as
        // a last-resort fallback so open/evaluate of valid solids still works.
        return Ok(preview_edges_from_mesh_chords(mesh_segments, span));
    }
    Ok(edges)
}

fn preview_edges_from_mesh_chords(
    mesh_segments: &[([f64; 3], [f64; 3])],
    span: Span,
) -> Vec<BRepEdge> {
    let mut unique = BTreeSet::new();
    let mut edges = Vec::new();
    for &(p, q) in mesh_segments {
        let mut key = [quantize_point(p), quantize_point(q)];
        key.sort_unstable();
        if key[0] == key[1] || !unique.insert(key) {
            continue;
        }
        let mut polyline = Vec::with_capacity(6);
        polyline.extend(p);
        polyline.extend(q);
        let idx = edges.len();
        edges.push(BRepEdge {
            edge_id: format!("occt_edge_{idx}"),
            curve_type: CurveType::Polyline,
            polyline,
            provenance_tags: Vec::new(),
            component_id: None,
            ast_span: span,
        });
    }
    edges
}

fn vertex(vertices: &[f64], index: u32) -> [f64; 3] {
    let i = 3 * index as usize;
    [vertices[i], vertices[i + 1], vertices[i + 2]]
}

// Use the triangle topology, not a global spatial weld: coincident vertices on
// opposite sides of a periodic face seam must stay distinct while tracing loops.
fn face_boundaries(
    vertices: &[f64],
    indices: &[u32],
    mut edge: impl FnMut([f64; 3], [f64; 3]),
) -> (Vec<[f64; 3]>, Vec<Vec<[f64; 3]>>) {
    let mut counts = BTreeMap::new();
    for tri in indices.chunks_exact(3) {
        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            if a != b {
                let (count, _) = counts.entry((a.min(b), a.max(b))).or_insert((0u32, (a, b)));
                *count += 1;
            }
        }
    }
    let mut boundary = BTreeSet::new();
    for (count, (a, b)) in counts.into_values() {
        if count == 1 {
            edge(vertex(vertices, a), vertex(vertices, b));
            boundary.insert((a, b));
        }
    }
    let mut loops = Vec::new();
    while let Some((first, mut next)) = boundary.pop_first() {
        let mut points = vec![vertex(vertices, first)];
        while next != first {
            points.push(vertex(vertices, next));
            let Some(&(a, b)) = boundary.range((next, 0)..=(next, u32::MAX)).next() else {
                break;
            };
            boundary.remove(&(a, b));
            next = b;
        }
        if next == first && points.len() >= 3 {
            loops.push(points);
        }
    }
    // The largest projected boundary area is the planar outer rim (a small
    // perforation can have more points than its rectangular outer rim).
    let area = |points: &Vec<[f64; 3]>| {
        let mut n = [0.0; 3];
        let origin = points[0];
        for pair in points[1..].windows(2) {
            let a = std::array::from_fn(|i| pair[0][i] - origin[i]);
            let b = std::array::from_fn(|i| pair[1][i] - origin[i]);
            let cross = cross(a, b);
            for i in 0..3 {
                n[i] += cross[i];
            }
        }
        n.into_iter().map(|x| x * x).sum::<f64>()
    };
    if let Some((i, _)) = loops
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| area(a).total_cmp(&area(b)))
    {
        (loops.remove(i), loops)
    } else {
        (Vec::new(), loops)
    }
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn native_csg(faces: &[BRepFace], guard: &Arc<OcctShapeGuard>) -> CsgNode {
    let metrics = &guard.metrics;
    let triangles: Arc<[_]> = faces
        .iter()
        .flat_map(|face| {
            face.indices
                .chunks_exact(3)
                .map(|tri| std::array::from_fn(|i| vertex(&face.vertices, tri[i])))
        })
        .collect();
    CsgNode::Occt {
        bbox: BBox3 {
            min: metrics.bbox.min,
            max: metrics.bbox.max,
        },
        volume_mm3: metrics.volume_mm3,
        triangles,
        native: Some(guard.clone()),
    }
}

struct ComponentGeometry {
    metrics: OcctBridgeMetrics,
    faces: Vec<BRepFace>,
    edges: Vec<BRepEdge>,
}

impl ComponentGeometry {
    fn read(id: &ShapeId, options: &OcctImportOptions, span: Span) -> Result<Self, KernelError> {
        let metrics = id.metrics()?;
        let (faces, edges) = if metrics.face_count == 0 {
            (Vec::new(), Vec::new())
        } else {
            vertices_and_faces(id, options, span)?
        };
        Ok(Self {
            metrics,
            faces,
            edges,
        })
    }

    fn translated(&self, pose: Mat4) -> Option<Self> {
        let min = pose.transform_point(self.metrics.bbox.min);
        let max = pose.transform_point(self.metrics.bbox.max);
        if !min.into_iter().chain(max).all(f64::is_finite) {
            return None;
        }
        // A translation preserves the exact B-Rep AABB extents, volume and
        // orientation. This AABB comes from the native B-Rep, not its mesh.
        let mut metrics = self.metrics.clone();
        metrics.bbox = OcctBridgeBBox { min, max };
        let (mut faces, mut edges) = (self.faces.clone(), self.edges.clone());
        if pose != Mat4::identity() {
            transform_preview_geometry(&mut faces, &mut edges, pose);
        }
        Some(Self {
            metrics,
            faces,
            edges,
        })
    }
}

fn is_pure_translation(matrix: Mat4) -> bool {
    let mut rotation = matrix;
    for i in 0..3 {
        if !rotation.m[i][3].is_finite() {
            return false;
        }
        rotation.m[i][3] = 0.0;
    }
    rotation == Mat4::identity()
}

fn build_shape(
    id: Arc<ShapeId>,
    name: &str,
    span: Span,
    options: OcctImportOptions,
    parts: Vec<NativeComponent>,
) -> Result<SolidShape, KernelError> {
    let metrics = id.metrics()?;
    let mut components = Vec::with_capacity(parts.len());
    let (faces, edges) = if !parts.is_empty() {
        // The root is still authoritative for exact shape metrics and
        // watertightness. Reuse local tessellation only for repeated, pure
        // translations of one exact native prototype, not equal-looking parts.
        let mut translation_counts = BTreeMap::new();
        for part in &parts {
            if is_pure_translation(part.matrix) {
                *translation_counts.entry(part.prototype).or_insert(0usize) += 1;
            }
        }
        let mut prototypes = BTreeMap::new();
        let mut faces = Vec::new();
        let mut edges = Vec::new();
        for part in &parts {
            let reused = if is_pure_translation(part.matrix)
                && translation_counts[&part.prototype] > 1
            {
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    prototypes.entry(part.prototype)
                {
                    entry.insert(ComponentGeometry::read(&part.shape, &options, span)?);
                }
                prototypes[&part.prototype].translated(part.matrix)
            } else {
                None
            };
            let ComponentGeometry {
                metrics,
                faces: part_faces,
                edges: part_edges,
            } = match reused {
                Some(geometry) => geometry,
                None if part.matrix == Mat4::identity() => {
                    ComponentGeometry::read(&part.shape, &options, span)?
                }
                None => ComponentGeometry::read(&part.placed_shape()?, &options, span)?,
            };
            components.push(part.metadata(&metrics));
            faces.extend(part_faces.into_iter().map(|mut face| {
                face.face_id = format!("{}_{}", part.id, face.face_id);
                face.component_id = Some(part.id.clone());
                face
            }));
            edges.extend(part_edges.into_iter().map(|mut edge| {
                edge.edge_id = format!("{}_{}", part.id, edge.edge_id);
                edge.component_id = Some(part.id.clone());
                edge
            }));
        }
        (faces, edges)
    } else if metrics.face_count == 0 {
        (vec![], vec![])
    } else {
        vertices_and_faces(&id, &options, span)?
    };
    let guard = Arc::new(OcctShapeGuard {
        id,
        metrics,
        options,
        components: parts,
    });
    Ok(SolidShape {
        csg: native_csg(&faces, &guard),
        faces,
        edges,
        components,
        payload: KernelPayload::Occt(guard),
        ..SolidShape::empty(name)
    })
}

fn component_metadata(parts: &[NativeComponent]) -> Result<Vec<ComponentMeta>, KernelError> {
    parts
        .iter()
        .map(|part| Ok(part.metadata(&part.placed_shape()?.metrics()?)))
        .collect()
}

pub(crate) fn transform(
    shape: &SolidShape,
    handle: &OcctShapeGuard,
    matrix: Mat4,
) -> Result<SolidShape, KernelError> {
    let id = handle.id.transformed(matrix)?;
    let mut out = transform_preview(shape, matrix);
    let metrics = id.metrics()?;
    let mut parts = handle.components.clone();
    for part in &mut parts {
        part.matrix = matrix.mul_mat(part.matrix);
    }
    out.components = component_metadata(&parts)?;
    let guard = Arc::new(OcctShapeGuard {
        id: Arc::new(id),
        metrics,
        components: parts,
        options: handle.options,
    });
    out.csg = native_csg(&out.faces, &guard);
    out.payload = KernelPayload::Occt(guard);
    Ok(out)
}

pub(crate) fn through_hole_radii(
    handle: &OcctShapeGuard,
    face_indices: &[usize],
) -> Option<Vec<f64>> {
    let indices = face_indices
        .iter()
        .map(|&index| i32::try_from(index))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let count = i32::try_from(indices.len()).ok()?;
    let mut radii = vec![0.0; indices.len()];
    let status = handle.with_single_operand(|id| {
        Ok(unsafe {
            occt_bridge_read_through_holes(id.raw(), count, indices.as_ptr(), radii.as_mut_ptr())
        })
    }).ok()?;
    (status == OCCT_OK).then_some(radii)
}

impl OcctShapeGuard {
    fn with_single_operand<T>(
        &self,
        operation: impl FnOnce(&ShapeId) -> Result<T, KernelError>,
    ) -> Result<T, KernelError> {
        // Use the same placed component whose triangulation defines the face indices.
        match self.components.as_slice() {
            [] => operation(&self.id),
            [part] if part.matrix == Mat4::identity() => operation(&part.shape),
            [part] => operation(&part.placed_shape()?),
            _ => Err(KernelError::OperationFailed("requires a single native solid".into())),
        }
    }
}

pub(super) fn edit_through_hole(
    shape: &SolidShape,
    handle: &OcctShapeGuard,
    face_index: usize,
    edit: crate::ThroughHoleEdit,
    span: Span,
) -> Result<SolidShape, KernelError> {
    use crate::ThroughHoleEdit;

    let invalid = |message: &str| failure(&format!("{} {message}", edit.name()));
    let count = i32::try_from(shape.faces.len()).map_err(|_| invalid("has too many faces"))?;
    let index = i32::try_from(face_index)
        .ok()
        .filter(|&index| index < count)
        .ok_or_else(|| invalid("selection does not identify a face of this operand"))?;
    let capacity = count - i32::from(matches!(edit, ThroughHoleEdit::Delete));
    let mut sources = vec![-1; capacity as usize];
    let mut raw = 0;
    let status = handle.with_single_operand(|id| {
        let (handle, sources) = (id.raw(), sources.as_mut_ptr());
        Ok(unsafe {
            match edit {
                ThroughHoleEdit::Offset(delta) => {
                    occt_bridge_offset_through_hole(handle, index, delta, capacity, sources, &mut raw)
                }
                ThroughHoleEdit::Move(offset) => occt_bridge_move_through_hole(
                    handle,
                    index,
                    offset.as_ptr(),
                    capacity,
                    sources,
                    &mut raw,
                ),
                ThroughHoleEdit::Delete => {
                    occt_bridge_delete_through_hole(handle, index, capacity, sources, &mut raw)
                }
            }
        })
    })?;
    match status {
        OCCT_OK => {}
        -2 => {
            return Err(invalid(
                "supports only a single full straight cylindrical through-hole with perpendicular planar openings",
            ));
        }
        -3 if matches!(edit, ThroughHoleEdit::Offset(_)) => {
            return Err(invalid(
                "would collapse the hole, change other topology, or produce invalid geometry",
            ));
        }
        -3 => return Err(invalid("would change other topology or fail native geometry validation")),
        -4 => return Err(KernelError::BackendUnavailable),
        -5 => return Err(invalid("requires an offset perpendicular to the hole axis")),
        _ => return Err(invalid("could not resolve the selected native face")),
    }
    let id = Arc::new(ShapeId::new(raw, &format!("{} returned no solid", edit.name()))?);
    let mut result = build_shape(id, &shape.part_name, span, handle.options, vec![])?;
    if sources.len() != result.faces.len() {
        return Err(invalid("returned an invalid face mapping"));
    }
    for (new_face, old_index) in result.faces.iter_mut().zip(sources) {
        let old_face = usize::try_from(old_index)
            .ok()
            .and_then(|i| shape.faces.get(i))
            .ok_or_else(|| invalid("returned an invalid face mapping"))?;
        new_face.provenance_tags.clone_from(&old_face.provenance_tags);
    }
    result.pmi.clone_from(&shape.pmi);
    Ok(result)
}

pub(crate) fn make_bspline_loft(
    spec: &BsplineLoftSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    let count =
        i32::try_from(spec.sections.len()).map_err(|_| failure("too many loft sections"))?;
    let counts = spec
        .sections
        .iter()
        .map(|section| {
            i32::try_from(section.len()).map_err(|_| failure("loft section is too large"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let points: Vec<[f64; 3]> = spec.sections.iter().flatten().copied().collect();
    // The counts describe the consecutive triplets in points, and both buffers live through the call.
    let raw = unsafe {
        occt_bridge_make_bspline_loft(
            count,
            counts.as_ptr(),
            points.as_flattened().as_ptr(),
            i32::from(spec.make_solid),
            i32::from(spec.ruled),
        )
    };
    let id = Arc::new(ShapeId::new(raw, "B-spline loft failed")?);
    build_shape(
        id,
        "bspline loft",
        span,
        OcctImportOptions::default(),
        vec![],
    )
}

pub(crate) fn draft_solid_faces(
    shape: &SolidShape,
    spec: &DraftFacesSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    // Scale before OCCT normalizes the directions to avoid squaring very large or small components.
    fn scaled_direction(direction: [f64; 3]) -> [f64; 3] {
        let scale = direction.into_iter().map(f64::abs).fold(0.0, f64::max);
        direction.map(|component| component / scale)
    }

    let pull = scaled_direction(spec.pull_dir);
    let normal = scaled_direction(spec.neutral_normal);
    edit_freeform_shape(shape, span, "draft faces", |handle| {
        // All three pointers refer to live three-element arrays.
        unsafe {
            occt_bridge_draft_faces(
                handle,
                spec.draft_angle_deg,
                pull.as_ptr(),
                spec.neutral_origin_mm.as_ptr(),
                normal.as_ptr(),
            )
        }
    })
}

pub(crate) fn variable_fillet_solid(
    shape: &SolidShape,
    spec: &VariableFilletSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    edit_freeform_shape(shape, span, "variable fillet", |handle| unsafe {
        occt_bridge_variable_fillet(handle, spec.r_start_mm, spec.r_end_mm)
    })
}

pub(crate) fn fillet(
    shape: &SolidShape,
    radius: f64,
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    edit_native_shape(shape, span, "fillet", |handle| unsafe {
        occt_bridge_fillet_all(handle, radius)
    })
}

pub(crate) fn chamfer(
    shape: &SolidShape,
    distance: f64,
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    edit_native_shape(shape, span, "chamfer", |handle| unsafe {
        occt_bridge_chamfer_all(handle, distance)
    })
}

pub(crate) fn shell(
    shape: &SolidShape,
    thickness: f64,
    [x, y, z]: [f64; 3],
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    edit_native_shape(shape, span, "shell", |handle| unsafe {
        occt_bridge_shell(handle, thickness, x, y, z)
    })
}

fn edit_freeform_shape(
    shape: &SolidShape,
    span: Span,
    operation: &str,
    edit: impl FnOnce(u64) -> u64,
) -> Result<SolidShape, KernelError> {
    edit_native_shape(shape, span, operation, edit).map_err(Into::into)
}

fn edit_native_shape(
    shape: &SolidShape,
    span: Span,
    operation: &str,
    edit: impl FnOnce(u64) -> u64,
) -> Result<SolidShape, KernelOperationError> {
    let operand = native_operand(shape)?
        .ok_or_else(|| failure(&format!("{operation} requires a nonempty shape")))?;
    let id = Arc::new(ShapeId::from_ffi(
        edit(operand.raw()),
        &format!("{operation} failed"),
    )?);
    Ok(build_shape(
        id,
        &shape.part_name,
        span,
        native_options([shape]),
        vec![],
    )?)
}

pub(crate) fn quad_grid_to_bspline_solid(
    spec: &QuadGridSurfaceSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    let invalid = || failure("invalid B-spline grid dimensions");
    let nu = i32::try_from(spec.nu).map_err(|_| invalid())?;
    let nv = i32::try_from(spec.nv).map_err(|_| invalid())?;
    if nu.checked_mul(nv).map(|count| count as usize) != Some(spec.xyz_grid.len()) {
        return Err(invalid());
    }
    // The buffer contains exactly nu * nv triplets in U-major order.
    let raw = unsafe {
        occt_bridge_quad_grid_to_bspline_surface(
            nu,
            nv,
            spec.xyz_grid.as_flattened().as_ptr(),
            spec.thickness_mm,
        )
    };
    let id = Arc::new(ShapeId::new(raw, "quad-grid B-spline surface fit failed")?);
    build_shape(
        id,
        "quad grid surface",
        span,
        OcctImportOptions::default(),
        vec![],
    )
}

type NativeBoolean = unsafe extern "C" fn(u64, u64) -> u64;

fn apply(
    op: NativeBoolean,
    base: &ShapeId,
    tool: &ShapeId,
) -> Result<Arc<ShapeId>, KernelOperationError> {
    ShapeId::from_ffi(
        unsafe { op(base.raw(), tool.raw()) },
        "boolean operation failed",
    )
    .map(Arc::new)
}

fn native_operand(shape: &SolidShape) -> Result<Option<Arc<ShapeId>>, KernelOperationError> {
    match &shape.payload {
        KernelPayload::Analytic => lift_analytic_to_occt(&shape.csg),
        KernelPayload::Occt(handle) => {
            Ok((handle.metrics.face_count > 0).then(|| handle.id.clone()))
        }
        KernelPayload::TriMesh => Err(KernelError::UnsupportedMixedBoolean.into()),
    }
}

pub(crate) fn check_interference(
    a: &SolidShape,
    b: &SolidShape,
    min_clearance_mm: f64,
) -> Result<Option<crate::InterferenceViolation>, KernelError> {
    use crate::InterferenceViolation;

    if unsafe { occt_bridge_has_exact_interference() } != 1 {
        return Err(KernelError::BackendUnavailable);
    }
    let a = native_operand(a)?.ok_or(KernelError::OpenShell)?;
    let b = native_operand(b)?.ok_or(KernelError::OpenShell)?;
    let mut measurement = f64::NAN;
    let status = unsafe {
        occt_bridge_check_interference(a.raw(), b.raw(), min_clearance_mm, &mut measurement)
    };
    match status {
        OCCT_OK => Ok(None),
        1 if measurement.is_finite() && measurement > 0.0 => {
            Ok(Some(InterferenceViolation::Overlap {
                volume_mm3: measurement,
            }))
        }
        2 if measurement.is_finite() && measurement >= 0.0 => {
            Ok(Some(InterferenceViolation::Clearance {
                distance_mm: measurement,
            }))
        }
        -2 => Err(KernelError::OpenShell),
        -4 => Err(KernelError::BackendUnavailable),
        _ => Err(ffi_failure("exact interference check failed").into()),
    }
}

pub(crate) fn measure_symmetric_difference(
    candidate: &SolidShape,
    reference: &SolidShape,
) -> Result<crate::SymmetricDifference, KernelError> {
    let candidate = native_operand(candidate)?.ok_or(KernelError::OpenShell)?;
    let reference = native_operand(reference)?.ok_or(KernelError::OpenShell)?;
    let mut volumes = OcctBridgeSymmetricDifference::default();
    let status = unsafe {
        occt_bridge_measure_symmetric_difference(candidate.raw(), reference.raw(), &mut volumes)
    };
    match status {
        OCCT_OK => crate::SymmetricDifference::from_volumes(
            volumes.candidate_only_mm3,
            volumes.reference_only_mm3,
            volumes.reference_volume_mm3,
        ),
        -2 => Err(KernelError::OpenShell),
        -4 => Err(KernelError::BackendUnavailable),
        -5 => Err(failure("exact comparison requires one solid per operand")),
        _ => Err(ffi_failure("exact OCCT symmetric difference failed").into()),
    }
}

fn native_options<'a>(shapes: impl IntoIterator<Item = &'a SolidShape>) -> OcctImportOptions {
    shapes
        .into_iter()
        .find_map(|shape| match &shape.payload {
            KernelPayload::Occt(handle) => Some(handle.options),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn boolean_union(
    mut shapes: Vec<SolidShape>,
    name: &str,
) -> Result<SolidShape, KernelOperationError> {
    let options = native_options(&shapes);
    let mut operands = shapes.iter().enumerate().filter_map(|(i, shape)| {
        native_operand(shape)
            .transpose()
            .map(|id| id.map(|id| (i, id)))
    });
    let Some((i, mut id)) = operands.next().transpose()? else {
        return Ok(SolidShape::empty(name));
    };
    if let Some(other) = operands.next() {
        id = apply(occt_bridge_boolean_union, &id, &other?.1)?;
    } else {
        let mut solid = shapes.swap_remove(i);
        solid.part_name = name.into();
        return Ok(solid);
    }
    for other in operands {
        id = apply(occt_bridge_boolean_union, &id, &other?.1)?;
    }
    let span = first_span(&shapes[i]);
    let mut result = build_boolean(id, name, span, options)?;
    propagate_face_tags(&mut result, shapes.iter().flat_map(|shape| &shape.faces));
    result
        .pmi
        .extend(shapes.into_iter().flat_map(|shape| shape.pmi));
    Ok(result)
}

pub(crate) fn boolean_subtract(
    base: SolidShape,
    tools: Vec<SolidShape>,
) -> Result<SolidShape, KernelOperationError> {
    let options = native_options(std::iter::once(&base).chain(&tools));
    let Some(mut id) = native_operand(&base)? else {
        return Ok(base);
    };
    let mut changed = false;
    for tool in &tools {
        if let Some(tool) = native_operand(tool)? {
            id = apply(occt_bridge_boolean_cut, &id, &tool)?;
            changed = true;
        }
    }
    if !changed {
        return Ok(base);
    }
    let mut result = build_boolean(id, &base.part_name, first_span(&base), options)?;
    propagate_face_tags(
        &mut result,
        base.faces
            .iter()
            .chain(tools.iter().flat_map(|tool| &tool.faces)),
    );
    result.pmi = base.pmi;
    result
        .pmi
        .extend(tools.into_iter().flat_map(|tool| tool.pmi));
    Ok(result)
}

pub(crate) fn boolean_intersect(
    base: SolidShape,
    tool: SolidShape,
) -> Result<SolidShape, KernelOperationError> {
    let options = native_options([&base, &tool]);
    let (Some(a), Some(b)) = (native_operand(&base)?, native_operand(&tool)?) else {
        return Ok(SolidShape::empty(&base.part_name));
    };
    let id = apply(occt_bridge_boolean_intersect, &a, &b)?;
    let mut result = build_boolean(id, &base.part_name, first_span(&base), options)?;
    propagate_face_tags(&mut result, base.faces.iter().chain(&tool.faces));
    Ok(result)
}

fn build_boolean(
    id: Arc<ShapeId>,
    name: &str,
    span: Span,
    options: OcctImportOptions,
) -> Result<SolidShape, KernelError> {
    let solid = build_shape(id, name, span, options, vec![])?;
    if let KernelPayload::Occt(handle) = &solid.payload
        && handle.is_open_shell()
    {
        return Err(KernelError::OpenShell);
    }
    Ok(solid)
}

fn face_bbox(face: &BRepFace) -> BBox3 {
    let mut bbox = BBox3::empty();
    for p in face.vertices.chunks_exact(3) {
        bbox.expand_point([p[0], p[1], p[2]]);
    }
    bbox
}

fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}

fn axis_hint(face: &BRepFace) -> [f64; 3] {
    // Legacy analytic cylinders store a radial normal but expose the two rims.
    if face.surface_type == SurfaceType::Cylinder
        && let Some(a) = face.outer_loop.first()
        && let Some(b) = face.top_loop.as_ref().and_then(|points| points.first())
    {
        let v: [f64; 3] = std::array::from_fn(|i| b[i] - a[i]);
        let len = norm(v);
        if len > 1e-9 {
            return v.map(|x| x / len);
        }
    }
    face.normal_hint
}

// Phase A does not expose OCCT Boolean history. Preserve a tag only for a
// uniquely matched planar/cylindrical source face. Never use a face index from
// either operand as a result index (the boolean may renumber, split or fuse it).
fn propagate_face_tags<'a>(solid: &mut SolidShape, sources: impl Iterator<Item = &'a BRepFace>) {
    const TOL: f64 = 1e-6;
    let tagged: Vec<_> = sources
        .filter(|face| !face.provenance_tags.is_empty())
        .map(|face| (face, face_bbox(face), axis_hint(face)))
        .collect();
    if tagged.is_empty() {
        return;
    }
    for face in &mut solid.faces {
        let bbox = face_bbox(face);
        let mut compatible = tagged.iter().filter(|(src, bounds, axis)| {
            if src.surface_type != face.surface_type
                || !bounds.is_valid()
                || !bbox.is_valid()
                || norm(cross(*axis, face.normal_hint)) > TOL
            {
                return false;
            }
            let delta: [f64; 3] = std::array::from_fn(|i| src.center_hint[i] - face.center_hint[i]);
            match face.surface_type {
                SurfaceType::Plane => {
                    delta
                        .into_iter()
                        .zip(axis)
                        .map(|(a, b)| a * b)
                        .sum::<f64>()
                        .abs()
                        <= TOL
                        && (0..3).all(|i| {
                            bbox.min[i] >= bounds.min[i] - TOL && bbox.max[i] <= bounds.max[i] + TOL
                        })
                }
                SurfaceType::Cylinder => {
                    let (Some(a), Some(b)) = (src.radius_hint, face.radius_hint) else {
                        return false;
                    };
                    if (a - b).abs() > TOL || norm(cross(delta, *axis)) > TOL {
                        return false;
                    }
                    // A cut bore is an axial subset of the tool cylinder. Compare
                    // that span, not the tessellation AABB: samples at different
                    // angles sit outside each other's chordal bounding box even
                    // when both lie on the same exact cylinder.
                    let interval = |f: &BRepFace| {
                        f.vertices
                            .chunks_exact(3)
                            .map(|p| p.iter().zip(axis).map(|(x, d)| x * d).sum::<f64>())
                            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                                (lo.min(v), hi.max(v))
                            })
                    };
                    let (a0, a1) = interval(src);
                    let (b0, b1) = interval(face);
                    a1.is_finite() && b1.is_finite() && b0 >= a0 - TOL && b1 <= a1 + TOL
                }
                _ => false,
            }
        });
        if let Some((src, _, _)) = compatible.next()
            && compatible.next().is_none()
        {
            face.provenance_tags.clone_from(&src.provenance_tags);
            face.ast_span = src.ast_span;
        }
    }
}

fn first_span(shape: &SolidShape) -> Span {
    shape
        .faces
        .first()
        .map(|face| face.ast_span)
        .unwrap_or_default()
}

fn cylinder(
    origin: [f64; 3],
    axis: [f64; 3],
    radius: f64,
    height: f64,
) -> Result<ShapeId, KernelError> {
    // Exact placement: the bridge builds the cylinder on gp_Ax2(origin, axis).
    ShapeId::new(
        unsafe { occt_bridge_make_cylinder_axis(radius * 2.0, height, origin.as_ptr(), axis.as_ptr()) },
        "make cylinder",
    )
}

/// Wrap a freshly constructed native solid. Constructors only ever return
/// validated closed solids; anything else is reported, never substituted.
fn constructed(id: ShapeId, name: &str, span: Span) -> Result<SolidShape, KernelOperationError> {
    let solid = build_shape(Arc::new(id), name, span, OcctImportOptions::default(), vec![])?;
    match &solid.payload {
        KernelPayload::Occt(handle) if handle.is_open_shell() => Err(KernelError::OpenShell.into()),
        KernelPayload::Occt(handle) if handle.metrics.face_count == 0 => {
            Err(failure(&format!("{name} produced an empty native shape")).into())
        }
        _ => Ok(solid),
    }
}

fn finite_vector(values: &[f64], what: &str) -> Result<(), KernelOperationError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(failure(&format!("{what} must contain only finite values")).into())
    }
}

/// Native axis-aligned box spanning `origin .. origin + size` (positive sizes).
pub(crate) fn make_box(
    size: [f64; 3],
    origin: [f64; 3],
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    finite_vector(&size, "box size")?;
    finite_vector(&origin, "box origin")?;
    if size.iter().any(|&extent| extent <= 0.0) {
        return Err(failure(&format!("box size must be positive in every axis (got {size:?})")).into());
    }
    let [dx, dy, dz] = size;
    let [x, y, z] = origin;
    let id = ShapeId::from_ffi(unsafe { occt_bridge_make_box(dx, dy, dz, x, y, z) }, "make box")?;
    constructed(id, "box", span)
}

/// Native exact cylinder on `gp_Ax2(origin, axis)`; the axis keeps its
/// direction and the origin its position.
pub(crate) fn make_cylinder(
    diameter: f64,
    height: f64,
    origin: [f64; 3],
    axis: [f64; 3],
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    finite_vector(&[diameter, height], "cylinder dimensions")?;
    finite_vector(&origin, "cylinder origin")?;
    finite_vector(&axis, "cylinder axis")?;
    if diameter <= 0.0 || height <= 0.0 {
        return Err(failure(&format!(
            "cylinder requires a positive diameter and height (got d={diameter}, h={height})"
        ))
        .into());
    }
    let id = ShapeId::from_ffi(
        unsafe { occt_bridge_make_cylinder_axis(diameter, height, origin.as_ptr(), axis.as_ptr()) },
        "make cylinder",
    )?;
    constructed(id, "cylinder", span)
}

/// Native prism of a sketch polygon. Non-symmetric extrusions run from the
/// sketch plane along the signed `height`; symmetric ones span `±height/2`.
pub(crate) fn extrude(
    plane: SketchPlane,
    polygon: &[[f64; 2]],
    height: f64,
    symmetric: bool,
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    let (data, count) = polygon_data(polygon)?;
    crate::validate_occt_bridge_extrude_polygon_params(plane, &polygon[..count as usize], height)
        .map_err(KernelError::OperationFailed)?;
    let id = ShapeId::from_ffi(
        unsafe {
            occt_bridge_extrude_polygon(
                sketch_plane_to_occt_code(plane),
                data.as_ptr(),
                count,
                height,
                i32::from(symmetric),
            )
        },
        "extrude polygon",
    )?;
    constructed(id, "extrude", span)
}

/// Native revolution of a sketch polygon about `axis` through the world origin.
pub(crate) fn revolve(
    plane: SketchPlane,
    polygon: &[[f64; 2]],
    axis: [f64; 3],
    angle_deg: f64,
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    finite_vector(&axis, "revolve axis")?;
    if !angle_deg.is_finite() || angle_deg == 0.0 || angle_deg.abs() > 360.0 {
        return Err(failure(&format!(
            "revolve angle must be finite, nonzero and at most 360 degrees (got {angle_deg})"
        ))
        .into());
    }
    let (data, count) = polygon_data(polygon)?;
    let id = ShapeId::from_ffi(
        unsafe {
            occt_bridge_revolve_polygon(
                sketch_plane_to_occt_code(plane),
                data.as_ptr(),
                count,
                [0.0; 3].as_ptr(),
                axis.as_ptr(),
                angle_deg,
            )
        },
        "revolve polygon",
    )?;
    constructed(id, "revolve", span)
}

/// Native mitred sweep of a closed profile along an open polyline. The
/// profile frame at `path[0]` matches the analytic preview's frame.
pub(crate) fn sweep(
    profile: &[[f64; 2]],
    path: &[[f64; 3]],
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    let (data, count) = polygon_data(profile)?;
    if count < 3 {
        return Err(failure("sweep profile requires at least 3 vertices").into());
    }
    if path.len() < 2 {
        return Err(failure("sweep path requires at least 2 3D points").into());
    }
    let path_count = i32::try_from(path.len()).map_err(|_| failure("sweep path is too large"))?;
    let id = ShapeId::from_ffi(
        unsafe {
            occt_bridge_sweep_polygon(data.as_ptr(), count, path.as_flattened().as_ptr(), path_count)
        },
        "sweep polygon",
    )?;
    let mut solid = constructed(id, "sweep", span)?;
    solid.edges.push(BRepEdge {
        edge_id: "sweep_spine".to_string(),
        curve_type: CurveType::Polyline,
        polyline: path.as_flattened().to_vec(),
        provenance_tags: Vec::new(),
        component_id: None,
        ast_span: span,
    });
    Ok(solid)
}

/// Native ruled loft through sketch polygons in the given order.
pub(crate) fn loft(
    sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
    span: Span,
) -> Result<SolidShape, KernelOperationError> {
    if sections.len() < 2 {
        return Err(failure("loft requires at least 2 sketch sections").into());
    }
    let section_count =
        i32::try_from(sections.len()).map_err(|_| failure("too many loft sections"))?;
    let mut counts = Vec::with_capacity(sections.len());
    let mut planes = Vec::with_capacity(sections.len());
    let mut offsets = Vec::with_capacity(sections.len());
    let mut xy = Vec::new();
    for (index, (polygon, plane, offset)) in sections.iter().enumerate() {
        let (data, count) = polygon_data(polygon)?;
        if count < 3 {
            return Err(failure(&format!("loft section {index} has fewer than 3 vertices")).into());
        }
        counts.push(count);
        planes.push(sketch_plane_to_occt_code(*plane));
        offsets.push(*offset);
        xy.extend(data);
    }
    let id = ShapeId::from_ffi(
        unsafe {
            occt_bridge_loft_polygons(
                section_count,
                counts.as_ptr(),
                planes.as_ptr(),
                offsets.as_ptr(),
                xy.as_ptr(),
                1,
            )
        },
        "loft polygons",
    )?;
    constructed(id, "loft", span)
}

fn polygon_data(polygon: &[[f64; 2]]) -> Result<(Vec<f64>, i32), KernelError> {
    let polygon = if polygon.len() > 1 && polygon.first() == polygon.last() {
        &polygon[..polygon.len() - 1]
    } else {
        polygon
    };
    let count = i32::try_from(polygon.len()).map_err(|_| failure("polygon is too large"))?;
    Ok((polygon.iter().flatten().copied().collect(), count))
}

fn lift_analytic_to_occt(csg: &CsgNode) -> Result<Option<Arc<ShapeId>>, KernelOperationError> {
    let id = match csg {
        CsgNode::Empty => return Ok(None),
        CsgNode::Box { min, max } => ShapeId::from_ffi(
            unsafe {
                occt_bridge_make_box(
                    max[0] - min[0],
                    max[1] - min[1],
                    max[2] - min[2],
                    min[0],
                    min[1],
                    min[2],
                )
            },
            "make box",
        )?,
        CsgNode::Cylinder {
            origin,
            axis,
            radius,
            height,
        } => cylinder(*origin, *axis, *radius, *height)?,
        CsgNode::Counterbore {
            origin: [x, y, z],
            r_through,
            r_cbore,
            depth_through,
            depth_cbore,
        } => {
            let depth_at_shelf = depth_through.min(*depth_cbore);
            let upper = cylinder(
                [*x, *y, z - depth_at_shelf],
                [0.0, 0.0, 1.0],
                *r_cbore,
                1.0 + depth_at_shelf,
            )?;
            if *depth_through <= depth_at_shelf {
                upper
            } else {
                let lower = cylinder(
                    [*x, *y, z - depth_through],
                    [0.0, 0.0, 1.0],
                    *r_through,
                    depth_through - depth_at_shelf,
                )?;
                return apply(occt_bridge_boolean_union, &upper, &lower).map(Some);
            }
        }
        CsgNode::Prism {
            plane,
            polygon_2d,
            offset_min,
            offset_max,
        } => {
            let (data, count) = polygon_data(polygon_2d)?;
            let id = ShapeId::from_ffi(
                unsafe {
                    occt_bridge_extrude_polygon(
                        sketch_plane_to_occt_code(*plane),
                        data.as_ptr(),
                        count,
                        offset_max - offset_min,
                        0,
                    )
                },
                "extrude polygon",
            )?;
            if *offset_min == 0.0 {
                id
            } else {
                let [x, y, z] = plane.to_3d(0.0, 0.0, *offset_min);
                id.transformed(Mat4::translate(x, y, z))?
            }
        }
        CsgNode::Revolve {
            plane,
            polygon_2d,
            axis,
            angle_deg,
        } => {
            let (data, count) = polygon_data(polygon_2d)?;
            ShapeId::from_ffi(
                unsafe {
                    occt_bridge_revolve_polygon(
                        sketch_plane_to_occt_code(*plane),
                        data.as_ptr(),
                        count,
                        [0.0; 3].as_ptr(),
                        axis.as_ptr(),
                        *angle_deg,
                    )
                },
                "revolve polygon",
            )?
        }
        CsgNode::Transform { child, fwd, .. } => {
            let Some(child) = lift_analytic_to_occt(child)? else {
                return Ok(None);
            };
            child.transformed(*fwd)?
        }
        CsgNode::Reflect { child, plane } => {
            let Some(child) = lift_analytic_to_occt(child)? else {
                return Ok(None);
            };
            child.transformed(Mat4::reflect(*plane))?
        }
        CsgNode::Union(items) => {
            let mut acc: Option<Arc<ShapeId>> = None;
            for item in items {
                if let Some(b) = lift_analytic_to_occt(item)? {
                    acc = Some(match acc {
                        Some(a) => apply(occt_bridge_boolean_union, &a, &b)?,
                        None => b,
                    });
                }
            }
            return Ok(acc);
        }
        CsgNode::Subtract { base, tools } => {
            let Some(mut id) = lift_analytic_to_occt(base)? else {
                return Ok(None);
            };
            for tool in tools {
                if let Some(tool) = lift_analytic_to_occt(tool)? {
                    id = apply(occt_bridge_boolean_cut, &id, &tool)?;
                }
            }
            return Ok(Some(id));
        }
        // OCCT chamfer_all cuts a different set of edges than our legacy
        // horizontal-rim chamfer. Never substitute different geometry on lift.
        CsgNode::Mesh { .. } | CsgNode::Occt { .. } | CsgNode::ChamferedBox { .. } => {
            return Err(KernelError::UnsupportedMixedBoolean.into());
        }
    };
    Ok(Some(Arc::new(id)))
}

/// Rebuild an exact analytic construction as a native OCCT solid, keeping its
/// name, PMI, mates and any uniquely matched face provenance tags. Native
/// shapes are returned unchanged; mesh payloads are rejected.
pub(crate) fn to_native(shape: &SolidShape) -> Result<SolidShape, KernelError> {
    match &shape.payload {
        KernelPayload::Occt(_) => Ok(shape.clone()),
        KernelPayload::TriMesh => Err(KernelError::UnsupportedMixedBoolean),
        KernelPayload::Analytic => {
            let Some(id) = lift_analytic_to_occt(&shape.csg)? else {
                return Ok(shape.clone());
            };
            let mut result = build_shape(
                id,
                &shape.part_name,
                first_span(shape),
                OcctImportOptions::default(),
                vec![],
            )?;
            if let KernelPayload::Occt(handle) = &result.payload
                && handle.is_open_shell()
            {
                return Err(KernelError::OpenShell);
            }
            result.exact_brep = shape.exact_brep;
            propagate_face_tags(&mut result, shape.faces.iter());
            result.pmi.clone_from(&shape.pmi);
            result.mates.clone_from(&shape.mates);
            Ok(result)
        }
    }
}

/// Constant-size OCCT edge dress-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DressUpKind {
    Fillet,
    Chamfer,
}

impl DressUpKind {
    fn name(self) -> &'static str {
        match self {
            Self::Fillet => "fillet",
            Self::Chamfer => "chamfer",
        }
    }
}

/// Dress up all sharp edges (`edges == None`) or exactly the listed zero-based
/// native edge indices (as reported by `topology::extract` for this shape).
/// Fails instead of returning the unmodified input.
pub(crate) fn dress_up(
    shape: &SolidShape,
    kind: DressUpKind,
    size_mm: f64,
    edges: Option<&[usize]>,
    span: Span,
) -> Result<SolidShape, KernelError> {
    let name = kind.name();
    if !size_mm.is_finite() || size_mm <= 0.0 {
        return Err(failure(&format!(
            "{name} requires a finite positive size in mm (got {size_mm})"
        )));
    }
    if edges.is_some() && !matches!(shape.payload, KernelPayload::Occt(_)) {
        return Err(failure(&format!(
            "{name} by edge index requires a native solid whose topology defines the indices"
        )));
    }
    let operand = native_operand(shape)?
        .ok_or_else(|| failure(&format!("{name} requires a nonempty solid")))?;
    let raw = match edges {
        None => unsafe {
            match kind {
                DressUpKind::Fillet => occt_bridge_fillet_all(operand.raw(), size_mm),
                DressUpKind::Chamfer => occt_bridge_chamfer_all(operand.raw(), size_mm),
            }
        },
        Some(edges) => {
            let indices = edges
                .iter()
                .map(|&edge| i32::try_from(edge))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| failure(&format!("{name} edge index out of range")))?;
            let count = i32::try_from(indices.len())
                .map_err(|_| failure(&format!("too many {name} edges")))?;
            if count == 0 {
                return Err(failure(&format!("{name} requires at least one edge")));
            }
            // `indices` holds `count` initialized values and outlives the call.
            unsafe {
                match kind {
                    DressUpKind::Fillet => {
                        occt_bridge_fillet_edges(operand.raw(), size_mm, indices.as_ptr(), count)
                    }
                    DressUpKind::Chamfer => {
                        occt_bridge_chamfer_edges(operand.raw(), size_mm, indices.as_ptr(), count)
                    }
                }
            }
        }
    };
    let id = Arc::new(ShapeId::new(
        raw,
        &format!("{name} of {size_mm} mm could not produce a valid closed solid"),
    )?);
    let mut result = build_boolean(id, &shape.part_name, span, native_options([shape]))?;
    propagate_face_tags(&mut result, shape.faces.iter());
    result.pmi.clone_from(&shape.pmi);
    Ok(result)
}

/// Hollow a solid inward with one opening face selected by its outward normal.
pub(crate) fn shell_open_normals(
    shape: &SolidShape,
    thickness_mm: f64,
    open_normals: &[[f64; 3]],
    span: Span,
) -> Result<SolidShape, KernelError> {
    let normal = match open_normals {
        [] => [0.0, 0.0, 1.0],
        [normal] => *normal,
        _ => {
            return Err(failure(
                "shell supports exactly one opening face in the native kernel",
            ));
        }
    };
    let operand =
        native_operand(shape)?.ok_or_else(|| failure("shell requires a nonempty solid"))?;
    let [nx, ny, nz] = crate::validate_occt_bridge_shell_params(operand.raw(), thickness_mm, normal)
        .map_err(KernelError::OperationFailed)?;
    let raw = unsafe { occt_bridge_shell(operand.raw(), thickness_mm, nx, ny, nz) };
    let id = Arc::new(ShapeId::new(
        raw,
        &format!(
            "shell of {thickness_mm} mm could not produce a valid closed solid (opening normal must match a face)"
        ),
    )?);
    let mut result = build_boolean(id, &shape.part_name, span, native_options([shape]))?;
    propagate_face_tags(&mut result, shape.faces.iter());
    result.pmi.clone_from(&shape.pmi);
    Ok(result)
}

/// Export the exact native B-Rep (lifting exact analytic constructions first).
pub(crate) fn export_native_step(shape: &SolidShape) -> Result<String, KernelError> {
    let native = to_native(shape)?;
    match &native.payload {
        KernelPayload::Occt(handle) => Ok(handle.export_step(&shape.part_name)?),
        _ => Err(failure("cannot export an empty solid as STEP")),
    }
}

/// Collect the exact shapes of a grouping `csg` (an assembly): nested unions
/// whose leaves are native nodes, optionally under rigid placements. Returns
/// `None` when any leaf is not native, so callers never substitute geometry.
fn native_group_ids(csg: &CsgNode) -> Result<Option<Vec<Arc<ShapeId>>>, KernelError> {
    fn collect(csg: &CsgNode, out: &mut Vec<Arc<ShapeId>>) -> Result<bool, KernelError> {
        match csg {
            CsgNode::Occt {
                native: Some(guard),
                ..
            } if guard.metrics.face_count > 0 => {
                out.push(guard.id.clone());
                Ok(true)
            }
            CsgNode::Union(items) if !items.is_empty() => {
                for item in items {
                    if !collect(item, out)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            CsgNode::Transform { child, fwd, .. } => place(child, *fwd, out),
            CsgNode::Reflect { child, plane } => place(child, Mat4::reflect(*plane), out),
            _ => Ok(false),
        }
    }
    fn place(child: &CsgNode, matrix: Mat4, out: &mut Vec<Arc<ShapeId>>) -> Result<bool, KernelError> {
        let mut inner = Vec::new();
        if !collect(child, &mut inner)? {
            return Ok(false);
        }
        for id in inner {
            out.push(Arc::new(id.transformed(matrix)?));
        }
        Ok(true)
    }
    let mut ids = Vec::new();
    Ok(collect(csg, &mut ids)?.then_some(ids))
}

/// Whether `csg` groups only native solids (see [`native_group_ids`]).
pub(crate) fn is_native_group(csg: &CsgNode) -> bool {
    match csg {
        CsgNode::Occt {
            native: Some(guard),
            ..
        } => guard.metrics.face_count > 0,
        CsgNode::Union(items) => !items.is_empty() && items.iter().all(is_native_group),
        CsgNode::Transform { child, .. } | CsgNode::Reflect { child, .. } => is_native_group(child),
        _ => false,
    }
}

/// Exact checks of every grouped native solid, aggregated like an assembly:
/// each member must be watertight; volumes, areas and counts are summed.
pub(crate) fn native_group_report(shape: &SolidShape) -> Result<CheckReport, KernelError> {
    let ids = native_group_ids(&shape.csg)?
        .ok_or_else(|| failure("assembly contains a non-native member"))?;
    let mut report = CheckReport {
        watertight: true,
        volume_mm3: 0.0,
        surface_area_mm2: shape.faces.iter().map(|face| face.area_mm2).sum(),
        bbox_mm: BBox3::empty(),
        face_count: 0,
        edge_count: 0,
        open_edge_count: 0,
        issues: Vec::new(),
    };
    for (index, id) in ids.iter().enumerate() {
        let metrics = id.metrics()?;
        if metrics.watertight != 1 {
            report.watertight = false;
            report
                .issues
                .push(format!("member {index}: {}", KernelError::OpenShell));
        }
        report.volume_mm3 += metrics.volume_mm3;
        report.bbox_mm = report.bbox_mm.union(BBox3 {
            min: metrics.bbox.min,
            max: metrics.bbox.max,
        });
        report.face_count += metrics.face_count as usize;
        report.edge_count += metrics.edge_count as usize;
    }
    Ok(report)
}

/// Export a group of native solids as one AP242 assembly through OCCT XCAF.
/// Component products are named after `shape.components` when they match.
pub(crate) fn export_native_group_step(shape: &SolidShape) -> Result<String, KernelOperationError> {
    let ids = native_group_ids(&shape.csg)?.ok_or_else(|| {
        failure("native assembly STEP export requires every member to be an exact native solid")
    })?;
    let names = ids
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let name = match shape.components.get(index) {
                Some(meta) if shape.components.len() == ids.len() => meta.component_id.clone(),
                _ => format!("{}_{index}", shape.part_name),
            };
            CString::new(name).map_err(|_| failure("STEP component name contains NUL"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let name_ptrs: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
    let handles: Vec<u64> = ids.iter().map(|id| id.raw()).collect();
    let count = i32::try_from(ids.len()).map_err(|_| failure("too many assembly members"))?;
    let assembly = CString::new(if shape.part_name.is_empty() {
        "assembly"
    } else {
        shape.part_name.as_str()
    })
    .map_err(|_| failure("STEP assembly name contains NUL"))?;
    let dir = PrivateDir::new()?;
    let path = dir.0.join("assembly.step");
    let path_c = path_string(&path)?;
    // Every buffer holds `count` live entries and outlives the call; `ids`
    // keeps the native handles alive.
    ffi_status(
        unsafe {
            occt_bridge_export_step_assembly(
                count,
                handles.as_ptr(),
                name_ptrs.as_ptr(),
                assembly.as_ptr(),
                path_c.as_ptr(),
            )
        },
        "export STEP assembly",
    )?;
    let text = fs::read_to_string(path).map_err(io_failure)?;
    Ok(compact_step_entities(&text))
}

#[repr(C)]
struct RawHlrPolyline {
    point_offset: u32,
    point_count: u32,
    hidden: i32,
}

#[derive(Default)]
#[repr(C)]
struct RawHlrResult {
    points: *const [f64; 2],
    polylines: *const RawHlrPolyline,
    point_count: u32,
    polyline_count: u32,
    storage: *mut std::ffi::c_void,
}

unsafe extern "C" {
    fn occt_bridge_compute_hlr(
        handle: u64,
        toward_eye: *const f64,
        view_x: *const f64,
        deflection_mm: f64,
        out: *mut RawHlrResult,
    ) -> i32;
    fn occt_bridge_free_hlr(result: *mut RawHlrResult);
}

impl Drop for RawHlrResult {
    fn drop(&mut self) {
        unsafe { occt_bridge_free_hlr(self) };
    }
}

pub(crate) fn compute_hlr(
    handle: &OcctShapeGuard,
    toward_eye: [f64; 3],
    view_x: [f64; 3],
    deflection_mm: f64,
) -> Result<crate::hlr::HlrProjection, KernelError> {
    verify_abi_handshake()?;
    let mut raw = RawHlrResult::default();
    let code = unsafe {
        occt_bridge_compute_hlr(
            handle.id.raw(),
            toward_eye.as_ptr(),
            view_x.as_ptr(),
            deflection_mm,
            &mut raw,
        )
    };
    if code == -4 {
        return Err(KernelError::BackendUnavailable);
    }
    status(code, "exact hidden-line projection")?;
    let invalid = || failure("invalid hidden-line projection output");
    fn valid_buffer<T>(ptr: *const T, count: usize) -> bool {
        count == 0 || (!ptr.is_null() && count <= isize::MAX as usize / size_of::<T>())
    }
    let point_count = raw.point_count as usize;
    let polyline_count = raw.polyline_count as usize;
    if !valid_buffer(raw.points, point_count) || !valid_buffer(raw.polylines, polyline_count) {
        return Err(invalid());
    }
    if polyline_count == 0 {
        return Ok(crate::hlr::HlrProjection::default());
    }
    if point_count == 0 {
        return Err(invalid());
    }
    // Both buffers are owned by `raw` and remain live until its Drop.
    let points = unsafe { std::slice::from_raw_parts(raw.points, point_count) };
    let polylines = unsafe { std::slice::from_raw_parts(raw.polylines, polyline_count) };
    let mut output = crate::hlr::HlrProjection::default();
    for line in polylines {
        let start = line.point_offset as usize;
        let count = line.point_count as usize;
        let end = start.checked_add(count).ok_or_else(invalid)?;
        let curve = points.get(start..end).ok_or_else(invalid)?;
        if count < 2 || !curve.iter().flatten().all(|p| p.is_finite()) {
            return Err(invalid());
        }
        match line.hidden {
            0 => output.visible.push(curve.to_vec()),
            1 => output.hidden.push(curve.to_vec()),
            _ => return Err(invalid()),
        }
    }
    Ok(output)
}
