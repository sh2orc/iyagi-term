import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/terminal-ui",
  workers: 1,
  timeout: 30_000,
  outputDir: "./node_modules/.cache/terminal-ui",
  use: { baseURL: "http://localhost:5187", viewport: { width: 1200, height: 800 } },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ],
  webServer: {
    command: "npm run dev:frontend -- --port 5187 --strictPort",
    url: "http://localhost:5187",
    reuseExistingServer: !process.env.CI,
  },
});
