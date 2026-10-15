//! Decide how to jump back to a session.
//!
//! The caller (`actions.rs`) gathers process facts — the session's
//! environment, its ancestor processes, the GUI app that owns it, and any
//! tmux/Herdr client — into a [`Location`]. [`plan_jump`] turns that into the
//! exact commands and AppleScript to run. Nothing here touches the system, so
//! the decisions and generated command strings are unit-tested directly.

use crate::actions::OpenTarget;

/// Where a session lives, from the innermost surface outwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A GUI app owns the session directly (no multiplexer in between).
    App(AppSurface),
    /// The session runs in a tmux pane.
    Tmux {
        bin: String,
        socket: String,
        /// Pane id such as `%3` (from `TMUX_PANE`).
        pane: String,
        /// Client tty to move onto the pane's session, when the chosen client
        /// currently shows another session.
        switch_client: Option<String>,
        /// The terminal (or multiplexer) that shows the tmux client.
        outer: Option<Box<Location>>,
    },
    /// The session runs in a Herdr pane.
    Herdr {
        bin: String,
        socket: Option<String>,
        /// Pane id such as `w1:p2` (from `HERDR_PANE_ID`).
        pane: String,
        /// The terminal (or multiplexer) that shows the Herdr client.
        outer: Option<Box<Location>>,
    },
}

/// A GUI app that hosts a session or a multiplexer client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppSurface {
    pub app: String,
    /// Controlling tty, used to pick the exact Terminal.app / iTerm2 tab.
    pub tty: Option<String>,
    /// Project-opener CLI (VS Code family, Zed).
    pub cli: Option<String>,
    /// URL that focuses a JetBrains project window.
    pub jetbrains_url: Option<String>,
    /// Directory to match against Ghostty terminals.
    pub ghostty_cwd: Option<String>,
    /// Project path to open with `cli`.
    pub project_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JumpStep {
    /// Run a program. On failure, activate `fallback_app` if set, otherwise fail.
    Run {
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        fallback_app: Option<String>,
    },
    /// Run AppleScript. When `require_found`, the script must print `found`.
    AppleScript { script: String, require_found: bool },
    /// Bring an app to the front and restore its minimized windows.
    Activate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpPlan {
    pub target: OpenTarget,
    pub steps: Vec<JumpStep>,
}

/// Which multiplexer owns the session's pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    Tmux,
    Herdr,
}

pub fn plan_jump(location: &Location) -> JumpPlan {
    match location {
        Location::App(surface) => plan_app(surface),
        Location::Tmux {
            bin,
            socket,
            pane,
            switch_client,
            outer,
        } => {
            let tmux = |args: &[&str]| JumpStep::Run {
                program: bin.clone(),
                args: ["-S", socket.as_str()]
                    .iter()
                    .chain(args)
                    .map(|s| s.to_string())
                    .collect(),
                env: Vec::new(),
                fallback_app: None,
            };
            let mut steps = vec![
                tmux(&["select-window", "-t", pane]),
                tmux(&["select-pane", "-t", pane]),
            ];
            if let Some(client) = switch_client {
                steps.push(tmux(&["switch-client", "-c", client, "-t", pane]));
            }
            with_outer(steps, outer)
        }
        Location::Herdr {
            bin,
            socket,
            pane,
            outer,
        } => {
            let env = socket
                .iter()
                .map(|s| ("HERDR_SOCKET_PATH".to_string(), s.clone()))
                .collect();
            let steps = vec![JumpStep::Run {
                program: bin.clone(),
                args: vec!["agent".into(), "focus".into(), pane.clone()],
                env,
                fallback_app: None,
            }];
            with_outer(steps, outer)
        }
    }
}

/// A multiplexer selects the exact pane; then the outer surface is focused.
fn with_outer(mut steps: Vec<JumpStep>, outer: &Option<Box<Location>>) -> JumpPlan {
    if let Some(outer) = outer {
        steps.extend(plan_jump(outer).steps);
    }
    JumpPlan {
        target: OpenTarget::Terminal,
        steps,
    }
}

fn plan_app(surface: &AppSurface) -> JumpPlan {
    let app = surface.app.as_str();
    if let Some(tty) = &surface.tty {
        if app == "Terminal" {
            return applescript(OpenTarget::Terminal, terminal_tty_script(tty), true);
        }
        if app == "iTerm" || app == "iTerm2" {
            return applescript(OpenTarget::Terminal, iterm_tty_script(tty), true);
        }
    }
    if app == "Ghostty" {
        if let Some(cwd) = &surface.ghostty_cwd {
            // Exact only when one terminal matches, so the guarantee is the app.
            return applescript(OpenTarget::Application, ghostty_cwd_script(cwd), false);
        }
    }
    if let Some(url) = &surface.jetbrains_url {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        return JumpPlan {
            target: OpenTarget::Project,
            steps: vec![JumpStep::Run {
                program: opener.into(),
                args: vec![url.clone()],
                env: Vec::new(),
                fallback_app: Some(app.into()),
            }],
        };
    }
    if let (Some(cli), Some(path)) = (&surface.cli, &surface.project_path) {
        let mut args = Vec::new();
        if matches!(app, "Visual Studio Code" | "Cursor" | "Windsurf") {
            // Reuse the window that already has the project open.
            args.push("-r".to_string());
            args.push("-g".to_string());
        }
        args.push(path.clone());
        return JumpPlan {
            target: OpenTarget::Project,
            steps: vec![JumpStep::Run {
                program: cli.clone(),
                args,
                env: Vec::new(),
                fallback_app: None,
            }],
        };
    }
    JumpPlan {
        target: OpenTarget::Application,
        steps: vec![JumpStep::Activate(app.into())],
    }
}

fn applescript(target: OpenTarget, script: String, require_found: bool) -> JumpPlan {
    JumpPlan {
        target,
        steps: vec![JumpStep::AppleScript {
            script,
            require_found,
        }],
    }
}

/// Quote a value as an AppleScript string literal.
pub fn applescript_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// iTerm2: match the session by tty, select it, raise its window. The tab is
/// selected first: `select t` focuses the pane the tab last had, so selecting
/// it after the session would undo the pane choice in a split tab.
pub fn iterm_tty_script(tty: &str) -> String {
    format!(
        r#"
        tell application "iTerm2"
            activate
            repeat with w in windows
                repeat with t in tabs of w
                    repeat with s in sessions of t
                        if tty of s ends with {tty} then
                            select t
                            select s
                            set index of w to 1
                            return "found"
                        end if
                    end repeat
                end repeat
            end repeat
            return "not found"
        end tell
        "#,
        tty = applescript_quote(tty)
    )
}

/// Terminal.app: select the tab by tty and raise its window (by stable id)
/// BEFORE `activate`. Activating first surfaces whatever window was already
/// frontmost, and a later raise of a background window does not reliably
/// re-order the display. `miniaturized` is a native window property, so
/// restoring a Dock-minimized window needs no Accessibility permission.
pub fn terminal_tty_script(tty: &str) -> String {
    format!(
        r#"
        tell application "Terminal"
            set targetWinId to missing value
            repeat with w in windows
                repeat with t in tabs of w
                    if tty of t ends with {tty} then
                        set selected of t to true
                        set targetWinId to id of w
                        exit repeat
                    end if
                end repeat
                if targetWinId is not missing value then exit repeat
            end repeat
            if targetWinId is missing value then return "not found"
            try
                if miniaturized of (window id targetWinId) then set miniaturized of (window id targetWinId) to false
            end try
            set index of (window id targetWinId) to 1
            set frontmost of (window id targetWinId) to true
            activate
            return "found"
        end tell
        "#,
        tty = applescript_quote(tty)
    )
}

/// Ghostty (1.3+): its terminals expose no tty, only a working directory.
/// Focus the terminal only when exactly one matches; otherwise bring the app
/// to the front. Two terminals in the same directory are indistinguishable.
pub fn ghostty_cwd_script(cwd: &str) -> String {
    format!(
        r#"
        tell application "Ghostty"
            set matches to {{}}
            repeat with term in terminals
                if working directory of term is {cwd} then set end of matches to term
            end repeat
            if (count of matches) is 1 then
                focus (item 1 of matches)
                activate
                return "found"
            end if
            activate
            return "app"
        end tell
        "#,
        cwd = applescript_quote(cwd)
    )
}

/// Socket path from `TMUX` (`<socket>,<server pid>,<session index>`).
pub fn tmux_socket(tmux_env: &str) -> Option<&str> {
    let socket = tmux_env.split(',').next()?;
    (!socket.is_empty()).then_some(socket)
}

/// Server pid from `TMUX`.
pub fn tmux_server_pid(tmux_env: &str) -> Option<u32> {
    tmux_env.split(',').nth(1)?.parse().ok()
}

/// Pick the multiplexer whose server is the nearest ancestor of the session.
///
/// Environment variables leak: a Herdr server started inside tmux passes
/// `TMUX` on to every Herdr pane. Only the process tree says which one
/// actually owns the session's pane. `ancestors` is ordered from the session
/// outwards as `(pid, command name)`.
pub fn innermost_multiplexer(
    ancestors: &[(u32, String)],
    tmux_server: Option<u32>,
    herdr_pane: bool,
) -> Option<Multiplexer> {
    ancestors.iter().find_map(|(pid, comm)| {
        if tmux_server == Some(*pid) {
            Some(Multiplexer::Tmux)
        } else if herdr_pane && basename(comm) == "herdr" {
            Some(Multiplexer::Herdr)
        } else {
            None
        }
    })
}

/// Choose the tmux client to show `pane_session` on.
///
/// `clients` is `tmux list-clients -F '#{client_tty}\t#{session_name}\t#{client_activity}'`.
/// Prefer the most recently active client already on the pane's session;
/// otherwise take the most recently active client and switch it over.
/// Returns `(client tty, needs switch-client)`.
pub fn choose_tmux_client(clients: &str, pane_session: &str) -> Option<(String, bool)> {
    let parsed: Vec<(&str, &str, u64)> = clients
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let tty = parts.next()?.trim();
            let session = parts.next()?;
            let activity = parts
                .next()
                .and_then(|a| a.trim().parse().ok())
                .unwrap_or(0);
            (!tty.is_empty()).then_some((tty, session, activity))
        })
        .collect();
    let latest = |it: &mut dyn Iterator<Item = &(&str, &str, u64)>| {
        it.max_by_key(|(_, _, activity)| *activity)
            .map(|(tty, _, _)| tty.to_string())
    };
    if let Some(tty) = latest(&mut parsed.iter().filter(|(_, s, _)| *s == pane_session)) {
        return Some((tty, false));
    }
    latest(&mut parsed.iter()).map(|tty| (tty, true))
}

/// Herdr session name from its socket path: `.../sessions/<name>/herdr.sock`
/// is a named session; anything else is the default session.
pub fn herdr_session_name(socket: Option<&str>) -> Option<String> {
    let path = std::path::Path::new(socket?);
    let dir = path.parent()?;
    let sessions = dir.parent()?;
    (sessions.file_name()? == "sessions")
        .then(|| dir.file_name().map(|n| n.to_string_lossy().to_string()))
        .flatten()
}

/// Herdr client processes for a session, from `ps -axo pid=,tty=,command=`.
///
/// A client is a `herdr` process with a real tty run as `herdr`,
/// `herdr --session <name>` or `herdr session attach <name>`. Servers and
/// one-shot CLI calls are skipped.
pub fn herdr_clients(ps_output: &str, session: Option<&str>) -> Vec<(u32, String)> {
    ps_output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid: u32 = words.next()?.parse().ok()?;
            let tty = words.next()?;
            let program = words.next()?;
            if tty == "??" || tty == "?" || basename(program) != "herdr" {
                return None;
            }
            let args: Vec<&str> = words.collect();
            let named = match args.as_slice() {
                [] => None,
                ["--session", name] | ["session", "attach", name] => Some(*name),
                // Servers and one-shot CLI calls (`herdr pane list`) are not clients.
                _ => return None,
            };
            (named == session).then(|| (pid, tty.to_string()))
        })
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(program: &str, args: &[&str]) -> JumpStep {
        JumpStep::Run {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            fallback_app: None,
        }
    }

    fn app(name: &str) -> AppSurface {
        AppSurface {
            app: name.into(),
            ..Default::default()
        }
    }

    fn iterm_outer(tty: &str) -> Option<Box<Location>> {
        Some(Box::new(Location::App(AppSurface {
            tty: Some(tty.into()),
            ..app("iTerm")
        })))
    }

    /// `select t` focuses the pane the tab last had, so in a split tab it
    /// must run before `select s`, or the jump lands on the wrong pane.
    #[test]
    fn iterm_selects_the_tab_before_the_split_pane() {
        let script = iterm_tty_script("ttys060");
        let tab = script.find("select t").expect("selects the tab");
        let pane = script.find("select s").expect("selects the pane");
        assert!(tab < pane, "select t must come before select s:\n{script}");
    }

    #[test]
    fn terminal_and_iterm_with_tty_select_the_exact_tab() {
        let plan = plan_jump(&Location::App(AppSurface {
            tty: Some("ttys004".into()),
            ..app("Terminal")
        }));
        assert_eq!(plan.target, OpenTarget::Terminal);
        assert_eq!(
            plan.steps,
            vec![JumpStep::AppleScript {
                script: terminal_tty_script("ttys004"),
                require_found: true
            }]
        );
        assert!(terminal_tty_script("ttys004").contains(r#"if tty of t ends with "ttys004" then"#));

        for name in ["iTerm", "iTerm2"] {
            let plan = plan_jump(&Location::App(AppSurface {
                tty: Some("ttys009".into()),
                ..app(name)
            }));
            assert_eq!(plan.target, OpenTarget::Terminal);
            assert!(matches!(
                &plan.steps[..],
                [JumpStep::AppleScript { script, require_found: true }]
                    if script.contains(r#"if tty of s ends with "ttys009" then"#)
            ));
        }
    }

    #[test]
    fn terminal_without_tty_only_activates_the_app() {
        let plan = plan_jump(&Location::App(app("Terminal")));
        assert_eq!(plan.target, OpenTarget::Application);
        assert_eq!(plan.steps, vec![JumpStep::Activate("Terminal".into())]);
    }

    #[test]
    fn ghostty_matches_a_unique_working_directory() {
        let plan = plan_jump(&Location::App(AppSurface {
            ghostty_cwd: Some("/Users/me/my \"proj\"".into()),
            ..app("Ghostty")
        }));
        assert_eq!(plan.target, OpenTarget::Application);
        let [JumpStep::AppleScript {
            script,
            require_found: false,
        }] = &plan.steps[..]
        else {
            panic!("unexpected plan: {plan:?}");
        };
        assert!(script.contains(r#"if working directory of term is "/Users/me/my \"proj\"" then"#));
        assert!(script.contains("if (count of matches) is 1 then"));
        assert!(script.contains("focus (item 1 of matches)"));
    }

    #[test]
    fn ghostty_as_outer_terminal_only_activates() {
        // A multiplexer client's cwd says nothing about which Ghostty tab it is in.
        let plan = plan_jump(&Location::App(app("Ghostty")));
        assert_eq!(plan.steps, vec![JumpStep::Activate("Ghostty".into())]);
    }

    #[test]
    fn warp_and_unknown_terminals_only_activate() {
        for name in ["Warp", "kitty", "WezTerm", "Alacritty"] {
            let plan = plan_jump(&Location::App(AppSurface {
                tty: Some("ttys001".into()),
                project_path: Some("/p".into()),
                ..app(name)
            }));
            assert_eq!(plan.target, OpenTarget::Application, "{name}");
            assert_eq!(plan.steps, vec![JumpStep::Activate(name.into())]);
        }
    }

    #[test]
    fn vscode_family_reuses_the_project_window() {
        for name in ["Visual Studio Code", "Cursor", "Windsurf"] {
            let plan = plan_jump(&Location::App(AppSurface {
                cli: Some("/bin/code".into()),
                project_path: Some("/p/x".into()),
                ..app(name)
            }));
            assert_eq!(plan.target, OpenTarget::Project);
            assert_eq!(plan.steps, vec![run("/bin/code", &["-r", "-g", "/p/x"])]);
        }
        let zed = plan_jump(&Location::App(AppSurface {
            cli: Some("/zed/cli".into()),
            project_path: Some("/p/x".into()),
            ..app("Zed")
        }));
        assert_eq!(zed.steps, vec![run("/zed/cli", &["/p/x"])]);
    }

    #[test]
    fn jetbrains_opens_the_project_url_and_falls_back_to_the_app() {
        let plan = plan_jump(&Location::App(AppSurface {
            jetbrains_url: Some("idea://open?file=/p".into()),
            ..app("IntelliJ IDEA")
        }));
        assert_eq!(plan.target, OpenTarget::Project);
        let [JumpStep::Run {
            args, fallback_app, ..
        }] = &plan.steps[..]
        else {
            panic!("unexpected plan: {plan:?}");
        };
        assert_eq!(args, &vec!["idea://open?file=/p".to_string()]);
        assert_eq!(fallback_app.as_deref(), Some("IntelliJ IDEA"));
    }

    #[test]
    fn tmux_selects_window_and_pane_then_focuses_the_client_terminal() {
        let plan = plan_jump(&Location::Tmux {
            bin: "/opt/homebrew/bin/tmux".into(),
            socket: "/private/tmp/tmux-501/work".into(),
            pane: "%3".into(),
            switch_client: None,
            outer: iterm_outer("ttys007"),
        });
        assert_eq!(plan.target, OpenTarget::Terminal);
        let sock = "/private/tmp/tmux-501/work";
        assert_eq!(
            plan.steps[..2],
            [
                run(
                    "/opt/homebrew/bin/tmux",
                    &["-S", sock, "select-window", "-t", "%3"]
                ),
                run(
                    "/opt/homebrew/bin/tmux",
                    &["-S", sock, "select-pane", "-t", "%3"]
                ),
            ]
        );
        assert!(matches!(
            &plan.steps[2],
            JumpStep::AppleScript { script, require_found: true } if script.contains("\"ttys007\"")
        ));
        assert_eq!(plan.steps.len(), 3);
    }

    #[test]
    fn tmux_switches_a_client_that_shows_another_session() {
        let plan = plan_jump(&Location::Tmux {
            bin: "tmux".into(),
            socket: "/s".into(),
            pane: "%9".into(),
            switch_client: Some("/dev/ttys002".into()),
            outer: None,
        });
        assert_eq!(
            plan.steps.last(),
            Some(&run(
                "tmux",
                &[
                    "-S",
                    "/s",
                    "switch-client",
                    "-c",
                    "/dev/ttys002",
                    "-t",
                    "%9"
                ]
            ))
        );
    }

    #[test]
    fn herdr_focuses_the_agent_pane_on_its_socket_then_the_outer_terminal() {
        let plan = plan_jump(&Location::Herdr {
            bin: "/opt/homebrew/bin/herdr".into(),
            socket: Some("/Users/me/.config/herdr/herdr.sock".into()),
            pane: "wP:p12".into(),
            outer: Some(Box::new(Location::App(app("Ghostty")))),
        });
        assert_eq!(plan.target, OpenTarget::Terminal);
        assert_eq!(
            plan.steps,
            vec![
                JumpStep::Run {
                    program: "/opt/homebrew/bin/herdr".into(),
                    args: vec!["agent".into(), "focus".into(), "wP:p12".into()],
                    env: vec![(
                        "HERDR_SOCKET_PATH".into(),
                        "/Users/me/.config/herdr/herdr.sock".into()
                    )],
                    fallback_app: None,
                },
                JumpStep::Activate("Ghostty".into()),
            ]
        );
    }

    #[test]
    fn nested_tmux_inside_herdr_selects_both_panes() {
        let plan = plan_jump(&Location::Tmux {
            bin: "tmux".into(),
            socket: "/s".into(),
            pane: "%1".into(),
            switch_client: None,
            outer: Some(Box::new(Location::Herdr {
                bin: "herdr".into(),
                socket: None,
                pane: "w1:p2".into(),
                outer: iterm_outer("ttys003"),
            })),
        });
        let programs: Vec<&str> = plan
            .steps
            .iter()
            .map(|s| match s {
                JumpStep::Run { program, .. } => program.as_str(),
                JumpStep::AppleScript { .. } => "osascript",
                JumpStep::Activate(_) => "activate",
            })
            .collect();
        assert_eq!(programs, ["tmux", "tmux", "herdr", "osascript"]);
    }

    #[test]
    fn applescript_quote_escapes_quotes_and_backslashes() {
        assert_eq!(applescript_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn tmux_env_parsing() {
        let env = "/private/tmp/tmux-501/default,4242,0";
        assert_eq!(tmux_socket(env), Some("/private/tmp/tmux-501/default"));
        assert_eq!(tmux_server_pid(env), Some(4242));
        assert_eq!(tmux_socket(""), None);
        assert_eq!(tmux_server_pid("/s"), None);
    }

    #[test]
    fn innermost_multiplexer_follows_the_process_tree() {
        let tree = |names: &[(u32, &str)]| -> Vec<(u32, String)> {
            names.iter().map(|(p, n)| (*p, n.to_string())).collect()
        };
        // Claude in a Herdr pane; Herdr's server was started inside tmux, so
        // TMUX leaked into the pane env. Herdr is nearer.
        let in_herdr = tree(&[
            (10, "claude"),
            (9, "-zsh"),
            (8, "/opt/homebrew/bin/herdr"),
            (7, "tmux"),
        ]);
        assert_eq!(
            innermost_multiplexer(&in_herdr, Some(7), true),
            Some(Multiplexer::Herdr)
        );
        // Claude in a tmux pane whose server runs inside a Herdr pane.
        let in_tmux = tree(&[(10, "claude"), (9, "zsh"), (7, "tmux"), (8, "herdr")]);
        assert_eq!(
            innermost_multiplexer(&in_tmux, Some(7), true),
            Some(Multiplexer::Tmux)
        );
        // Stale TMUX: the server pid is not an ancestor.
        assert_eq!(innermost_multiplexer(&in_tmux, Some(99), false), None);
        // A herdr ancestor without HERDR_PANE_ID is not used.
        assert_eq!(innermost_multiplexer(&in_herdr, None, false), None);
    }

    #[test]
    fn choose_tmux_client_prefers_one_already_on_the_session() {
        let clients =
            "/dev/ttys001\twork\t100\n/dev/ttys002\tother\t300\n/dev/ttys003\twork\t200\n";
        assert_eq!(
            choose_tmux_client(clients, "work"),
            Some(("/dev/ttys003".into(), false))
        );
        assert_eq!(
            choose_tmux_client(clients, "detached"),
            Some(("/dev/ttys002".into(), true))
        );
        assert_eq!(choose_tmux_client("", "work"), None);
    }

    #[test]
    fn herdr_session_name_from_socket() {
        assert_eq!(
            herdr_session_name(Some("/Users/me/.config/herdr/sessions/demo/herdr.sock")),
            Some("demo".into())
        );
        assert_eq!(
            herdr_session_name(Some("/Users/me/.config/herdr/herdr.sock")),
            None
        );
        assert_eq!(herdr_session_name(None), None);
    }

    #[test]
    fn herdr_clients_skip_servers_and_other_sessions() {
        let ps = "\
 1897 ??       /opt/homebrew/bin/herdr server
43431 ??       herdr --session demo server
 5001 ttys004  herdr
 5002 ttys005  /opt/homebrew/bin/herdr --session demo
 5003 ttys006  herdr session attach demo
 5004 ttys007  herdr pane list
 5005 ttys008  vim herdr
";
        assert_eq!(herdr_clients(ps, None), vec![(5001, "ttys004".into())]);
        assert_eq!(
            herdr_clients(ps, Some("demo")),
            vec![(5002, "ttys005".into()), (5003, "ttys006".into())]
        );
    }
}
