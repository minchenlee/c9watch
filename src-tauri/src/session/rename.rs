//! Session rename, shared by the Tauri command, the WebSocket handler and
//! the engine's `session.rename`.

use crate::session::{
    validate_rename_request, CustomTitles, ProviderSourceOwners, SessionProvider,
};

/// Check the request against the provider owners, then run `write`. A
/// refused request never writes.
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub(crate) fn run_validated_rename<F>(
    provider: Option<SessionProvider>,
    session_id: &str,
    owners: &ProviderSourceOwners,
    write: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    validate_rename_request(provider, session_id, owners)?;
    write()
}

/// Persist a session rename: Claude Code's native JSONL entry (primary)
/// plus c9watch's own custom titles file (fallback for the history view).
pub fn write_session_title(session_id: &str, new_name: &str) -> Result<(), String> {
    crate::write_native_custom_title(session_id, new_name);

    let mut custom_titles = CustomTitles::load();
    custom_titles.set(session_id.to_string(), new_name.to_string());
    custom_titles.save()
}
