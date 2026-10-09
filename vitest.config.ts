import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    include: ["src/**/*.test.ts", "src/**/*.test.tsx", "tests/**/*.test.ts", "tests/**/*.test.tsx"],
    environment: "node",
    globals: false,
    // 상호작용이 필요한 컴포넌트 시험(O16)만 happy-dom으로 돌린다 — 나머지는
    // 기존 node 환경 계약을 그대로 유지한다.
    environmentMatchGlobs: [["**/*.dom.test.tsx", "happy-dom"]],
  },
});
