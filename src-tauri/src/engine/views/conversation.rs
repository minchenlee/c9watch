//! `conversation.get`: one session's transcript, read by the GUI's
//! `get_conversation` reader, in pages. A connection that asked for the
//! `conversation.progress` capability gets `progress` frames for the read
//! before the response.

use super::{params, to_value, user_home};
use crate::engine::api::{blocking, ServerCtx};
use crate::engine::proto::{
    encode_progress, ApiError, ConversationGetParams, ConversationGetResult, ErrorCode,
    ProgressFrame, ProgressType, Request, API_MAJOR,
};
use crate::session::SessionProvider;
use tokio::sync::mpsc;

/// The capability that turns on `progress` frames.
pub const PROGRESS_CAPABILITY: &str = "conversation.progress";

/// Where a read sends its `progress` frames: the connection's write queue
/// and the request id. A full queue drops the frame; the read never waits.
#[derive(Clone)]
pub struct Progress {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub id: Option<String>,
}

impl Progress {
    fn send(&self, bytes_read: u64, bytes_total: u64) {
        let frame = ProgressFrame {
            v: API_MAJOR,
            kind: ProgressType::Progress,
            id: self.id.clone(),
            bytes_read,
            bytes_total,
        };
        let _ = self.tx.try_send(encode_progress(&frame));
    }
}

/// Split `<provider>:<id>`; a key without a known provider prefix is a bare
/// id. A `provider` param must agree with the prefix.
pub fn split_key(
    key: &str,
    provider: Option<SessionProvider>,
) -> Result<(Option<SessionProvider>, String), ApiError> {
    if let Some((prefix, id)) = key.split_once(':') {
        if let Some(p) = SessionProvider::from_key(prefix) {
            if provider.is_some_and(|q| q != p) {
                return Err(ApiError::new(
                    ErrorCode::BadRequest,
                    "bad conversation.get params: provider does not match sessionKey",
                ));
            }
            return Ok((Some(p), id.to_string()));
        }
    }
    Ok((provider, key.to_string()))
}

fn read_error(e: String) -> ApiError {
    if e.contains("not found") {
        ApiError::new(ErrorCode::UnknownSession, e)
    } else if e.contains("ambiguous") {
        ApiError::new(ErrorCode::BadRequest, e)
    } else {
        ApiError::new(ErrorCode::Internal, e)
    }
}

pub async fn conversation_get(
    req: Request,
    ctx: &ServerCtx,
    progress: Option<Progress>,
) -> Result<serde_json::Value, ApiError> {
    let p: ConversationGetParams = params("conversation.get", req)?;
    if p.session_key.is_empty() {
        return Err(ApiError::new(
            ErrorCode::BadRequest,
            "bad conversation.get params: sessionKey is required",
        ));
    }
    let (provider, id) = split_key(&p.session_key, p.provider)?;
    let home = user_home(ctx)?;
    let include_tools = p.include_tools;
    let conv = blocking(move || {
        let mut on_progress = |read: u64, total: u64| {
            if let Some(pr) = &progress {
                pr.send(read, total);
            }
        };
        crate::session::conversation::get_conversation_data_in(
            &home,
            &id,
            provider,
            include_tools,
            &mut on_progress,
        )
        .map_err(read_error)
    })
    .await?;
    let total = conv.messages.len();
    let page = p.page.unwrap_or_default();
    let offset = page.offset.min(total);
    let end = page
        .limit
        .map_or(total, |l| offset.saturating_add(l).min(total));
    let messages = conv.messages[offset..end].to_vec();
    Ok(to_value(&ConversationGetResult {
        session_id: conv.session_id,
        provider: conv.provider,
        messages,
        total,
        offset,
        next_offset: (end < total).then_some(end),
    }))
}
