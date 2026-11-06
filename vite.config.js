import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import { readFileSync } from "node:fs";
import { c9extDev } from "./scripts/vite-c9ext-dev.mjs";

const host = process.env.TAURI_DEV_HOST;
const appVersion = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")).version;

// https://vite.dev/config/
export default defineConfig(async () => ({
  // c9extDev: dev-only `/c9ext/*` file server for glass extensions (G6).
  plugins: [c9extDev(), sveltekit()],
  // The glass extension host compares `minVersion.app` with this.
  define: { __C9_APP_VERSION__: JSON.stringify(appVersion) },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
