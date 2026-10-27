# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: u22-message-receipt.spec.ts >> 전송 응답 손실 후 같은 lead 요청을 확인한다: zoom200
- Location: tests/mission-ui/u22-message-receipt.spec.ts:8:5

# Error details

```
Error: expect(received).toHaveLength(expected)

Expected length: 1
Received length: 0
Received array:  []
```

# Page snapshot

```yaml
- generic [ref=e3]:
  - banner [ref=e4]:
    - navigation "터미널 탭" [ref=e5]:
      - tab "탭 1 1.5코어 · 188 MiB 탭 1 닫기" [ref=e6] [cursor=pointer]:
        - generic [ref=e7]: 탭 1
        - generic "이 탭의 살아 있는 터미널 중 최대 사용량(CPU · RAM). 에이전트가 모델 응답을 기다리는 동안은 CPU가 0에 가깝습니다 — 살아 있어도 <0.1코어로 보일 수 있습니다." [ref=e8]: 1.5코어 · 188 MiB
        - button "탭 1 닫기" [ref=e9]: ×
      - tab "로그인 기능 실행 중 2 탭 2 닫기" [selected] [ref=e10] [cursor=pointer]:
        - generic [ref=e11]: 로그인 기능
        - generic "실행 중 2" [ref=e12]
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
      - button "팀 작업" [ref=e52] [cursor=pointer]
      - button "Lead 대화" [pressed] [ref=e53] [cursor=pointer]
      - button "결과" [ref=e54] [cursor=pointer]
    - main [ref=e56]:
      - generic [ref=e57]:
        - generic [ref=e59]:
          - generic [ref=e60]:
            - heading "사용자 목표" [level=3] [ref=e61]
            - paragraph [ref=e62]: 로그인 기능을 구현해 주세요.
          - button "계획 펼치기" [ref=e64] [cursor=pointer]:
            - generic [aria-hidden] [ref=e65]: ▸
            - generic [ref=e66]: 채택된 계획
            - generic [ref=e67]: 할 일 2개
          - article [ref=e68]:
            - generic [ref=e69]:
              - generic [ref=e70]: 사용자
              - generic [ref=e71]: 19:24
              - generic [ref=e72]: 전달됨
            - paragraph [ref=e73]: 로그인 기능을 구현해 주세요.
          - article [ref=e74]:
            - generic [ref=e75]:
              - generic [ref=e76]: 사용자
              - generic [ref=e77]: 19:24
              - generic [ref=e78]: 전달됨
            - paragraph [ref=e79]: 외부 요청 기록을 먼저 확인하고 입력 검증을 보완해 주세요.
        - generic [ref=e80]:
          - generic [ref=e81]: AI 작업 전체에 요청
          - generic [ref=e82]:
            - textbox "AI 작업 전체에 요청" [ref=e83]:
              - /placeholder: AI 작업 전체에 요청…
            - button "보내기" [disabled] [ref=e84] [cursor=pointer]
  - contentinfo [ref=e85]:
    - button "자원 그래프 열기(최근 5분)" [ref=e86] [cursor=pointer]:
      - 'generic "mock.cpu에서 측정하지 못했습니다 · CPU 압력: 사용 코어/전체 코어 비율 · 85% 경고 · 95% 위험 · 70% 이하 10초 지속 시 회복" [ref=e87]':
        - text: CPU — / 12코어
        - generic [ref=e88]: · 압력 보통
      - 'generic "사용률 65.3% · mock.host · 시스템 전체 물리 메모리: 사용 / 전체. ‘사용’은 OS 기준(앱+wired+압축, 활성 모니터와 같음)이며, 재활용 가능한 캐시는 뺀 값이라 이 프로세스 하나의 메모리가 아닙니다." [ref=e89]':
        - text: RAM 20.9 GiB / 32 GiB
        - generic [ref=e90]: · 압력 보통
      - generic "저장 공간 사용률 64.8% · 호스트 디스크 R/W 속도는 계약에 없어 제공하지 않습니다" [ref=e91]: Disk R/W — · 여유 180.1 GiB
      - generic "mock.net에서 측정하지 못했습니다" [ref=e92]:
        - text: NET ↓ — ↑ —
        - generic [ref=e93]: · ethernet
    - button "AI 잔여량 —" [ref=e95] [cursor=pointer]
    - button "작업 대기열 열기" [ref=e96] [cursor=pointer]: 에이전트 없음
```

# Test source

```ts
  1  | import { expect, test } from "@playwright/test";
  2  | import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
  3  | import type { MissionMessageParams } from "../../src/generated/MissionMessageParams";
  4  | import { prepare, seedAndOpen } from "./helpers";
  5  | 
  6  | for (const layout of ["700", "zoom200"] as const) {
  7  |   for (const recipient of ["lead", "task"] as const) {
  8  |     test(`전송 응답 손실 후 같은 ${recipient} 요청을 확인한다: ${layout}`, async ({ page }, testInfo) => {
  9  |       await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
  10 |       await prepare(page);
  11 |       const missionId = await seedAndOpen(page, { agents: 2 });
  12 |       if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
  13 |       const before = await page.evaluate(async id => {
  14 |         const client = window.__mockDaemon!.client as MockDaemonClient;
  15 |         const original = client.missionMessage.bind(client);
  16 |         const calls: MissionMessageParams[] = [];
  17 |         client.missionMessage = async params => {
  18 |           calls.push(structuredClone(params));
  19 |           const result = await original(params);
  20 |           if (calls.length === 1) throw new Error("저장 응답 연결이 끊어졌습니다.");
  21 |           if (JSON.stringify(calls[0]) !== JSON.stringify(params)) throw new Error("Retry changed the original request");
  22 |           return result;
  23 |         };
  24 |         const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
  25 |         return snapshot.entities.flatMap(e => "Message" in e && e.Message.role === "user" ? [e.Message] : []);
  26 |       }, missionId);
  27 |       if (recipient === "task") {
  28 |         await page.getByTestId("toggle-detail").click();
  29 |         await page.getByTestId("team-row").first().click();
  30 |         await page.getByTestId("instruct-toggle").click();
  31 |       }
  32 |       if (recipient === "lead") await page.getByTestId("toggle-lead").click();
  33 |       const input = page.getByTestId(recipient === "lead" ? "lead-composer" : "task-composer-input");
  34 |       const send = page.getByTestId(recipient === "lead" ? "lead-send" : "task-composer-send");
  35 |       const body = "외부 요청 기록을 먼저 확인하고 입력 검증을 보완해 주세요.";
  36 |       await input.fill(body);
  37 |       await send.click();
  38 |       await expect(send).toHaveText("전송 결과 다시 확인");
  39 |       await expect(input).toBeDisabled();
  40 |       await expect(page.getByTestId("message-receipt-note")).toBeVisible();
  41 |       if (recipient === "lead") {
  42 |         await page.getByTestId("toggle-detail").click();
  43 |         await expect(input).toHaveCount(0);
  44 |         await page.getByTestId("toggle-lead").click();
  45 |       } else {
  46 |         await page.getByTestId("instruct-toggle").click();
  47 |         await expect(input).toHaveCount(0);
  48 |         await page.getByTestId("instruct-toggle").click();
  49 |       }
  50 |       await expect(input).toHaveValue(body);
  51 |       await expect(input).toBeDisabled();
  52 |       await expect(send).toBeEnabled();
  53 |       await send.scrollIntoViewIfNeeded();
  54 |       await expect(send).toBeInViewport({ ratio: 0.99 });
  55 |       const note = page.getByTestId("message-receipt-note");
  56 |       await note.scrollIntoViewIfNeeded();
  57 |       await expect(note).toBeInViewport({ ratio: 0.99 });
  58 |       await expect(send).toBeInViewport({ ratio: 0.99 });
  59 |       await expect(page.getByRole("alert")).toContainText("저장 응답 확인 필요");
  60 |       const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
  61 |       expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
  62 |       await page.screenshot({ path: testInfo.outputPath(`message-receipt-${recipient}-${layout}.png`) });
  63 |       await send.click();
  64 |       await expect(input).toHaveValue("");
  65 |       await expect(page.getByTestId("message-receipt-note")).toHaveCount(0);
  66 |       const messages = await page.evaluate(async id => {
  67 |         const client = window.__mockDaemon!.client as MockDaemonClient;
  68 |         const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
  69 |         return snapshot.entities.flatMap(e => "Message" in e && e.Message.role === "user" ? [e.Message] : []);
  70 |       }, missionId);
  71 |       const added = messages.filter(message => !before.some(old => old.id === message.id));
> 72 |       expect(messages).toHaveLength(before.length + 1);
     |                        ^ Error: expect(received).toHaveLength(expected)
  73 |       expect(messages.filter(message => before.some(old => old.id === message.id))).toEqual(before);
  74 |       expect(added).toHaveLength(1);
  75 |       expect(added[0].target_task_id === null).toBe(recipient === "lead");
  76 |     });
  77 |   }
  78 | }
  79 | 
```