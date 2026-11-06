//! Agent creates agent (E5, D16): the rules a child of an `agent:` caller
//! obeys, and its place in the parent's tree.
//!
//! - A child is at most as free as its parent (child ⊆ parent). It must
//!   bring a guard. Every `denyWriteUnder` of the parent is under one of
//!   the child's, and no `allowWrite` of the child opens a path the parent
//!   may not write (`check_child_guard`).
//! - The parent's write area `W` is its cwd and its `allowWrite`
//!   subtrees; a parent without a guard has only its cwd. The child's cwd
//!   is in `W`, is not the parent's cwd, is not denied to the parent, and
//!   has no `.claude` or `.git` component (`check_child_cwd`).
//! - The tree has limits (`MAX_DEPTH`, `MAX_CHILDREN`, `MAX_TREE`),
//!   checked in the transaction that writes the child's row.
//! - A child lives while its parent can come back. When the parent is
//!   retired, or failed (its process exited, or a resume of it failed for
//!   good), or its row is gone, the engine retires the child
//!   (`PARENT_GONE`). A paused or interrupted parent keeps its tree: a
//!   drain and resume (D4) brings it back.

use super::guard::Fence;
use rusqlite::{Connection, OptionalExtension};
use std::path::{Component, Path};

/// The deepest a child may be: the root agent is depth 0.
pub const MAX_DEPTH: usize = 3;

/// The most live children one parent may have.
pub const MAX_CHILDREN: i64 = 8;

/// The most live agents one tree may hold, its root included.
pub const MAX_TREE: i64 = 16;

/// How often the engine looks for children whose parent is gone.
pub const SWEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// The audit reason of a child the engine retires after its parent.
pub const PARENT_GONE: &str = "parent-gone";

/// The directories a child's cwd may not be in: the control dirs of the
/// parent (or anyone else).
const CONTROL_DIRS: &[&str] = &[".claude", ".git"];

/// SQL: the row (of the table alias `t`, if any) is a live agent. A
/// `failed` or `retired` row is a tombstone.
fn live_sql(t: &str) -> String {
    let t = if t.is_empty() {
        String::new()
    } else {
        format!("{t}.")
    };
    format!("{t}gone_at IS NULL AND {t}state NOT IN ('retired', 'failed')")
}

/// Whether `target` is in the write area of a parent in `parent_cwd` with
/// the fence `parent`.
fn in_area(target: &Path, parent_cwd: &Path, parent: Option<&Fence>) -> bool {
    target.starts_with(parent_cwd)
        || parent.is_some_and(|f| f.allow_write.iter().any(|a| target.starts_with(a)))
}

/// The child's cwd `cwd` (a realpath) against its parent's. `parent_cwd`
/// is the parent's realpath.
pub fn check_child_cwd(
    cwd: &Path,
    parent_cwd: &Path,
    parent: Option<&Fence>,
) -> Result<(), String> {
    if cwd == parent_cwd {
        return Err(format!("the child's cwd {:?} is the parent's cwd", cwd));
    }
    if let Some(dir) = cwd.components().find_map(|c| match c {
        Component::Normal(n) => CONTROL_DIRS
            .iter()
            .find(|d| n.to_str().is_some_and(|n| n.eq_ignore_ascii_case(d))),
        _ => None,
    }) {
        return Err(format!(
            "the child's cwd {:?} is inside a {} directory",
            cwd, dir
        ));
    }
    if !in_area(cwd, parent_cwd, parent) {
        return Err(format!(
            "the child's cwd {:?} is outside the parent's write area (its cwd {:?}{})",
            cwd,
            parent_cwd,
            if parent.is_some_and(|f| !f.allow_write.is_empty()) {
                " and guard.allowWrite"
            } else {
                ""
            }
        ));
    }
    if parent.is_some_and(|f| f.denies(cwd)) {
        return Err(format!(
            "the parent's guard denies writes in the child's cwd {:?}",
            cwd
        ));
    }
    Ok(())
}

/// The child's fence against its parent's: the child may not write where
/// the parent may not. A child `allowWrite` must lie in the parent's
/// write area, must not be denied to the parent, and must not hold one of
/// the parent's `denyWriteUnder` paths.
pub fn check_child_guard(
    child: &Fence,
    parent_cwd: &Path,
    parent: Option<&Fence>,
) -> Result<(), String> {
    let none: &[std::path::PathBuf] = &[];
    let parent_deny = parent.map_or(none, |f| &f.deny_write_under);
    if let Some(d) = parent_deny
        .iter()
        .find(|d| !child.deny_write_under.iter().any(|c| d.starts_with(c)))
    {
        return Err(format!(
            "guard.denyWriteUnder does not cover the parent's {:?}",
            d
        ));
    }
    for a in &child.allow_write {
        if !in_area(a, parent_cwd, parent) {
            return Err(format!(
                "guard.allowWrite {:?} is outside the parent's write area",
                a
            ));
        }
        if parent.is_some_and(|f| f.denies(a)) || parent_deny.iter().any(|d| d.starts_with(a)) {
            return Err(format!(
                "guard.allowWrite {:?} opens a path the parent's guard denies",
                a
            ));
        }
    }
    Ok(())
}

/// The ids above `id`, its parent first, at most `MAX_DEPTH + 1` of them.
pub fn ancestors(conn: &Connection, id: &str) -> rusqlite::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut at = id.to_string();
    while out.len() <= MAX_DEPTH {
        let parent: Option<String> = conn
            .query_row("SELECT parent_id FROM agents WHERE id = ?1", [&at], |r| {
                r.get(0)
            })
            .optional()?
            .flatten();
        let Some(p) = parent else { break };
        if p == id || out.contains(&p) {
            break;
        }
        out.push(p.clone());
        at = p;
    }
    Ok(out)
}

/// Whether `id` is below `of` in a tree.
pub fn is_descendant(conn: &Connection, id: &str, of: &str) -> rusqlite::Result<bool> {
    Ok(ancestors(conn, id)?.iter().any(|a| a == of))
}

/// The live engine agents of the tree whose root is `root`, the root too.
fn tree_live(conn: &Connection, root: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!(
            "WITH RECURSIVE tree(id, depth) AS (SELECT ?1, 0 UNION \
             SELECT a.id, t.depth + 1 FROM agents a JOIN tree t ON a.parent_id = t.id \
             WHERE t.depth < ?2) \
             SELECT COUNT(*) FROM agents WHERE id IN (SELECT id FROM tree) AND {}",
            live_sql("")
        ),
        rusqlite::params![root, MAX_DEPTH as i64 + 1],
        |r| r.get(0),
    )
}

/// Check the tree limits for a new child of `parent`, in the write
/// transaction. The parent must be a live engine agent.
pub fn check_limits(conn: &Connection, parent: &str) -> Result<(), String> {
    let fail = |e: rusqlite::Error| format!("Failed to read the agent tree: {}", e);
    let live: bool = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) > 0 FROM agents WHERE id = ?1 AND source = 'engine' AND {}",
                live_sql("")
            ),
            [parent],
            |r| r.get(0),
        )
        .map_err(fail)?;
    if !live {
        return Err(format!("the parent {} is not a live engine agent", parent));
    }
    let above = ancestors(conn, parent).map_err(fail)?;
    if above.len() + 1 > MAX_DEPTH {
        return Err(format!(
            "a child of {} would be at depth {}; the limit is {}",
            parent,
            above.len() + 1,
            MAX_DEPTH
        ));
    }
    let children: i64 = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM agents WHERE parent_id = ?1 AND {}",
                live_sql("")
            ),
            [parent],
            |r| r.get(0),
        )
        .map_err(fail)?;
    if children >= MAX_CHILDREN {
        return Err(format!(
            "{} has {} live children; the limit is {}",
            parent, children, MAX_CHILDREN
        ));
    }
    let root = above.last().map_or(parent, |r| r.as_str());
    let tree = tree_live(conn, root).map_err(fail)?;
    if tree >= MAX_TREE {
        return Err(format!(
            "the tree of {} has {} live agents; the limit is {}",
            root, tree, MAX_TREE
        ));
    }
    Ok(())
}

/// The live engine agents whose parent can no longer come back: its row
/// is gone, `retired` or `failed`. With the parent id. An `orphaned` child
/// is left to a retire by hand: each try kills by pid again.
pub fn orphans(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut st = conn.prepare(&format!(
        "SELECT c.id, c.parent_id FROM agents c LEFT JOIN agents p ON p.id = c.parent_id \
         WHERE c.source = 'engine' AND c.parent_id IS NOT NULL AND {} \
         AND c.state != 'orphaned' \
         AND (p.id IS NULL OR p.gone_at IS NOT NULL OR p.state IN ('retired', 'failed')) \
         ORDER BY c.created_at, c.id",
        live_sql("c")
    ))?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fence(deny: &[&str], allow: &[&str]) -> Fence {
        Fence {
            deny_write_under: deny.iter().map(PathBuf::from).collect(),
            allow_write: allow.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn a_child_cwd_stays_in_the_parent_area() {
        let p = Path::new("/w/p");
        // A parent without a guard: W is its cwd.
        assert_eq!(check_child_cwd(Path::new("/w/p/c"), p, None), Ok(()));
        for bad in [
            "/w/p",
            "/w/q",
            "/w",
            "/w/p/.git/c",
            "/w/p/.claude",
            "/w/p/x/.Claude/y",
        ] {
            assert!(check_child_cwd(Path::new(bad), p, None).is_err(), "{bad}");
        }
        // With allowWrite, W grows; a deny of the parent still holds.
        let g = fence(&["/w"], &["/w/shared"]);
        assert_eq!(
            check_child_cwd(Path::new("/w/shared/c"), p, Some(&g)),
            Ok(())
        );
        let g = fence(&["/w/p/secret"], &[]);
        let e = check_child_cwd(Path::new("/w/p/secret/c"), p, Some(&g)).unwrap_err();
        assert!(e.contains("denies"), "{e}");
    }

    #[test]
    fn a_child_guard_is_at_least_as_strict() {
        let p = Path::new("/w/p");
        let parent = fence(&["/w/p/secret", "/w/other"], &[]);
        let ok = fence(&["/w/p/secret", "/w"], &["/w/p/c"]);
        assert_eq!(check_child_guard(&ok, p, Some(&parent)), Ok(()));
        // Drops a parent deny.
        let e = check_child_guard(&fence(&["/w/p/secret"], &[]), p, Some(&parent)).unwrap_err();
        assert!(e.contains("/w/other"), "{e}");
        // Opens the parent's denied subtree, or one that holds it.
        for allow in ["/w/p/secret/x", "/w/p", "/w/other"] {
            let g = fence(&["/w"], &[allow]);
            assert!(check_child_guard(&g, p, Some(&parent)).is_err(), "{allow}");
        }
        // Without a parent guard: allowWrite only inside the parent's cwd.
        assert_eq!(
            check_child_guard(&fence(&["/w"], &["/w/p/c"]), p, None),
            Ok(())
        );
        assert!(check_child_guard(&fence(&["/w"], &["/elsewhere"]), p, None).is_err());
    }
}
