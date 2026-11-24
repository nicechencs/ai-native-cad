//! Pluggable B-Rep kernel identity and capability contracts.
//!
//! `GeometryKernel` contains the operation surface used by the evaluator. This
//! module adds the provider contract used by callers that need to decide
//! whether a result can be delivered as authoritative geometry. When the
//! native OCCT bridge is loaded, `OcctKernel` is the authoritative provider;
//! `AnalyticKernel` remains the construction and preview implementation when
//! the native OCCT bridge is unavailable.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{AnalyticKernel, GeometryKernel};

/// The implementation providing B-Rep operations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KernelProviderKind {
    /// OpenCASCADE through the native C ABI bridge.
    Occt,
    /// The Rust analytic/preview implementation used without a native bridge.
    AnalyticFallback,
    /// A separately installed commercial kernel provider.
    CommercialPlugin(String),
}

/// Operations exposed by the provider. Exact paths may require native operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KernelCapabilities {
    pub exact_boolean: bool,
    pub exact_fillet_chamfer: bool,
    pub exact_shell: bool,
    /// Box/cylinder primitives and extrude/revolve/sweep/loft sketch features
    /// are built directly as exact native B-Rep solids.
    pub exact_sketch_features: bool,
    pub freeform_nurbs: bool,
    pub subd_to_nurbs: bool,
    pub exact_hlr: bool,
    pub exact_step_ap242: bool,
    pub exact_interference: bool,
}

/// Stable identity and operation support for one kernel instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BRepKernelMetadata {
    pub provider: KernelProviderKind,
    pub version: String,
    pub capabilities: KernelCapabilities,
}

/// A geometry kernel that can be selected as the B-Rep provider.
///
/// The supertraits intentionally keep the contract object-safe so the
/// evaluator can receive a provider through `Box<dyn IBRepKernel>`.
pub trait IBRepKernel: GeometryKernel + Send + Sync {
    fn metadata(&self) -> BRepKernelMetadata;

    /// Whether this provider can produce authoritative geometry. Individual
    /// results still depend on the operation and its input payloads.
    fn supports_authoritative_delivery(&self) -> bool;

    /// Convenience access to the provider's capability flags.
    fn capabilities(&self) -> KernelCapabilities {
        self.metadata().capabilities
    }
}

/// Operations `OcctKernel` performs on native OCCT B-Rep handles.
#[cfg(feature = "occt-ffi")]
pub(crate) const OCCT_CAPABILITIES: KernelCapabilities = KernelCapabilities {
    exact_boolean: true,
    // BRepFilletAPI_MakeFillet / MakeChamfer through the bridge.
    exact_fillet_chamfer: true,
    // BRepOffsetAPI_MakeThickSolid through the bridge.
    exact_shell: true,
    // BRepPrimAPI_MakeBox/MakeCylinder(gp_Ax2), MakePrism, MakeRevol,
    // BRepOffsetAPI_MakePipeShell and ThruSections through the bridge.
    exact_sketch_features: true,
    // The bridge currently exposes B-spline descriptors for imported
    // topology, but no NURBS construction operation in GeometryKernel.
    freeform_nurbs: false,
    subd_to_nurbs: false,
    exact_hlr: true,
    exact_step_ap242: true,
    exact_interference: true,
};

/// `AnalyticKernel` with a loaded bridge: Booleans on native or liftable
/// operands, HLR, STEP and interference are exact, but its analytic dress-up,
/// shell, sweep and loft paths are faceted approximations.
#[cfg(feature = "occt-ffi")]
const ANALYTIC_WITH_OCCT_CAPABILITIES: KernelCapabilities = KernelCapabilities {
    exact_fillet_chamfer: false,
    exact_shell: false,
    exact_sketch_features: false,
    ..OCCT_CAPABILITIES
};

/// Expected C ABI version exported by `occt_bridge` via `aicad_kernel_abi_version`.
pub const EXPECTED_OCCT_ABI_VERSION: u32 = 1;

/// Validate the ABI version reported by `aicad_kernel_abi_version`.
///
/// Returns a clear diagnostic error when the symbol is missing from an older
/// bridge binary or when the reported ABI version does not match
/// [`EXPECTED_OCCT_ABI_VERSION`].
pub fn validate_occt_abi_version(actual: Option<u32>) -> Result<u32, crate::KernelError> {
    match actual {
        Some(version) if version == EXPECTED_OCCT_ABI_VERSION => Ok(version),
        Some(version) => Err(crate::KernelError::OperationFailed(format!(
            "OCCT bridge ABI version mismatch: expected {EXPECTED_OCCT_ABI_VERSION}, got {version}; rebuild or reinstall occt_bridge"
        ))),
        None => Err(crate::KernelError::OperationFailed(format!(
            "OCCT bridge ABI handshake failed: missing symbol `aicad_kernel_abi_version` (expected ABI version {EXPECTED_OCCT_ABI_VERSION}); rebuild or reinstall occt_bridge"
        ))),
    }
}

#[cfg(feature = "occt-ffi")]
unsafe extern "C" {
    fn occt_bridge_has_exact_interference() -> i32;
}

#[cfg(feature = "occt-ffi")]
pub(crate) fn loaded_occt_version() -> Option<&'static str> {
    use std::ffi::CStr;
    use std::sync::OnceLock;

    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            if let Err(err) = crate::occt_ffi::verify_abi_handshake() {
                tracing::error!(
                    component = "cad-kernel-occt",
                    operation = "abi_handshake",
                    "{err}"
                );
                return None;
            }
            // Both bridge builds expose the version symbol. Only the native
            // build reports support for exact interference.
            // SAFETY: This is a side-effect-free C ABI query with no arguments.
            if unsafe { occt_bridge_has_exact_interference() } != 1 {
                return None;
            }

            let ptr = unsafe { crate::occt_ffi::occt_bridge_version() };
            if ptr.is_null() {
                return None;
            }
            // SAFETY: The bridge returns a static NUL-terminated version string.
            let version = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
            if version.is_empty() {
                None
            } else {
                Some(version.into_owned())
            }
        })
        .as_deref()
}

fn analytic_metadata() -> BRepKernelMetadata {
    #[cfg(feature = "occt-ffi")]
    let capabilities = if loaded_occt_version().is_some() {
        ANALYTIC_WITH_OCCT_CAPABILITIES
    } else {
        KernelCapabilities::default()
    };
    #[cfg(not(feature = "occt-ffi"))]
    let capabilities = KernelCapabilities::default();

    BRepKernelMetadata {
        provider: KernelProviderKind::AnalyticFallback,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities,
    }
}

impl IBRepKernel for AnalyticKernel {
    fn metadata(&self) -> BRepKernelMetadata {
        analytic_metadata()
    }

    fn supports_authoritative_delivery(&self) -> bool {
        false
    }
}

#[cfg(feature = "occt-ffi")]
impl IBRepKernel for crate::OcctKernel {
    fn metadata(&self) -> BRepKernelMetadata {
        match loaded_occt_version() {
            Some(version) => BRepKernelMetadata {
                provider: KernelProviderKind::Occt,
                version: version.to_owned(),
                capabilities: OCCT_CAPABILITIES,
            },
            // A mock bridge cannot back any exact operation.
            None => BRepKernelMetadata {
                provider: KernelProviderKind::Occt,
                version: "unavailable".to_owned(),
                capabilities: KernelCapabilities::default(),
            },
        }
    }

    fn supports_authoritative_delivery(&self) -> bool {
        loaded_occt_version().is_some()
    }
}

/// Construct the process-default B-Rep provider: the authoritative OCCT
/// kernel when the native bridge is loaded, otherwise the analytic fallback.
pub fn default_brep_kernel() -> Box<dyn IBRepKernel> {
    #[cfg(feature = "occt-ffi")]
    if crate::OcctKernel::is_available() {
        return Box::new(crate::OcctKernel::new());
    }
    Box::new(AnalyticKernel::new())
}

/// Toolkits `occt_bridge` links. Runtime loads these from the prebuilt bin/lib
/// directory; the file names below are the ones a missing install must report.
const OCCT_TOOLKIT_STEMS: &[&str] = &[
    "TKernel",
    "TKMath",
    "TKBRep",
    "TKGeomBase",
    "TKG2d",
    "TKG3d",
    "TKGeomAlgo",
    "TKTopAlgo",
    "TKPrim",
    "TKBO",
    "TKFillet",
    "TKOffset",
    "TKShHealing",
    "TKMesh",
    "TKHLR",
    "TKCDF",
    "TKLCAF",
    "TKXCAF",
    "TKXSBase",
    "TKDESTEP",
    "TKDEIGES",
];

const WINDOWS_CRT_LIBRARIES: &[&str] = &["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"];

const BRIDGE_CONFIGS: &[&str] = &["Release", "RelWithDebInfo", "MinSizeRel", "Debug"];

/// Whether this process has the native OCCT bridge mapped and reporting a version.
///
/// Disk files alone are not a loaded kernel. A mock `occt_bridge` returns false.
pub(crate) fn native_occt_bridge_loaded() -> bool {
    #[cfg(feature = "occt-ffi")]
    {
        loaded_occt_version().is_some()
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        false
    }
}

fn bridge_linked_without_native() -> bool {
    #[cfg(feature = "occt-ffi")]
    {
        loaded_occt_version().is_none()
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        false
    }
}

fn bridge_library_name() -> &'static str {
    if cfg!(windows) {
        "occt_bridge.dll"
    } else if cfg!(target_os = "macos") {
        "libocct_bridge.dylib"
    } else {
        "libocct_bridge.so"
    }
}

fn toolkit_library_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{stem}.dylib")
    } else {
        format!("lib{stem}.so")
    }
}

fn system_runtime_library_names() -> Vec<&'static str> {
    if cfg!(windows) {
        WINDOWS_CRT_LIBRARIES.to_vec()
    } else if cfg!(target_os = "macos") {
        vec!["libc++.1.dylib"]
    } else {
        vec!["libstdc++.so.6"]
    }
}

fn prebuilt_runtime_dir(root: &Path) -> PathBuf {
    let prebuilt = root.join("cpp").join("occt-prebuilt");
    if cfg!(windows) {
        prebuilt.join("Library").join("bin")
    } else {
        prebuilt.join("lib")
    }
}

fn bridge_search_dirs(root: &Path, honor_env: bool) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if honor_env {
        if let Some(custom) = std::env::var_os("OCCT_BRIDGE_LIB_DIR") {
            push_dir(&mut dirs, PathBuf::from(custom));
        }
    }
    let build = root.join("build").join("occt-bridge");
    for config in BRIDGE_CONFIGS {
        push_dir(&mut dirs, build.join(config));
    }
    push_dir(&mut dirs, build);
    dirs
}

fn push_dir(dirs: &mut Vec<PathBuf>, dir: PathBuf) {
    if dir.is_dir() && !dirs.iter().any(|existing| existing == &dir) {
        dirs.push(dir);
    }
}

fn is_dynamic_library(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 4];
    let Ok(read) = file.read(&mut magic) else {
        return false;
    };
    if cfg!(windows) {
        return read >= 2 && magic[0] == b'M' && magic[1] == b'Z';
    }
    if read < 4 {
        return false;
    }
    if cfg!(target_os = "macos") {
        matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
        )
    } else {
        magic == [0x7f, b'E', b'L', b'F']
    }
}

fn name_matches_library(file_name: &str, canonical: &str) -> bool {
    let (file_name, canonical) = if cfg!(windows) {
        (
            file_name.to_ascii_lowercase(),
            canonical.to_ascii_lowercase(),
        )
    } else {
        (file_name.to_owned(), canonical.to_owned())
    };
    if file_name == canonical {
        return true;
    }
    let Some((stem, _)) = canonical.rsplit_once('.') else {
        return false;
    };
    file_name.starts_with(&format!("{stem}.")) || file_name.starts_with(&format!("{stem}-"))
}

fn dir_has_library(dir: &Path, canonical: &str) -> bool {
    if !dir.is_dir() {
        return false;
    }
    if is_dynamic_library(&dir.join(canonical)) {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        name_matches_library(&name.to_string_lossy(), canonical)
            && is_dynamic_library(&entry.path())
    })
}

fn exact_library(dir: &Path, canonical: &str) -> bool {
    is_dynamic_library(&dir.join(canonical))
}

fn system_library_present(name: &str, extra_dirs: &[PathBuf]) -> bool {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR")) {
            dirs.push(PathBuf::from(root).join("System32"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/usr/lib"));
    } else {
        dirs.extend(
            [
                "/lib",
                "/lib64",
                "/usr/lib",
                "/usr/lib64",
                "/lib/x86_64-linux-gnu",
                "/usr/lib/x86_64-linux-gnu",
                "/lib/aarch64-linux-gnu",
                "/usr/lib/aarch64-linux-gnu",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
    }
    if let Some(path) = std::env::var_os(if cfg!(windows) {
        "PATH"
    } else {
        "LD_LIBRARY_PATH"
    }) {
        dirs.extend(std::env::split_paths(&path));
    }
    if cfg!(target_os = "macos") {
        if let Some(path) = std::env::var_os("DYLD_LIBRARY_PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
    }
    dirs.extend(extra_dirs.iter().cloned());
    // Exact names only. Version-prefix scans of System32 or PATH would walk huge directories.
    dirs.iter().any(|dir| exact_library(dir, name))
}

fn defined_u32(text: &str, macro_name: &str) -> Option<u32> {
    let marker = format!("#define {macro_name}");
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix(&marker) else {
            continue;
        };
        let digits: String = rest
            .trim()
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if let Ok(value) = digits.parse() {
            return Some(value);
        }
    }
    None
}

/// `Some(found_version)` when a readable header is older than OCCT 7.9.
fn occt_header_older_than_7_9(root: &Path) -> Option<String> {
    let candidates = [
        root.join("cpp/occt-prebuilt/Library/include/opencascade/Standard_Version.hxx"),
        root.join("cpp/occt-prebuilt/include/opencascade/Standard_Version.hxx"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (Some(major), Some(minor)) = (
            defined_u32(&text, "OCC_VERSION_MAJOR"),
            defined_u32(&text, "OCC_VERSION_MINOR"),
        ) else {
            continue;
        };
        if major < 7 || (major == 7 && minor < 9) {
            return Some(format!("{major}.{minor}"));
        }
        return None;
    }
    None
}

struct RuntimeProbe {
    missing: Vec<String>,
    bridge_missing: bool,
    occt_missing: bool,
    crt_missing: bool,
    version_too_old: bool,
}

fn probe_runtime(root: Option<&Path>, honor_env: bool) -> RuntimeProbe {
    let mut missing = Vec::new();
    let bridge_name = bridge_library_name();
    let bridge_dirs = root
        .map(|root| bridge_search_dirs(root, honor_env))
        .unwrap_or_default();
    let prebuilt = root.map(prebuilt_runtime_dir);
    let mut library_dirs = bridge_dirs.clone();
    if let Some(dir) = prebuilt.as_ref().filter(|dir| dir.is_dir()) {
        push_dir(&mut library_dirs, dir.clone());
    }
    let bridge_missing = !library_dirs
        .iter()
        .any(|dir| dir_has_library(dir, bridge_name));
    if bridge_missing {
        missing.push(bridge_name.to_owned());
    }
    let mut occt_missing = false;
    for stem in OCCT_TOOLKIT_STEMS {
        let name = toolkit_library_name(stem);
        let present = prebuilt
            .as_ref()
            .is_some_and(|dir| dir_has_library(dir, &name));
        if !present {
            occt_missing = true;
            missing.push(name);
        }
    }
    let mut crt_missing = false;
    for name in system_runtime_library_names() {
        if !system_library_present(name, &library_dirs) {
            crt_missing = true;
            missing.push((*name).to_owned());
        }
    }
    let mut version_too_old = false;
    if let Some(root) = root {
        if let Some(found) = occt_header_older_than_7_9(root) {
            version_too_old = true;
            missing.push(format!("OCCT>=7.9 (found {found} in Standard_Version.hxx)"));
        }
    }
    missing.sort();
    missing.dedup();
    RuntimeProbe {
        missing,
        bridge_missing,
        occt_missing,
        crt_missing,
        version_too_old,
    }
}

fn quote_path(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

fn fix_hints(root: Option<&Path>, probe: &RuntimeProbe) -> Vec<String> {
    let mut hints = Vec::new();
    if probe.bridge_missing {
        let (source, build) = match root {
            Some(root) => (
                quote_path(&root.join("cpp").join("occt-bridge")),
                quote_path(&root.join("build").join("occt-bridge")),
            ),
            None => ("cpp/occt-bridge".to_owned(), "build/occt-bridge".to_owned()),
        };
        let hint = if cfg!(windows) {
            format!("cmake -S {source} -B {build}; cmake --build {build} --config Release")
        } else {
            format!("cmake -S {source} -B {build} && cmake --build {build} --config Release")
        };
        hints.push(hint);
    }
    if probe.occt_missing || probe.version_too_old {
        hints.push(match root {
            Some(root) if cfg!(windows) => format!(
                "powershell -ExecutionPolicy Bypass -File {}",
                quote_path(&root.join("scripts").join("fetch-occt-prebuilt.ps1"))
            ),
            Some(root) => quote_path(&root.join("scripts").join("fetch-occt-prebuilt.sh")),
            None if cfg!(windows) => {
                "powershell -ExecutionPolicy Bypass -File scripts/fetch-occt-prebuilt.ps1"
                    .to_owned()
            }
            None => "./scripts/fetch-occt-prebuilt.sh".to_owned(),
        });
    }
    if probe.crt_missing {
        hints.push(if cfg!(windows) {
            "winget install --id Microsoft.VCRedist.2015+.x64 -e".to_owned()
        } else if cfg!(target_os = "macos") {
            "xcode-select --install".to_owned()
        } else {
            "sudo apt-get install --yes libstdc++6".to_owned()
        });
    }
    hints
}

fn diagnostic_summary(ready: bool, linked_mock: bool, missing: &[String]) -> String {
    if ready {
        return "OCCT runtime libraries are loaded. The authoritative kernel is ready.".to_owned();
    }
    if linked_mock {
        let gap = missing_clause(missing);
        return format!(
            "The linked occt_bridge is not the native OCCT backend. The authoritative kernel is not ready.{gap}"
        );
    }
    if missing.is_empty() {
        return "OCCT runtime files are present on disk, but this process has not loaded the native bridge. The authoritative kernel is not ready.".to_owned();
    }
    format!(
        "OCCT runtime libraries are not loaded{}. The authoritative kernel is not ready.",
        missing_clause(missing)
    )
}

fn missing_clause(missing: &[String]) -> String {
    if missing.is_empty() {
        return String::new();
    }
    let shown = missing
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let extra = if missing.len() > 3 {
        format!(" +{} more", missing.len() - 3)
    } else {
        String::new()
    };
    format!(" ({shown}{extra})")
}

fn discover_repo_root() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
    }
    starts.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    starts.into_iter().find_map(|start| {
        let mut current = Some(start);
        while let Some(dir) = current {
            if dir.join("cpp").join("occt-bridge").is_dir()
                && dir.join("crates").join("cad-kernel-occt").is_dir()
            {
                return Some(dir);
            }
            current = dir.parent().map(Path::to_path_buf);
        }
        None
    })
}

/// Dynamic-library load status for an upper-level health check.
///
/// `authoritative_ready` is true only when this process has loaded the native
/// OCCT bridge. A checkout that merely contains DLLs, a mock bridge, or the
/// analytic fallback is never reported as ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcctRuntimeDiagnostic {
    /// Native OCCT bridge is loaded in this process.
    pub libraries_loaded: bool,
    /// Authoritative geometry is available. Equal to `libraries_loaded`.
    pub authoritative_ready: bool,
    /// Runtime files that are absent, not dynamic libraries, or older than OCCT 7.9.
    pub missing_libraries: Vec<String>,
    /// Copy-pasteable commands that replace the missing files. Empty when nothing is missing.
    pub fix_hints: Vec<String>,
    /// One short health-check sentence. Says the authoritative kernel is ready only when it is.
    pub summary: String,
}

/// Diagnose OCCT dynamic libraries for the checkout that contains this process.
pub fn diagnose_occt_runtime() -> OcctRuntimeDiagnostic {
    diagnose_runtime(discover_repo_root().as_deref(), true)
}

/// Probe `root` only. Ignores `OCCT_BRIDGE_LIB_DIR` so callers can inspect an explicit checkout.
pub fn diagnose_occt_runtime_at(root: &Path) -> OcctRuntimeDiagnostic {
    diagnose_runtime(Some(root), false)
}

fn diagnose_runtime(root: Option<&Path>, honor_env: bool) -> OcctRuntimeDiagnostic {
    let loaded = native_occt_bridge_loaded();
    let probe = probe_runtime(root, honor_env);
    let summary = diagnostic_summary(loaded, bridge_linked_without_native(), &probe.missing);
    OcctRuntimeDiagnostic {
        libraries_loaded: loaded,
        authoritative_ready: loaded,
        missing_libraries: probe.missing.clone(),
        fix_hints: fix_hints(root, &probe),
        summary,
    }
}
