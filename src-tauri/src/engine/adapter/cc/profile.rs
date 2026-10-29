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
//!   `adapter::claude_hook_settings`).
//! - The cwd is a realpath before anything is built from it (S:69).
//! - Three layers block writes to the cwd's `.claude/` (C1a-31, see
//!   `deny_rules`): `permissions.deny`, `sandbox.filesystem.denyWrite` and
//!   `allowUnsandboxedCommands:false`. The price, accepted in D28 Q2=A: a
//!   free agent cannot change the repo's own `.claude/`, skills and
//!   commands included. The first two layers also cover the engine's
//!   `bin/` and `spawn/` and its `standing-orders.md` (C1a-25).
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
    /// The `--session-id` of this spawn.
    pub session_id: String,
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
        })
    }
}

/// Realpath of a spawn cwd (S:69). Every path rule is built from it, so a
/// symlinked cwd cannot point the rules at another directory. It must be a
/// directory and not `/`.
pub fn resolve_cwd(cwd: &Path) -> Result<PathBuf, String> {
    let real = cwd
        .canonicalize()
        .map_err(|e| format!("cwd {:?}: {}", cwd, e))?;
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
/// parent of `bin`, as `agent_bin::installed_path` lays it out.
pub fn engine_dirs(p: &Profile) -> Vec<PathBuf> {
    let bin = p.agent_bin.parent();
    bin.into_iter()
        .map(Path::to_path_buf)
        .chain(bin.and_then(Path::parent).map(|h| h.join(SPAWN_DIR)))
        .collect()
}

/// Engine-owned files the agent must not write: `<home>/standing-orders.md`,
/// which the engine sends to every agent (A:545, C1a-25).
pub fn engine_files(p: &Profile) -> Vec<PathBuf> {
    let home = p.agent_bin.parent().and_then(Path::parent);
    home.map(|h| h.join(STANDING_ORDERS_NAME))
        .into_iter()
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
/// Layer 2 is `sandbox.filesystem.denyWrite` (`deny_write`) and layer 3
/// `allowUnsandboxedCommands:false` (`settings`). No layer is enough alone.
pub fn deny_rules(p: &Profile) -> Vec<String> {
    let cwd = p.cwd.display().to_string();
    let mut v: Vec<String> = engine_dirs(p)
        .iter()
        .map(|d| format!("Edit(/{}/**)", d.display()))
        .collect();
    v.extend(
        engine_files(p)
            .iter()
            .map(|f| format!("Edit(/{})", f.display())),
    );
    v.extend(
        SETTINGS_FILES
            .iter()
            .map(|f| format!("Edit(/{}/{})", cwd, f)),
    );
    v.push(format!("Edit(/{}/.claude/**)", cwd));
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
/// refuses `mv .claude` and a `.claude` symlink in its place.
pub fn deny_write(p: &Profile) -> Vec<String> {
    engine_dirs(p)
        .into_iter()
        .chain(engine_files(p))
        .chain([claude_dir(p)])
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
    home.join(SPAWN_DIR)
        .join(format!("{}-{}.json", p.agent_id, p.generation))
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
/// auto` plus `ISOLATE_ARGS`. No model: the user's default applies.
pub fn argv(p: &Profile, claude: &Path, settings_file: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec![
        claude.into(),
        "--session-id".into(),
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
            assert_eq!(allow_rules(&p), [rule.clone()]);
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
            // Every rule is rooted at `/`, never at the settings source.
            for r in deny {
                assert!(r.as_str().unwrap().starts_with("Edit(//"), "{}", r);
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
            assert_eq!(
                s["sandbox"]["filesystem"]["denyWrite"],
                json!([
                    "/b/Application Support/c9/bin",
                    "/b/Application Support/c9/spawn",
                    "/b/Application Support/c9/standing-orders.md",
                    format!("{}/.claude", cwd)
                ])
            );
        });
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
        assert_eq!(
            s["permissions"]["deny"],
            json!([
                "Edit(//b/Application Support/c9/bin/**)",
                "Edit(//b/Application Support/c9/spawn/**)",
                "Edit(//b/Application Support/c9/standing-orders.md)",
                format!("Edit(/{}/.claude/settings.json)", cwd),
                format!("Edit(/{}/.claude/settings.local.json)", cwd),
                format!("Edit(/{}/.claude/**)", cwd),
            ])
        );
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
}
