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
//! This part lives outside the `engine` feature, so a dev GUI finds the
//! engine of its channel without being built with it.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Env var that replaces the per-channel engine home.
pub const ENGINE_HOME_ENV: &str = "C9WATCH_ENGINE_HOME";

/// The engine's socket inside the home.
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

/// `<home>/engine-v1.sock`
pub fn sock_path(home: &Path) -> PathBuf {
    home.join(ENGINE_SOCK_NAME)
}
