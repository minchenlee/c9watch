---
title: Install
description: How to install c9watch on macOS.
---

c9watch is a native macOS desktop app. There are three ways to install it.

## Quick install

The fastest way to get started. Run this command in your terminal:

```bash
curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install.sh | bash
```

This script downloads the latest `.dmg` from GitHub Releases, mounts it, copies `c9watch.app` to your `/Applications` folder, and cleans up. If c9watch is already installed, it will be replaced with the latest version.

## Download manually

Grab the latest `.dmg` from the [Releases](https://github.com/minchenlee/c9watch/releases) page. Open the `.dmg` and drag c9watch to your Applications folder.

On first launch, macOS may show a security warning because the app is not notarized by Apple. Go to **System Settings → Privacy & Security** and click **"Open Anyway"**.

## Build from source

If you want to build c9watch yourself or contribute to development, you can build from source.

### Prerequisites

- [Rust](https://rustup.rs/) — install via `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [Node.js](https://nodejs.org/) (v18+) — install via [nvm](https://github.com/nvm-sh/nvm) or the official installer
- [Tauri CLI](https://v2.tauri.app/start/prerequisites/) — install via `cargo install tauri-cli`

### Build steps

```bash
git clone https://github.com/minchenlee/c9watch.git
cd c9watch
npm install
npm run tauri build
```

The built `.app` will be in `src-tauri/target/release/bundle/macos/`. You can drag it to your Applications folder or run it directly.

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
