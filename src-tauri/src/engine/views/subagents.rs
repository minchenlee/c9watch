//! `subagent.list`, `subagent.transcript` and `session.tasks`: the GUI's
//! `get_subagents`, `get_subagent_transcript` and `get_session_tasks`
//! readers. The two subagent scans share the GUI's gates
//! (`session::blocking`): a scan that finds its gate taken answers `busy`.

use super::{check_id, params, to_value, user_home};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{
    ApiError, ErrorCode, Request, SessionTasksParams, SessionTasksResult, SubagentListParams,
    SubagentListResult, SubagentTranscriptParams,
};
use crate::session::blocking::{self as gates, ScanError};

fn scan_error(e: ScanError, not_found: ErrorCode) -> ApiError {
    match e {
        ScanError::Busy => ApiError::new(ErrorCode::Busy, e.to_string()),
        ScanError::Failed(m) => ApiError::new(not_found, m),
    }
}

pub async fn subagent_list(req: Request, _ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: SubagentListParams = params("subagent.list", req)?;
    let subagents = gates::try_scan(&gates::SUBAGENTS, move || {
        let live: std::collections::HashSet<String> = p.session_ids.into_iter().collect();
        Ok(crate::session::all_subagents_by_session(&live))
    })
    .await
    .map_err(|e| scan_error(e, ErrorCode::Internal))?;
    Ok(to_value(&SubagentListResult { subagents }))
}

pub async fn subagent_transcript(
    req: Request,
    _ctx: &ServerCtx,
) -> Result<serde_json::Value, ApiError> {
    let p: SubagentTranscriptParams = params("subagent.transcript", req)?;
    check_id(
        "subagent.transcript",
        "parentSessionId",
        &p.parent_session_id,
    )?;
    if p.subagent_id.is_empty() {
        return Err(ApiError::new(
            ErrorCode::BadRequest,
            "bad subagent.transcript params: subagentId is required",
        ));
    }
    let t = gates::try_scan(&gates::SUBAGENT_TRANSCRIPT, move || {
        crate::session::get_subagent_transcript(&p.parent_session_id, &p.subagent_id).ok_or_else(
            || {
                format!(
                    "subagent {} not found in session {}",
                    p.subagent_id, p.parent_session_id
                )
            },
        )
    })
    .await
    .map_err(|e| scan_error(e, ErrorCode::UnknownSession))?;
    Ok(to_value(&t))
}

pub async fn session_tasks(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: SessionTasksParams = params("session.tasks", req)?;
    check_id("session.tasks", "sessionId", &p.session_id)?;
    let home = user_home(ctx)?;
    let tasks = blocking(move || {
        crate::session::tasks::read_session_tasks_in(&home, &p.session_id).map_err(internal)
    })
    .await?;
    Ok(to_value(&SessionTasksResult { tasks }))
}
