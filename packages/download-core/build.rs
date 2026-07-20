/// Build script for Download Core cdylib.
///
/// Generates C# bindings for the WinUI shell via csbindgen
/// and builds the cdylib for Windows x64/ARM64.
///
/// ## Prerequisites
///
/// ```ignore
/// cargo install csbindgen
/// ```
///
/// ## Usage
///
/// ```ignore
/// cargo build --release --target x86_64-pc-windows-msvc
/// ```

fn main() {
    // Tell Cargo to re-run if the cabi source changes
    println!("cargo:rerun-if-changed=src/cabi.rs");

    // Generate C# bindings with csbindgen
    #[cfg(feature = "generate-bindings")]
    {
        let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let output_path = std::path::Path::new(&crate_dir)
            .parent()
            .unwrap()
            .join("winui-shell")
            .join("Phlegethon")
            .join("bindings")
            .join("CoreBindings.g.cs");

        csbindgen::Builder::default()
            .input_extern_file("src/cabi.rs")
            .csharp_dll_name("download_core")
            .generate_csharp_file(&output_path)
            .expect("csbindgen generation failed");
    }
}
