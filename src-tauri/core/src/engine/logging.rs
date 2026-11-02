//! Engine log lines and `engine.log` rotation (D10).
//!
//! Where the log goes: a client that auto-starts the engine points the
//! engine's stdout and stderr at `<home>/engine.log`
//! (`engine_proto::spawn::log_path`, opened append-only). The engine never
//! opens the log for its own writes; it writes to stderr.
//!
//! Line format: `elog!` (`crate::log_line`) writes one line per record,
//!
//! ```text
//! 2026-11-01T08:15:02.123Z INFO run: Listening on "/…/engine.sock"
//! ```
//!
//! that is an RFC 3339 UTC timestamp with milliseconds, the level, the
//! target (the module that logs) and the message, with newlines in the
//! message escaped, so one record is always one line. Lines that something
//! other than `elog!` writes to stderr (a panic message, a child's output)
//! stay plain text in the same file.
//!
//! Rotation is by size: `rotate_stderr_log` moves `engine.log` to
//! `engine.log.1` (and `.1` to `.2`, up to `LOG_KEEP`) once it passes
//! `LOG_MAX_BYTES`, puts a new `engine.log` (0600, no symlink, created
//! before anything is renamed) in its place and points fd 2 at it, and fd 1
//! too when fd 1 wrote to the same file. It does that only when fd 2 is
//! that very file (same device and inode), so an engine run in a terminal
//! or a test is never redirected. The engine checks after the socket is bound (a client that
//! is waiting for hello reads boot errors at an offset in the old file)
//! and then every minute.

use std::fs::OpenOptions;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// `engine.log` is rotated once it is larger than this.
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Rotated files kept (`engine.log.1` … `engine.log.N`), so the log takes
/// at most about `(LOG_KEEP + 1) * LOG_MAX_BYTES` on disk.
pub const LOG_KEEP: u32 = 3;

pub use crate::log_line::{format_line, write, Level};

/// Rotate `<home>/engine.log` if the engine's stderr writes to it and it
/// is over `LOG_MAX_BYTES`. Returns whether it rotated. Stderr (fd 2) is
/// the gate; stdout (fd 1) moves too only when it writes to the same file.
pub fn rotate_stderr_log(home: &Path) -> Result<bool, String> {
    let path = crate::engine_proto::spawn::log_path(home);
    rotate_fds(&path, 2, &[1], LOG_MAX_BYTES, LOG_KEEP)
}

/// `path` with `.n` appended.
fn numbered(path: &Path, n: u32) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(format!(".{}", n));
    PathBuf::from(s)
}

/// `path` with `.new` appended: the next log, before it takes the name.
fn staged(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".new");
    PathBuf::from(s)
}

/// (device, inode) of an open fd, when it is a regular file.
fn fd_file_id(fd: RawFd) -> Option<(u64, u64)> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return None;
    }
    if (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return None;
    }
    Some((st.st_dev as u64, st.st_ino as u64))
}

/// The rotation behind `rotate_stderr_log`. It runs only when `gate`
/// writes to `path` (same device and inode) and `path` is larger than
/// `max`. Then:
///
/// 1. Create the next log at `path.new` (0600, no symlink). A failure here
///    changes nothing.
/// 2. Shift the numbered files and move `path` to `path.1`.
/// 3. Move `path.new` to `path`; if that fails, move `path.1` back.
/// 4. `dup2` the new file onto `gate`, and onto each fd in `also` that
///    wrote to the same old file. Other fds are left alone.
///
/// Returns whether it rotated.
pub fn rotate_fds(
    path: &Path,
    gate: RawFd,
    also: &[RawFd],
    max: u64,
    keep: u32,
) -> Result<bool, String> {
    let Some(ours) = fd_file_id(gate) else {
        return Ok(false);
    };
    // Not following a symlink: a planted link is never rotated through.
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("Failed to stat {:?}: {}", path, e)),
    };
    if !meta.file_type().is_file() || (meta.dev(), meta.ino()) != ours || meta.len() <= max {
        return Ok(false);
    }

    let next = staged(path);
    // A file or link left by a rotation that failed midway; a directory
    // there is not ours to remove, and the create below then fails.
    if let Ok(m) = std::fs::symlink_metadata(&next) {
        if !m.is_dir() {
            let _ = std::fs::remove_file(&next);
        }
    }
    let new = OpenOptions::new()
        .create_new(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&next)
        .map_err(|e| format!("Failed to create the next log {:?}: {}", next, e))?;

    let keep = keep.max(1);
    for n in (1..keep).rev() {
        match std::fs::rename(numbered(path, n), numbered(path, n + 1)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let _ = std::fs::remove_file(&next);
                return Err(format!("Failed to rotate {:?}: {}", path, e));
            }
        }
    }
    let first = numbered(path, 1);
    if let Err(e) = std::fs::rename(path, &first) {
        let _ = std::fs::remove_file(&next);
        return Err(format!("Failed to rotate {:?}: {}", path, e));
    }
    if let Err(e) = std::fs::rename(&next, path) {
        let _ = std::fs::rename(&first, path);
        let _ = std::fs::remove_file(&next);
        return Err(format!("Failed to rotate {:?}: {}", path, e));
    }
    for &fd in std::iter::once(&gate).chain(also) {
        if fd_file_id(fd) == Some(ours) && unsafe { libc::dup2(new.as_raw_fd(), fd) } == -1 {
            return Err(format!(
                "Failed to point fd {} at {:?}: {}",
                fd,
                path,
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_line_has_timestamp_level_target_and_one_line_message() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-11-01T08:15:02.123Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let line = format_line(ts, Level::Warn, "db", format_args!("a\nb {}", 7));
        assert_eq!(line, "2026-11-01T08:15:02.123Z WARN db: a\\nb 7");
    }

    /// An fd for `path` opened like the spawner does (append), as a stand-in
    /// for the engine's stderr.
    fn open_append(path: &Path) -> std::fs::File {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    }

    #[test]
    fn rotates_past_the_cap_and_moves_the_fd_to_the_new_file() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.log");
        let mut f = open_append(&path);
        f.write_all(b"0123456789").unwrap();
        let fd = f.as_raw_fd();
        // At the cap: nothing happens.
        assert!(!rotate_fds(&path, fd, &[], 10, 2).unwrap());
        f.write_all(b"x").unwrap();
        assert!(rotate_fds(&path, fd, &[], 10, 2).unwrap());
        assert_eq!(std::fs::read(numbered(&path, 1)).unwrap(), b"0123456789x");
        // The fd now writes to the new, private file.
        f.write_all(b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        // Two more rotations with keep = 2: .1 shifts to .2, the oldest goes.
        f.write_all(b"-second-file").unwrap();
        assert!(rotate_fds(&path, fd, &[], 10, 2).unwrap());
        f.write_all(b"third-file!").unwrap();
        assert!(rotate_fds(&path, fd, &[], 10, 2).unwrap());
        assert_eq!(std::fs::read(numbered(&path, 1)).unwrap(), b"third-file!");
        assert_eq!(
            std::fs::read(numbered(&path, 2)).unwrap(),
            b"new-second-file"
        );
        assert!(!numbered(&path, 3).exists());
    }

    #[test]
    fn an_fd_that_is_not_the_log_is_left_alone() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.log");
        std::fs::write(&path, vec![b'a'; 100]).unwrap();
        let other = open_append(&t.path().join("other.log"));
        assert!(!rotate_fds(&path, other.as_raw_fd(), &[], 10, 2).unwrap());
        assert!(!numbered(&path, 1).exists());
        // A pipe (a terminal or test harness) is never rotated either.
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        assert!(!rotate_fds(&path, fds[1], &[], 10, 2).unwrap());
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn a_symlinked_log_is_not_rotated() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real.log");
        let f = open_append(&real);
        std::fs::write(&real, vec![b'a'; 100]).unwrap();
        let link = t.path().join("engine.log");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(!rotate_fds(&link, f.as_raw_fd(), &[], 10, 2).unwrap());
        assert!(real.exists());
    }

    #[test]
    fn a_failed_create_of_the_next_log_changes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.log");
        let mut f = open_append(&path);
        f.write_all(&[b'a'; 20]).unwrap();
        // A directory where the next log goes: the create fails.
        std::fs::create_dir(staged(&path)).unwrap();
        assert!(rotate_fds(&path, f.as_raw_fd(), &[], 10, 2).is_err());
        assert!(!numbered(&path, 1).exists());
        f.write_all(b"b").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap().len(),
            21,
            "still the live log"
        );
        // Once the path is free, rotation works again.
        std::fs::remove_dir(staged(&path)).unwrap();
        assert!(rotate_fds(&path, f.as_raw_fd(), &[], 10, 2).unwrap());
        assert_eq!(std::fs::read(numbered(&path, 1)).unwrap().len(), 21);
        assert!(!staged(&path).exists());
    }

    #[test]
    fn a_stale_next_log_file_is_replaced() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.log");
        let mut f = open_append(&path);
        f.write_all(&[b'a'; 20]).unwrap();
        std::fs::write(staged(&path), b"stale").unwrap();
        assert!(rotate_fds(&path, f.as_raw_fd(), &[], 10, 2).unwrap());
        f.write_all(b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert!(!staged(&path).exists());
    }

    #[test]
    fn the_gate_fd_decides_and_only_fds_on_the_same_file_move() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.log");
        let mut log_fd = open_append(&path);
        log_fd.write_all(&[b'a'; 20]).unwrap();
        let mut other = open_append(&t.path().join("other.log"));
        // The gate is not the log: no rotation, even though `also` is.
        assert!(!rotate_fds(&path, other.as_raw_fd(), &[log_fd.as_raw_fd()], 10, 2).unwrap());
        assert!(!numbered(&path, 1).exists());
        // The gate is the log: it moves; an `also` fd on another file does not.
        assert!(rotate_fds(&path, log_fd.as_raw_fd(), &[other.as_raw_fd()], 10, 2).unwrap());
        log_fd.write_all(b"new").unwrap();
        other.write_all(b"x").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read(t.path().join("other.log")).unwrap(), b"x");
    }
}
