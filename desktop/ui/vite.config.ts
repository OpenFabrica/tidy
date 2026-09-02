// Vite config for the Tauri frontend. The dev port comes from TAURI_DEV_PORT
// (the Makefile picks a free one and hands the same value to `tauri dev`
// as devUrl), falling back to 5173; strictPort keeps both sides in agreement
// instead of letting Vite silently drift to 5174. No auto-open: Tauri owns
// the window.
import { defineConfig } from "vite";

/** 开发端口：Makefile 传入的 TAURI_DEV_PORT，缺省 5173。 */
const devPort = Number.parseInt(process.env.TAURI_DEV_PORT ?? "", 10) || 5173;

export default defineConfig({
  clearScreen: false,
  server: {
    port: devPort,
    strictPort: true,
  },
  build: {
    target: "safari15",
  },
});
