//! The Claude Code spawn profile (plan C1a-23, D28 Q2=A): the settings
//! file passed with `--settings`, the argv and the env of one free-mode
//! spawn. Ported from spike `profile.rs:45-200`.
//!
//! These are harness features that the engine sets on every spawn; c9watch
//! has no sandbox of its own:
//! - `sandbox.enabled` (D25), with `allowUnsandboxedCommands:false`
//!   (D:276) and the engine socket in `network.allowUnixSockets` (D24.7,
//!   D:249). Without that entry `connect()` fails with EPERM inside the
//!   sandbox and the agent's hooks give up silently.
//! - Permission mode `auto` plus `permissions.allow` (O1=A, D:265). The
//!   allow list names only the engine-owned absolute
//!   `<home>/bin/c9watch-agent`, never a bare name that `PATH` could
//!   resolve to a binary the agent can write.
//! - Isolation (D24.9): `--setting-sources project --strict-mcp-config
//!   --no-chrome` and `remoteControlAtStartup:false`.
//! - Identity: `--session-id <uuid>` on every spawn (D:244), and the hooks
//!   run `<home>/bin/c9watch-agent hook <event>` (D:243,
//!   `adapter::claude_hook_settings`). A boot resume (C1a-28) passes
//!   `--resume <uuid>` instead, with the same rest of the profile and a new
//!   settings file for the new generation: CC keeps the session id and the
//!   transcript (D24, M6 precheck).
//! - The cwd is a realpath before anything is built from it (S:69).
//! - Three layers block writes to the cwd's `.claude/` (C1a-31, see
//!   `deny_rules`): `permissions.deny`, `sandbox.filesystem.denyWrite` and
//!   `allowUnsandboxedCommands:false`. The price, accepted in D28 Q2=A: a
//!   free agent cannot change the repo's own `.claude/`, skills and
//!   commands included. The first two layers also cover the engine's
//!   `bin/` and `spawn/` and its `standing-orders.md` (C1a-25).
//! - The first two layers also cover the `.claude/` of every other live
//!   engine agent at spawn time (C1a-32, `Profile::with_peers`): in live
//!   run X14 one agent rewrote another agent's `settings.json` with a Bash
//!   redirect. Rules built at spawn cannot name a later agent, so
//!   `agent.create` takes one cwd at a time (M7 reviews H1).
//! - Both layers also cover the store and machine key (M7 reviews M3).
//!   The owner's `~/.claude`, `~/.claude.json` and CLAUDE.md are left to
//!   CC's own sandbox and auto-mode classifier (owner, 2026-10-30).
//! - The env carries only `C9_AGENT_ID`, `C9_AGENT_GEN` and
//!   `C9_ENGINE_SOCK`, none of them a secret. There is no `C9_AGENT_TOKEN`
//!   (D28 Q6=B) and no `C9_STANDING_ORDERS` (spike `c0-engine.rs:407`):
//!   the SessionStart hook asks the engine for its context (C1a-25).
//!
//! The spike's `verify_cmd`, task-done command, `excludedCommands` and the
//! sandbox/isolate switches are gone: a free agent has no task, the agent
//! binary runs fine under the sandbox from a path with a space (D24
//! 補充), and the engine never spawns without the profile.

use crate::cli::task::{ENV_AGENT_ID, ENV_SOCK};
use crate::engine::adapter::{claude_hook_settings, CLAUDE};
use crate::engine::driver::SpawnSpec;
use crate::engine::fsutil::ensure_private_dir;
use crate::engine::standing::STANDING_ORDERS_NAME;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// The agent's generation, as the engine stamped it. Not a secret: the
/// engine reads the generation from its live table, never from the agent.
pub const ENV_AGENT_GEN: &str = "C9_AGENT_GEN";

/// Flags that keep the user's own setup out of the agent (D24.9).
///
/// Not `local`: CC hot-reloads `.claude/settings.local.json` while the
/// session runs, and for a git worktree it also reads the main checkout's
/// copy (live run sett1). Not an empty source list: CC 2.1.312 loads the
/// project CLAUDE.md and `.claude/rules` only with the `project` source on
/// (live run sett2).
pub const ISOLATE_ARGS: [&str; 4] = [
    "--setting-sources",
    "project",
    "--strict-mcp-config",
    "--no-chrome",
];

/// `<home>/spawn`: the per-spawn settings files.
pub const SPAWN_DIR: &str = "spawn";

/// The cwd settings files CC reads and hot-reloads while a session runs
/// (`.claude/settings.json` = project source, `.local` = local source).
pub const SETTINGS_FILES: [&str; 2] = [".claude/settings.json", ".claude/settings.local.json"];

/// Characters that may not appear in the agent binary's path. The allow
/// rule is a prefix match, so a wildcard would widen it and a shell
/// metacharacter would let it cover a compound command. A space is fine
/// (`Application Support`): the hook commands quote the path.
const BAD_PATH_CHARS: &[char] = &[
    '*', '?', '[', ']', '{', '}', ';', '|', '&', '$', '`', '<', '>', '(', ')', '\\', '\n', '\r',
    '!', '~', '#', '"', '\'',
];

/// Characters that may not appear in the cwd's realpath. CC permission
/// rules are gitignore-style patterns and sandbox paths take globs, so
/// `/x/proj[v2]` would make the `.claude` rules name `/x/projv` instead
/// (reviews M3); `(` and `)` would bend an `Edit(...)` rule.
const BAD_CWD_CHARS: &[char] = &['*', '?', '[', ']', '{', '}', '(', ')', '\\', '\n', '\r'];

/// One spawn of one free agent.
#[derive(Debug, Clone)]
pub struct Profile {
    /// `<home>/bin/c9watch-agent`, absolute (`AgentBin::path`).
    pub agent_bin: PathBuf,
    /// The engine socket, as the engine bound it.
    pub sock: PathBuf,
    /// Realpath of the agent's cwd.
    pub cwd: PathBuf,
    pub agent_id: String,
    pub generation: i64,
    /// The `--session-id` of this spawn, or the session it resumes.
    pub session_id: String,
    /// Resume `session_id` (`--resume`) instead of starting it.
    pub resume: bool,
    /// Realpath cwds of the other live engine agents at spawn time. Their
    /// `.claude/` gets the same deny rules as this agent's own (C1a-32).
    pub peers: Vec<PathBuf>,
}

impl Profile {
    /// Check every input and realpath `cwd`. `session_id` must be a uuid,
    /// as CC requires for `--session-id`.
    pub fn new(
        agent_bin: &Path,
        sock: &Path,
        cwd: &Path,
        agent_id: &str,
        generation: i64,
        session_id: &str,
    ) -> Result<Profile, String> {
        check_agent_bin(agent_bin)?;
        if !sock.is_absolute() {
            return Err(format!("engine socket {:?} is not absolute", sock));
        }
        check_agent_id(agent_id)?;
        uuid::Uuid::parse_str(session_id)
            .map_err(|e| format!("session id {:?} is not a uuid: {}", session_id, e))?;
        Ok(Profile {
            agent_bin: agent_bin.to_path_buf(),
            sock: sock.to_path_buf(),
            cwd: resolve_cwd(cwd)?,
            agent_id: agent_id.into(),
            generation,
            session_id: session_id.into(),
            resume: false,
            peers: Vec::new(),
        })
    }

    /// Also deny writes to the `.claude/` of these cwds: the other live
    /// engine agents (C1a-32, plan §6 item 6). Live run X14 row 8: agent B
    /// overwrote agent A's `.claude/settings.json` with a Bash redirect;
    /// B's rules named only B's cwd, the sandbox allowed the path and the
    /// classifier let it through. Peers equal to this cwd, relative paths
    /// and paths a rule would read as a pattern are left out.
    pub fn with_peers(mut self, peers: impl IntoIterator<Item = PathBuf>) -> Profile {
        let mut v: Vec<PathBuf> = peers
            .into_iter()
            .filter(|p| p.is_absolute() && *p != self.cwd && p.parent().is_some())
            .filter(|p| {
                p.to_str()
                    .is_some_and(|s| !s.chars().any(|c| BAD_CWD_CHARS.contains(&c)))
            })
            .collect();
        v.sort();
        v.dedup();
        self.peers = v;
        self
    }

    /// The same spawn, resuming `session_id` (C1a-28).
    pub fn resuming(mut self) -> Profile {
        self.resume = true;
        self
    }
}

/// Realpath of a spawn cwd (S:69). Every path rule is built from it, so a
/// symlinked cwd cannot point the rules at another directory. It must be a
/// directory, not `/`, UTF-8 and free of `BAD_CWD_CHARS`.
pub fn resolve_cwd(cwd: &Path) -> Result<PathBuf, String> {
    let real = cwd
        .canonicalize()
        .map_err(|e| format!("cwd {:?}: {}", cwd, e))?;
    let s = real
        .to_str()
        .ok_or_else(|| format!("cwd {:?} is not UTF-8", real))?;
    if let Some(c) = s.chars().find(|c| BAD_CWD_CHARS.contains(c)) {
        return Err(format!(
            "cwd {:?} has {:?}, which a permission rule reads as a pattern",
            s, c
        ));
    }
    if !real.is_dir() {
        return Err(format!("cwd {:?} is not a directory", real));
    }
    if real.parent().is_none() {
        return Err("cwd must not be /".into());
    }
    Ok(real)
}

/// The agent binary must be an absolute path with no wildcard or shell
/// metacharacter in it.
pub fn check_agent_bin(bin: &Path) -> Result<(), String> {
    let s = bin
        .to_str()
        .ok_or_else(|| format!("agent binary {:?} is not UTF-8", bin))?;
    if !bin.is_absolute() {
        return Err(format!("agent binary must be an absolute path: {:?}", s));
    }
    if let Some(b) = s.chars().find(|c| BAD_PATH_CHARS.contains(c)) {
        return Err(format!("agent binary path has {:?}: {:?}", b, s));
    }
    Ok(())
}

/// Agent ids name a file in `<home>/spawn`.
fn check_agent_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("agent id {:?} has unsupported characters", id));
    }
    Ok(())
}

/// CC permission rules. `Bash(<prefix>:*)` is a prefix match. Only the
/// engine's agent binary, by absolute path. Under the sandbox CC sends
/// every Bash call to the auto-mode classifier anyway (D24 corrections),
/// so this rule matters for the unsandboxed path.
pub fn allow_rules(p: &Profile) -> Vec<String> {
    vec![format!("Bash({}:*)", p.agent_bin.display())]
}

/// Engine-owned directories the agent must not write: `<home>/bin` (the
/// agent binary) and `<home>/spawn` (its own settings file). `home` is the
/// parent of `bin`, as `agent_bin::installed_path` lays it out. A
/// `live-test` build adds `<home>/live-test`: the engine types the bytes of
/// a file there into an agent's PTY, so a write there is a `!` command
/// outside the sandbox, into any agent's terminal (M7 reviews M3).
pub fn engine_dirs(p: &Profile) -> Vec<PathBuf> {
    let bin = p.agent_bin.parent();
    let home = bin.and_then(Path::parent);
    let mut v: Vec<PathBuf> = bin
        .into_iter()
        .map(Path::to_path_buf)
        .chain(home.map(|h| h.join(SPAWN_DIR)))
        .collect();
    #[cfg(feature = "live-test")]
    v.extend(home.map(|h| h.join(crate::engine::live_test::DIR)));
    v.sort();
    v.dedup();
    v
}

/// Engine-owned files the agent must not write: `<home>/standing-orders.md`,
/// which the engine sends to every agent (A:545, C1a-25), and the store
/// (`engine.db` with its WAL files, the audit rows among them) and
/// `machine.key` (M7 reviews M3). The socket is not here: the agent must
/// connect to it.
pub fn engine_files(p: &Profile) -> Vec<PathBuf> {
    let home = p.agent_bin.parent().and_then(Path::parent);
    let db = crate::engine::db::DB_NAME;
    home.into_iter()
        .flat_map(|h| {
            [
                h.join(STANDING_ORDERS_NAME),
                h.join(db),
                h.join(format!("{}-wal", db)),
                h.join(format!("{}-shm", db)),
                h.join(crate::engine::machine::KEY_NAME),
            ]
        })
        .collect()
}

/// Layer 1, deny rules for CC's own file tools. `Edit` covers every file
/// tool; CC 2.1.312 warns that a `Write(path)` rule is not matched by file
/// permission checks. Paths use the `//abs` form: CC roots `//x` at `/`,
/// but roots a single-slash `/x` at the settings source's own root, so
/// `Edit(/abs/...)` would not name the absolute path.
///
/// 1. The engine directories and the standing orders. Not a boundary on
///    their own (the engine checks the agent binary's sha anyway), but
///    they stop the obvious Edit/Write path.
/// 2. The cwd's settings files. Live run sett1 (auto + sandbox): the
///    auto-mode classifier allowed a Write of `.claude/settings.local.json`
///    and CC hot-reloaded it. The sandbox's built-in write-deny of settings
///    files covers only Bash (spike `profile.rs:85`).
/// 3. Everything else under the cwd's `.claude/`. Live run setB: CC
///    hot-reloads a `.claude/settings.json` written from outside, and the
///    project source also loads `.claude/skills`, `agents` and `rules`.
///    Live run setC: `cp`, `python3` and `mv .claude` writes there were
///    stopped only by the classifier, which is a judgement, not a rule.
///
/// 4. The same for every peer cwd (`Profile::peers`, C1a-32): one agent
///    must not rewrite another agent's settings, which CC hot-reloads.
///    Only the agents live at this spawn; `create` therefore refuses a
///    second cwd while an agent is live (M7 reviews H1).
///
/// No rule names the owner's `~/.claude` or `~/.claude.json`: CC's own
/// sandbox and auto-mode classifier guard them (owner, 2026-10-30).
///
/// Layer 2 is `sandbox.filesystem.denyWrite` (`deny_write`) and layer 3
/// `allowUnsandboxedCommands:false` (`settings`). No layer is enough alone.
pub fn deny_rules(p: &Profile) -> Vec<String> {
    let mut v: Vec<String> = engine_dirs(p)
        .iter()
        .map(|d| format!("Edit(/{}/**)", d.display()))
        .collect();
    v.extend(
        engine_files(p)
            .iter()
            .map(|f| format!("Edit(/{})", f.display())),
    );
    for dir in std::iter::once(&p.cwd).chain(&p.peers) {
        let cwd = dir.display().to_string();
        v.extend(
            SETTINGS_FILES
                .iter()
                .map(|f| format!("Edit(/{}/{})", cwd, f)),
        );
        v.push(format!("Edit(/{}/.claude/**)", cwd));
    }
    v
}

/// The cwd's `.claude` directory.
pub fn claude_dir(p: &Profile) -> PathBuf {
    p.cwd.join(".claude")
}

/// Layer 2, `sandbox.filesystem.denyWrite` for sandboxed Bash: the engine
/// directories and files, and the `.claude` directory itself, not only its
/// files. A
/// sandbox path is a subtree, so this covers both settings files and also
/// refuses `mv .claude` and a `.claude` symlink in its place. The peers'
/// `.claude` directories too (C1a-32).
pub fn deny_write(p: &Profile) -> Vec<String> {
    engine_dirs(p)
        .into_iter()
        .chain(engine_files(p))
        .chain([claude_dir(p)])
        .chain(p.peers.iter().map(|d| d.join(".claude")))
        .map(|d| d.display().to_string())
        .collect()
}

/// `sandbox.network.allowUnixSockets`: the socket as bound and, when it
/// differs, as a realpath. A home below `/tmp` is `/private/tmp` on macOS
/// (D24 補充); the agent connects through `C9_ENGINE_SOCK`, and the
/// sandbox may see either form.
pub fn unix_sockets(p: &Profile) -> Vec<String> {
    let given = p.sock.display().to_string();
    let mut v = vec![given.clone()];
    let real = p
        .sock
        .parent()
        .and_then(|d| d.canonicalize().ok())
        .zip(p.sock.file_name())
        .map(|(d, n)| d.join(n).display().to_string());
    if let Some(r) = real.filter(|r| *r != given) {
        v.push(r);
    }
    v
}

/// The `--settings` content.
pub fn settings(p: &Profile) -> Value {
    let mut s = claude_hook_settings(&p.agent_bin);
    // Pinned so the screen detector sees the layout it knows (D24.9).
    s["tui"] = json!("fullscreen");
    // Pinned so a mail paste is text, never vim NORMAL-mode commands. A
    // `--settings` value beats the project's `editorMode` (live run, CC
    // 2.1.314, C1b-M1-3), and vim NORMAL mode shows nothing on screen to
    // detect it by.
    s["editorMode"] = json!("normal");
    s["permissions"] = json!({ "allow": allow_rules(p), "deny": deny_rules(p) });
    // Layer 3, allowUnsandboxedCommands false: no `dangerouslyDisableSandbox`
    // retry, so Bash cannot step around the write-deny (honored from a
    // `--settings` file, CC 2.1.312).
    s["sandbox"] = json!({
        "enabled": true,
        "autoAllowBashIfSandboxed": true,
        "allowUnsandboxedCommands": false,
        "network": { "allowUnixSockets": unix_sockets(p) },
        "filesystem": { "denyWrite": deny_write(p) },
    });
    s["remoteControlAtStartup"] = json!(false);
    s
}

/// `<home>/spawn/<agent_id>-<generation>.json`
pub fn settings_path(home: &Path, p: &Profile) -> PathBuf {
    settings_path_of(home, &p.agent_id, p.generation)
}

/// `settings_path` by agent id and generation, for removing the file of a
/// retired or failed agent.
pub fn settings_path_of(home: &Path, agent_id: &str, generation: i64) -> PathBuf {
    home.join(SPAWN_DIR)
        .join(format!("{}-{}.json", agent_id, generation))
}

/// Write `settings(p)` to `settings_path`, mode 0600 in a 0700 directory,
/// through a temp file and a rename.
pub fn write_settings(home: &Path, p: &Profile) -> Result<PathBuf, String> {
    let dir = home.join(SPAWN_DIR);
    ensure_private_dir(&dir)?;
    let path = settings_path(home, p);
    let tmp = dir.join(format!(".{}-{}.tmp", p.agent_id, p.generation));
    let _ = std::fs::remove_file(&tmp);
    let body = serde_json::to_vec_pretty(&settings(p)).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|e| format!("Failed to create {:?}: {}", tmp, e))?;
    f.write_all(&body)
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("Failed to write {:?}: {}", tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Failed to rename to {:?}: {}", path, e))?;
    Ok(path)
}

/// argv: `claude --session-id <uuid> --settings <file> --permission-mode
/// auto` plus `ISOLATE_ARGS`; `--resume <uuid>` for a resume. No model: the
/// user's default applies.
pub fn argv(p: &Profile, claude: &Path, settings_file: &Path) -> Vec<OsString> {
    let session_flag = if p.resume { "--resume" } else { "--session-id" };
    let mut v: Vec<OsString> = vec![
        claude.into(),
        session_flag.into(),
        p.session_id.clone().into(),
        "--settings".into(),
        settings_file.into(),
    ];
    v.extend(CLAUDE.permission_flags.auto.iter().map(OsString::from));
    v.extend(ISOLATE_ARGS.iter().map(OsString::from));
    v
}

/// The whole spawn: `argv` in the realpath cwd, `driver::base_env()` plus
/// the three non-secret engine vars.
pub fn spawn_spec(p: &Profile, claude: &Path, settings_file: &Path) -> SpawnSpec {
    let mut spec = SpawnSpec::new(argv(p, claude, settings_file), p.cwd.clone());
    spec.env(ENV_AGENT_ID, p.agent_id.as_str())
        .env(ENV_AGENT_GEN, p.generation.to_string())
        .env(ENV_SOCK, p.sock.as_os_str());
    spec
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::adapter::CC_HOOKS;
    use std::os::unix::fs::PermissionsExt;

    const SID: &str = "6f1c2b8e-3d4a-4b5c-9e7f-0a1b2c3d4e5f";
    const BIN: &str = "/b/Application Support/c9/bin/c9watch-agent";
    const HOME: &str = "/b/Application Support/c9";
    #[cfg(not(feature = "live-test"))]
    const ENGINE_DIRS: &[&str] = &["/bin", "/spawn"];
    #[cfg(feature = "live-test")]
    const ENGINE_DIRS: &[&str] = &["/bin", "/live-test", "/spawn"];
    const ENGINE_FILES: &[&str] = &[
        "/standing-orders.md",
        "/engine.db",
        "/engine.db-wal",
        "/engine.db-shm",
        "/machine.key",
    ];

    fn p_in(cwd: &Path) -> Profile {
        Profile::new(Path::new(BIN), Path::new("/s/e.sock"), cwd, "agt_1", 3, SID).unwrap()
    }

    fn with_cwd<T>(f: impl FnOnce(&Path, Profile) -> T) -> T {
        let t = tempfile::tempdir().unwrap();
        let p = p_in(t.path());
        f(t.path(), p)
    }

    #[test]
    fn allow_list_is_only_the_absolute_agent_bin() {
        with_cwd(|_, p| {
            let rule = format!("Bash({}:*)", BIN);
            assert_eq!(allow_rules(&p), std::slice::from_ref(&rule));
            assert!(!allow_rules(&p)
                .iter()
                .any(|r| r.starts_with("Bash(c9watch")));
            assert_eq!(settings(&p)["permissions"]["allow"], json!([rule]));
        });
    }

    #[test]
    fn agent_bin_must_be_absolute() {
        assert!(check_agent_bin(Path::new(BIN)).is_ok());
        for bad in ["c9watch-agent", "./bin/c9watch-agent", "bin/c9watch-agent"] {
            assert!(check_agent_bin(Path::new(bad)).is_err(), "{:?}", bad);
        }
        let t = tempfile::tempdir().unwrap();
        let err = Profile::new(
            Path::new("c9watch-agent"),
            Path::new("/s/e.sock"),
            t.path(),
            "agt_1",
            1,
            SID,
        )
        .unwrap_err();
        assert!(err.contains("absolute"), "{}", err);
        let err = Profile::new(
            Path::new(BIN),
            Path::new("e.sock"),
            t.path(),
            "agt_1",
            1,
            SID,
        )
        .unwrap_err();
        assert!(err.contains("socket"), "{}", err);
    }

    #[test]
    fn agent_bin_rejects_wildcards_and_metachars() {
        for bad in [
            "/b/c9*",
            "/b/c9?",
            "/b/[c9]",
            "/b/c9;id",
            "/b/c9|x",
            "/b/c9&x",
            "/b/$(id)",
            "/b/`id`",
            "/b/c9>f",
            "/b/c9\nx",
            "/b/'c9'",
            "/b/\"c9\"",
            "/b/~c9",
            "/b/c9#x",
        ] {
            assert!(check_agent_bin(Path::new(bad)).is_err(), "{:?}", bad);
        }
        let t = tempfile::tempdir().unwrap();
        for (id, sid) in [("agt 1", SID), ("../x", SID), ("", SID), ("agt_1", "nope")] {
            assert!(
                Profile::new(Path::new(BIN), Path::new("/s/e.sock"), t.path(), id, 1, sid).is_err(),
                "{:?} {:?}",
                id,
                sid
            );
        }
    }

    #[test]
    fn standing_orders_are_not_in_the_env_or_the_settings() {
        // C1a-25 sends them through SessionStart additionalContext.
        with_cwd(|cwd, p| {
            let spec = spawn_spec(&p, Path::new("/x/claude"), Path::new("/h/spawn/a.json"));
            assert!(spec.env.iter().all(|(k, _)| k != "C9_STANDING_ORDERS"));
            // The settings name the orders file only to deny writes to it.
            let mut s = settings(&p);
            s["permissions"]["deny"] = json!(null);
            s["sandbox"]["filesystem"]["denyWrite"] = json!(null);
            let s = serde_json::to_string(&s).unwrap();
            assert!(!s.contains("standing"), "{}", s);
            assert_eq!(spec.cwd, cwd.canonicalize().unwrap());
        });
    }

    #[test]
    fn env_has_no_agent_token() {
        with_cwd(|_, p| {
            let spec = spawn_spec(&p, Path::new("/x/claude"), Path::new("/h/spawn/a.json"));
            let get = |k: &str| {
                spec.env
                    .iter()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.to_str().unwrap().to_string())
            };
            assert_eq!(get("C9_AGENT_ID").as_deref(), Some("agt_1"));
            assert_eq!(get("C9_AGENT_GEN").as_deref(), Some("3"));
            assert_eq!(get("C9_ENGINE_SOCK").as_deref(), Some("/s/e.sock"));
            assert!(get("C9_AGENT_TOKEN").is_none());
            // Only the inherited keys, the terminal and the three engine
            // vars; nothing named like a secret.
            for (k, _) in &spec.env {
                let k = k.to_str().unwrap();
                assert!(
                    crate::engine::driver::INHERITED_ENV.contains(&k)
                        || [
                            "TERM",
                            "COLORTERM",
                            "C9_AGENT_ID",
                            "C9_AGENT_GEN",
                            "C9_ENGINE_SOCK"
                        ]
                        .contains(&k),
                    "{}",
                    k
                );
                assert!(!k.contains("TOKEN") && !k.contains("SECRET"), "{}", k);
            }
        });
    }

    #[test]
    fn isolate_default_settings() {
        with_cwd(|_, p| {
            let s = settings(&p);
            assert_eq!(s["remoteControlAtStartup"], false);
            assert_eq!(s["tui"], "fullscreen");
            assert_eq!(s["editorMode"], "normal");
            let hooks = s["hooks"].as_object().unwrap();
            assert_eq!(hooks.len(), CC_HOOKS.len());
            for (cc_event, event) in CC_HOOKS {
                // The path has a space, so it goes in single quotes.
                assert_eq!(
                    hooks[*cc_event][0]["hooks"][0]["command"],
                    format!("'{}' hook {} --kind claude", BIN, event),
                    "{}",
                    cc_event
                );
            }
            let a: Vec<String> = argv(&p, Path::new("/x/claude"), Path::new("/h/s.json"))
                .into_iter()
                .map(|s| s.into_string().unwrap())
                .collect();
            assert_eq!(
                a,
                [
                    "/x/claude",
                    "--session-id",
                    SID,
                    "--settings",
                    "/h/s.json",
                    "--permission-mode",
                    "auto",
                    "--setting-sources",
                    "project",
                    "--strict-mcp-config",
                    "--no-chrome",
                ]
            );
        });
    }

    #[test]
    fn sandbox_is_on_and_allows_only_the_engine_socket() {
        with_cwd(|_, p| {
            let s = settings(&p);
            assert_eq!(s["sandbox"]["enabled"], true);
            assert_eq!(s["sandbox"]["autoAllowBashIfSandboxed"], true);
            assert_eq!(s["sandbox"]["allowUnsandboxedCommands"], false);
            assert_eq!(
                s["sandbox"]["network"]["allowUnixSockets"],
                json!(["/s/e.sock"])
            );
            assert!(s["sandbox"].get("excludedCommands").is_none());
            assert!(s["sandbox"]["network"].get("allowAllUnixSockets").is_none());
        });
        // A socket below a symlinked directory is listed both ways.
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, t.path().join("link")).unwrap();
        let mut p = p_in(t.path());
        p.sock = t.path().join("link/engine.sock");
        let want_real = real.canonicalize().unwrap().join("engine.sock");
        assert_eq!(
            unix_sockets(&p),
            [
                p.sock.display().to_string(),
                want_real.display().to_string()
            ]
        );
    }

    #[test]
    fn cwd_is_a_realpath() {
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("repo dir");
        std::fs::create_dir(&real).unwrap();
        let link = t.path().join("ln");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let want = real.canonicalize().unwrap();
        assert_eq!(resolve_cwd(&link).unwrap(), want);
        assert_eq!(resolve_cwd(&real.join(".")).unwrap(), want);
        assert_eq!(p_in(&link).cwd, want);
        // A temp dir is `/private/var/...` on macOS: the realpath, never the
        // symlinked form.
        assert_eq!(p_in(t.path()).cwd, t.path().canonicalize().unwrap());
        assert!(resolve_cwd(&t.path().join("missing")).is_err());
        let file = t.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(resolve_cwd(&file).is_err());
        assert!(resolve_cwd(Path::new("/")).is_err());
        // Glob and rule characters, also when only the realpath has them.
        for name in ["proj[v2]", "a*b", "a?b", "a{b,c}", "a(b)", "a\\b"] {
            let d = t.path().join(name);
            std::fs::create_dir(&d).unwrap();
            let e = resolve_cwd(&d).unwrap_err();
            assert!(e.contains("pattern"), "{}: {}", name, e);
            let ln = t.path().join(format!("ok-{}", name.len()));
            let _ = std::fs::remove_file(&ln);
            std::os::unix::fs::symlink(&d, &ln).unwrap();
            assert!(resolve_cwd(&ln).is_err(), "{}", name);
        }
    }

    /// C1a-28: a resume is `--resume <uuid>` plus the same profile, with
    /// the settings file of the new generation.
    #[test]
    fn a_resume_keeps_the_profile_and_swaps_the_session_flag() {
        with_cwd(|_, p| {
            let strs = |v: Vec<OsString>| -> Vec<String> {
                v.into_iter().map(|s| s.into_string().unwrap()).collect()
            };
            let fresh = strs(argv(&p, Path::new("/x/claude"), Path::new("/h/s.json")));
            let r = p.clone().resuming();
            let resumed = strs(argv(&r, Path::new("/x/claude"), Path::new("/h/s.json")));
            assert_eq!(&resumed[..3], ["/x/claude", "--resume", SID]);
            assert_eq!(resumed[3..], fresh[3..]);
            assert!(!resumed.iter().any(|a| a == "--session-id"));
            // The settings do not depend on the flag.
            assert_eq!(settings(&r), settings(&p));
        });
    }

    #[test]
    fn settings_file_is_private() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonicalize().unwrap();
        let p = p_in(t.path());
        let path = write_settings(&home, &p).unwrap();
        assert_eq!(path, home.join("spawn/agt_1-3.json"));
        let mode = |q: &Path| std::fs::metadata(q).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&home.join("spawn")), 0o700);
        let back: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, settings(&p));
        // A second write of the same generation replaces the file.
        assert_eq!(write_settings(&home, &p).unwrap(), path);
    }

    #[test]
    fn agent_cannot_widen_its_own_settings() {
        with_cwd(|_, p| {
            // The local source (hot-reloaded, also read from the main
            // checkout) does not load; project stays on for CLAUDE.md.
            let a = argv(&p, Path::new("/x/claude"), Path::new("/h/s.json"));
            let i = a.iter().position(|x| x == "--setting-sources").unwrap();
            assert_eq!(a[i + 1], "project");
            // The file tools may not write the cwd's settings files.
            let cwd = p.cwd.display().to_string();
            let s = settings(&p);
            let deny = s["permissions"]["deny"].as_array().unwrap();
            for f in SETTINGS_FILES {
                assert!(
                    deny.contains(&json!(format!("Edit(/{}/{})", cwd, f))),
                    "{}",
                    f
                );
            }
            // No unsandboxed retry around the write-deny.
            assert_eq!(s["sandbox"]["allowUnsandboxedCommands"], false);
            // Every rule is rooted at `/` or `~`, never at the settings
            // source.
            for r in deny {
                let r = r.as_str().unwrap();
                assert!(
                    r.starts_with("Edit(//") || r.starts_with("Edit(~/"),
                    "{}",
                    r
                );
            }
        });
        // A relative cwd or `/` never reaches a rule.
        let err = Profile::new(
            Path::new(BIN),
            Path::new("/s/e.sock"),
            Path::new("/"),
            "agt_1",
            1,
            SID,
        )
        .unwrap_err();
        assert!(err.contains("must not be /"), "{}", err);
        assert!(resolve_cwd(Path::new(".")).unwrap().is_absolute());
    }

    #[test]
    fn agent_cannot_write_anything_under_its_claude_dir() {
        with_cwd(|_, p| {
            let cwd = p.cwd.display().to_string();
            // File tools: every path under .claude (skills, agents, rules
            // and the settings files).
            let s = settings(&p);
            let deny = s["permissions"]["deny"].as_array().unwrap();
            assert!(deny.contains(&json!(format!("Edit(/{}/.claude/**)", cwd))));
            // Sandboxed Bash: the directory itself, so a rename or a symlink
            // in its place is refused too, not only writes to the files.
            let mut want: Vec<String> = ENGINE_DIRS
                .iter()
                .map(|d| format!("{}{}", HOME, d))
                .collect();
            want.extend(ENGINE_FILES.iter().map(|f| format!("{}{}", HOME, f)));
            want.push(format!("{}/.claude", cwd));
            assert_eq!(s["sandbox"]["filesystem"]["denyWrite"], json!(want));
        });
    }

    #[test]
    fn peer_cwds_get_the_same_claude_rules() {
        // Live run X14 row 8 (C1a-32): agent B wrote agent A's settings.
        let t = tempfile::tempdir().unwrap();
        let (own, peer) = (t.path().join("b"), t.path().join("a"));
        std::fs::create_dir(&own).unwrap();
        std::fs::create_dir(&peer).unwrap();
        let peer = peer.canonicalize().unwrap();
        let p = p_in(&own).with_peers([
            peer.clone(),
            peer.clone(),
            own.canonicalize().unwrap(),
            PathBuf::from("rel/x"),
            PathBuf::from("/x/proj[v2]"),
            PathBuf::from("/"),
        ]);
        assert_eq!(p.peers, vec![peer.clone()]);
        let s = settings(&p);
        let deny: Vec<String> = serde_json::from_value(s["permissions"]["deny"].clone()).unwrap();
        let pd = peer.display();
        for want in [
            format!("Edit(/{}/.claude/settings.json)", pd),
            format!("Edit(/{}/.claude/settings.local.json)", pd),
            format!("Edit(/{}/.claude/**)", pd),
        ] {
            assert!(deny.contains(&want), "{} not in {:?}", want, deny);
        }
        let dw: Vec<String> =
            serde_json::from_value(s["sandbox"]["filesystem"]["denyWrite"].clone()).unwrap();
        assert!(
            dw.contains(&peer.join(".claude").display().to_string()),
            "{:?}",
            dw
        );
        assert!(
            dw.contains(&claude_dir(&p).display().to_string()),
            "{:?}",
            dw
        );
        // No peers: the rules are the agent's own only.
        let alone = p_in(&own);
        assert!(alone.peers.is_empty());
        assert!(!deny_rules(&alone)
            .iter()
            .any(|r| r.contains(&pd.to_string())));
    }

    #[test]
    fn settings_writes_are_blocked_in_three_layers() {
        // A cwd with a space, below a symlink: every rule names the realpath.
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("my repo");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, t.path().join("ln")).unwrap();
        let p = p_in(&t.path().join("ln"));
        let cwd = real.canonicalize().unwrap().display().to_string();
        let s: Value =
            serde_json::from_str(&serde_json::to_string(&settings(&p)).unwrap()).unwrap();

        // Layer 1: permissions.deny.
        let mut want: Vec<String> = ENGINE_DIRS
            .iter()
            .map(|d| format!("Edit(/{}{}/**)", HOME, d))
            .collect();
        want.extend(ENGINE_FILES.iter().map(|f| format!("Edit(/{}{})", HOME, f)));
        want.extend([
            format!("Edit(/{}/.claude/settings.json)", cwd),
            format!("Edit(/{}/.claude/settings.local.json)", cwd),
            format!("Edit(/{}/.claude/**)", cwd),
        ]);
        assert_eq!(s["permissions"]["deny"], json!(want));
        // Layer 2: sandbox.filesystem.denyWrite, the same places.
        let dw = s["sandbox"]["filesystem"]["denyWrite"].as_array().unwrap();
        assert!(dw.contains(&json!(format!("{}/.claude", cwd))));
        for f in SETTINGS_FILES {
            let file = format!("{}/{}", cwd, f);
            assert!(
                dw.iter()
                    .any(|d| file.starts_with(&format!("{}/", d.as_str().unwrap()))),
                "{}",
                f
            );
        }
        // Layer 3: no unsandboxed retry, with the sandbox on.
        assert_eq!(s["sandbox"]["enabled"], true);
        assert_eq!(s["sandbox"]["allowUnsandboxedCommands"], false);
        // The allow list does not reopen any of it.
        assert_eq!(
            s["permissions"]["allow"],
            json!([format!("Bash({}:*)", BIN)])
        );
    }
    #[test]
    fn engine_store_is_denied_and_owner_claude_is_not() {
        // M7 reviews M3: the store and the machine key are outside the cwd,
        // where the file tools meet only the classifier. The owner's
        // ~/.claude is left to CC (owner, 2026-10-30).
        let t = tempfile::tempdir().unwrap();
        let p = p_in(t.path());
        let deny = deny_rules(&p);
        let dw = deny_write(&p);
        for f in ["engine.db", "engine.db-wal", "engine.db-shm", "machine.key"] {
            let path = format!("{}/{}", HOME, f);
            assert!(deny.contains(&format!("Edit(/{})", path)), "{:?}", deny);
            assert!(dw.contains(&path), "{:?}", dw);
        }
        assert!(!dw.iter().any(|d| d.ends_with(".sock")));
        assert!(!deny.iter().any(|r| r.contains("~/.claude")), "{:?}", deny);
        assert!(!deny.iter().any(|r| r.contains("//**/")), "{:?}", deny);
    }

    #[cfg(feature = "live-test")]
    #[test]
    fn live_test_input_dir_is_denied_in_both_layers() {
        // M7 reviews M3: bytes written there reach a PTY as typed keys.
        let t = tempfile::tempdir().unwrap();
        let p = p_in(t.path());
        let dir = format!("{}/{}", HOME, crate::engine::live_test::DIR);
        assert!(deny_write(&p).contains(&dir));
        assert!(deny_rules(&p).contains(&format!("Edit(/{}/**)", dir)));
    }
}

/// Peer permission label of the actual spawn profile (all free agents use auto).
pub fn peer_mode() -> &'static str {
    if CLAUDE.permission_flags.auto.iter().any(|flag| {
        matches!(
            *flag,
            "bypassPermissions" | "--dangerously-skip-permissions"
        )
    }) {
        "bypass"
    } else {
        "prompting"
    }
}
