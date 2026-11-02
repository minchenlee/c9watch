//! The engine's lifetime lock, `<home>/engine.lock`.
//!
//! The engine takes an exclusive flock on it at start and holds it until the
//! process exits, so at most one engine runs per home. Ported from
//! the legacy PM daemon's locking helpers, with two
//! changes: the lock file is opened with `O_NOFOLLOW`, and the holder writes
//! its pid at once (readiness is a successful `hello`, not a file mark).

use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// Error code returned when another engine already holds `engine.lock`.
pub const ENGINE_ALREADY_RUNNING: &str = "engine_already_running";

/// How long `acquire_engine_lock` retries a busy lock. Clients probe the
/// lock with a brief `LOCK_SH`, so a fresh engine must not give up on the
/// first `EWOULDBLOCK`; a live engine holds it forever, so this only delays
/// the second engine's error.
const LOCK_RETRY: Duration = Duration::from_secs(1);

/// The held lock. Dropping it releases the lock for every copy of the fd.
#[derive(Debug)]
pub struct EngineLock(File);

impl AsRawFd for EngineLock {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0.as_raw_fd()
    }
}

impl Drop for EngineLock {
    fn drop(&mut self) {
        // A flock belongs to the open file description, which a child forked
        // by another thread shares until it execs. `LOCK_UN` releases it for
        // every copy; a plain close would not.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Open `path` as a 0600 regular file without following a symlink.
fn open_lock_file(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => format!("{:?} is a symlink, refusing to use it", path),
            _ => format!("Failed to open {:?}: {}", path, e),
        })?;
    let md = file
        .metadata()
        .map_err(|e| format!("Failed to stat {:?}: {}", path, e))?;
    if !md.is_file() {
        return Err(format!("{:?} is not a regular file", path));
    }
    if md.mode() & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", path, e))?;
    }
    Ok(file)
}

/// Take the exclusive lifetime lock on `path` and write our pid into it.
/// Fails with `engine_already_running` when another engine holds it. Keep
/// the returned lock alive for the life of the engine.
pub fn acquire_engine_lock(path: &Path) -> Result<EngineLock, String> {
    acquire_with_retry(path, LOCK_RETRY)
}

fn acquire_with_retry(path: &Path, retry: Duration) -> Result<EngineLock, String> {
    let file = open_lock_file(path)?;
    let deadline = Instant::now() + retry;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("Failed to lock {:?}: {}", path, err));
        }
        if Instant::now() >= deadline {
            return Err(ENGINE_ALREADY_RUNNING.to_string());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let lock = EngineLock(file);
    let mut f = &lock.0;
    f.set_len(0)
        .and_then(|_| f.seek(std::io::SeekFrom::Start(0)))
        .and_then(|_| f.write_all(std::process::id().to_string().as_bytes()))
        .map_err(|e| format!("Failed to write pid into {:?}: {}", path, e))?;
    Ok(lock)
}

/// True when some process holds the exclusive lock on `path` (an engine is
/// alive). The probe takes `LOCK_SH|LOCK_NB` and releases it before
/// returning, so an engine spawned right after can take its `LOCK_EX`.
pub fn engine_lock_held(path: &Path) -> Result<bool, String> {
    let file = open_lock_file(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return Ok(false);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(true)
    } else {
        Err(format!("Failed to probe lock {:?}: {}", path, err))
    }
}

/// The pid of the engine that holds `path`, or `None` when no engine holds
/// it. A pid left behind by an engine that exited does not count.
pub fn engine_pid(path: &Path) -> Result<Option<libc::pid_t>, String> {
    if !engine_lock_held(path)? {
        return Ok(None);
    }
    Ok(std::fs::read_to_string(path)
        .ok()
        .and_then(|c| c.trim().parse::<libc::pid_t>().ok())
        .filter(|pid| *pid > 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_engine_cannot_take_the_lock() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.lock");
        let first = acquire_with_retry(&path, Duration::ZERO).unwrap();
        assert!(engine_lock_held(&path).unwrap());

        let err = acquire_with_retry(&path, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err, ENGINE_ALREADY_RUNNING);

        drop(first);
        assert!(!engine_lock_held(&path).unwrap());
        acquire_with_retry(&path, Duration::ZERO).unwrap();
    }

    #[test]
    fn holder_pid_is_readable_only_while_held() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.lock");
        assert_eq!(engine_pid(&path).unwrap(), None);
        let lock = acquire_engine_lock(&path).unwrap();
        assert_eq!(
            engine_pid(&path).unwrap(),
            Some(std::process::id() as libc::pid_t)
        );
        drop(lock);
        // The pid is still in the file, but nobody holds the lock.
        assert_eq!(engine_pid(&path).unwrap(), None);
    }

    #[test]
    fn lock_file_is_0600_and_not_a_symlink() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("engine.lock");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _lock = acquire_engine_lock(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let target = t.path().join("target");
        let link = t.path().join("link.lock");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = acquire_engine_lock(&link).unwrap_err();
        assert!(err.contains("symlink"), "{}", err);
        assert!(!target.exists());
    }
}
