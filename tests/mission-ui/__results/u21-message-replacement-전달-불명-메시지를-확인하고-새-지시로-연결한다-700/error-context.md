# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: u21-message-replacement.spec.ts >> 전달 불명 메시지를 확인하고 새 지시로 연결한다: 700
- Location: tests/mission-ui/u21-message-replacement.spec.ts:6:3

# Error details

```
Test timeout of 60000ms exceeded.
```

```
Error: locator.click: Test timeout of 60000ms exceeded.
Call log:
  - waiting for locator('[id="mission-message-25f1e262-12cb-4241-bced-e95b7939b8c9"]').getByTestId('message-replace-open')

```

# Page snapshot

```yaml
- generic [ref=e3]:
  - banner [ref=e4]:
    - navigation "터미널 탭" [ref=e5]:
      - tab "탭 1 1.6코어 · 355 MiB 탭 1 닫기" [ref=e6] [cursor=pointer]:
        - generic [ref=e7]: 탭 1
        - generic "이 탭의 살아 있는 터미널 중 최대 사용량(CPU · RAM). 에이전트가 모델 응답을 기다리는 동안은 CPU가 0에 가깝습니다 — 살아 있어도 <0.1코어로 보일 수 있습니다." [ref=e8]: 1.6코어 · 355 MiB
        - button "탭 1 닫기" [ref=e9]: ×
      - tab "로그인 기능 실행 중 1 탭 2 닫기" [selected] [ref=e10] [cursor=pointer]:
        - generic [ref=e11]: 로그인 기능
        - generic "실행 중 1" [ref=e12]
        - button "탭 2 닫기" [ref=e13]: ×
      - button "새 탭" [ref=e14] [cursor=pointer]: +
    - generic [ref=e15]:
      - button "동시 입력" [ref=e16] [cursor=pointer]
      - button "배치 편집" [ref=e23] [cursor=pointer]
      - button "언어 전환" [ref=e26] [cursor=pointer]
      - button "+ 새 AI 작업" [ref=e30] [cursor=pointer]
      - button "AI 작업 목록" [ref=e31] [cursor=pointer]: ☰
      - generic [ref=e32]:
        - button "새 터미널(zsh)" [ref=e33] [cursor=pointer]: + 새 터미널 · zsh
        - button "셸 프로필 선택" [ref=e34] [cursor=pointer]: ▾
      - button "설정" [ref=e35] [cursor=pointer]
  - generic [ref=e41]:
    - banner [ref=e42]:
      - generic [ref=e43]:
        - heading "로그인 기능" [level=2] [ref=e44]
        - generic [ref=e45]: 실행 중
        - generic [ref=e46]: · 계획 중
        - generic "활성 시간은 AI 작업이 실행 중이거나 일시정지를 기다리는 시간입니다. 병렬 실행 시간을 합산하지 않으며, 일시정지와 데몬 중단 시간은 제외합니다." [ref=e47]: 활성 시간 0:00:00 / 24:00:00
      - button "실행 제어 ▾" [ref=e49] [cursor=pointer]
    - navigation "로그인 기능" [ref=e51]:
      - button "팀 작업" [pressed] [ref=e52] [cursor=pointer]
      - button "Lead 대화" [ref=e53] [cursor=pointer]
      - button "결과" [ref=e54] [cursor=pointer]
    - complementary "팀 작업" [ref=e56]:
      - region "할 일 목록" [ref=e57]:
        - generic [ref=e58]:
          - generic [ref=e59]:
            - heading "할 일" [level=3] [ref=e60]
            - generic [ref=e61]:
              - generic [ref=e62]: 실행 1
              - generic [ref=e63]: 완료 0
              - generic [ref=e64]: 결정 0
          - group "할 일 목록" [ref=e65]:
            - button "전체" [pressed] [ref=e66] [cursor=pointer]
            - button "실행 중" [ref=e67] [cursor=pointer]
            - button "응답 필요" [ref=e68] [cursor=pointer]
            - button "대기" [ref=e69] [cursor=pointer]
            - button "완료" [ref=e70] [cursor=pointer]
          - searchbox "할 일 검색" [ref=e71]
        - list [ref=e72]:
          - listitem [ref=e73]:
            - button "작업 1 GLM-5.3 실행 중" [ref=e74] [cursor=pointer]:
              - generic [aria-hidden] [ref=e75]: ▶
              - generic [ref=e76]: 작업 1
              - generic [ref=e77]: GLM-5.3
              - generic [ref=e79]: 실행 중
            - generic [ref=e80]: 실행 중 · 1분 미만
  - contentinfo [ref=e82]:
    - button "자원 그래프 열기(최근 5분)" [ref=e83] [cursor=pointer]:
      - 'generic "사용률 17.3% · mock.cpu · measured · CPU 압력: 사용 코어/전체 코어 비율 · 85% 경고 · 95% 위험 · 70% 이하 10초 지속 시 회복" [ref=e84]':
        - text: CPU 2.1 / 12코어
        - generic [ref=e85]: · 압력 보통
      - 'generic "사용률 63.8% · mock.host · 시스템 전체 물리 메모리: 사용 / 전체. ‘사용’은 OS 기준(앱+wired+압축, 활성 모니터와 같음)이며, 재활용 가능한 캐시는 뺀 값이라 이 프로세스 하나의 메모리가 아닙니다." [ref=e86]':
        - text: RAM 20.4 GiB / 32 GiB
        - generic [ref=e87]: · 압력 보통
      - generic "저장 공간 사용률 65.1% · 호스트 디스크 R/W 속도는 계약에 없어 제공하지 않습니다" [ref=e88]: Disk R/W — · 여유 178.5 GiB
      - generic "mock.net · measured" [ref=e89]:
        - text: NET ↓ 1.4 MiB/s ↑ 317.4 KiB/s
        - generic [ref=e90]: · ethernet
    - button "AI 잔여량 —" [ref=e92] [cursor=pointer]
    - button "작업 대기열 열기" [ref=e93] [cursor=pointer]: 에이전트 없음
```

# Test source

```ts
  1  | import { expect, test } from "@playwright/test";
  2  | import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
  3  | import { prepare, seedAndOpen, harnessAddMessage } from "./helpers";
  4  | 
  5  | for (const layout of ["700", "zoom200"] as const) {
  6  |   test(`전달 불명 메시지를 확인하고 새 지시로 연결한다: ${layout}`, async ({ page }, testInfo) => {
  7  |     await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
  8  |     await prepare(page);
  9  |     const missionId = await seedAndOpen(page, { agents: 1 });
  10 |     const sourceId = await harnessAddMessage(page, missionId, { role: "user", delivery: "unknown", text: "외부 요청 전에 입력값을 확인해 주세요." });
  11 |     if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
  12 |     const source = page.locator(`[id="mission-message-${sourceId}"]`);
> 13 |     await source.getByTestId("message-replace-open").click();
     |                                                      ^ Error: locator.click: Test timeout of 60000ms exceeded.
  14 |     await expect(source).toContainText("이전 지시가 이미 반영되었을 수 있습니다");
  15 |     const input = source.getByTestId("message-replacement-body");
  16 |     await expect(input).toHaveValue("외부 요청 전에 입력값을 확인해 주세요.");
  17 |     await input.fill("기록을 확인했습니다. 이미 완료한 외부 요청은 반복하지 말고 입력 검증을 보완해 주세요.");
  18 |     const send = source.getByTestId("message-replacement-send");
  19 |     await send.scrollIntoViewIfNeeded();
  20 |     await expect(send).toBeInViewport({ ratio: 0.99 });
  21 |     const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
  22 |     expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
  23 |     await page.screenshot({ path: testInfo.outputPath(`message-replacement-${layout}.png`) });
  24 |     await send.click();
  25 |     await expect(source.getByTestId("message-replacement-link")).toBeVisible();
  26 |     await expect(source).toContainText("전달 여부 확인 필요");
  27 |     const result = await page.evaluate(async ({ missionId, sourceId }) => {
  28 |       const client = window.__mockDaemon!.client as MockDaemonClient;
  29 |       const page = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
  30 |       const messages = page.entities.flatMap(e => "Message" in e ? [e.Message] : []);
  31 |       const replacements = messages.filter(m => m.supersedes_message_id === sourceId);
  32 |       const body = await client.artifactRead({ artifact_id: replacements[0].body_ref.id, offset: "0", max_bytes: 4096 });
  33 |       const text = new TextDecoder().decode(Uint8Array.from(atob(body.data_b64), c => c.charCodeAt(0)));
  34 |       return { count: replacements.length, nextId: replacements[0].id, delivery: replacements[0].delivery, text };
  35 |     }, { missionId, sourceId });
  36 |     expect(result.count).toBe(1);
  37 |     expect(result.delivery).toBe("queued");
  38 |     expect(result.text).toContain("이미 완료한 외부 요청은 반복하지 말고");
  39 |     const next = page.locator(`[id="mission-message-${result.nextId}"]`);
  40 |     await source.getByTestId("message-replacement-link").click();
  41 |     await expect(next).toBeFocused();
  42 |     await next.getByTestId("message-original-link").click();
  43 |     await expect(source).toBeFocused();
  44 |     await expect(source.getByTestId("message-replace-open")).toHaveCount(0);
  45 |   });
  46 | }
  47 | 
```