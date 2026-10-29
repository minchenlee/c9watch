use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

// ── Path helpers ──────────────────────────────────────────────────────

/// Validate a session id before it is used as a filesystem path component.
///
/// Accepts UUIDs and UUID-like strings (ASCII alphanumerics plus `-`), 1..=64
/// chars. Rejects anything that could traverse paths or escape the expected
/// directory (e.g. `..`, `/`, empty string, overlong input).
pub fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("invalid session id: empty".to_string());
    }
    if id.len() > 64 {
        return Err(format!("invalid session id length: {}", id.len()));
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!("invalid session id: {}", id));
    }
    Ok(())
}

/// Returns `~/.claude/c9watch/`
pub fn c9watch_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Failed to get home directory")?;
    Ok(home.join(".claude").join("c9watch"))
}

pub const DAEMON_SOCK_NAME: &str = "daemon.sock";
pub const DAEMON_PID_NAME: &str = "daemon.pid";
pub const DAEMON_LOCK_NAME: &str = "daemon.lock";

/// Returns `~/.claude/c9watch/daemon.sock`
pub fn daemon_sock_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join(DAEMON_SOCK_NAME))
}

/// Returns `~/.claude/c9watch/daemon.pid`
pub fn daemon_pid_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join(DAEMON_PID_NAME))
}

/// Returns `~/.claude/c9watch/daemon.lock`. The running daemon holds an
/// exclusive flock on it for its whole lifetime. It is separate from
/// `daemon.pid`, which clients lock to serialize spawning.
pub fn daemon_lock_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join(DAEMON_LOCK_NAME))
}

/// Returns `~/.claude/c9watch/daemon.log`
pub fn daemon_log_path() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("daemon.log"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/`
pub fn worker_dir(session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(session_id)?;
    Ok(c9watch_dir()?.join("workers").join(session_id))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/meta.json`
pub fn worker_meta_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("meta.json"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/stdout.log`
pub fn worker_stdout_log_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("stdout.log"))
}

/// Returns `~/.claude/c9watch/workers/<session-id>/stderr.log`
pub fn worker_stderr_log_path(session_id: &str) -> Result<PathBuf, String> {
    Ok(worker_dir(session_id)?.join("stderr.log"))
}

pub fn inbox_dir() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("inbox"))
}

pub fn inbox_pm_dir(pm_session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(pm_session_id)?;
    Ok(inbox_dir()?.join(pm_session_id))
}

/// Returns `~/.claude/c9watch/inbox/<worker-session-id>/`.
/// Inbox is now keyed by worker (stable) not PM (unstable).
pub fn inbox_worker_dir(worker_session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(worker_session_id)?;
    Ok(inbox_dir()?.join(worker_session_id))
}

/// Returns `~/.claude/c9watch/adoptions/`.
pub fn adoptions_dir() -> Result<PathBuf, String> {
    Ok(c9watch_dir()?.join("adoptions"))
}

/// Returns `~/.claude/c9watch/adoptions/<pm-session-id>.json`.
pub fn adoption_file(pm_session_id: &str) -> Result<PathBuf, String> {
    validate_session_id(pm_session_id)?;
    Ok(adoptions_dir()?.join(format!("{}.json", pm_session_id)))
}

// ── Structs ───────────────────────────────────────────────────────────

/// Subset of spawn arguments persisted into `meta.json` for each worker.
/// JSON field names are preserved for on-disk compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSpawnArgs {
    /// System prompt text to append for this worker
    pub append_system_prompt: Option<String>,
    /// Permission mode (e.g. "default", "acceptEdits", "bypassPermissions")
    pub permission_mode: String,
    /// Model override (e.g. "claude-opus-4-5")
    pub model: Option<String>,
    /// Additional directories to make available
    pub add_dirs: Vec<String>,
}

/// Metadata written for each worker session spawned by the PM daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkerMeta {
    /// UUID of the worker's Claude Code session
    pub session_id: String,
    /// OS process ID of the worker
    pub pid: u32,
    /// Human-readable name for the worker
    pub name: Option<String>,
    /// Working directory the worker was spawned in
    pub cwd: String,
    /// ISO-8601 timestamp when the worker was spawned
    pub spawned_at: String,
    /// Identifier of who/what spawned the worker (e.g. "pm-daemon")
    pub spawned_by: Option<String>,
    /// OS process ID of the PM's Claude Code process at spawn time. Used for
    /// default ownership resolution after the PM's session UUID changes via
    /// `/clear`. `None` only for workers spawned by non-Claude callers.
    #[serde(default)]
    pub pm_pid: Option<u32>,
    /// Arguments used to spawn the worker
    pub spawn_args: PersistedSpawnArgs,
    /// ISO-8601 timestamp when the worker stopped, if known
    pub stopped_at: Option<String>,
}

// ── Private dir and file helpers ──────────────────────────────────────

/// Create `dir` if needed and make it a 0700 directory owned by us.
///
/// The dir is opened with `O_NOFOLLOW` and every check and the chmod run on
/// that fd, so a symlink is rejected before anything is changed and the
/// chmod can never land on the target of a swapped-in link. A dir owned by
/// another uid is rejected, not chmodded.
pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create dir {:?}: {}", parent, e))?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("Failed to create dir {:?}: {}", dir, e)),
    }

    let symlink_err = || format!("{:?} is a symlink, refusing to use it", dir);
    if std::fs::symlink_metadata(dir)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(symlink_err());
    }
    // O_NOFOLLOW closes the race with a link swapped in after the check above.
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(dir)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => symlink_err(),
            Some(libc::ENOTDIR) => format!("{:?} is not a directory", dir),
            _ => format!("Failed to open dir {:?}: {}", dir, e),
        })?;
    let md = handle
        .metadata()
        .map_err(|e| format!("Failed to stat dir {:?}: {}", dir, e))?;
    if !md.is_dir() {
        return Err(format!("{:?} is not a directory", dir));
    }
    let me = unsafe { libc::geteuid() };
    if md.uid() != me {
        return Err(format!(
            "{:?} is owned by uid {}, not by us (uid {})",
            dir,
            md.uid(),
            me
        ));
    }
    if md.mode() & 0o777 != 0o700 {
        handle
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Failed to chmod 0700 {:?}: {}", dir, e))?;
        let mode = handle
            .metadata()
            .map_err(|e| format!("Failed to stat dir {:?}: {}", dir, e))?
            .mode();
        if mode & 0o777 != 0o700 {
            return Err(format!("{:?} is {:o} after chmod 0700", dir, mode & 0o777));
        }
    }
    Ok(())
}

/// Open `path` with `opts`, creating it as 0600. `mode()` only applies when
/// the file is created, so an existing file is chmodded to 0600 on the fd.
pub fn open_private_file(path: &Path, opts: &mut OpenOptions) -> Result<File, String> {
    let file = opts
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("Failed to open {:?}: {}", path, e))?;
    let mode = file
        .metadata()
        .map_err(|e| format!("Failed to stat {:?}: {}", path, e))?
        .mode();
    if mode & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", path, e))?;
    }
    Ok(file)
}

/// macOS `sun_path` is 104 bytes including the trailing NUL.
const SUN_PATH_MAX: usize = 104;

/// Name of the temp socket `bind_private` binds before renaming onto `path`.
pub fn bind_tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(DAEMON_SOCK_NAME);
    path.with_file_name(format!(".{}.tmp-{}", name, std::process::id()))
}

/// Bind a Unix socket under a temp name, chmod it 0600 (any error fails the
/// bind), then rename it onto `path`. A client can never connect to the
/// final path while the socket still has the umask permissions.
pub fn bind_private(path: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    use std::os::unix::ffi::OsStrExt;
    let tmp = bind_tmp_path(path);
    if tmp.as_os_str().as_bytes().len() >= SUN_PATH_MAX {
        return Err(format!(
            "Socket path {:?} is too long ({} bytes, limit {})",
            tmp,
            tmp.as_os_str().len(),
            SUN_PATH_MAX - 1
        ));
    }
    let _ = std::fs::remove_file(&tmp);
    let listener = std::os::unix::net::UnixListener::bind(&tmp)
        .map_err(|e| format!("Failed to bind Unix socket {:?}: {}", tmp, e))?;
    let res = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", tmp, e))
        .and_then(|_| {
            let mode = std::fs::symlink_metadata(&tmp)
                .map_err(|e| format!("Failed to stat {:?}: {}", tmp, e))?
                .mode()
                & 0o777;
            if mode == 0o600 {
                Ok(())
            } else {
                Err(format!("Socket {:?} is {:o} after chmod 0600", tmp, mode))
            }
        })
        .and_then(|_| {
            std::fs::rename(&tmp, path)
                .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, path, e))
        });
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(listener)
}

/// Error code returned when another daemon already holds `daemon.lock`.
pub const DAEMON_ALREADY_RUNNING: &str = "DAEMON_ALREADY_RUNNING";

/// Release a flock explicitly, then close the fd. Closing alone is not
/// enough: a flock belongs to the open file description, and a child forked
/// by another thread shares that description until it execs or exits, so a
/// plain close would leave the lock held. `LOCK_UN` releases it for every
/// copy.
fn unlock_and_close(file: File) {
    use std::os::unix::io::AsRawFd;
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    drop(file);
}

/// The daemon's exclusive lock. Dropping it releases the lock (see
/// `unlock_and_close`).
#[derive(Debug)]
pub struct DaemonLock(File);

impl std::os::unix::io::AsRawFd for DaemonLock {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0.as_raw_fd()
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// How long `acquire_daemon_lock` retries a busy lock. Clients probe the lock
/// with a brief `LOCK_SH`, so a fresh daemon must not give up on the first
/// `EWOULDBLOCK`; a live daemon holds it forever, so this only delays the
/// second daemon's error.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

/// Open `path` (0600) and take an exclusive flock on it, then empty it: the
/// file only carries the ready mark (see `mark_daemon_ready`) of the daemon
/// that holds it. Keep the returned `DaemonLock` alive for as long as the
/// lock must be held.
pub fn acquire_daemon_lock(path: &Path) -> Result<DaemonLock, String> {
    use std::os::unix::io::AsRawFd;
    let file = open_private_file(path, OpenOptions::new().create(true).read(true).write(true))?;
    let deadline = std::time::Instant::now() + LOCK_RETRY;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            file.set_len(0)
                .map_err(|e| format!("Failed to truncate {:?}: {}", path, e))?;
            return Ok(DaemonLock(file));
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(format!("Failed to lock {:?}: {}", path, err));
        }
        if std::time::Instant::now() >= deadline {
            return Err(DAEMON_ALREADY_RUNNING.to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Write our pid into the held lock file once the socket is listening.
/// Clients wait for this mark, not just for a connectable socket: while a
/// new daemon retires a pre-lock daemon, the old daemon's socket still
/// accepts connections.
pub fn mark_daemon_ready(lock: &DaemonLock) -> Result<(), String> {
    use std::io::{Seek, Write};
    let mut f = &lock.0;
    f.seek(std::io::SeekFrom::Start(0))
        .and_then(|_| f.write_all(std::process::id().to_string().as_bytes()))
        .map_err(|e| format!("Failed to write daemon ready mark: {}", e))
}

/// True once the daemon holding `path` has called `mark_daemon_ready`.
///
/// The mark is a pid, and it only counts while that pid is alive. A daemon
/// that exited cleanly leaves its pid behind; until the next daemon takes
/// the lock and truncates the file, that stale mark must not make a client
/// talk to whatever answers the socket (for example a pre-lock daemon that
/// the new one is about to retire).
pub fn daemon_ready_marked(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| c.trim().parse::<libc::pid_t>().ok())
        .is_some_and(|pid| pid > 1 && unsafe { libc::kill(pid, 0) } == 0)
}

/// True when some process holds an exclusive flock on `path` (the daemon is
/// alive). The probe takes `LOCK_SH|LOCK_NB` and releases it before
/// returning, so a daemon spawned right after can take its `LOCK_EX`.
pub fn daemon_lock_held(path: &Path) -> Result<bool, String> {
    use std::os::unix::io::AsRawFd;
    let file = open_private_file(path, OpenOptions::new().create(true).read(true).write(true))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        unlock_and_close(file);
        return Ok(false);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(true)
    } else {
        Err(format!("Failed to probe lock {:?}: {}", path, err))
    }
}

// ── I/O functions ─────────────────────────────────────────────────────

/// Ensure the base c9watch dir (private, 0700) and its subdirs exist.
pub fn ensure_dirs() -> Result<(), String> {
    let base = c9watch_dir()?;
    ensure_private_dir(&base)?;
    let workers = base.join("workers");
    std::fs::create_dir_all(&workers)
        .map_err(|e| format!("Failed to create workers dir {:?}: {}", workers, e))?;
    let inbox = inbox_dir()?;
    std::fs::create_dir_all(&inbox)
        .map_err(|e| format!("Failed to create inbox dir {:?}: {}", inbox, e))?;
    let adoptions = adoptions_dir()?;
    std::fs::create_dir_all(&adoptions)
        .map_err(|e| format!("Failed to create adoptions dir {:?}: {}", adoptions, e))?;
    Ok(())
}

/// Write a `WorkerMeta` atomically (write to `.tmp` then rename).
pub fn write_worker_meta(meta: &WorkerMeta) -> Result<(), String> {
    let dir = worker_dir(&meta.session_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create worker dir {:?}: {}", dir, e))?;

    let dest = worker_meta_path(&meta.session_id)?;
    let tmp = dest.with_extension("json.tmp");

    let json = serde_json::to_string_pretty(meta)
        .map_err(|e| format!("Failed to serialize WorkerMeta: {}", e))?;

    std::fs::write(&tmp, &json)
        .map_err(|e| format!("Failed to write tmp meta file {:?}: {}", tmp, e))?;

    std::fs::rename(&tmp, &dest)
        .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, dest, e))?;

    Ok(())
}

/// Read a `WorkerMeta` for a given session ID.
pub fn read_worker_meta(session_id: &str) -> Result<WorkerMeta, String> {
    let path = worker_meta_path(session_id)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read worker meta {:?}: {}", path, e))?;
    serde_json::from_str::<WorkerMeta>(&content)
        .map_err(|e| format!("Failed to parse worker meta {:?}: {}", path, e))
}

/// Return all session IDs that have a worker directory under workers/.
pub fn list_worker_ids() -> Result<Vec<String>, String> {
    let workers_dir = c9watch_dir()?.join("workers");

    if !workers_dir.exists() {
        return Ok(vec![]);
    }

    let mut ids: Vec<String> = std::fs::read_dir(&workers_dir)
        .map_err(|e| format!("Failed to read workers dir {:?}: {}", workers_dir, e))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();

    ids.sort();
    Ok(ids)
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c9watch_dir_is_under_home() {
        let dir = c9watch_dir().expect("c9watch_dir should succeed");
        let home = dirs::home_dir().expect("home dir should exist");
        assert!(
            dir.starts_with(&home),
            "c9watch_dir {:?} should be under home {:?}",
            dir,
            home
        );
        assert!(dir.ends_with(".claude/c9watch"));
    }

    #[test]
    fn test_daemon_paths_are_under_c9watch_dir() {
        let base = c9watch_dir().unwrap();

        let sock = daemon_sock_path().unwrap();
        assert!(sock.starts_with(&base));
        assert_eq!(sock.file_name().unwrap(), "daemon.sock");

        let pid = daemon_pid_path().unwrap();
        assert!(pid.starts_with(&base));
        assert_eq!(pid.file_name().unwrap(), "daemon.pid");

        let log = daemon_log_path().unwrap();
        assert!(log.starts_with(&base));
        assert_eq!(log.file_name().unwrap(), "daemon.log");
    }

    #[test]
    fn test_worker_dir_structure() {
        let session_id = "test-session-abc123";
        let base = c9watch_dir().unwrap();

        let dir = worker_dir(session_id).unwrap();
        assert!(dir.starts_with(base.join("workers")));
        assert!(dir.ends_with(session_id));

        let meta = worker_meta_path(session_id).unwrap();
        assert!(meta.starts_with(&dir));
        assert_eq!(meta.file_name().unwrap(), "meta.json");

        let stdout = worker_stdout_log_path(session_id).unwrap();
        assert!(stdout.starts_with(&dir));
        assert_eq!(stdout.file_name().unwrap(), "stdout.log");

        let stderr = worker_stderr_log_path(session_id).unwrap();
        assert!(stderr.starts_with(&dir));
        assert_eq!(stderr.file_name().unwrap(), "stderr.log");
    }

    #[test]
    fn test_worker_meta_roundtrip() {
        let session_id = "roundtrip-test-session-001";

        let meta = WorkerMeta {
            session_id: session_id.to_string(),
            pid: 12345,
            name: Some("test-worker".to_string()),
            cwd: "/Users/test/project".to_string(),
            spawned_at: "2026-04-16T00:00:00Z".to_string(),
            spawned_by: Some("pm-daemon".to_string()),
            pm_pid: Some(55123),
            spawn_args: PersistedSpawnArgs {
                append_system_prompt: Some("Be concise.".to_string()),
                permission_mode: "default".to_string(),
                model: Some("claude-opus-4-5".to_string()),
                add_dirs: vec!["/tmp/extra".to_string()],
            },
            stopped_at: None,
        };

        // Serialize and deserialize directly (without touching real ~/.claude/c9watch)
        let json = serde_json::to_string_pretty(&meta).expect("serialize should succeed");
        let parsed: WorkerMeta = serde_json::from_str(&json).expect("deserialize should succeed");

        assert_eq!(meta, parsed);

        // Verify camelCase field names in JSON
        assert!(json.contains("sessionId"));
        assert!(json.contains("spawnedAt"));
        assert!(json.contains("spawnedBy"));
        assert!(json.contains("stoppedAt"));
        assert!(json.contains("appendSystemPrompt"));
        assert!(json.contains("permissionMode"));
        assert!(json.contains("addDirs"));
        assert!(json.contains("pmPid"));
    }

    #[test]
    fn test_list_worker_ids_empty_when_no_dir() {
        // If the workers directory doesn't exist, list_worker_ids returns empty vec (not error).
        // We test this by checking the function signature handles missing dir gracefully.
        // Since we can't easily redirect home, we verify that a non-existent workers dir
        // results in an empty vec by testing the code path directly.
        let workers_dir = c9watch_dir().unwrap().join("workers");
        if !workers_dir.exists() {
            // The real dir doesn't exist on this machine — list_worker_ids should return Ok([])
            let ids = list_worker_ids().expect("should not error when workers dir missing");
            assert!(ids.is_empty());
        } else {
            // Workers dir exists — just verify we get a Vec<String> back without panicking
            let ids = list_worker_ids().expect("should not error when workers dir exists");
            // All returned IDs should be non-empty strings
            for id in &ids {
                assert!(!id.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod private_fs_tests {
    use super::*;
    use std::fs;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("c9w-pmfs-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn mode_of(p: &Path) -> u32 {
        fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dir_is_0700_even_if_it_existed_wider() {
        let d = tmpdir("wide");
        fs::create_dir_all(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&d).unwrap();
        assert_eq!(mode_of(&d), 0o700);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn private_dir_is_created_0700() {
        let d = tmpdir("new");
        ensure_private_dir(&d.join("c9watch")).unwrap();
        assert_eq!(mode_of(&d.join("c9watch")), 0o700);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn private_dir_rejects_symlink() {
        let d = tmpdir("link");
        let target = d.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = d.join("c9watch");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = ensure_private_dir(&link).unwrap_err();
        assert!(err.contains("symlink"), "got: {}", err);
        // The link target must not have been chmodded.
        assert_eq!(mode_of(&target), 0o755);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bind_private_renames_a_0600_socket_and_it_still_accepts() {
        use std::io::{BufRead, BufReader, Write};
        let d = tmpdir("bind");
        ensure_private_dir(&d).unwrap();
        let p = d.join("e.sock");
        let l = bind_private(&p).unwrap();
        assert_eq!(mode_of(&p), 0o600);
        // No temp name left behind.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let h = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let mut s = String::new();
            BufReader::new(&c).read_line(&mut s).unwrap();
            s
        });
        let mut c = std::os::unix::net::UnixStream::connect(&p).unwrap();
        writeln!(c, "hi").unwrap();
        assert_eq!(h.join().unwrap(), "hi\n");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bind_private_fails_cleanly_in_a_missing_dir() {
        let d = tmpdir("missing");
        assert!(bind_private(&d.join("e.sock")).is_err());
        assert!(!d.exists());
    }

    #[test]
    fn a_ready_mark_from_an_exited_daemon_is_not_ready() {
        let d = tmpdir("stale-mark");
        ensure_private_dir(&d).unwrap();
        let p = d.join(DAEMON_LOCK_NAME);
        // A daemon that exited cleanly leaves its pid in the lock file.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let gone_pid = gone.id();
        gone.wait().unwrap();
        fs::write(&p, gone_pid.to_string()).unwrap();
        assert!(!daemon_ready_marked(&p), "stale mark of pid {gone_pid}");
        fs::write(&p, "not a pid").unwrap();
        assert!(!daemon_ready_marked(&p));
        fs::write(&p, std::process::id().to_string()).unwrap();
        assert!(daemon_ready_marked(&p));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn second_lock_fails_while_first_held() {
        let d = tmpdir("lock");
        ensure_private_dir(&d).unwrap();
        let p = d.join(DAEMON_LOCK_NAME);
        assert!(!daemon_lock_held(&p).unwrap());

        let first = acquire_daemon_lock(&p).unwrap();
        assert_eq!(mode_of(&p), 0o600);
        assert!(daemon_lock_held(&p).unwrap());
        assert_eq!(acquire_daemon_lock(&p).unwrap_err(), DAEMON_ALREADY_RUNNING);

        assert!(!daemon_ready_marked(&p));
        mark_daemon_ready(&first).unwrap();
        assert!(daemon_ready_marked(&p));

        drop(first);
        assert!(!daemon_lock_held(&p).unwrap());
        // The probe released its shared lock, so the next daemon can lock.
        let _second = acquire_daemon_lock(&p).unwrap();
        // A new holder clears the previous holder's ready mark.
        assert!(!daemon_ready_marked(&p));
        fs::remove_dir_all(&d).unwrap();
    }

    /// Another test in this binary forks a child that parks before exec, so
    /// it shares every open file description we hold at that moment. A `dup`
    /// gives the same shared description deterministically: dropping our
    /// lock (or the probe) must still release the flock.
    #[test]
    fn lock_and_probe_release_even_if_the_fd_was_inherited() {
        use std::os::unix::io::AsRawFd;
        let d = tmpdir("inherited");
        ensure_private_dir(&d).unwrap();
        let p = d.join(DAEMON_LOCK_NAME);

        let first = acquire_daemon_lock(&p).unwrap();
        let copy = unsafe { libc::dup(first.as_raw_fd()) };
        assert!(copy >= 0);
        drop(first);
        assert!(!daemon_lock_held(&p).unwrap(), "lock outlived its guard");
        unsafe { libc::close(copy) };

        // The probe's shared lock must not linger in a copy either.
        let probe = OpenOptions::new().read(true).open(&p).unwrap();
        assert_eq!(
            unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        let probe_copy = unsafe { libc::dup(probe.as_raw_fd()) };
        unlock_and_close(probe);
        let _second = acquire_daemon_lock(&p).unwrap();
        unsafe { libc::close(probe_copy) };
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn pid_and_log_files_are_0600() {
        let d = tmpdir("files");
        ensure_private_dir(&d).unwrap();

        // New file: created 0600.
        let pid = d.join("daemon.pid");
        open_private_file(&pid, OpenOptions::new().create(true).write(true)).unwrap();
        assert_eq!(mode_of(&pid), 0o600);

        // Existing wider file: narrowed to 0600.
        let log = d.join("daemon.log");
        fs::write(&log, b"old").unwrap();
        fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
        open_private_file(&log, OpenOptions::new().create(true).append(true)).unwrap();
        assert_eq!(mode_of(&log), 0o600);
        assert_eq!(fs::read(&log).unwrap(), b"old");

        fs::remove_dir_all(&d).unwrap();
    }
}

#[cfg(test)]
mod inbox_path_tests {
    use super::*;

    #[test]
    fn inbox_dir_is_child_of_c9watch_dir() {
        let d = inbox_dir().unwrap();
        assert!(d.ends_with("inbox"));
        assert!(d.starts_with(c9watch_dir().unwrap()));
    }

    #[test]
    fn inbox_pm_dir_includes_pm_session_id() {
        let d = inbox_pm_dir("abc-123").unwrap();
        assert!(d.ends_with("abc-123"));
        assert!(d.starts_with(inbox_dir().unwrap()));
    }

    #[test]
    fn validate_session_id_accepts_uuid_like() {
        validate_session_id("550e8400-e29b-41d4-a716-446655440000").unwrap();
        validate_session_id("abc-123").unwrap();
        validate_session_id("a").unwrap();
    }

    #[test]
    fn validate_session_id_rejects_traversal_and_bad_chars() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id("..").is_err());
        assert!(validate_session_id("../../etc").is_err());
        assert!(validate_session_id("foo/bar").is_err());
        assert!(validate_session_id("foo\\bar").is_err());
        assert!(validate_session_id("foo bar").is_err());
        assert!(validate_session_id(".hidden").is_err());
        assert!(validate_session_id(&"a".repeat(65)).is_err());
    }

    #[test]
    fn inbox_worker_dir_includes_worker_id() {
        let d = inbox_worker_dir("w-abc").unwrap();
        assert!(d.ends_with("w-abc"));
        assert!(d.starts_with(inbox_dir().unwrap()));
    }

    #[test]
    fn inbox_worker_dir_rejects_traversal() {
        assert!(inbox_worker_dir("../etc").is_err());
        assert!(inbox_worker_dir("").is_err());
    }

    #[test]
    fn adoption_file_includes_pm_id() {
        let f = adoption_file("pm-abc").unwrap();
        assert!(f.ends_with("pm-abc.json"));
        assert!(f.starts_with(adoptions_dir().unwrap()));
    }

    #[test]
    fn adoption_file_rejects_traversal() {
        assert!(adoption_file("../etc").is_err());
        assert!(adoption_file("").is_err());
    }

    #[test]
    fn inbox_pm_dir_rejects_traversal() {
        assert!(inbox_pm_dir("../etc").is_err());
        assert!(inbox_pm_dir("").is_err());
    }

    #[test]
    fn worker_dir_rejects_traversal() {
        assert!(worker_dir("../etc").is_err());
        assert!(worker_dir("").is_err());
    }
}
