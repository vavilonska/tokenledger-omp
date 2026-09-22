fn main() {
    // Tauri links its resource file only to binaries. Library unit-test executables
    // also import TaskDialogIndirect and need the Common Controls v6 activation
    // context before Rust's test harness starts. Keep one shared manifest for all
    // executable targets, while Tauri still supplies binary icons/version info.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("Tauri build setup failed");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=windows-manifest.rc");
        println!("cargo:rerun-if-changed=windows-manifest.xml");
        // Use unrestricted rustc-link-arg so library unit-test executables are
        // covered as well as binaries and integration tests.
        embed_resource::compile_for_everything("windows-manifest.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Windows Common Controls manifest compilation failed");
    }
}
