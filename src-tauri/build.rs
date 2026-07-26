fn main() {
    // Ensure build/ exists so rust-embed's derive macro doesn't fail
    // during `cargo check` when the frontend hasn't been built yet.
    let build_dir = std::path::Path::new("../build");
    if !build_dir.exists() {
        std::fs::create_dir_all(build_dir).ok();
    }

    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/macos_now_playing.m")
            .flag("-fobjc-arc")
            .compile("c9watch_now_playing");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=MediaPlayer");
    }

    // Only run tauri_build when the gui feature is enabled.
    // CLI-only builds don't need Tauri's code generation.
    #[cfg(feature = "gui")]
    tauri_build::build();
}
