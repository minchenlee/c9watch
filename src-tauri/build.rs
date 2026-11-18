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
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "c9p_request",
            "c9p_subscribe",
            "c9p_unsubscribe",
            "get_sessions",
            "get_detection_diagnostics",
            "get_subscription_usage",
            "get_conversation",
            "get_away_activity",
            "get_session_history",
            "deep_search_sessions",
            "get_cost_data",
            "get_memory_files",
            "get_subagents",
            "get_subagent_transcript",
            "get_session_tasks",
            "save_temp_image",
            "reveal_in_file_manager",
            "stop_session",
            "open_session",
            "rename_session",
            "show_main_window",
            "get_server_info",
            "create_pairing_code",
            "forget_paired_clients",
            "get_remote_access",
            "set_remote_access",
            "get_debug_logs",
            "get_build_channel",
            "opencode_connection_status",
            "opencode_connect",
            "get_notification_preferences",
            "save_notification_preferences",
            "test_native_notification",
        ]),
    ))
    .expect("Tauri ACL build failed");
}
