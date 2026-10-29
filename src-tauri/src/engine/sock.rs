//! The engine socket, `<home>/engine-v1.sock`: a private bind, cleanup that
//! only removes our own socket, and the shutdown signals.
//!
//! Ported from `cli/pm_fs.rs` (`bind_tmp_path`, `bind_private`) and the C0
//! spike `sock.rs`. Unlike the legacy daemon, the engine never deletes a
//! socket it does not own (A:175): replacing the path needs the lifetime
//! lock, and exit only unlinks the inode it bound. The legacy
//! `shutdown_daemon` (it kills workers and cleans worker dirs) is not used.

use super::home::check_sun_path;
use super::lock::EngineLock;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use tokio::signal::unix::{signal, Signal, SignalKind};

/// Name of the temp socket `bind_private` binds before renaming onto `path`.
fn bind_tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(super::home::ENGINE_SOCK_NAME);
    path.with_file_name(format!(".{}.tmp-{}", name, std::process::id()))
}

/// The socket this engine bound. Dropping it unlinks the socket, but only
/// while `path` is still the inode we bound.
#[derive(Debug)]
pub struct OwnedSocket {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl OwnedSocket {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True while `path` is the socket we bound (same device and inode).
    pub fn is_ours(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|md| {
            md.file_type().is_socket() && md.dev() == self.dev && md.ino() == self.ino
        })
    }

    /// Unlink the socket if it is still ours. Returns whether it unlinked.
    pub fn remove(&self) -> bool {
        self.is_ours() && std::fs::remove_file(&self.path).is_ok()
    }
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Bind a Unix socket under a temp name, chmod it 0600 (any error fails the
/// bind), then rename it onto `path`. A client can never connect to the
/// final path while the socket still has the umask permissions.
///
/// Taking `&EngineLock` makes the caller prove it holds the lifetime lock:
/// only then is a socket already at `path` a leftover of a dead engine that
/// the rename may replace.
pub fn bind_private(
    path: &Path,
    _lock: &EngineLock,
) -> Result<(UnixListener, OwnedSocket), String> {
    let tmp = bind_tmp_path(path);
    check_sun_path(path)?;
    check_sun_path(&tmp)?;
    // The temp name carries our pid, so a socket there is our own leftover.
    let _ = std::fs::remove_file(&tmp);
    let listener = UnixListener::bind(&tmp)
        .map_err(|e| format!("Failed to bind Unix socket {:?}: {}", tmp, e))?;
    let res = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("Failed to chmod 0600 {:?}: {}", tmp, e))
        .and_then(|_| {
            let md = std::fs::symlink_metadata(&tmp)
                .map_err(|e| format!("Failed to stat {:?}: {}", tmp, e))?;
            let mode = md.mode() & 0o777;
            if mode == 0o600 {
                Ok(md)
            } else {
                Err(format!("Socket {:?} is {:o} after chmod 0600", tmp, mode))
            }
        })
        .and_then(|md| {
            std::fs::rename(&tmp, path)
                .map_err(|e| format!("Failed to rename {:?} -> {:?}: {}", tmp, path, e))?;
            Ok(md)
        });
    match res {
        // rename keeps the inode, so the temp name's dev/ino identify `path`.
        Ok(md) => Ok((
            listener,
            OwnedSocket {
                path: path.to_path_buf(),
                dev: md.dev(),
                ino: md.ino(),
            },
        )),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Which shutdown signal arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    Term,
    Int,
}

/// SIGTERM and SIGINT handlers. Install them before binding: from then on
/// either signal is delivered here instead of killing the process, so the
/// engine always gets to drop its `OwnedSocket`.
pub struct ShutdownSignals {
    term: Signal,
    int: Signal,
}

impl ShutdownSignals {
    /// Must run inside a tokio runtime.
    pub fn install() -> std::io::Result<Self> {
        Ok(Self {
            term: signal(SignalKind::terminate())?,
            int: signal(SignalKind::interrupt())?,
        })
    }

    /// Wait for the next SIGTERM or SIGINT.
    pub async fn recv(&mut self) -> ShutdownSignal {
        tokio::select! {
            _ = self.term.recv() => ShutdownSignal::Term,
            _ = self.int.recv() => ShutdownSignal::Int,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::home::{lock_path, sock_path, SOCKET_PATH_TOO_LONG};
    use super::super::lock::acquire_engine_lock;
    use super::*;
    use std::time::{Duration, Instant};

    fn mode_of(p: &Path) -> u32 {
        std::fs::symlink_metadata(p).unwrap().mode() & 0o777
    }

    #[test]
    fn bind_is_0600_and_leaves_no_temp_name() {
        let t = tempfile::tempdir().unwrap();
        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let path = sock_path(t.path());
        let (_l, owned) = bind_private(&path, &lock).unwrap();
        assert_eq!(mode_of(&path), 0o600);
        assert!(owned.is_ours());
        assert!(!bind_tmp_path(&path).exists());
        std::os::unix::net::UnixStream::connect(&path).unwrap();
    }

    #[test]
    fn dropping_removes_our_socket() {
        let t = tempfile::tempdir().unwrap();
        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let path = sock_path(t.path());
        let (_l, owned) = bind_private(&path, &lock).unwrap();
        drop(owned);
        assert!(!path.exists());
    }

    #[test]
    fn never_removes_a_socket_we_do_not_own() {
        let t = tempfile::tempdir().unwrap();
        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let path = sock_path(t.path());
        let (_l, owned) = bind_private(&path, &lock).unwrap();

        // Someone else replaces the path with their own socket.
        std::fs::remove_file(&path).unwrap();
        let _theirs = UnixListener::bind(&path).unwrap();
        assert!(!owned.is_ours());
        assert!(!owned.remove());
        drop(owned);
        assert!(path.exists());
        std::os::unix::net::UnixStream::connect(&path).unwrap();
    }

    #[test]
    fn stale_socket_is_replaced_under_the_lock() {
        let t = tempfile::tempdir().unwrap();
        let path = sock_path(t.path());
        // A dead engine's leftover: the listener is gone, the file stays.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());

        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let (_l, owned) = bind_private(&path, &lock).unwrap();
        assert!(owned.is_ours());
        std::os::unix::net::UnixStream::connect(&path).unwrap();
    }

    #[test]
    fn too_long_path_fails_before_binding() {
        let t = tempfile::tempdir().unwrap();
        let lock = acquire_engine_lock(&lock_path(t.path())).unwrap();
        let deep = t.path().join("d".repeat(120));
        let err = bind_private(&deep.join("engine-v1.sock"), &lock).unwrap_err();
        assert!(err.starts_with(SOCKET_PATH_TOO_LONG), "{}", err);
    }

    const HELPER_ENV: &str = "C9WATCH_TEST_SIGNAL_HELPER_HOME";

    /// Child side of the signal tests: a minimal engine lifetime (signals,
    /// lock, bind, wait, drop). Runs only when the parent test sets
    /// `HELPER_ENV`; a plain `--ignored` run returns at once.
    #[test]
    #[ignore]
    fn signal_helper() {
        let Some(home) = std::env::var_os(HELPER_ENV) else {
            return;
        };
        let home = PathBuf::from(home);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut sigs = ShutdownSignals::install().unwrap();
            let lock = acquire_engine_lock(&lock_path(&home)).unwrap();
            let (_l, owned) = bind_private(&sock_path(&home), &lock).unwrap();
            let got = sigs.recv().await;
            drop(owned);
            std::fs::write(home.join("signal"), format!("{:?}", got)).unwrap();
        });
    }

    /// Run `signal_helper` in a child process, send it `sig`, and check that
    /// it removed its socket and saw the right signal.
    fn socket_gone_after(sig: libc::c_int, expect: ShutdownSignal) {
        use wait_timeout::ChildExt;
        let t = tempfile::tempdir().unwrap();
        let path = sock_path(t.path());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::sock::tests::signal_helper",
                "--ignored",
                "--test-threads=1",
            ])
            .env(HELPER_ENV, t.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(20);
        while !path.exists() {
            if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                panic!("helper never bound {:?}", path);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, sig) }, 0);
        let status = child
            .wait_timeout(Duration::from_secs(20))
            .unwrap()
            .unwrap_or_else(|| {
                let _ = child.kill();
                panic!("helper did not exit after the signal");
            });
        assert!(status.success(), "helper exited with {:?}", status);
        assert!(!path.exists(), "socket still there after {:?}", expect);
        let got = std::fs::read_to_string(t.path().join("signal")).unwrap();
        assert_eq!(got, format!("{:?}", expect));
    }

    #[test]
    fn sigterm_removes_the_socket() {
        socket_gone_after(libc::SIGTERM, ShutdownSignal::Term);
    }

    #[test]
    fn sigint_removes_the_socket() {
        socket_gone_after(libc::SIGINT, ShutdownSignal::Int);
    }
}
