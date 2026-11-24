fn main() {
    println!("cargo:rerun-if-env-changed=OCCT_BRIDGE_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_OCCT_FFI").is_some() {
        if let Some(dir) = std::env::var_os("OCCT_BRIDGE_LIB_DIR") {
            println!(
                "cargo:rustc-link-search=native={}",
                std::path::Path::new(&dir).display()
            );
        }
        println!("cargo:rustc-link-lib=dylib=occt_bridge");
    }
}
