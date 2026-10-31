//! `history.list` and `history.search`: the GUI's `get_session_history`
//! and `deep_search_sessions` readers, on the blocking pool. Frames on one
//! connection run in order, so a connection runs one search at a time.

use super::{omitted, params, take_page, to_value, user_home, window};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{
    ApiError, HistoryListParams, HistoryListResult, HistorySearchParams, HistorySearchResult,
    Request,
};
use crate::session::HistoryEntry;

/// Titles longer than this go too when an entry is over the page budget.
const LONG_TITLE: usize = 4096;

/// An entry too large for a page: its prompt and long titles become notes.
fn shrink(e: &mut HistoryEntry) {
    e.display = omitted(e.display.len());
    for title in [
        &mut e.custom_title,
        &mut e.codex_title,
        &mut e.cursor_title,
        &mut e.ai_title,
    ] {
        if let Some(t) = title.as_mut().filter(|t| t.len() > LONG_TITLE) {
            *t = omitted(t.len());
        }
    }
    e.truncated = true;
}

/// One page of the history (`PageParams`); without `page`, the first.
pub async fn history_list(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: HistoryListParams = params("history.list", req)?;
    let w = window("history.list", p.page)?;
    let home = user_home(ctx)?;
    blocking(move || {
        let all = crate::session::history::get_history_in(&home).map_err(internal)?;
        let page = take_page(all, w, shrink);
        Ok(to_value(&HistoryListResult {
            entries: page.items,
            total: page.total,
            offset: page.offset,
            next_offset: page.next_offset,
        }))
    })
    .await
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
