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
use crate::engine::proto::{
    ApiError, ErrorCode, PageParams, Request, PAGE_BUDGET, PAGE_DEFAULT_LIMIT, PAGE_MAX_LIMIT,
};
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

/// The window a `page` param asks for: `(offset, limit)`, with the default
/// and the cap applied. `limit: 0` is `bad_request`.
pub(crate) fn window(method: &str, page: Option<PageParams>) -> Result<(usize, usize), ApiError> {
    let page = page.unwrap_or_default();
    match page.limit {
        Some(0) => Err(ApiError::new(
            ErrorCode::BadRequest,
            format!("bad {} params: page.limit must be at least 1", method),
        )),
        limit => Ok((
            page.offset,
            limit.unwrap_or(PAGE_DEFAULT_LIMIT).min(PAGE_MAX_LIMIT),
        )),
    }
}

/// One page of a list (`PageParams`).
pub(crate) struct Paged<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

/// The serialized JSON size of `v`, without building it.
pub(crate) fn json_len<T: serde::Serialize>(v: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = Count(0);
    serde_json::to_writer(&mut c, v).expect("result serializes");
    c.0
}

/// At most `limit` items from `offset`, stopping before their JSON passes
/// `PAGE_BUDGET`. A page from before the end holds at least one item; an
/// item over the budget on its own goes through `shrink` first.
pub(crate) fn take_page<T: serde::Serialize>(
    items: Vec<T>,
    (offset, limit): (usize, usize),
    shrink: impl Fn(&mut T),
) -> Paged<T> {
    take_page_within(items, (offset, limit), PAGE_BUDGET, shrink)
}

pub(crate) fn take_page_within<T: serde::Serialize>(
    items: Vec<T>,
    (offset, limit): (usize, usize),
    budget: usize,
    shrink: impl Fn(&mut T),
) -> Paged<T> {
    let total = items.len();
    let offset = offset.min(total);
    let mut page = Vec::new();
    let mut used = 0usize;
    for mut item in items.into_iter().skip(offset).take(limit) {
        // One byte for the comma between items.
        let mut size = json_len(&item) + 1;
        if page.is_empty() && size > budget {
            shrink(&mut item);
            size = json_len(&item) + 1;
        }
        if !page.is_empty() && used + size > budget {
            break;
        }
        used += size;
        page.push(item);
    }
    let end = offset + page.len();
    Paged {
        items: page,
        total,
        offset,
        next_offset: (end < total).then_some(end),
    }
}

/// The note that replaces text too large for a page.
pub(crate) fn omitted(bytes: usize) -> String {
    format!(
        "[c9watch: {} bytes omitted; too large for one c9p page]",
        bytes
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_defaults_caps_and_rejects_zero() {
        assert_eq!(window("m", None).unwrap(), (0, PAGE_DEFAULT_LIMIT));
        let p = |offset, limit| Some(PageParams { offset, limit });
        assert_eq!(window("m", p(3, Some(5))).unwrap(), (3, 5));
        assert_eq!(
            window("m", p(0, Some(1 << 40))).unwrap(),
            (0, PAGE_MAX_LIMIT)
        );
        let e = window("m", p(0, Some(0))).unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);
    }

    #[test]
    fn take_page_stops_at_the_budget_and_always_moves_forward() {
        // Each item is 12 bytes of JSON ("aaaaaaaaaa"), 13 with its comma.
        let items = || vec!["a".repeat(10); 5];
        let p = take_page_within(items(), (0, 10), 30, |_| {});
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.next_offset, Some(2));
        // An item over the budget on its own comes alone, shrunk.
        let mut big = items();
        big[2] = "b".repeat(100);
        let p = take_page_within(big.clone(), (2, 10), 30, |s| *s = "x".into());
        assert_eq!(
            p.items,
            vec!["x".to_string(), "a".repeat(10), "a".repeat(10)]
        );
        assert_eq!(p.next_offset, None);
        // A big item after the first ends the page; the next page has it.
        let p = take_page_within(big, (1, 10), 30, |s| *s = "x".into());
        assert_eq!(p.items, vec!["a".repeat(10)]);
        assert_eq!(p.next_offset, Some(2));
        // Past the end: an empty last page.
        let p = take_page_within(items(), (9, 10), 30, |_| {});
        assert!(p.items.is_empty());
        assert_eq!((p.offset, p.next_offset), (5, None));
    }
}
