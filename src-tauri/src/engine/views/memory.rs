//! `memory.list`: the GUI's `get_memory_files` reader.

use super::{params, to_value, user_home};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{ApiError, MemoryListParams, MemoryListResult, Request};

pub async fn memory_list(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let _: MemoryListParams = params("memory.list", req)?;
    let home = user_home(ctx)?;
    let projects = blocking(move || {
        crate::session::memory::get_memory_files_from_home(&home).map_err(internal)
    })
    .await?;
    Ok(to_value(&MemoryListResult { projects }))
}
