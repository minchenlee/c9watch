//! `history.list` and `history.search`: the GUI's `get_session_history`
//! and `deep_search_sessions` readers, on the blocking pool. Frames on one
//! connection run in order, so a connection runs one search at a time.

use super::{params, to_value, user_home};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{
    ApiError, HistoryListParams, HistoryListResult, HistorySearchParams, HistorySearchResult,
    Request,
};

pub async fn history_list(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let _: HistoryListParams = params("history.list", req)?;
    let home = user_home(ctx)?;
    let entries =
        blocking(move || crate::session::history::get_history_in(&home).map_err(internal)).await?;
    Ok(to_value(&HistoryListResult { entries }))
}

pub async fn history_search(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: HistorySearchParams = params("history.search", req)?;
    if p.query.trim().is_empty() {
        return Ok(to_value(&HistorySearchResult { hits: Vec::new() }));
    }
    let home = user_home(ctx)?;
    let hits = blocking(move || {
        crate::session::history::deep_search_under(&home, &p.query, p.case_sensitive, p.whole_word)
            .map_err(internal)
    })
    .await?;
    Ok(to_value(&HistorySearchResult { hits }))
}
