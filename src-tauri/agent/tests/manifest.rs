//! The package layout the CI jobs and release scripts rely on (D32).
//!
//! This test also makes `cargo test` build the plain `c9watch-agent`
//! binary, which `tests/engine_agent.rs` in the GUI package runs.

#![cfg(feature = "engine")]

/// `cargo run` picks the main binary (`default-run`), and the agent binary
/// exists only with the engine (`required-features`). The Linux CI job also
/// runs `cargo run ... -- --version` and builds `c9watch-agent` by name, so
/// all three packages stay default members of the workspace.
#[test]
fn the_manifest_keeps_c9watch_the_default_binary() {
    let root = include_str!("../../Cargo.toml");
    assert!(root.contains("\ndefault-run = \"c9watch\"\n"));
    assert!(root.contains("\ndefault-members = [\".\", \"core\", \"agent\"]\n"));
    let manifest = include_str!("../Cargo.toml");
    let bin = manifest.split("[[bin]]").nth(1).unwrap();
    assert!(bin.contains("name = \"c9watch-agent\""));
    assert!(bin.contains("required-features = [\"engine\"]"));
}

#[test]
fn the_agent_binary_runs() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_c9watch-agent"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", out);
    let version = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        version.trim(),
        format!("c9watch-agent {}", env!("CARGO_PKG_VERSION"))
    );
}
