//! `memory.list`: the GUI's `get_memory_files` reader, in pages of
//! projects.

use super::{json_len, omitted, params, take_page, to_value, user_home, window};
use crate::engine::api::{blocking, internal, ServerCtx};
use crate::engine::proto::{ApiError, MemoryListParams, MemoryListResult, Request, PAGE_BUDGET};
use crate::session::ProjectMemory;

/// A project too large for a page: its largest files become notes until
/// it fits (or none are left).
fn shrink(p: &mut ProjectMemory) {
    let mut order: Vec<usize> = (0..p.files.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(p.files[i].content.len()));
    for i in order {
        if json_len(p) < PAGE_BUDGET {
            break;
        }
        let f = &mut p.files[i];
        f.content = omitted(f.content.len());
        f.truncated = true;
    }
}

/// One page of memory projects (`PageParams`); without `page`, the first.
pub async fn memory_list(req: Request, ctx: &ServerCtx) -> Result<serde_json::Value, ApiError> {
    let p: MemoryListParams = params("memory.list", req)?;
    let w = window("memory.list", p.page)?;
    let home = user_home(ctx)?;
    blocking(move || {
        let all = crate::session::memory::get_memory_files_from_home(&home).map_err(internal)?;
        let page = take_page(all, w, shrink);
        Ok(to_value(&MemoryListResult {
            projects: page.items,
            total: page.total,
            offset: page.offset,
            next_offset: page.next_offset,
        }))
    })
    .await
}
