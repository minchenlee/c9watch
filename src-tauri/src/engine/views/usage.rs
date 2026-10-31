//! `usage.subscriptions`: the GUI's `get_subscription_usage` reader. It
//! spawns `codex app-server` and `curl`, so the engine reuses one snapshot
//! for `USAGE_MAX_AGE`; callers that arrive during a read share it.

use super::{params, to_value};
use crate::engine::api::ServerCtx;
use crate::engine::proto::{
    ApiError, ErrorCode, Request, UsageSubscriptionsParams, UsageSubscriptionsResult,
};
use crate::session::subscription_usage::SubscriptionUsage;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub const USAGE_MAX_AGE: Duration = Duration::from_secs(60);

pub type UsageFuture = Pin<Box<dyn Future<Output = Vec<SubscriptionUsage>> + Send>>;

/// Where `usage.subscriptions` reads. The engine uses `live_source`; a test
/// engine passes a fake so that no test spawns `codex` or `curl`.
pub type UsageSource = Arc<dyn Fn() -> UsageFuture + Send + Sync>;

/// The GUI's reader behind a `USAGE_MAX_AGE` cache.
pub fn live_source() -> UsageSource {
    Arc::new(|| {
        Box::pin(crate::session::subscription_usage::get_subscription_usage_within(USAGE_MAX_AGE))
    })
}

pub async fn usage_subscriptions(
    req: Request,
    ctx: &ServerCtx,
) -> Result<serde_json::Value, ApiError> {
    let _: UsageSubscriptionsParams = params("usage.subscriptions", req)?;
    let source = ctx.usage.as_ref().ok_or_else(|| {
        ApiError::new(
            ErrorCode::Internal,
            "this engine reads no subscription usage",
        )
    })?;
    let usage = source().await;
    Ok(to_value(&UsageSubscriptionsResult { usage }))
}
