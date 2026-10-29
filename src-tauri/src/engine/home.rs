//! Engine home: the private directory that holds the engine's socket, lock
//! and (later) database.
//!
//! - macOS: `~/Library/Application Support/c9watch/engine/` (prod) or
//!   `.../engine-dev/` (dev builds).
//! - Linux: `$XDG_STATE_HOME/c9watch/engine[-dev]/`, with the XDG default
//!   `~/.local/state` when the variable is unset.
//! - `C9WATCH_ENGINE_HOME` overrides both.
//!
//! The dev and prod homes differ, so a dev engine never drains or restarts
//! the agents of the prod engine. This home is unrelated to the legacy
//! daemon's `~/.claude/c9watch/`.
//!
//! The channel and the home path live in `engine_proto::home`, outside the
//! `engine` feature, and are re-exported here.

pub use crate::engine_proto::home::{
    engine_home, resolve_home, sock_path, Channel, ENGINE_HOME_ENV, ENGINE_SOCK_NAME,
};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// The engine's lifetime lock inside the home.
pub const ENGINE_LOCK_NAME: &str = "engine.lock";

/// Error code for a socket path that does not fit in `sun_path`.
pub const SOCKET_PATH_TOO_LONG: &str = "socket_path_too_long";

/// `<home>/engine.lock`
pub fn lock_path(home: &Path) -> PathBuf {
    home.join(ENGINE_LOCK_NAME)
}

/// Size of `sockaddr_un.sun_path`, including the trailing NUL: 104 bytes on
/// macOS, 108 on Linux.
pub const SUN_PATH_MAX: usize = {
    // Only the field size is read; the zeroed value is never used.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    std::mem::size_of_val(&addr.sun_path)
};

/// Fail with `socket_path_too_long` when `path` does not fit in `sun_path`
/// with its NUL. Call it before every bind (also for the temp bind name).
pub fn check_sun_path(path: &Path) -> Result<(), String> {
    let len = path.as_os_str().as_bytes().len();
    if len >= SUN_PATH_MAX {
        return Err(format!(
            "{}: {:?} is {} bytes, the limit is {}",
            SOCKET_PATH_TOO_LONG,
            path,
            len,
            SUN_PATH_MAX - 1
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    const HOME: &str = "/Users/someone";

    fn resolve(channel: Channel, over: Option<&str>, xdg: Option<&str>) -> Result<PathBuf, String> {
        resolve_home(
            channel,
            over.map(OsStr::new),
            Some(Path::new(HOME)),
            xdg.map(OsStr::new),
        )
    }

    #[test]
    fn channels_get_different_homes() {
        let prod = resolve(Channel::Prod, None, None).unwrap();
        let dev = resolve(Channel::Dev, None, None).unwrap();
        assert_ne!(prod, dev);
        assert!(prod.ends_with("c9watch/engine"), "{:?}", prod);
        assert!(dev.ends_with("c9watch/engine-dev"), "{:?}", dev);
        // Never under the legacy daemon's ~/.claude/c9watch.
        assert!(!prod.starts_with(Path::new(HOME).join(".claude")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_home_is_application_support() {
        let prod = resolve(Channel::Prod, None, Some("/ignored")).unwrap();
        assert_eq!(
            prod,
            PathBuf::from("/Users/someone/Library/Application Support/c9watch/engine")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_home_follows_xdg_state_home() {
        assert_eq!(
            resolve(Channel::Prod, None, Some("/xdg/state")).unwrap(),
            PathBuf::from("/xdg/state/c9watch/engine")
        );
        assert_eq!(
            resolve(Channel::Dev, None, None).unwrap(),
            PathBuf::from("/Users/someone/.local/state/c9watch/engine-dev")
        );
        // A relative XDG value is ignored, as the spec says.
        assert_eq!(
            resolve(Channel::Prod, None, Some("rel/state")).unwrap(),
            PathBuf::from("/Users/someone/.local/state/c9watch/engine")
        );
    }

    #[test]
    fn override_wins_for_both_channels() {
        for ch in [Channel::Prod, Channel::Dev] {
            assert_eq!(
                resolve(ch, Some("/tmp/eng"), Some("/xdg")).unwrap(),
                PathBuf::from("/tmp/eng")
            );
        }
    }

    #[test]
    fn empty_override_is_unset_and_relative_is_rejected() {
        assert_eq!(
            resolve(Channel::Prod, Some(""), None).unwrap(),
            resolve(Channel::Prod, None, None).unwrap()
        );
        let err = resolve(Channel::Prod, Some("rel/eng"), None).unwrap_err();
        assert!(err.contains(ENGINE_HOME_ENV), "{}", err);
    }

    #[test]
    fn no_user_home_is_an_error_without_override() {
        assert!(resolve_home(Channel::Prod, None, None, None).is_err());
        assert!(resolve_home(Channel::Prod, Some(OsStr::new("/e")), None, None).is_ok());
    }

    #[test]
    fn sun_path_limit_matches_the_platform() {
        #[cfg(target_os = "macos")]
        assert_eq!(SUN_PATH_MAX, 104);
        #[cfg(target_os = "linux")]
        assert_eq!(SUN_PATH_MAX, 108);
    }

    #[test]
    fn too_long_socket_path_is_rejected() {
        let fits = PathBuf::from(format!("/{}", "a".repeat(SUN_PATH_MAX - 2)));
        assert_eq!(fits.as_os_str().len(), SUN_PATH_MAX - 1);
        check_sun_path(&fits).unwrap();

        let too_long = PathBuf::from(format!("/{}", "a".repeat(SUN_PATH_MAX - 1)));
        let err = check_sun_path(&too_long).unwrap_err();
        assert!(err.starts_with(SOCKET_PATH_TOO_LONG), "{}", err);
    }

    #[test]
    fn sock_and_lock_live_in_the_home() {
        let home = Path::new("/e");
        assert_eq!(sock_path(home), PathBuf::from("/e/engine-v1.sock"));
        assert_eq!(lock_path(home), PathBuf::from("/e/engine.lock"));
    }
}
