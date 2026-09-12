//! Opt-in Desktop transport bridge. No app bundle or Codex configuration edits.
use std::{
    ffi::OsString,
    fs, io,
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::net::UnixStream;
use tokio_tungstenite::{client_async_with_config, tungstenite::protocol::WebSocketConfig};

const MAX_FRAME: usize = 8 * 1024 * 1024;

pub fn root() -> PathBuf {
    PathBuf::from(format!("/tmp/c9watch-codex-{}", unsafe { libc::geteuid() }))
}

fn private_directory(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bridge directory is not private",
        ));
    }
    Ok(())
}

/// Discover only ready, user-owned endpoints; never follow symlinks or read transcripts.
pub fn endpoints() -> Vec<PathBuf> {
    endpoints_under(&root())
}

fn endpoints_under(root: &Path) -> Vec<PathBuf> {
    let Ok(m) = fs::symlink_metadata(&root) else {
        return vec![];
    };
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return vec![];
    }
    let Ok(entries) = fs::read_dir(root) else {
        return vec![];
    };
    let mut result = vec![];
    for entry in entries.flatten().take(128) {
        let path = entry.path();
        let Ok(m) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
            continue;
        }
        let ready = path.join("ready");
        let Ok(ready_meta) = fs::symlink_metadata(&ready) else {
            continue;
        };
        if !ready_meta.is_file() || ready_meta.len() > 32 {
            continue;
        }
        let Some(pid) = fs::read_to_string(&ready)
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .filter(|p| *p > 1)
        else {
            continue;
        };
        if unsafe { libc::kill(pid, 0) } != 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            continue;
        }
        let socket = path.join("server.sock");
        if let Ok(m) = fs::symlink_metadata(&socket) {
            if m.file_type().is_socket()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o077 == 0
            {
                result.push(socket);
            }
        }
    }
    result.sort();
    result
}

/// Locate the top-level CLI command without mistaking a config value for it.
fn server_index(args: &[OsString]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].to_str()?;
        if ["-c", "--config", "--enable", "--disable"].contains(&a) {
            i += 2;
            continue;
        }
        if a.starts_with("--config=") || a.starts_with("--enable=") || a.starts_with("--disable=") {
            i += 1;
            continue;
        }
        return (a == "app-server").then_some(i);
    }
    None
}

fn bridge_args(args: &[OsString], socket: &Path) -> Option<Vec<OsString>> {
    let index = server_index(args)?;
    let mut out = args[..=index].to_vec();
    let mut i = index + 1;
    while i < args.len() {
        let a = args[i].to_str()?;
        if [
            "-c",
            "--config",
            "--enable",
            "--disable",
            "--code-mode-host",
        ]
        .contains(&a)
        {
            out.push(args[i].clone());
            out.push(args.get(i + 1)?.clone());
            i += 2;
            continue;
        }
        match a {
            "--stdio" | "--listen=stdio://" => i += 1,
            "--listen" => {
                if args.get(i + 1)? != "stdio://" {
                    return None;
                }
                i += 2;
            }
            "--analytics-default-enabled" => {
                out.push(args[i].clone());
                i += 1;
            }
            _ if a.starts_with("--config=")
                || a.starts_with("--enable=")
                || a.starts_with("--disable=")
                || a.starts_with("--code-mode-host=") =>
            {
                out.push(args[i].clone());
                i += 1;
            }
            // Help, subcommands, explicit non-stdio transports and unknown future options
            // use the original binary unchanged, instead of guessing their semantics.
            _ => return None,
        }
    }
    out.extend([
        OsString::from("--listen"),
        OsString::from(format!("unix://{}", socket.display())),
    ]);
    Some(out)
}

struct OwnedServer {
    pid: u32,
    directory: PathBuf,
}

/// A separate, sleeping process watches a parent-owned pipe. EOF also happens
/// on SIGKILL, so an abruptly killed bridge cannot leave an agent running.
pub fn reaper(args: &[OsString]) {
    use std::io::Read;
    let Some(pid) = args
        .first()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|p| *p > 1)
    else {
        return;
    };
    let Some(directory) = args.get(1).map(PathBuf::from) else {
        return;
    };
    if directory.parent() != Some(root().as_path())
        || directory
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .is_none()
    {
        return;
    }
    let mut byte = [0u8; 1];
    while std::io::stdin().read(&mut byte).is_ok_and(|n| n != 0) {}
    if private_directory(&directory).is_err() {
        return;
    }
    // Never kill a group based on stale arguments after normal cleanup.
    if fs::read_to_string(directory.join("owner")).ok().as_deref() != Some(pid.to_string().as_str())
    {
        let _ = fs::remove_dir(&directory);
        return;
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = fs::remove_file(directory.join("ready"));
    let _ = fs::remove_file(directory.join("owner"));
    let _ = fs::remove_file(directory.join("server.sock"));
    let _ = fs::remove_file(directory.join("interactions.sock"));
    let _ = fs::remove_dir(directory);
}
impl Drop for OwnedServer {
    fn drop(&mut self) {
        // Only the isolated process group created by this invocation.
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
        let _ = fs::remove_file(self.directory.join("ready"));
        let _ = fs::remove_file(self.directory.join("owner"));
        let _ = fs::remove_file(self.directory.join("server.sock"));
        let _ = fs::remove_file(self.directory.join("interactions.sock"));
        let _ = fs::remove_dir(&self.directory);
    }
}

async fn bridge(binary: &Path, original: &[OsString]) -> Result<i32, String> {
    let base = root();
    private_directory(&base).map_err(|e| e.to_string())?;
    let directory = base.join(uuid::Uuid::new_v4().to_string());
    private_directory(&directory).map_err(|e| e.to_string())?;
    let socket = directory.join("server.sock");
    let args = bridge_args(original, &socket).ok_or("Unsupported app-server invocation")?;
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command.as_std_mut().process_group(0);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_dir(&directory);
            return Err(e.to_string());
        }
    };
    let guard = OwnedServer {
        pid: child.id().ok_or("Missing child PID")?,
        directory,
    };
    fs::write(guard.directory.join("owner"), guard.pid.to_string()).map_err(|e| e.to_string())?;
    let mut reaper_command =
        tokio::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    reaper_command
        .args([
            OsString::from("--codex-bridge-reaper"),
            OsString::from(guard.pid.to_string()),
            guard.directory.as_os_str().to_owned(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    reaper_command.as_std_mut().process_group(0);
    let mut watcher = reaper_command.spawn().map_err(|e| e.to_string())?;
    let lifetime_pipe = watcher.stdin.take();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| e.to_string())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|e| e.to_string())?;
    let relay = async {
        let stream = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(stream) = UnixStream::connect(&socket).await {
                    break Ok::<_, String>(stream);
                }
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    break Err("Codex exited before opening its listener".into());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| "Codex startup timed out")??;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        let config = WebSocketConfig {
            max_message_size: Some(MAX_FRAME),
            max_frame_size: Some(MAX_FRAME),
            ..Default::default()
        };
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(3),
            client_async_with_config("ws://localhost/", stream, Some(config)),
        ).await.map_err(|_| "Codex WebSocket handshake timed out".to_string())?
            .map_err(|e| e.to_string())?;
        fs::write(guard.directory.join("ready"), guard.pid.to_string())
            .map_err(|e| e.to_string())?;
        crate::codex_interactions::relay(ws, &guard.directory).await
    };
    let result: Result<(), String> = tokio::select! {
        r = relay => r,
        _ = term.recv() => Ok(()),
        _ = interrupt.recv() => Ok(()),
    };
    // A graceful signal first; Drop is the fallback for every error/cancellation path.
    unsafe {
        libc::kill(-(guard.pid as i32), libc::SIGTERM);
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    drop(guard);
    drop(lifetime_pipe);
    let _ = tokio::time::timeout(Duration::from_secs(2), watcher.wait()).await;
    result.map(|_| 0)
}

pub fn entry(args: &[OsString]) -> i32 {
    let Some(real) = args.first() else {
        eprintln!("Missing Codex executable");
        return 2;
    };
    let binary = Path::new(real);
    if !binary.is_absolute()
        || !binary.is_file()
        || fs::canonicalize(binary).ok()
            == std::env::current_exe()
                .ok()
                .and_then(|p| fs::canonicalize(p).ok())
    {
        eprintln!("Invalid Codex executable");
        return 2;
    }
    let original = &args[1..];
    if bridge_args(original, Path::new("/tmp/unused.sock")).is_none() {
        let e = std::process::Command::new(binary).args(original).exec();
        eprintln!("Cannot execute Codex: {e}");
        return 1;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Tokio runtime");
    let code = match runtime.block_on(bridge(binary, original)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("c9watch Codex bridge: {e}");
            1
        }
    };
    // Tokio stdin uses a blocking reader: do not hang shutdown waiting for an idle
    // parent's stdin after a server crash or SIGTERM.
    runtime.shutdown_timeout(Duration::from_millis(100));
    code
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[tauri::command]
pub async fn launch_codex_desktop_bridge() -> Result<String, String> {
    tokio::task::spawn_blocking(launch_desktop)
        .await
        .map_err(|e| e.to_string())?
}

fn launch_desktop() -> Result<String, String> {
    let app = ["/Applications/ChatGPT.app", "/Applications/Codex.app"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.join("Contents/Resources/codex").is_file())
        .ok_or("Codex Desktop is not installed in /Applications")?;
    let plist = app.join("Contents/Info.plist");
    let info = std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleExecutable", "raw"])
        .arg(plist)
        .output()
        .map_err(|e| e.to_string())?;
    let name = String::from_utf8(info.stdout).map_err(|_| "Invalid app executable")?;
    let name = name.trim();
    if !info.status.success() || name.is_empty() || name.contains('/') {
        return Err("Cannot identify Codex Desktop executable".into());
    }
    let executable = app.join("Contents/MacOS").join(name);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    if system
        .processes()
        .values()
        .any(|p| p.exe() == Some(executable.as_path()))
    {
        return Err("Quit Codex / ChatGPT after its tasks finish, then click Launch with support. Your conversations and settings will be preserved.".into());
    }
    let directory = root();
    private_directory(&directory).map_err(|e| e.to_string())?;
    let launcher = directory.join("launcher.sh");
    let temp = directory.join(format!("launcher-{}.tmp", uuid::Uuid::new_v4()));
    let helper = std::env::current_exe().map_err(|e| e.to_string())?;
    let real = app.join("Contents/Resources/codex");
    fs::write(
        &temp,
        format!(
            "#!/bin/sh\nexec {} --codex-desktop-bridge {} \"$@\"\n",
            shell_quote(&helper.to_string_lossy()),
            shell_quote(&real.to_string_lossy())
        ),
    )
    .map_err(|e| e.to_string())?;
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    fs::rename(temp, &launcher).map_err(|e| e.to_string())?;
    let status = std::process::Command::new("/usr/bin/open")
        .args(["-n", "--env"])
        .arg(format!("CODEX_CLI_PATH={}", launcher.display()))
        .args([
            "--env",
            "CODEX_APP_SERVER_FORCE_CLI=1",
            "--env",
            "CODEX_APP_SERVER_WS_URL=",
        ])
        .arg(app)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Codex Desktop could not be launched".into());
    }
    Ok("Codex launch requested. Open the conversation in Codex, then recheck here. To disable support, quit Codex and reopen it normally.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }
    #[test]
    fn keeps_overrides_and_only_replaces_stdio() {
        let input = args(&[
            "-c",
            "x=app-server",
            "app-server",
            "--stdio",
            "-c",
            "mcp_servers.app={a='b c'}",
            "--analytics-default-enabled",
        ]);
        assert_eq!(
            bridge_args(&input, Path::new("/tmp/test.sock")).unwrap(),
            args(&[
                "-c",
                "x=app-server",
                "app-server",
                "-c",
                "mcp_servers.app={a='b c'}",
                "--analytics-default-enabled",
                "--listen",
                "unix:///tmp/test.sock"
            ])
        );
    }
    #[test]
    fn passes_other_commands_through() {
        for a in [
            args(&["--version"]),
            args(&["exec", "app-server"]),
            args(&["app-server", "daemon", "version"]),
            args(&["app-server", "--help"]),
            args(&["app-server", "--listen", "ws://localhost:9000"]),
        ] {
            assert!(bridge_args(&a, Path::new("/tmp/x")).is_none());
        }
    }
    #[test]
    fn rejects_non_private_directory() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_directory(temp.path()).is_err());
    }
    #[test]
    fn discovers_only_private_live_sockets_and_ignores_symlinks() {
        let base = tempfile::tempdir().unwrap();
        fs::set_permissions(base.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let run = base.path().join("run");
        private_directory(&run).unwrap();
        let socket = run.join("server.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(run.join("ready"), std::process::id().to_string()).unwrap();
        assert_eq!(endpoints_under(base.path()), vec![socket.clone()]);
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(endpoints_under(base.path()).is_empty());
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(run.join("ready")).unwrap();
        assert!(endpoints_under(base.path()).is_empty());
        std::os::unix::fs::symlink(&run, base.path().join("alias")).unwrap();
        fs::write(run.join("ready"), std::process::id().to_string()).unwrap();
        assert_eq!(endpoints_under(base.path()), vec![socket]);
    }
    #[test]
    fn quotes_launcher_paths() {
        assert_eq!(shell_quote("a'b $(x)"), "'a'\\''b $(x)'");
    }
}
