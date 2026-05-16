import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// @tauri-apps/cli sets this when running `tauri dev` from a remote host —
// we just forward it through.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

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
