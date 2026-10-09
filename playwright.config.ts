import { defineConfig } from "@playwright/test";

/**
 * test:mission-ui(O16): vite dev server 위의 plain-browser 미리보기
 * (MockDaemonClient + window.__mockDaemon harness)를 실제 브라우저로
 * 검사한다. 스크린샷은 tests/mission-ui/__screenshots__에 남긴다.
 */
export default defineConfig({
  testDir: "./tests/mission-ui",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  reporter: [["list"]],
  outputDir: "./tests/mission-ui/__results",
  globalSetup: "./tests/mission-ui/globalSetup.ts",
  use: {
    // vite dev는 localhost(::1 포함)에만 바인딩한다 — 127.0.0.1로 probing하면
    // 거절되므로 호스트명을 그대로 쓴다.
    baseURL: "http://localhost:5183",
    viewport: { width: 1440, height: 900 },
    trace: "off",
  },
  webServer: {
    command: "npm run dev:frontend -- --port 5183 --strictPort",
    url: "http://localhost:5183",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
