# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: u02-create.spec.ts >> U02: start 실패는 오류+재시도를 표시하고 중복 mission을 만들지 않는다
- Location: tests/mission-ui/u02-create.spec.ts:74:1

# Error details

```
Error: locator.click: Error: strict mode violation: locator('.mission-settings').getByTestId('mission-guide').locator('summary') resolved to 2 elements:
    1) <summary>AI 작업 사용법</summary> aka getByText('AI 작업 사용법')
    2) <summary>문제가 생겼을 때</summary> aka getByText('문제가 생겼을 때')

Call log:
  - waiting for locator('.mission-settings').getByTestId('mission-guide').locator('summary')

```

# Page snapshot

```yaml
- main [ref=e3]:
  - generic [ref=e4]:
    - generic [ref=e5]:
      - heading "설정" [level=1] [ref=e6]
      - paragraph [ref=e7]: 언어와 화면, 터미널 기본값, 실행 프로필과 자원 정책을 한곳에서 관리합니다. 각 항목의 저장 버튼으로 적용합니다.
    - button "터미널로 이동" [ref=e8] [cursor=pointer]
  - generic [ref=e9]:
    - navigation "설정 그룹" [ref=e10]:
      - searchbox "설정 검색" [ref=e11]
      - button "일반 언어와 화면 테마" [ref=e12] [cursor=pointer]:
        - generic [ref=e13]: 일반
        - generic [ref=e14]: 언어와 화면 테마
      - button "터미널 글꼴 크기, 기본 셸, 닫을 때 동작" [ref=e15] [cursor=pointer]:
        - generic [ref=e16]: 터미널
        - generic [ref=e17]: 글꼴 크기, 기본 셸, 닫을 때 동작
      - button "자원 관리 작업별 CPU·메모리 한도와 자동 일시정지·양보" [ref=e18] [cursor=pointer]:
        - generic [ref=e19]: 자원 관리
        - generic [ref=e20]: 작업별 CPU·메모리 한도와 자동 일시정지·양보
      - button "트랙패드 두 손가락 제스처로 탭 전환" [ref=e21] [cursor=pointer]:
        - generic [ref=e22]: 트랙패드
        - generic [ref=e23]: 두 손가락 제스처로 탭 전환
      - button "연동 AI 구독 사용량, Z.ai 라우팅과 CLI hooks" [ref=e24] [cursor=pointer]:
        - generic [ref=e25]: 연동
        - generic [ref=e26]: AI 구독 사용량, Z.ai 라우팅과 CLI hooks
      - button "AI 작업 여러 AI 에이전트에 목표 맡기기 — 빠른 설정·팀·검증" [ref=e27] [cursor=pointer]:
        - generic [ref=e28]: AI 작업
        - generic [ref=e29]: 여러 AI 에이전트에 목표 맡기기 — 빠른 설정·팀·검증
      - button "관리 실행 실행할 도구와 자원 할당" [ref=e30] [cursor=pointer]:
        - generic [ref=e31]: 관리 실행
        - generic [ref=e32]: 실행할 도구와 자원 할당
      - button "실행 프로필 명령어, 환경 변수와 기본 정책" [ref=e33] [cursor=pointer]:
        - generic [ref=e34]: 실행 프로필
        - generic [ref=e35]: 명령어, 환경 변수와 기본 정책
      - button "단축키 이 플랫폼의 키 조합" [ref=e36] [cursor=pointer]:
        - generic [ref=e37]: 단축키
        - generic [ref=e38]: 이 플랫폼의 키 조합
      - button "호환성 플랫폼별 자원 제어 지원 범위" [ref=e39] [cursor=pointer]:
        - generic [ref=e40]: 호환성
        - generic [ref=e41]: 플랫폼별 자원 제어 지원 범위
      - button "모든 설정 기본값으로" [ref=e43] [cursor=pointer]
    - region "AI 작업" [ref=e45]:
      - generic [ref=e46]:
        - heading "AI 작업" [level=2] [ref=e47]
        - generic [ref=e48]:
          - heading "빠른 설정" [level=3] [ref=e49]
          - paragraph [ref=e50]: 설치된 CLI를 찾아 모델 하나로 Lead·Builder·Reviewer·Integrator 팀을 한 번에 만듭니다.
          - button "다시 찾기" [ref=e52] [cursor=pointer]
          - generic [ref=e53]:
            - group "Codex" [ref=e54]:
              - generic [ref=e55]:
                - strong [ref=e56]: Codex
                - generic [ref=e57]: 버전 0.154.0
              - code [ref=e58]: /opt/homebrew/bin/codex
              - paragraph [ref=e59]: 로그인 기록 있음
              - paragraph [ref=e61]: 자동 실행 검증됨
              - generic [ref=e62]:
                - text: 모델
                - combobox "모델" [ref=e63]:
                  - option "gpt-5.6-luna · 검증됨" [selected]
                  - option "gpt-6-astra"
                  - option "gpt-5.6-sol"
                  - option "직접 입력…"
              - button "이 모델로 팀 만들기" [active] [ref=e64] [cursor=pointer]
              - status [ref=e65]: 팀 ‘Codex · gpt-5.6-luna’ 준비 완료. 이제 새 AI 작업을 시작할 수 있습니다.
            - group "Claude Code" [ref=e66]:
              - generic [ref=e67]:
                - strong [ref=e68]: Claude Code
                - generic [ref=e69]: 버전 2.1.271
              - code [ref=e70]: /opt/homebrew/bin/claude
              - paragraph [ref=e71]: 로그인 기록 있음
              - paragraph [ref=e73]: 이 PC에서 확인됨
              - generic [ref=e74]:
                - text: 모델
                - combobox "모델" [ref=e75]:
                  - option "claude-opus-5" [selected]
                  - option "opus"
                  - option "sonnet"
                  - option "haiku"
                  - option "직접 입력…"
              - button "이 모델로 팀 만들기" [ref=e76] [cursor=pointer]
            - group "Claude Code · Z.ai Coding Plan" [ref=e77]:
              - generic [ref=e78]:
                - strong [ref=e79]: Claude Code · Z.ai Coding Plan
                - generic [ref=e80]: 버전 2.1.271
              - code [ref=e81]: /opt/homebrew/bin/claude
              - paragraph [ref=e82]: 같은 Claude Code CLI를 Z.ai Coding Plan(GLM)으로 실행합니다 — 구독 로그인 대신 등록된 Z.ai API 키를 씁니다.
              - paragraph [ref=e83]: Z.ai API 키 상태 확인 불가
              - paragraph [ref=e85]: 자동 실행 미검증
              - generic [ref=e86]:
                - text: 모델
                - combobox "모델" [ref=e87]:
                  - option "glm-5.3[1m]" [selected]
                  - option "glm-5.3-flash[1m]"
                  - option "glm-5.3"
                  - option "직접 입력…"
              - generic [ref=e88]:
                - generic [ref=e89]:
                  - checkbox "실험적으로 사용" [ref=e90]
                  - text: 실험적으로 사용
                - paragraph [ref=e91]: Z.ai 경로는 출시 검증 대상이 아닙니다 — CLI는 같아도 이 경로의 결과 형식·취소·파일 변경 범위는 확인되지 않았습니다. 결과에 실험적 연결 표시가 남습니다. 동의는 이 연결에 한 번만 받습니다.
              - button "이 모델로 팀 만들기" [disabled] [ref=e92] [cursor=pointer]
              - paragraph [ref=e93]: 실험적으로 사용에 동의하면 팀을 만들 수 있습니다.
            - group "OpenCode" [ref=e94]:
              - strong [ref=e96]: OpenCode
              - paragraph [ref=e97]: 설치되지 않음
              - paragraph [ref=e98]: OpenCode는 저장된 연결이 필요합니다. 아래 직접 입력에서 설정하세요.
              - button "이 모델로 팀 만들기" [disabled] [ref=e99] [cursor=pointer]
        - group [ref=e100]:
          - generic "AI 작업 사용법" [ref=e101] [cursor=pointer]
        - generic [ref=e102]:
          - generic [ref=e103]:
            - heading "모델 연결" [level=3] [ref=e104]
            - paragraph [ref=e105]: AI 작업에 사용할 CLI와 모델을 등록합니다.
          - group [ref=e106]:
            - generic "직접 입력(고급)" [ref=e107] [cursor=pointer]
            - option "새 모델 연결" [selected]
            - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
            - option "Codex" [selected]
            - option "Claude Code"
            - option "OpenCode"
            - option "모델 선택" [selected]
            - option "gpt-6-astra"
            - option "gpt-5.6-sol"
            - option "gpt-5.6-luna"
            - option "직접 입력…"
            - option "CLI 로그인 / 구독" [selected]
            - option "API 키"
            - option "로컬 모델"
            - option "CLI의 사용자 설정"
        - generic [ref=e108]:
          - generic [ref=e109]:
            - heading "팀 템플릿" [level=3] [ref=e110]
            - paragraph [ref=e111]: 계획·구현·리뷰·충돌 해결 역할에 모델을 지정합니다.
          - generic [ref=e112]:
            - generic [ref=e113]: 모든 역할에 같은 모델
            - generic [ref=e114]: 한 모델로 네 역할을 한 번에 채웁니다. 역할별로 다른 모델을 쓰려면 아래에서 바꾸세요.
            - combobox "모든 역할에 같은 모델" [ref=e115]:
              - option "모델 선택" [selected]
              - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
              - option "gpt-5.6-luna"
              - option "gpt-6-astra"
              - option "gpt-5.6-sol"
              - option "claude-opus-5"
              - option "opus"
              - option "sonnet"
              - option "haiku"
              - option "glm-5.3[1m]"
              - option "glm-5.3-flash[1m]"
              - option "glm-5.3"
          - generic [ref=e116]:
            - generic [ref=e117]: 팀 이름
            - textbox "팀 이름" [ref=e118]
          - generic [ref=e119]:
            - heading "역할별 모델" [level=4] [ref=e120]
            - generic [ref=e121]:
              - generic [ref=e122]:
                - generic [ref=e123]: Lead · 계획
                - combobox "Lead · 계획" [ref=e124]:
                  - option "모델 선택" [selected]
                  - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
                  - option "gpt-5.6-luna"
                  - option "gpt-6-astra"
                  - option "gpt-5.6-sol"
                  - option "claude-opus-5"
                  - option "opus"
                  - option "sonnet"
                  - option "haiku"
                  - option "glm-5.3[1m]"
                  - option "glm-5.3-flash[1m]"
                  - option "glm-5.3"
              - generic [ref=e125]:
                - generic [ref=e126]: Builder · 구현
                - combobox "Builder · 구현" [ref=e127]:
                  - option "모델 선택" [selected]
                  - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
                  - option "gpt-5.6-luna"
                  - option "gpt-6-astra"
                  - option "gpt-5.6-sol"
                  - option "claude-opus-5"
                  - option "opus"
                  - option "sonnet"
                  - option "haiku"
                  - option "glm-5.3[1m]"
                  - option "glm-5.3-flash[1m]"
                  - option "glm-5.3"
              - generic [ref=e128]:
                - generic [ref=e129]: Reviewer · 독립 리뷰
                - combobox "Reviewer · 독립 리뷰" [ref=e130]:
                  - option "모델 선택" [selected]
                  - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
                  - option "gpt-5.6-luna"
                  - option "gpt-6-astra"
                  - option "gpt-5.6-sol"
                  - option "claude-opus-5"
                  - option "opus"
                  - option "sonnet"
                  - option "haiku"
                  - option "glm-5.3[1m]"
                  - option "glm-5.3-flash[1m]"
                  - option "glm-5.3"
              - generic [ref=e131]:
                - generic [ref=e132]: Integrator · 충돌 해결
                - combobox "Integrator · 충돌 해결" [ref=e133]:
                  - option "모델 선택" [selected]
                  - option "Codex · gpt-5.6-luna · gpt-5.6-luna"
                  - option "gpt-5.6-luna"
                  - option "gpt-6-astra"
                  - option "gpt-5.6-sol"
                  - option "claude-opus-5"
                  - option "opus"
                  - option "sonnet"
                  - option "haiku"
                  - option "glm-5.3[1m]"
                  - option "glm-5.3-flash[1m]"
                  - option "glm-5.3"
            - paragraph [ref=e134]: 감지된 모델을 고르면 팀을 저장할 때 그 모델의 연결을 만들고 설치를 확인합니다.
            - paragraph [ref=e135]: Claude Code의 Z.ai Coding Plan(GLM) 실행에는 설정 → 연동 → Z.ai Coding Plan에 등록된 키가 필요합니다. 터미널의 ccg 프로필과 같은 키이며, 키가 없으면 시작할 때 zai_key_missing으로 거절됩니다.
          - button "팀 저장" [ref=e137] [cursor=pointer]
          - generic [ref=e138]:
            - heading "저장된 팀" [level=4] [ref=e139]
            - list [ref=e140]:
              - listitem [ref=e141]:
                - strong [ref=e142]: Codex · gpt-5.6-luna
                - list [ref=e143]:
                  - listitem [ref=e144]: "Lead · 계획: Codex · gpt-5.6-luna"
                  - listitem [ref=e145]: "Builder · 구현: Codex · gpt-5.6-luna"
                  - listitem [ref=e146]: "Reviewer · 독립 리뷰: Codex · gpt-5.6-luna"
                  - listitem [ref=e147]: "Integrator · 충돌 해결: Codex · gpt-5.6-luna"
        - generic [ref=e148]:
          - generic [ref=e149]:
            - heading "저장소별 검증 명령" [level=3] [ref=e150]
            - paragraph [ref=e151]: AI 작업 결과에서 실행할 검증 명령을 저장소별로 저장합니다.
          - generic [ref=e152]:
            - heading "대상 저장소" [level=4] [ref=e153]
            - generic [ref=e154]:
              - generic [ref=e155]:
                - generic [ref=e156]: 저장소
                - textbox "저장소" [ref=e157]
              - button "저장소 확인" [disabled] [ref=e158] [cursor=pointer]
            - paragraph [ref=e159]: 저장소를 먼저 확인하면 이 저장소의 검증 명령을 저장할 수 있습니다.
          - generic [ref=e160]:
            - heading "검증 명령 추가" [level=4] [ref=e161]
            - generic [ref=e162]:
              - generic [ref=e163]:
                - generic [ref=e164]: 검증 이름
                - textbox "검증 이름" [ref=e165]
              - generic [ref=e166]:
                - generic [ref=e167]: 실행 프로그램
                - textbox "실행 프로그램" [ref=e168]:
                  - /placeholder: npm
              - generic [ref=e169]:
                - generic [ref=e170]: 인자 목록(공백 구분 또는 JSON 배열)
                - textbox "인자 목록(공백 구분 또는 JSON 배열)" [ref=e171]:
                  - /placeholder: test -- --run
              - generic [ref=e172]:
                - generic [ref=e173]: 저장소 안의 실행 경로(비우면 최상위)
                - textbox "저장소 안의 실행 경로(비우면 최상위)" [ref=e174]
          - button "검증 명령 저장" [disabled] [ref=e176] [cursor=pointer]
```

# Test source

```ts
  1   | /**
  2   |  * U02 — 새 AI 작업 대화상자: goal upload → mission.create → start.
  3   |  * start 실패 → 오류 + 재시도(같은 mission, 중복 생성 없음).
  4   |  *
  5   |  * 팀은 설정 → AI 작업의 빠른 설정으로 만든다(Codex 카드 · 검증 모델 gpt-5.6-luna).
  6   |  * mock probe가 이 조합을 데몬 registry처럼 검증해 네 역할이 모두 준비된다.
  7   |  * 수동 연결 폼 경로는 u07/u29가 다룬다.
  8   |  */
  9   | 
  10  | import { expect, test, type Locator, type Page } from "@playwright/test";
  11  | import { harnessFailNextStart, harnessMissions, prepare } from "./helpers";
  12  | 
  13  | const QUICK_TEAM = "Codex · gpt-5.6-luna";
  14  | 
  15  | test.beforeEach(async ({ page }) => {
  16  |   await prepare(page);
  17  |   await page.getByRole("button", { name: "설정", exact: true }).click();
  18  |   await page.locator(".settings-nav button").filter({hasText: /^AI 작업/}).click();
  19  |   const settings=page.locator(".mission-settings");
  20  |   const quick=settings.getByTestId("mission-quick-setup");
  21  |   const codex=quick.locator("[data-runtime=codex]");
  22  |   await expect(codex.locator(".mission-quick-setup-model select")).toHaveValue("gpt-5.6-luna");
  23  |   await codex.getByRole("button",{name:"이 모델로 팀 만들기",exact:true}).click();
  24  |   await expect(codex.locator(".mission-quick-setup-status")).toContainText(`팀 ‘${QUICK_TEAM}’ 준비 완료`);
  25  |   await expect(settings.locator("[data-setting-id=missionTeams] li").first()).toContainText(QUICK_TEAM);
> 26  |   await settings.getByTestId("mission-guide").locator("summary").click();
      |                                                                  ^ Error: locator.click: Error: strict mode violation: locator('.mission-settings').getByTestId('mission-guide').locator('summary') resolved to 2 elements:
  27  |   await expect(settings.getByTestId("mission-guide")).toContainText("Lead가 할 일을 나누고");
  28  |   await page.screenshot({path:"tests/mission-ui/__results/mission-settings.png",fullPage:true});
  29  |   await page.getByRole("button",{name:"터미널로 이동",exact:true}).click();
  30  | });
  31  | 
  32  | /** 새 대화상자는 첫 팀을 골라 두고, 완료 조건은 접혀 있다. */
  33  | async function fillDialog(page: Page, goal: string): Promise<Locator> {
  34  |   const dialog = page.getByTestId("mission-create");
  35  |   await expect(dialog).toBeVisible();
  36  |   await expect(page.getByTestId("mission-create-sidebar")).toBeVisible();
  37  |   await expect(page.locator(".modal-backdrop")).toHaveCount(0);
  38  |   await expect(dialog.getByTestId("create-team").locator("option:checked")).toHaveText(QUICK_TEAM);
  39  |   await dialog.getByTestId("create-goal").fill(goal);
  40  |   return dialog;
  41  | }
  42  | 
  43  | async function fillRepositoryAndRequirement(dialog: Locator): Promise<void> {
  44  |   const location = dialog.getByTestId("create-location");
  45  |   if (!(await location.evaluate((element) => (element as HTMLDetailsElement).open))) await location.locator("summary").click();
  46  |   await dialog.getByTestId("create-repo").fill("/repo");
  47  |   // 완료 조건은 선택 사항이라 접혀 있다 — 펼친 뒤 적는다.
  48  |   await dialog.getByTestId("create-requirements").locator("summary").click();
  49  |   await dialog.getByTestId("create-req-0").fill("The requested behavior works.");
  50  |   await expect(dialog.getByTestId("create-repo-status")).toContainText("/repo");
  51  |   await expect(dialog.getByTestId("create-submit")).toBeEnabled();
  52  |   await expect(dialog.getByTestId("create-submit-reason")).toHaveCount(0);
  53  | }
  54  | 
  55  | test("U02: 상단 새 AI 작업 버튼으로 mission을 만들고 시작한다", async ({ page }) => {
  56  |   await page.getByTestId("new-mission-button").click();
  57  | 
  58  |   const dialog = await fillDialog(page, "로그인 기능을 구현해 주세요.");
  59  |   await fillRepositoryAndRequirement(dialog);
  60  |   await dialog.getByTestId("create-submit").click();
  61  | 
  62  |   await expect(dialog).toBeHidden();
  63  |   await expect(page.getByTestId("mission-page")).toBeVisible();
  64  |   await expect.poll(async () => (await harnessMissions(page))[0]?.state).toBe("running");
  65  |   const missions = await harnessMissions(page);
  66  |   expect(missions.length).toBe(1);
  67  |   expect(missions[0].state).toBe("running");
  68  |   // goal artifact가 Lead 대화의 사용자 목표로 나타난다.
  69  |   await page.getByTestId("toggle-lead").click();
  70  |   const lead = page.getByTestId("lead-scroll");
  71  |   await expect(lead).toContainText("로그인 기능을 구현해 주세요.");
  72  | });
  73  | 
  74  | test("U02: start 실패는 오류+재시도를 표시하고 중복 mission을 만들지 않는다", async ({ page }) => {
  75  |   await page.getByTestId("new-mission-button").click();
  76  |   const dialog = await fillDialog(page, "결제 리팩터링");
  77  |   await harnessFailNextStart(page);
  78  |   await fillRepositoryAndRequirement(dialog);
  79  |   await dialog.getByTestId("create-submit").click();
  80  | 
  81  |   await expect(dialog).toBeHidden();
  82  |   await expect(page.getByTestId("mission-control-error")).toBeVisible();
  83  |   let missions = await harnessMissions(page);
  84  |   expect(missions.length).toBe(1);
  85  |   expect(missions[0].state).toBe("draft");
  86  | 
  87  |   await page.getByTestId("mission-start").click();
  88  |   await expect.poll(async () => (await harnessMissions(page))[0]?.state).toBe("running");
  89  |   await expect(dialog).toBeHidden();
  90  |   await expect(page.getByTestId("mission-page")).toBeVisible();
  91  |   missions = await harnessMissions(page);
  92  |   expect(missions.length).toBe(1);
  93  |   expect(missions[0].state).toBe("running");
  94  | });
  95  | 
  96  | for (const [label, width, zoom] of [["700", 700, 1], ["zoom200", 1440, 2]] as const) {
  97  |   test(`AI 작업 설정과 사용법은 ${label} 화면에서도 넘치지 않는다`, async ({ page }) => {
  98  |     await page.setViewportSize({ width, height: 900 });
  99  |     await page.getByRole("button", { name: "설정", exact: true }).click();
  100 |     await page.locator(".settings-nav button").filter({ hasText: /^AI 작업/ }).click();
  101 |     await page.evaluate((value) => { document.documentElement.style.zoom = String(value); }, zoom);
  102 |     const settings = page.locator(".mission-settings");
  103 |     await expect(settings.getByTestId("mission-guide")).toBeVisible();
  104 |     await settings.getByTestId("mission-guide").locator("summary").click();
  105 |     const overflow = await settings.evaluate((node) => ({ width: node.clientWidth, scroll: node.scrollWidth }));
  106 |     expect(overflow.scroll).toBeLessThanOrEqual(overflow.width + 1);
  107 |     await page.screenshot({ path: `tests/mission-ui/__results/mission-settings-${label}.png`, fullPage: false });
  108 |     await settings.getByRole("button", { name: "팀 저장", exact: true }).scrollIntoViewIfNeeded();
  109 |     await expect(settings.getByRole("button", { name: "팀 저장", exact: true })).toBeVisible();
  110 |   });
  111 | }
  112 | 
```