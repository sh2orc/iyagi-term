import { appendFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";

/**
 * 개발 전용: WKWebView IME 이벤트 트레이스(window.__imeEvents)를 HMR 소켓으로
 * 받아 파일에 덧붙인다 — 인스펙터 없이 실제 이벤트 순서를 확정하기 위함.
 * 경로: node_modules/.cache/iyagi-ime-trace.log
 */
function imeTracePlugin(): Plugin {
  return {
    name: "iyagi-ime-trace",
    apply: "serve",
    configureServer(server) {
      const file = path.join(server.config.root, "node_modules/.cache/iyagi-ime-trace.log");
      mkdirSync(path.dirname(file), { recursive: true });
      server.ws.on("iyagi:ime-trace", (data: { lines?: unknown }) => {
        if (Array.isArray(data?.lines)) appendFileSync(file, `${data.lines.join("\n")}\n`);
      });
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), imeTracePlugin()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      // Native build products and nested agent worktrees are not web assets.
      // Walking them during Rust builds can starve the dev server's event
      // loop, delaying HMR and the running WKWebView for seconds at a time.
      ignored: ["**/target/**", "**/src-tauri/binaries/**", "**/.claude/**",
        "**/.omc/**", "**/.omo/**", "**/.sisyphus/**", "**/releases/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2022",
    minify: "esbuild",
    sourcemap: false,
  },
});
