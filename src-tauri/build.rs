fn main() {
    // On Windows, tauri-build embeds the Common-Controls v6 manifest into the app
    // binary only. The unit-test binary also imports TaskDialogIndirect (comctl32 v6,
    // through tauri-plugin-dialog) and, without a manifest, fails to start with
    // STATUS_ENTRYPOINT_NOT_FOUND. So we embed the same manifest into every target
    // ourselves and tell tauri-build not to add its own.
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

    if windows_msvc {
        let manifest =
            std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());

        let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
            .expect("failed to run tauri-build");
    } else {
        tauri_build::build()
    }
}
