/// Common Controls v6 dependency for Windows test binaries.
/// Application binaries already receive it from `tauri-build`.
/// Other targets must not see this MSVC switch; `cc` treats it as a file.
pub fn windows_manifest_link_arg(target_os: &str) -> Option<&'static str> {
    if target_os == "windows" {
        Some("/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'")
    } else {
        None
    }
}
