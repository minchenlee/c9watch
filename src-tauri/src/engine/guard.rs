//! Write fence (D30, E2): caller data on `agent.create` that the engine
//! applies to the agent's file tools through a blocking `PreToolUse` hook
//! (`hook.pre_tool_use`).
//!
//! A fence is `denyWriteUnder` (absolute directories) and `allowWrite`
//! (absolute subtrees exempt from them). The core knows no root, sibling
//! or topology: the caller picks both lists. Bash is not covered here; the
//! CC sandbox keeps it in the cwd and TMPDIR (live run
//! run-20261102-144814, check3).
//!
//! Every path the fence holds is a realpath taken at create. A target is
//! resolved the same way before it is compared (`resolve_target`), so a
//! symlink or `..` cannot walk out of an allowed subtree.

use crate::engine_proto::wire::AgentGuard;
use std::path::{Component, Path, PathBuf};

/// The most paths one list of a fence may hold.
pub const MAX_GUARD_PATHS: usize = 64;

/// The prefix of every refusal reason. CC shows it to the model after
/// `PreToolUse:<tool> hook error:`; the live harness finds the layer by it.
pub const DENY_PREFIX: &str = "c9watch guard:";

/// The tool input keys that name the file a file tool writes.
pub const PATH_KEYS: &[&str] = &["file_path", "notebook_path"];

/// A fence with every path as a realpath.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    pub deny_write_under: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
}

impl Fence {
    /// Check the caller's lists and take each path's realpath. Every path
    /// must be absolute and exist; `denyWriteUnder` must not be empty and
    /// must not be `/`. `home` (the engine home) must not be inside an
    /// allowed subtree, nor `cwd` outside it when it is fenced.
    pub fn new(g: &AgentGuard, cwd: &Path, home: &Path) -> Result<Fence, String> {
        if g.deny_write_under.is_empty() {
            return Err("guard.denyWriteUnder is empty".into());
        }
        let real = |what: &str, list: &[String]| -> Result<Vec<PathBuf>, String> {
            if list.len() > MAX_GUARD_PATHS {
                return Err(format!(
                    "guard.{} has more than {} paths",
                    what, MAX_GUARD_PATHS
                ));
            }
            let mut v = Vec::new();
            for p in list {
                let path = Path::new(p);
                if !path.is_absolute() {
                    return Err(format!("guard.{} path {:?} is not absolute", what, p));
                }
                let r = path
                    .canonicalize()
                    .map_err(|e| format!("guard.{} path {:?}: {}", what, p, e))?;
                if r.to_str().is_none() {
                    return Err(format!("guard.{} path {:?} is not UTF-8", what, p));
                }
                v.push(r);
            }
            v.sort();
            v.dedup();
            Ok(v)
        };
        let fence = Fence {
            deny_write_under: real("denyWriteUnder", &g.deny_write_under)?,
            allow_write: real("allowWrite", &g.allow_write)?,
        };
        if fence.deny_write_under.iter().any(|d| d.parent().is_none()) {
            return Err("guard.denyWriteUnder may not name /".into());
        }
        if let Some(a) = fence.allow_write.iter().find(|a| home.starts_with(a)) {
            return Err(format!(
                "guard.allowWrite {:?} holds the engine home {:?}",
                a, home
            ));
        }
        if fence.denies(cwd) {
            return Err(format!(
                "guard denies writes in the agent's own cwd {:?}",
                cwd
            ));
        }
        Ok(fence)
    }

    /// Whether a write to the resolved path `target` is denied.
    pub fn denies(&self, target: &Path) -> bool {
        self.deny_write_under.iter().any(|d| target.starts_with(d))
            && !self.allow_write.iter().any(|a| target.starts_with(a))
    }

    /// The fence as the wire and the db hold it.
    pub fn to_wire(&self) -> AgentGuard {
        let s = |v: &[PathBuf]| v.iter().map(|p| p.display().to_string()).collect();
        AgentGuard {
            deny_write_under: s(&self.deny_write_under),
            allow_write: s(&self.allow_write),
        }
    }

    /// A stored fence, as `to_wire` wrote it. The paths are taken as they
    /// are: they were realpaths at create.
    pub fn from_wire(g: &AgentGuard) -> Fence {
        let p = |v: &[String]| v.iter().map(PathBuf::from).collect();
        Fence {
            deny_write_under: p(&g.deny_write_under),
            allow_write: p(&g.allow_write),
        }
    }
}

/// The path a write to `target` lands on: the realpath of its longest
/// existing prefix, then the rest, with `.` dropped and `..` taken
/// against what is resolved so far. An existing component is resolved
/// through its symlinks. A component that is a symlink whose target does
/// not exist is an error: a write through it would land on that target.
/// `target` must be absolute.
pub fn resolve_target(target: &Path) -> Result<PathBuf, String> {
    if !target.is_absolute() {
        return Err(format!("{:?} is not absolute", target));
    }
    let mut out = PathBuf::from("/");
    let mut exists = true;
    for c in target.components() {
        match c {
            Component::RootDir | Component::CurDir => {}
            Component::Prefix(_) => return Err(format!("{:?} has a prefix", target)),
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => {
                out.push(n);
                if !exists {
                    continue;
                }
                match out.symlink_metadata() {
                    Ok(m) => {
                        if m.file_type().is_symlink() || out.is_dir() {
                            out = out
                                .canonicalize()
                                .map_err(|e| format!("{:?}: {}", out, e))?;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => exists = false,
                    Err(e) => return Err(format!("{:?}: {}", out, e)),
                }
            }
        }
    }
    Ok(out)
}

/// The engine's answer to one `PreToolUse` call: `None` to allow, or the
/// refusal reason. `input` is the hook input as the agent's hook sent it.
/// A fenced agent's file tool call without a readable absolute path is
/// refused: the fence cannot be checked.
pub fn decide(fence: &Fence, input: &serde_json::Value) -> Option<String> {
    let tool = input["tool_name"].as_str().unwrap_or("?");
    let paths: Vec<&str> = PATH_KEYS
        .iter()
        .filter_map(|k| input["tool_input"][*k].as_str())
        .collect();
    if paths.is_empty() {
        return Some(format!(
            "{} {} names no file path, so the write fence cannot be checked",
            DENY_PREFIX, tool
        ));
    }
    for p in paths {
        let target = match resolve_target(Path::new(p)) {
            Ok(t) => t,
            Err(e) => return Some(format!("{} {} on {:?}: {}", DENY_PREFIX, tool, p, e)),
        };
        if fence.denies(&target) {
            return Some(format!(
                "{} {} on {:?} is outside this agent's writable area (it resolves to {:?})",
                DENY_PREFIX, tool, p, target
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn guard(deny: &[&Path], allow: &[&Path]) -> AgentGuard {
        let s = |v: &[&Path]| v.iter().map(|p| p.display().to_string()).collect();
        AgentGuard {
            deny_write_under: s(deny),
            allow_write: s(allow),
        }
    }

    /// `<tmp>/R/{a,b}` with `a` the agent's cwd, and the engine home
    /// beside `R`.
    fn layout() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let (r, a, b, home) = (
            base.join("R"),
            base.join("R/a"),
            base.join("R/b"),
            base.join("home"),
        );
        for d in [&a, &b, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        (t, r, a, b, home)
    }

    fn write(tool: &str, key: &str, path: &Path) -> serde_json::Value {
        json!({"tool_name": tool, "tool_input": {key: path.display().to_string(), "content": "x"}})
    }

    #[test]
    fn a_fence_allows_its_own_subtree_and_denies_the_rest_of_the_root() {
        let (_t, r, a, b, home) = layout();
        let f = Fence::new(&guard(&[&r], &[&a]), &a, &home).unwrap();
        assert_eq!(
            decide(&f, &write("Write", "file_path", &a.join("new.rs"))),
            None
        );
        assert_eq!(
            decide(&f, &write("Edit", "file_path", &a.join("x/y/z.rs"))),
            None
        );
        for (tool, key, p) in [
            ("Write", "file_path", b.join("code.txt")),
            ("Edit", "file_path", b.join("code.txt")),
            ("MultiEdit", "file_path", b.join("code.txt")),
            ("NotebookEdit", "notebook_path", b.join("nb.ipynb")),
            ("Write", "file_path", r.join("top.txt")),
            ("Write", "file_path", b.join("later/new.txt")),
        ] {
            let why = decide(&f, &write(tool, key, &p)).expect("denied");
            assert!(why.starts_with(DENY_PREFIX), "{why}");
        }
        // Outside every denied root: not this fence's business.
        assert_eq!(
            decide(&f, &write("Write", "file_path", &home.join("x"))),
            None
        );
    }

    #[test]
    fn dot_dot_and_symlinks_resolve_before_the_check() {
        let (_t, r, a, b, home) = layout();
        let f = Fence::new(&guard(&[&r], &[&a]), &a, &home).unwrap();
        let up = a.join("../b/code.txt");
        assert!(decide(&f, &write("Write", "file_path", &up)).is_some());
        let up_new = a.join("new/../../b/code.txt");
        assert!(decide(&f, &write("Write", "file_path", &up_new)).is_some());
        // A symlink in the agent's own cwd to the other dir, and to a file.
        std::os::unix::fs::symlink(&b, a.join("to-b")).unwrap();
        std::fs::write(b.join("code.txt"), "B").unwrap();
        std::os::unix::fs::symlink(b.join("code.txt"), a.join("to-code")).unwrap();
        assert!(decide(&f, &write("Write", "file_path", &a.join("to-b/code.txt"))).is_some());
        assert!(decide(&f, &write("Write", "file_path", &a.join("to-code"))).is_some());
        // A dangling symlink: a write would create its target.
        std::os::unix::fs::symlink(b.join("not-yet"), a.join("dangling")).unwrap();
        assert!(decide(&f, &write("Write", "file_path", &a.join("dangling"))).is_some());
        // Its own files stay writable.
        std::fs::write(a.join("own.txt"), "A").unwrap();
        assert_eq!(
            decide(&f, &write("Write", "file_path", &a.join("own.txt"))),
            None
        );
    }

    #[test]
    fn a_call_without_an_absolute_path_is_refused() {
        let (_t, r, a, _b, home) = layout();
        let f = Fence::new(&guard(&[&r], &[&a]), &a, &home).unwrap();
        assert!(decide(&f, &json!({"tool_name": "Write", "tool_input": {}})).is_some());
        assert!(decide(&f, &json!({"tool_name": "Write"})).is_some());
        assert!(decide(
            &f,
            &json!({"tool_name": "Write", "tool_input": {"file_path": "rel.txt"}})
        )
        .is_some());
        assert!(decide(&f, &serde_json::Value::Null).is_some());
    }

    #[test]
    fn a_fence_is_checked_at_create() {
        let (_t, r, a, b, home) = layout();
        assert!(Fence::new(&guard(&[], &[&a]), &a, &home).is_err());
        assert!(Fence::new(&guard(&[Path::new("/")], &[&a]), &a, &home).is_err());
        assert!(Fence::new(&guard(&[Path::new("rel")], &[&a]), &a, &home).is_err());
        assert!(Fence::new(&guard(&[&r.join("gone")], &[&a]), &a, &home).is_err());
        // The own cwd fenced off.
        assert!(Fence::new(&guard(&[&r], &[&b]), &a, &home).is_err());
        // An allowed subtree that holds the engine home.
        let base = r.parent().unwrap();
        assert!(Fence::new(&guard(&[&r], &[&a, base]), &a, &home).is_err());
        // Realpaths, sorted and deduplicated; a round trip keeps them.
        let f = Fence::new(&guard(&[&r, &a.join("..")], &[&a]), &a, &home).unwrap();
        assert_eq!(f.deny_write_under, vec![r.clone()]);
        assert_eq!(Fence::from_wire(&f.to_wire()), f);
    }

    #[test]
    fn resolve_keeps_a_missing_tail_and_folds_dot_dot() {
        let (_t, r, a, _b, _home) = layout();
        assert_eq!(
            resolve_target(&a.join("./x/../y/z.txt")).unwrap(),
            a.join("y/z.txt")
        );
        assert_eq!(
            resolve_target(&a.join("../..")).unwrap(),
            r.parent().unwrap()
        );
        assert_eq!(
            resolve_target(Path::new("/../..")).unwrap(),
            PathBuf::from("/")
        );
        assert!(resolve_target(Path::new("rel")).is_err());
    }
}
