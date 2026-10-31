//! Engine home: the private directory that holds the lock and database.
//!
//! - macOS: `~/Library/Application Support/c9watch/engine/` (prod) or
//!   `.../engine-dev/` (dev builds).
//! - Linux: `$XDG_STATE_HOME/c9watch/engine[-dev]/`, with the XDG default
//!   `~/.local/state` when the variable is unset.
//! - `C9WATCH_ENGINE_HOME` overrides both.
//! - Linux's default home uses a socket in `$XDG_RUNTIME_DIR/c9watch/`
//!   when available. Custom homes keep their socket next to their state.
//!
//! The dev and prod homes differ, so a dev engine never drains or restarts
//! the agents of the prod engine. This home is unrelated to the legacy
//! daemon's `~/.claude/c9watch/`.

//!
//! This part lives outside the `engine` feature, so a dev GUI finds the
//! engine of its channel without being built with it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Env var that replaces the per-channel engine home.
pub const ENGINE_HOME_ENV: &str = "C9WATCH_ENGINE_HOME";

/// The engine's socket inside a custom home, or the runtime fallback.
pub const ENGINE_SOCK_NAME: &str = "engine-v1.sock";

/// Build channel. Every build that is not a release gets `c9_channel_dev`
/// (see `build.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Prod,
    Dev,
}

impl Channel {
    /// The channel this binary was built for.
    pub const fn current() -> Self {
        if cfg!(c9_channel_dev) {
            Channel::Dev
        } else {
            Channel::Prod
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Channel::Prod => "engine",
            Channel::Dev => "engine-dev",
        }
    }
}

/// The engine home for this process: `C9WATCH_ENGINE_HOME` if set, else the
/// per-channel default. Does not create the directory.
pub fn engine_home() -> Result<PathBuf, String> {
    resolve_home(
        Channel::current(),
        std::env::var_os(ENGINE_HOME_ENV).as_deref(),
        dirs::home_dir().as_deref(),
        std::env::var_os("XDG_STATE_HOME").as_deref(),
    )
}

/// Pure form of `engine_home`, so tests never read the real environment.
///
/// An empty override counts as unset. A relative override is rejected: the
/// engine and its clients must agree on the path whatever their cwd.
pub fn resolve_home(
    channel: Channel,
    override_home: Option<&OsStr>,
    user_home: Option<&Path>,
    xdg_state_home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(dir) = override_home.filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return Err(format!(
                "{} must be an absolute path, got {:?}",
                ENGINE_HOME_ENV, dir
            ));
        }
        return Ok(dir);
    }
    Ok(state_base(user_home, xdg_state_home)?
        .join("c9watch")
        .join(channel.dir_name()))
}

#[cfg(target_os = "macos")]
fn state_base(
    user_home: Option<&Path>,
    _xdg_state_home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let home = user_home.ok_or("Failed to get home directory")?;
    Ok(home.join("Library").join("Application Support"))
}

#[cfg(not(target_os = "macos"))]
fn state_base(user_home: Option<&Path>, xdg_state_home: Option<&OsStr>) -> Result<PathBuf, String> {
    // The XDG spec says a relative value is invalid and must be ignored.
    if let Some(xdg) = xdg_state_home.map(PathBuf::from) {
        if xdg.is_absolute() {
            return Ok(xdg);
        }
    }
    let home = user_home.ok_or("Failed to get home directory")?;
    Ok(home.join(".local").join("state"))
}

/// Socket for `home`. On Linux the default state home uses the runtime
/// directory; custom homes remain self-contained (including test fixtures).
/// Compare the resolved home, rather than the override environment variable:
/// auto-start pins that variable to the parent's resolved home.
pub fn sock_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        let channel = Channel::current();
        let default_home = resolve_home(
            channel,
            None,
            dirs::home_dir().as_deref(),
            std::env::var_os("XDG_STATE_HOME").as_deref(),
        );
        if default_home.as_deref() == Ok(home) {
            return linux_sock_path(
                channel,
                home,
                std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            );
        }
    }
    home.join(ENGINE_SOCK_NAME)
}

/// Pure Linux path selection, also compiled on macOS for unit tests.
#[cfg(any(target_os = "linux", test))]
pub fn linux_sock_path(channel: Channel, home: &Path, runtime: Option<&OsStr>) -> PathBuf {
    match runtime.map(Path::new).filter(|p| p.is_absolute()) {
        Some(runtime) => runtime.join("c9watch").join(match channel {
            Channel::Prod => "engine-v1.sock",
            Channel::Dev => "engine-dev-v1.sock",
        }),
        None => home.join(ENGINE_SOCK_NAME),
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;

    #[test]
    fn linux_runtime_sockets_are_separate_by_channel() {
        let home = Path::new("/state/c9watch/engine");
        for (channel, name) in [
            (Channel::Prod, "engine-v1.sock"),
            (Channel::Dev, "engine-dev-v1.sock"),
        ] {
            assert_eq!(
                linux_sock_path(channel, home, Some(OsStr::new("/run/user/1000"))),
                Path::new("/run/user/1000/c9watch").join(name)
            );
            for runtime in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
                assert_eq!(
                    linux_sock_path(channel, home, runtime),
                    home.join(ENGINE_SOCK_NAME)
                );
            }
        }
    }
}

/// Lifetime lock shared by engine and spawning clients.
pub const ENGINE_LOCK_NAME: &str = "engine.lock";
pub fn lock_path(home: &Path) -> PathBuf {
    home.join(ENGINE_LOCK_NAME)
}
