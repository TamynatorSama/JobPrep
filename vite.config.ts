import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @tauri-apps/cli sets this when running `tauri dev` from a remote host —
// we just forward it through.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  // Pre-bundle the Tauri API subpaths we import up front. Otherwise Vite
  // discovers a new subpath (e.g. webviewWindow) on first load, re-optimizes,
  // and forces a full page reload — which WebView2 frequently fails to recover
  // from, leaving the window blank until a manual refresh.
  optimizeDeps: {
    include: [
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/api/webviewWindow",
    ],
  },

  // Tauri expects a fixed port; fail rather than silently switch.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: {
      // Don't reload on every Rust change — `tauri dev` handles that.
      ignored: ["**/src-tauri/**"],
    },
  },
});
