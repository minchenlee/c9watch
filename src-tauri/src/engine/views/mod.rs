//! What the engine serves to its clients in the shape the GUI shows today
//! (core-public M-a). One module per data group. Each calls the reader in
//! `session/` that the GUI's Tauri command calls, so both answer the same
//! JSON. Who may call what is decided in `api` (`dispatch`).

pub mod conversation;
pub mod cost;
pub mod history;
pub mod memory;
pub mod opencode;
pub mod sessions;
pub mod subagents;
pub mod usage;

use crate::engine::api::ServerCtx;
use crate::engine::proto::{ApiError, ErrorCode, Request};
use std::path::PathBuf;

/// Decode `req.params` as `T`; absent params are `T::default()`.
pub(crate) fn params<T: serde::de::DeserializeOwned + Default>(
    method: &str,
    req: Request,
) -> Result<T, ApiError> {
    if req.params.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(req.params).map_err(|e| {
        ApiError::new(
            ErrorCode::BadRequest,
            format!("bad {} params: {}", method, e),
        )
    })
}

/// The user home the readers look under: the test override, else the
/// engine user's home.
pub(crate) fn user_home(ctx: &ServerCtx) -> Result<PathBuf, ApiError> {
    ctx.user_home
        .clone()
        .or_else(dirs::home_dir)
        .ok_or_else(|| ApiError::new(ErrorCode::Internal, "the engine user has no home"))
}

/// A session id that is safe to join into a path: no separators, no `..`.
pub(crate) fn check_id(method: &str, field: &str, id: &str) -> Result<(), ApiError> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(ApiError::new(
            ErrorCode::BadRequest,
            format!("bad {} params: {} must be a session id", method, field),
        ));
    }
    Ok(())
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).expect("result serializes")
}
