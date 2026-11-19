//! C++ OpenCASCADE (OCCT) dynamic FFI bridge declarations.
//! Enabled only with `--features occt-ffi` when linking against `cpp/occt-bridge`.

use std::ffi::{c_char, c_double, c_int};

#[repr(C)]
pub struct OcctBridgeBBox {
    pub min: [c_double; 3],
    pub max: [c_double; 3],
}

#[repr(C)]
pub struct OcctBridgeMetrics {
    pub watertight: c_int,
    pub volume_mm3: c_double,
    pub bbox: OcctBridgeBBox,
    pub face_count: c_int,
    pub edge_count: c_int,
}

unsafe extern "C" {
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
    pub fn occt_bridge_check(handle: u64, out_metrics: *mut OcctBridgeMetrics) -> c_int;
    pub fn occt_bridge_export_step(handle: u64, path_utf8: *const c_char) -> c_int;
    pub fn occt_bridge_free(handle: u64);
}
