fn main() {
    // Ensure build/ exists so rust-embed's derive macro doesn't fail
    // during `cargo check` when the frontend hasn't been built yet.
    let build_dir = std::path::Path::new("../build");
    if !build_dir.exists() {
        std::fs::create_dir_all(build_dir).ok();
    }

    // Build channel: every build that is not a release (`C9WATCH_CHANNEL=prod`,
    // set by release.yml) gets `cfg(c9_channel_dev)`, so dev-only code paths
    // never reach a shipped binary.
    println!("cargo:rustc-check-cfg=cfg(c9_channel_dev)");
    println!("cargo:rerun-if-env-changed=C9WATCH_CHANNEL");
    if std::env::var("C9WATCH_CHANNEL").as_deref() != Ok("prod") {
        println!("cargo:rustc-cfg=c9_channel_dev");
    }

    // Only run tauri_build when the gui feature is enabled.
    // CLI-only builds don't need Tauri's code generation.
    #[cfg(feature = "gui")]
    tauri_build::build();
}
