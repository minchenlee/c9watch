//! `cost.get`: the GUI's `get_cost_data` reader. The engine keeps its own
//! cache file (`ServerCtx::cost_cache`, `<home>/cost-cache.json`), so it
//! never writes `~/.claude/cost-cache.json`, which the GUI owns.

use super::{params, to_value, user_home};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{ApiError, CostGetParams, ErrorCode, Request};

pub async fn cost_get(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let _: CostGetParams = params("cost.get", req)?;
    let home = user_home(ctx)?;
    let cache = ctx
        .cost_cache
        .clone()
        .ok_or_else(|| ApiError::new(ErrorCode::Internal, "this engine has no cost cache"))?;
    let data =
        blocking(move || crate::session::cost::get_cost_data_in(&home, &cache).map_err(internal))
            .await?;
    Ok(to_value(&data))
}
