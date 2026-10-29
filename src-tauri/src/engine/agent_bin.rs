//! The engine's own copy of `c9watch-agent` (C1a-21, D28 Q6=B).
//!
//! An agent proves who it is with two facts the engine reads itself: its
//! ancestry reaches the PTY child of a live generation (`engine::agents`),
//! and the peer runs `<home>/bin/c9watch-agent`, the copy the engine made at
//! boot, with the sha256 it had then. No secret goes into the agent's env.
//!
//! - The source is always `c9watch-agent` next to `realpath(current_exe())`
//!   (`target/debug` in dev, `target/release` for the dogfood build). There
//!   is no env override. Tests pass another source through
//!   `run::EngineConfig`.
//! - At boot the engine copies it to `<home>/bin/c9watch-agent` (a 0700
//!   directory), mode 0555, through a temp file and a rename, and records
//!   the sha256 and the inode.
//! - A missing source or a failed copy does not stop the engine. It runs
//!   without agents: `check` answers `agent_bin_missing`, and no peer
//!   passes `check_peer_exe`.
//! - `check(Rehash::Always)` rereads the file and runs before every spawn.
//!   The peer check on hook-class calls rehashes only when the file's
//!   metadata changed since the last good hash.
//!
//! The engine compares realpaths: a binary under `/tmp` runs as
//! `/private/tmp/...` on macOS (D24 補充).

use super::fsutil::ensure_private_dir;
use super::proto::{ApiError, ErrorCode};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// File name of the agent binary, next to `c9watch` and in `<home>/bin`.
pub const AGENT_BIN_NAME: &str = "c9watch-agent";

/// `<home>/bin`.
pub fn bin_dir(home: &Path) -> PathBuf {
    home.join("bin")
}

/// `<home>/bin/c9watch-agent`: the only agent binary a spawn profile names.
pub fn installed_path(home: &Path) -> PathBuf {
    bin_dir(home).join(AGENT_BIN_NAME)
}

/// `c9watch-agent` in the directory of `realpath(exe)`.
pub fn source_next_to(exe: &Path) -> Result<PathBuf, String> {
    let real = exe
        .canonicalize()
        .map_err(|e| format!("Failed to resolve {:?}: {}", exe, e))?;
    let dir = real
        .parent()
        .ok_or_else(|| format!("{:?} has no parent directory", real))?;
    Ok(dir.join(AGENT_BIN_NAME))
}

/// `c9watch-agent` next to the running binary.
pub fn default_source() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current exe path: {}", e))?;
    source_next_to(&exe)
}

/// When `check` reads the whole file again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rehash {
    /// Before a spawn.
    Always,
    /// On a peer check: only when the metadata changed since the last good
    /// hash.
    IfChanged,
}

/// Metadata that changes whenever the file's content can have changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stamp {
    fn of(m: &std::fs::Metadata) -> Self {
        Stamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        }
    }
}

#[derive(Debug)]
enum State {
    /// No usable copy, and why.
    Missing(String),
    Installed {
        sha: [u8; 32],
        dev: u64,
        ino: u64,
        /// The metadata at the last hash that matched.
        verified: Option<Stamp>,
    },
}

/// The engine's copy and what the engine recorded about it.
#[derive(Debug)]
pub struct AgentBin {
    /// `<home>/bin/c9watch-agent`, as a realpath once installed.
    path: PathBuf,
    state: Mutex<State>,
}

impl AgentBin {
    /// Copy `source` to `<home>/bin/c9watch-agent`. Never fails: without a
    /// usable copy the result answers `agent_bin_missing`.
    pub fn install(home: &Path, source: &Path) -> AgentBin {
        let path = installed_path(home);
        match copy_in(home, source) {
            Ok((sha, dev, ino)) => {
                // `home` may sit below a symlink like /tmp; peers report
                // the real path.
                let path = path.canonicalize().unwrap_or(path);
                // The copy was just hashed as it was written, so the first
                // hook does not pay for a full rehash within its 0.5 s.
                let verified = std::fs::symlink_metadata(&path)
                    .ok()
                    .filter(|m| m.dev() == dev && m.ino() == ino)
                    .map(|m| Stamp::of(&m));
                AgentBin {
                    path,
                    state: Mutex::new(State::Installed {
                        sha,
                        dev,
                        ino,
                        verified,
                    }),
                }
            }
            Err(e) => AgentBin {
                path,
                state: Mutex::new(State::Missing(e)),
            },
        }
    }

    /// Where the copy is (a realpath once installed).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The recorded sha256 as hex, or `None` without a copy.
    pub fn sha256_hex(&self) -> Option<String> {
        match &*self.lock() {
            State::Installed { sha, .. } => Some(hex(sha)),
            State::Missing(_) => None,
        }
    }

    /// Why there is no copy, if there is none.
    pub fn missing_reason(&self) -> Option<String> {
        match &*self.lock() {
            State::Missing(why) => Some(why.clone()),
            State::Installed { .. } => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The copy is still the file the engine installed, with the same
    /// sha256. `agent_bin_missing` without a copy, `agent_bin_mismatch` when
    /// it changed. Blocking (it may read the whole file).
    pub fn check(&self, rehash: Rehash) -> Result<(), ApiError> {
        let mut state = self.lock();
        let (sha, dev, ino, verified) = match &mut *state {
            State::Missing(why) => {
                return Err(ApiError::new(ErrorCode::AgentBinMissing, why.clone()))
            }
            State::Installed {
                sha,
                dev,
                ino,
                verified,
            } => (*sha, *dev, *ino, verified),
        };
        let meta = match std::fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ApiError::new(
                    ErrorCode::AgentBinMissing,
                    format!("{:?} is gone", self.path),
                ))
            }
            Err(e) => return Err(mismatch(format!("Failed to stat {:?}: {}", self.path, e))),
        };
        if !meta.file_type().is_file() {
            return Err(mismatch(format!("{:?} is not a regular file", self.path)));
        }
        if meta.dev() != dev || meta.ino() != ino {
            return Err(mismatch(format!(
                "{:?} was replaced (inode {} is now {})",
                self.path,
                ino,
                meta.ino()
            )));
        }
        let stamp = Stamp::of(&meta);
        if rehash == Rehash::IfChanged && *verified == Some(stamp) {
            return Ok(());
        }
        let now = sha256_file(&self.path).map_err(mismatch)?;
        if now != sha {
            *verified = None;
            return Err(mismatch(format!(
                "{:?} has sha256 {}, the engine installed {}",
                self.path,
                hex(&now),
                hex(&sha)
            )));
        }
        *verified = Some(stamp);
        Ok(())
    }

    /// The peer runs this copy: the realpath of `exe` is the copy's path,
    /// and the copy is unchanged. Anything else is `peer_not_authorized`.
    pub fn check_peer_exe(&self, exe: &Path) -> Result<(), ApiError> {
        let real = exe.canonicalize().map_err(|e| {
            not_authorized(format!("the peer exe {:?} does not resolve: {}", exe, e))
        })?;
        if real != self.path {
            return Err(not_authorized(format!(
                "the peer runs {:?}, not the engine's {:?}",
                real, self.path
            )));
        }
        self.check(Rehash::IfChanged)
            .map_err(|e| not_authorized(format!("{}: {}", e.code, e.msg)))
    }
}

fn mismatch(msg: String) -> ApiError {
    ApiError::new(ErrorCode::AgentBinMismatch, msg)
}

fn not_authorized(msg: String) -> ApiError {
    ApiError::new(ErrorCode::PeerNotAuthorized, msg)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect()
}

fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let mut f = File::open(path).map_err(|e| format!("Failed to open {:?}: {}", path, e))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("Failed to read {:?}: {}", path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// Copy `source` into `<home>/bin` as a 0555 file. Returns the sha256 of
/// the bytes written and the new file's device and inode.
fn copy_in(home: &Path, source: &Path) -> Result<([u8; 32], u64, u64), String> {
    let bytes = std::fs::read(source)
        .map_err(|e| format!("no {} at {:?}: {}", AGENT_BIN_NAME, source, e))?;
    let dir = bin_dir(home);
    ensure_private_dir(&dir)?;
    let dest = installed_path(home);
    let tmp = dir.join(format!(".{}.{}.tmp", AGENT_BIN_NAME, std::process::id()));
    // A temp file left by a crashed engine with our pid is ours to drop.
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|e| format!("Failed to create {:?}: {}", tmp, e))?;
        f.write_all(&bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("Failed to write {:?}: {}", tmp, e))?;
        f.set_permissions(std::fs::Permissions::from_mode(0o555))
            .map_err(|e| format!("Failed to chmod {:?}: {}", tmp, e))?;
        std::fs::rename(&tmp, &dest)
            .map_err(|e| format!("Failed to move {:?} to {:?}: {}", tmp, dest, e))
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let meta = std::fs::symlink_metadata(&dest)
        .map_err(|e| format!("Failed to stat {:?}: {}", dest, e))?;
    let sha: [u8; 32] = Sha256::digest(&bytes).into();
    Ok((sha, meta.dev(), meta.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp engine home and a fake agent binary in another directory.
    fn setup(content: &[u8]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("engine");
        let src_dir = t.path().join("target");
        std::fs::create_dir(&src_dir).unwrap();
        let source = src_dir.join(AGENT_BIN_NAME);
        std::fs::write(&source, content).unwrap();
        (t, home, source)
    }

    #[test]
    fn install_copies_the_source_0555_and_records_its_sha() {
        let (_t, home, source) = setup(b"agent v1");
        let bin = AgentBin::install(&home, &source);
        let dest = installed_path(&home);
        assert_eq!(bin.path(), dest.canonicalize().unwrap());
        assert_eq!(std::fs::read(&dest).unwrap(), b"agent v1");
        let meta = std::fs::metadata(&dest).unwrap();
        assert_eq!(meta.permissions().mode() & 0o7777, 0o555);
        let dir = std::fs::metadata(bin_dir(&home)).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700);
        assert_eq!(bin.sha256_hex().unwrap(), hex(&Sha256::digest(b"agent v1")));
        bin.check(Rehash::Always).unwrap();
        bin.check(Rehash::IfChanged).unwrap();
        // No temp file is left behind.
        let names: Vec<_> = std::fs::read_dir(bin_dir(&home))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(AGENT_BIN_NAME)]);
    }

    #[test]
    fn a_second_install_replaces_the_copy() {
        let (_t, home, source) = setup(b"agent v1");
        AgentBin::install(&home, &source);
        std::fs::write(&source, b"agent v2").unwrap();
        let bin = AgentBin::install(&home, &source);
        assert_eq!(std::fs::read(installed_path(&home)).unwrap(), b"agent v2");
        bin.check(Rehash::Always).unwrap();
    }

    #[test]
    fn a_missing_source_is_agent_bin_missing() {
        let (_t, home, source) = setup(b"x");
        std::fs::remove_file(&source).unwrap();
        let bin = AgentBin::install(&home, &source);
        let e = bin.check(Rehash::Always).unwrap_err();
        assert_eq!(e.code, ErrorCode::AgentBinMissing);
        assert!(e.msg.contains(AGENT_BIN_NAME), "{}", e.msg);
        assert!(bin.missing_reason().is_some());
        assert!(bin.sha256_hex().is_none());
        // No peer passes, whatever it runs.
        let e = bin.check_peer_exe(&source).unwrap_err();
        assert_eq!(e.code, ErrorCode::PeerNotAuthorized);
    }

    #[test]
    fn a_changed_copy_is_refused() {
        let (_t, home, source) = setup(b"agent v1");
        let bin = AgentBin::install(&home, &source);
        let dest = installed_path(&home);
        bin.check_peer_exe(&dest).unwrap();
        // Same inode, other bytes.
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&dest, b"agent v1 + something").unwrap();
        let e = bin.check(Rehash::IfChanged).unwrap_err();
        assert_eq!(e.code, ErrorCode::AgentBinMismatch);
        let e = bin.check(Rehash::Always).unwrap_err();
        assert_eq!(e.code, ErrorCode::AgentBinMismatch);
        let e = bin.check_peer_exe(&dest).unwrap_err();
        assert_eq!(e.code, ErrorCode::PeerNotAuthorized);
        assert!(e.msg.contains("agent_bin_mismatch"), "{}", e.msg);
    }

    #[test]
    fn a_replaced_or_removed_copy_is_refused() {
        let (_t, home, source) = setup(b"agent v1");
        let bin = AgentBin::install(&home, &source);
        let dest = installed_path(&home);
        // Same bytes, new inode.
        let other = bin_dir(&home).join("other");
        std::fs::write(&other, b"agent v1").unwrap();
        std::fs::rename(&other, &dest).unwrap();
        let e = bin.check(Rehash::IfChanged).unwrap_err();
        assert_eq!(e.code, ErrorCode::AgentBinMismatch);
        std::fs::remove_file(&dest).unwrap();
        let e = bin.check(Rehash::Always).unwrap_err();
        assert_eq!(e.code, ErrorCode::AgentBinMissing);
    }

    #[test]
    fn a_peer_running_another_file_is_not_authorized() {
        let (_t, home, source) = setup(b"agent v1");
        let bin = AgentBin::install(&home, &source);
        // The source has the same bytes but is not the engine's copy.
        let e = bin.check_peer_exe(&source).unwrap_err();
        assert_eq!(e.code, ErrorCode::PeerNotAuthorized);
        // A symlink to the copy resolves to it.
        let link = home.join("link");
        std::os::unix::fs::symlink(installed_path(&home), &link).unwrap();
        bin.check_peer_exe(&link).unwrap();
    }

    #[test]
    fn the_source_is_next_to_the_realpath_of_the_exe() {
        let t = tempfile::tempdir().unwrap();
        let real_dir = t.path().join("release");
        std::fs::create_dir(&real_dir).unwrap();
        let exe = real_dir.join("c9watch");
        std::fs::write(&exe, b"").unwrap();
        // ~/.local/bin/c9watch -> target/release/c9watch (dogfood).
        let link_dir = t.path().join("bin");
        std::fs::create_dir(&link_dir).unwrap();
        let link = link_dir.join("c9watch");
        std::os::unix::fs::symlink(&exe, &link).unwrap();
        assert_eq!(
            source_next_to(&link).unwrap(),
            real_dir.canonicalize().unwrap().join(AGENT_BIN_NAME)
        );
        assert!(source_next_to(&t.path().join("nope")).is_err());
    }
}
