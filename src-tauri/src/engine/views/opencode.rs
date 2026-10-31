//! `provider.opencode.status` and `provider.opencode.connect`: the
//! OpenCode server that detection reads. Credentials stay in the engine's
//! memory (`session::opencode`) and never go back out.

use super::{params, to_value};
use crate::engine::api::{blocking, ServerCtx};
use crate::engine::proto::{
    ApiError, ErrorCode, OpencodeConnectParams, OpencodeStatusParams, Request,
};

pub async fn opencode_status(
    req: Request,
    _ctx: &ServerCtx,
) -> Result<serde_json::Value, ApiError> {
    let _: OpencodeStatusParams = params("provider.opencode.status", req)?;
    let status = blocking(|| Ok(crate::session::opencode::connection_status())).await?;
    Ok(to_value(&status))
}

/// Connect (or with an empty `url`, disconnect), then answer the status.
pub async fn opencode_connect(
    req: Request,
    _ctx: &ServerCtx,
) -> Result<serde_json::Value, ApiError> {
    let p: OpencodeConnectParams = params("provider.opencode.connect", req)?;
    let status = blocking(move || {
        crate::session::opencode::connect(p.url, p.username, p.password)
            .map_err(|m| ApiError::new(ErrorCode::BadRequest, m))?;
        Ok(crate::session::opencode::connection_status())
    })
    .await?;
    Ok(to_value(&status))
}
