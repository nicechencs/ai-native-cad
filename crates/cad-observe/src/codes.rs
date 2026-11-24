//! Runtime error codes (R codes). Distinct from compile diagnostics (E/W codes).

pub const R010_GPU_FALLBACK: &str = "R010_GPU_FALLBACK";
pub const R011_IPC_FAILED: &str = "R011_IPC_FAILED";
pub const R012_KERNEL_NATIVE: &str = "R012_KERNEL_NATIVE";
pub const R013_EMPTY_ISOSURFACE: &str = "R013_EMPTY_ISOSURFACE";
pub const R014_VLM_DEGRADED: &str = "R014_VLM_DEGRADED";
pub const R015_DAEMON_RECONNECT: &str = "R015_DAEMON_RECONNECT";
pub const R016_IO_ERROR: &str = "R016_IO_ERROR";

/// Every runtime code, in numeric order.
pub const ALL: [&str; 7] = [
    R010_GPU_FALLBACK,
    R011_IPC_FAILED,
    R012_KERNEL_NATIVE,
    R013_EMPTY_ISOSURFACE,
    R014_VLM_DEGRADED,
    R015_DAEMON_RECONNECT,
    R016_IO_ERROR,
];
