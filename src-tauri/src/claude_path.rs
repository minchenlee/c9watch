//! Makes the `claude` CLI reachable through `PATH` for every spawn c9watch makes.
//!
//! A macOS app started from Finder, the Dock or a login item inherits launchd's
//! minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which contains none of the
//! places Claude Code installs to. Without this, the `claude agents --json`
//! probe fails and c9watch silently falls back to the legacy process scanner.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CLAUDE_EXE: &str = if cfg!(windows) {
    "claude.exe"
} else {
    "claude"
};

/// Adds the first usable well-known Claude Code install directory to `PATH`
/// when `claude` cannot already run through it.
///
/// Must run before any other thread starts, since it mutates the process
/// environment.
pub fn ensure_claude_on_path() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let current = std::env::var_os("PATH").unwrap_or_default();
    if let Some(updated) = augmented_path(&current, &candidate_dirs(&home)) {
        std::env::set_var("PATH", updated);
    }
}

/// Install locations in the order Claude Code's own installers use them:
/// native installer, legacy local install, Homebrew (Apple silicon, Intel).
fn candidate_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local").join("bin"),
        home.join(".claude").join("local"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]
}

fn augmented_path(current: &OsStr, candidates: &[PathBuf]) -> Option<OsString> {
    let dirs: Vec<PathBuf> = std::env::split_paths(current).collect();
    let inherited_claude = dirs.iter().any(|d| d.join(CLAUDE_EXE).is_file());
    if inherited_claude && can_run_claude(Path::new(CLAUDE_EXE), current) {
        return None;
    }
    for candidate in candidates {
        if !candidate.join(CLAUDE_EXE).is_file() {
            continue;
        }
        let mut updated = dirs.clone();
        if inherited_claude {
            // An executable but broken inherited shim would shadow an appended CLI.
            updated.insert(0, candidate.clone());
        } else {
            updated.push(candidate.clone());
        }
        let path = std::env::join_paths(updated).ok()?;
        if can_run_claude(&candidate.join(CLAUDE_EXE), &path) {
            return Some(path);
        }
    }
    None
}

fn can_run_claude(exe: &Path, path: &OsStr) -> bool {
    let Ok(mut child) = Command::new(exe)
        .arg("--version")
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // A stale shim must not hang startup indefinitely.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    struct Installations {
        _root: TempDir,
        inherited: PathBuf,
        candidates: Vec<PathBuf>,
    }

    impl Installations {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let inherited = root.path().join("inherited");
            let candidates = vec![
                root.path().join("home/.local/bin"),
                root.path().join("home/.claude/local"),
                root.path().join("opt/homebrew/bin"),
                root.path().join("usr/local/bin"),
            ];
            for dir in std::iter::once(&inherited).chain(&candidates) {
                fs::create_dir_all(dir).unwrap();
            }
            Self {
                _root: root,
                inherited,
                candidates,
            }
        }

        fn path(&self) -> OsString {
            std::env::join_paths([&self.inherited]).unwrap()
        }

        fn claude(dir: &Path, script: &str, mode: u32) {
            let exe = dir.join(CLAUDE_EXE);
            fs::write(&exe, script).unwrap();
            fs::set_permissions(exe, fs::Permissions::from_mode(mode)).unwrap();
        }

        fn working(dir: &Path) {
            Self::claude(dir, "#!/bin/sh\nprintf 'fixture-claude\\n'\n", 0o755);
        }

        fn assert_resolves_to(&self, dir: &Path) {
            let path = augmented_path(&self.path(), &self.candidates).unwrap();
            assert_eq!(
                std::env::split_paths(&path)
                    .filter(|p| p != &self.inherited)
                    .collect::<Vec<_>>(),
                vec![dir.to_path_buf()]
            );
            // Exercise the same bare-name lookup the later CLI probe uses.
            let output = Command::new(CLAUDE_EXE)
                .arg("--version")
                .env("PATH", path)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"fixture-claude\n");
        }
    }

    #[test]
    fn leaves_path_alone_when_claude_is_reachable() {
        let installs = Installations::new();
        Installations::working(&installs.inherited);
        Installations::working(&installs.candidates[0]);
        assert_eq!(augmented_path(&installs.path(), &installs.candidates), None);
    }

    #[test]
    fn appends_native_install_dir_to_launchd_path() {
        let installs = Installations::new();
        Installations::working(&installs.candidates[0]);
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[0]);
    }

    #[test]
    fn falls_back_to_homebrew_when_no_home_install() {
        let installs = Installations::new();
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }

    #[test]
    fn no_change_when_claude_is_not_installed_anywhere() {
        let installs = Installations::new();
        assert_eq!(augmented_path(&installs.path(), &installs.candidates), None);
    }

    #[test]
    fn skips_non_executable_candidate() {
        let installs = Installations::new();
        Installations::claude(&installs.candidates[0], "#!/bin/sh\nexit 0\n", 0o644);
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }

    #[test]
    fn skips_candidate_with_missing_interpreter() {
        let installs = Installations::new();
        // This PATH contains only fixture directories, so no installed Node can help.
        Installations::claude(&installs.candidates[0], "#!/usr/bin/env node\n", 0o755);
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }

    #[test]
    fn skips_unusable_inherited_shim_without_leaving_it_to_shadow_fallback() {
        let installs = Installations::new();
        Installations::claude(&installs.inherited, "#!/usr/bin/env node\n", 0o755);
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }

    #[test]
    fn skips_non_executable_inherited_file() {
        let installs = Installations::new();
        Installations::claude(&installs.inherited, "#!/bin/sh\nexit 0\n", 0o644);
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }

    #[test]
    fn probes_with_candidate_directory_on_path() {
        let installs = Installations::new();
        Installations::claude(&installs.candidates[0], "#!/usr/bin/env node\n", 0o755);
        let node = installs.candidates[0].join("node");
        fs::write(&node, "#!/bin/sh\nprintf 'fixture-claude\\n'\n").unwrap();
        fs::set_permissions(node, fs::Permissions::from_mode(0o755)).unwrap();
        installs.assert_resolves_to(&installs.candidates[0]);
    }

    #[test]
    fn skips_candidate_that_hangs_during_version_probe() {
        let installs = Installations::new();
        Installations::claude(
            &installs.candidates[0],
            "#!/bin/sh\nwhile :; do :; done\n",
            0o755,
        );
        Installations::working(&installs.candidates[2]);
        installs.assert_resolves_to(&installs.candidates[2]);
    }
}
