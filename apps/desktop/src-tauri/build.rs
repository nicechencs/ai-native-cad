#[path = "src/windows_manifest.rs"]
mod windows_manifest;

fn main() {
    tauri_build::build();
    // tauri-build embeds the Common Controls v6 manifest only in application
    // binaries. Test executables still import TaskDialogIndirect, and the
    // loader exits 0xc0000139 before any test runs when that entry point is
    // resolved from comctl32 5.82. A second copy of resource.lib duplicates
    // the VERSION resource, so Windows tests get the dependency through the
    // linker instead. Application binaries already carry the same dependency.
    // Linux `cc` treats the MSVC switch as an input file.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if let Some(arg) = windows_manifest::windows_manifest_link_arg(&target_os) {
        println!("cargo:rustc-link-arg={arg}");
    }
}
