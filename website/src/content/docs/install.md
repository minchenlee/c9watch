---
title: Install
description: How to install c9watch — desktop app for macOS or standalone CLI for macOS & Linux. One-command install, download, or build from source.
head:
  - tag: script
    attrs:
      type: application/ld+json
    content: '{"@context":"https://schema.org","@type":"BreadcrumbList","itemListElement":[{"@type":"ListItem","position":1,"name":"Home","item":"https://c9watch.mclee.dev"},{"@type":"ListItem","position":2,"name":"Install","item":"https://c9watch.mclee.dev/install/"}]}'
---

:::note[TL;DR]
**Desktop app (macOS):** `curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install.sh | bash`
**CLI only (macOS & Linux):** `curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash`
:::

c9watch comes in two forms: a **desktop app** with a full GUI dashboard, and a **standalone CLI** for scriptable session management.

## Desktop app (macOS)

### Quick install

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install.sh | bash
```

This script downloads the latest `.dmg` from GitHub Releases, mounts it, copies `c9watch.app` to your `/Applications` folder, and cleans up. If c9watch is already installed, it will be replaced with the latest version.

### Download manually

Grab the latest `.dmg` from the [Releases](https://github.com/minchenlee/c9watch/releases) page. Open the `.dmg` and drag c9watch to your Applications folder.

On first launch, macOS may show a security warning because the app is not notarized by Apple. Go to **System Settings → Privacy & Security** and click **"Open Anyway"**.

## CLI only (macOS & Linux)

The CLI is a standalone Rust binary with no GUI dependencies. It lets coding agents (or you) query and manage Claude Code sessions from the command line with JSON output.

### Quick install

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
```

Installs to `~/.local/bin` by default. Use `| bash -s -- --global` to install to `/usr/local/bin` instead.

### Manual download

Download the `c9watch-cli-*` tarball for your platform from [GitHub Releases](https://github.com/minchenlee/c9watch/releases), extract it, and place the `c9watch` binary on your `$PATH`.

Available targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`.

## Build from source

If you want to build c9watch yourself or contribute to development, you can build from source.

### Prerequisites

- [Rust](https://rustup.rs/) — install via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [Node.js](https://nodejs.org/) (v18+) — install via [nvm](https://github.com/nvm-sh/nvm) or the official installer (desktop app only)
- [Tauri CLI](https://v2.tauri.app/start/prerequisites/) — install via `cargo install tauri-cli` (desktop app only)

### Desktop app

```bash
git clone https://github.com/minchenlee/c9watch.git
cd c9watch
npm install
npm run tauri build
```

The built `.app` will be in `src-tauri/target/release/bundle/macos/`. You can drag it to your Applications folder or run it directly.

### CLI only

No Node.js or Tauri CLI needed:

```bash
git clone https://github.com/minchenlee/c9watch.git
cd c9watch/src-tauri
cargo build --release --no-default-features --features cli
```

The binary will be at `target/release/c9watch`.

### Development mode

For local development with hot-reload:

```bash
npm install
npm run tauri dev
```

This starts both the Vite dev server (hot-reload for the Svelte frontend) and the Tauri Rust backend. Changes to `.svelte` files are reflected instantly. Rust changes trigger a recompile.

## Demo mode

Press `Cmd+D` to toggle demo mode, which loads simulated sessions with animated status transitions. Useful for exploring the UI without running real Claude Code sessions.

## Auto-updates

c9watch checks for updates automatically using the Tauri updater plugin. When a new version is available, you'll see an update notification in the app.

## System requirements

- **OS:** macOS 12 (Monterey) or later
- **Architecture:** Apple Silicon (M1/M2/M3) and Intel
- **Claude Code:** Must be installed and running separately — c9watch monitors it, doesn't include it
