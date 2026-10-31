# Linux core and CLI (preview)

This page covers the headless core on Linux: install, socket and state paths, the optional systemd user service, and what still needs a real Linux host. Start with the [API guide](README.md).

Linux tarballs for x86_64 and aarch64 contain `c9watch` and `c9watch-agent`.
Keep both executables together when installing them, for example in
`~/.local/bin`. No Linux Tauri GUI is shipped. These binaries require a GNU/Linux
system with OpenSSL available (the builds use native TLS).

```sh
tar -xzf c9watch-cli-x86_64-unknown-linux-gnu.tar.gz
install -d "$HOME/.local/bin"
install -m 755 c9watch c9watch-agent "$HOME/.local/bin/"
export PATH="$HOME/.local/bin:$PATH"
c9watch engine                     # foreground; Ctrl-C drains and stops core
```

In another terminal, or without first starting the foreground core:

```sh
c9watch api hello '{}'
c9watch list
c9watch api session.views '{}'
c9watch api history.list '{}'
c9watch api cost.get '{}'
c9watch spawn --mode free --cwd /absolute/project --name helper
c9watch api agent.list '{}'
```

CLI data commands automatically start core when needed. It keeps running after
the CLI exits. `c9watch engine restart` drains it and starts a new process.
Core needs Claude Code on `PATH` to spawn free agents; no paid API is required
just to run the core or read local session data.

Build the pair from source with:

```sh
cd src-tauri
C9WATCH_CHANNEL=prod cargo build --release --no-default-features --features engine,cli --bins
```

## Socket and state paths

| Platform/configuration | Socket | Persistent state, lock and log |
| --- | --- | --- |
| Linux release | `$XDG_RUNTIME_DIR/c9watch/engine-v1.sock` | `$XDG_STATE_HOME/c9watch/engine/` |
| Linux dev build | `$XDG_RUNTIME_DIR/c9watch/engine-dev-v1.sock` | `$XDG_STATE_HOME/c9watch/engine-dev/` |
| Linux without an absolute, nonempty `XDG_RUNTIME_DIR` | `<engine home>/engine-v1.sock` | Same as above |
| macOS release/dev | `<engine home>/engine-v1.sock` | `~/Library/Application Support/c9watch/engine[-dev]/` |
| Custom `C9WATCH_ENGINE_HOME` | `<custom home>/engine-v1.sock` | The absolute custom home |

`XDG_STATE_HOME` defaults to `~/.local/state`; relative values are ignored.
An override equal to the default state home uses the default runtime socket,
including when a CLI pins that home for an auto-started engine.

The runtime directory must already exist, belong to your uid, and have mode
0700. Core refuses insecure runtime directories; it never changes their owner
or repairs their permissions. It creates its `c9watch` subdirectory as 0700,
binds sockets as 0600, and checks kernel peer credentials on both ends of the
connection. Another uid cannot substitute its engine for yours. Do not share
an engine home between users. Avoid the fallback on NFS homes: Unix sockets
need a local filesystem. Use the login session's runtime directory instead.
These rules follow the [XDG Base Directory specification](https://specifications.freedesktop.org/basedir/latest/).

The gateway's listeners, port selection and authentication are a separate
milestone. This core/CLI build does not expose a network listener or web UI.
The planned Linux gateway default is `20000 + (uid % 10000)`, recorded in
`<engine home>/gateway.json`; do not assume that port is listening in this build.

## Optional systemd user service

CLI auto-start needs no service manager. If you prefer a user service, review
the [example unit](../systemd/c9watch-engine.service), adjust its executable and
PATH for your installation, then install it yourself:

```sh
mkdir -p "$HOME/.config/systemd/user"
cp docs/systemd/c9watch-engine.service "$HOME/.config/systemd/user/"
systemctl --user daemon-reload
systemctl --user enable --now c9watch-engine.service
journalctl --user -u c9watch-engine.service
# Stop through systemd when it owns core:
systemctl --user stop c9watch-engine.service
```

Stop an auto-started core before enabling the service to avoid two launchers
contending for its lifetime lock. The example is never installed automatically.
While the service owns core, use `systemctl --user restart c9watch-engine.service`
to restart it, so the replacement stays managed by systemd.
Its stop timeout allows the core's 90-second drain, and `KillMode=mixed` lets
core handle its agent children before the final timeout.

Do not add `ProtectHome`, `PrivateTmp`, `ProtectSystem`, `PrivateDevices`, bind
mounts, root images or other filesystem namespace isolation to this unit.
Core compares a Linux peer's mount namespace with its own to prevent sandboxed
agents from obtaining local write authority. If systemd isolates core, ordinary
terminals appear confined and writes return `not_allowed`. Flatpak/snap
terminals can have the same problem: run core and the terminal client in the
same host namespace. Read calls do not grant write authority. See
[systemd execution environment documentation](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html).

## Subscription tools and Linux verification

`usage.subscriptions` resolves `codex` from the user's installation or PATH and
`curl` from PATH on Linux. For a service, check its PATH includes both
(`command -v codex; command -v curl` in an equivalent environment). Missing
tools are reported as provider failures; running core does not install them.

Linux CI runs the headless engine unit/integration suite and packages both
executables. Tests use temporary homes and fake Claude Code scripts. Real
Linux acceptance still needs a host with Claude Code sessions, `history.list`,
`cost.get` and subscription tools. Cursor's Linux `state.vscdb` discovery,
gateway dual-stack/two-user tests, web-client acceptance and export examples
require their respective milestones and a Linux host; they are not established
by a macOS test run.
