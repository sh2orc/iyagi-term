# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.

# Test info

- Name: u29-installation-probe.spec.ts >> 설치 확인의 실패와 미저장 변경을 700 화면에서 구분한다
- Location: tests/mission-ui/u29-installation-probe.spec.ts:6:3

# Error details

```
Error: expect(locator).toBeEnabled() failed

Locator: locator('.mission-settings').getByRole('button', { name: '설치 확인', exact: true })
Expected: enabled
Timeout: 10000ms
Error: element(s) not found

Call log:
  - Expect "toBeEnabled" locator('.mission-settings').getByRole('button', { name: '설치 확인', exact: true }) with timeout 10000ms
  - waiting for locator('.mission-settings').getByRole('button', { name: '설치 확인', exact: true })

```

```yaml
- main "설정":
  - heading "설정" [level=1]
  - paragraph: 언어와 화면, 터미널 기본값, 실행 프로필과 자원 정책을 한곳에서 관리합니다. 각 항목의 저장 버튼으로 적용합니다.
  - button "터미널로 이동"
  - navigation "설정 그룹":
    - searchbox "설정 검색"
    - button "일반 언어와 화면 테마"
    - button "터미널 글꼴 크기, 기본 셸, 닫을 때 동작"
    - button "자원 관리 작업별 CPU·메모리 한도와 자동 일시정지·양보"
    - button "트랙패드 두 손가락 제스처로 탭 전환"
    - button "연동 AI 구독 사용량, Z.ai 라우팅과 CLI hooks"
    - button "AI 작업 여러 AI 에이전트에 목표 맡기기 — 빠른 설정·팀·검증"
    - button "관리 실행 실행할 도구와 자원 할당"
    - button "실행 프로필 명령어, 환경 변수와 기본 정책"
    - button "단축키 이 플랫폼의 키 조합"
    - button "호환성 플랫폼별 자원 제어 지원 범위"
    - button "모든 설정 기본값으로"
  - region "AI 작업":
    - heading "AI 작업" [level=2]
    - heading "빠른 설정" [level=3]
    - paragraph: 설치된 CLI를 찾아 모델 하나로 Lead·Builder·Reviewer·Integrator 팀을 한 번에 만듭니다.
    - button "다시 찾기"
    - group "Codex":
      - strong: Codex
      - text: 버전 0.154.0
      - code: /opt/homebrew/bin/codex
      - paragraph: 로그인 기록 있음
      - paragraph: 자동 실행 검증됨
      - text: 모델
      - combobox "모델":
        - option "gpt-5.6-luna · 검증됨" [selected]
        - option "gpt-6-astra"
        - option "gpt-5.6-sol"
        - option "직접 입력…"
      - button "이 모델로 팀 만들기"
    - group "Claude Code":
      - strong: Claude Code
      - text: 버전 2.1.271
      - code: /opt/homebrew/bin/claude
      - paragraph: 로그인 기록 있음
      - paragraph: 이 PC에서 확인됨
      - text: 모델
      - combobox "모델":
        - option "claude-opus-5" [selected]
        - option "opus"
        - option "sonnet"
        - option "haiku"
        - option "직접 입력…"
      - button "이 모델로 팀 만들기"
    - group "Claude Code · Z.ai Coding Plan":
      - strong: Claude Code · Z.ai Coding Plan
      - text: 버전 2.1.271
      - code: /opt/homebrew/bin/claude
      - paragraph: 같은 Claude Code CLI를 Z.ai Coding Plan(GLM)으로 실행합니다 — 구독 로그인 대신 등록된 Z.ai API 키를 씁니다.
      - paragraph: Z.ai API 키 상태 확인 불가
      - paragraph: 자동 실행 미검증
      - text: 모델
      - combobox "모델":
        - option "glm-5.3[1m]" [selected]
        - option "glm-5.3-flash[1m]"
        - option "glm-5.3"
        - option "직접 입력…"
      - checkbox "실험적으로 사용"
      - text: 실험적으로 사용
      - paragraph: Z.ai 경로는 출시 검증 대상이 아닙니다 — CLI는 같아도 이 경로의 결과 형식·취소·파일 변경 범위는 확인되지 않았습니다. 결과에 실험적 연결 표시가 남습니다. 동의는 이 연결에 한 번만 받습니다.
      - button "이 모델로 팀 만들기" [disabled]
      - paragraph: 실험적으로 사용에 동의하면 팀을 만들 수 있습니다.
    - group "OpenCode":
      - strong: OpenCode
      - paragraph: 설치되지 않음
      - paragraph: OpenCode는 저장된 연결이 필요합니다. 아래 직접 입력에서 설정하세요.
      - button "이 모델로 팀 만들기" [disabled]
    - group: AI 작업 사용법
    - status: 저장했습니다.
    - heading "모델 연결" [level=3]
    - paragraph: AI 작업에 사용할 CLI와 모델을 등록합니다.
    - group:
      - text: 직접 입력(고급) 저장된 모델
      - combobox "저장된 모델":
        - option "새 모델 연결"
        - option "Probe model · fixture-model" [selected]
      - text: 표시 이름
      - textbox "표시 이름": Probe model
      - text: CLI 종류
      - combobox "CLI 종류":
        - option "Codex" [selected]
        - option "Claude Code"
        - option "OpenCode"
      - text: CLI 실행 파일의 전체 경로
      - textbox "CLI 실행 파일의 전체 경로": /fixture/codex
      - text: 제공자 ID
      - textbox "제공자 ID": openai
      - text: 모델 ID
      - combobox "모델 ID":
        - option "모델 선택"
        - option "gpt-6-astra"
        - option "gpt-5.6-sol"
        - option "gpt-5.6-luna"
        - option "직접 입력…" [selected]
      - textbox "모델 ID 직접 입력": fixture-model
      - text: 인증 방식
      - combobox "인증 방식":
        - option "CLI 로그인 / 구독" [selected]
        - option "API 키"
        - option "로컬 모델"
        - option "CLI의 사용자 설정"
      - text: 실행 1회 예상 비용(USD, 선택) 실행 예약에 사용할 양수 금액을 입력합니다. 비우면 비용 미확인입니다. 예상값은 이미 예약된 실행에 소급 적용되지 않으며 제공자의 실제 청구 상한이 아닙니다.
      - textbox "실행 1회 예상 비용(USD, 선택) 실행 예약에 사용할 양수 금액을 입력합니다. 비우면 비용 미확인입니다. 예상값은 이미 예약된 실행에 소급 적용되지 않으며 제공자의 실제 청구 상한이 아닙니다."
      - paragraph: 구독은 Codex CLI에서 ChatGPT로 로그인한 계정을 씁니다(데몬과 CLI가 같은 CODEX_HOME을 써야 합니다). API 키 로그인으로는 구독 실행을 시작하지 않으며, 로컬·사용자 지정 인증은 아직 지원하지 않습니다.
      - button "모델 저장"
      - button "지금 확인"
      - paragraph: 미확인
    - heading "팀 템플릿" [level=3]
    - paragraph: 계획·구현·리뷰·충돌 해결 역할에 모델을 지정합니다.
    - text: 모든 역할에 같은 모델 한 모델로 네 역할을 한 번에 채웁니다. 역할별로 다른 모델을 쓰려면 아래에서 바꾸세요.
    - combobox "모든 역할에 같은 모델":
      - option "모델 선택" [selected]
    - text: 팀 이름
    - textbox "팀 이름"
    - heading "역할별 모델" [level=4]
    - text: Lead · 계획
    - combobox "Lead · 계획":
      - option "모델 선택" [selected]
    - text: Builder · 구현
    - combobox "Builder · 구현":
      - option "모델 선택" [selected]
    - text: Reviewer · 독립 리뷰
    - combobox "Reviewer · 독립 리뷰":
      - option "모델 선택" [selected]
    - text: Integrator · 충돌 해결
    - combobox "Integrator · 충돌 해결":
      - option "모델 선택" [selected]
    - paragraph: 감지된 모델을 고르면 팀을 저장할 때 그 모델의 연결을 만들고 설치를 확인합니다.
    - paragraph: Claude Code의 Z.ai Coding Plan(GLM) 실행에는 설정 → 연동 → Z.ai Coding Plan에 등록된 키가 필요합니다. 터미널의 ccg 프로필과 같은 키이며, 키가 없으면 시작할 때 zai_key_missing으로 거절됩니다.
    - button "팀 저장"
    - heading "저장된 팀" [level=4]
    - paragraph: 아직 저장된 팀이 없습니다. 위에서 역할별 모델을 고른 뒤 팀을 저장하세요.
    - heading "저장소별 검증 명령" [level=3]
    - paragraph: AI 작업 결과에서 실행할 검증 명령을 저장소별로 저장합니다.
    - heading "대상 저장소" [level=4]
    - text: 저장소
    - textbox "저장소"
    - button "저장소 확인" [disabled]
    - paragraph: 저장소를 먼저 확인하면 이 저장소의 검증 명령을 저장할 수 있습니다.
    - heading "검증 명령 추가" [level=4]
    - text: 검증 이름
    - textbox "검증 이름"
    - text: 실행 프로그램
    - textbox "실행 프로그램":
      - /placeholder: npm
    - text: 인자 목록(공백 구분 또는 JSON 배열)
    - textbox "인자 목록(공백 구분 또는 JSON 배열)":
      - /placeholder: test -- --run
    - text: 저장소 안의 실행 경로(비우면 최상위)
    - textbox "저장소 안의 실행 경로(비우면 최상위)"
    - button "검증 명령 저장" [disabled]
```

# Test source

```ts
  1  | import { expect, test } from "@playwright/test";
  2  | import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
  3  | import { prepare } from "./helpers";
  4  | 
  5  | for (const layout of ["700", "zoom200"] as const) {
  6  |   test(`설치 확인의 실패와 미저장 변경을 ${layout} 화면에서 구분한다`, async ({ page }, testInfo) => {
  7  |     await prepare(page);
  8  |     await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 1000 });
  9  |     await page.evaluate(() => {
  10 |       const client=window.__mockDaemon!.client as MockDaemonClient;
  11 |       const probe=client.bindingProbe.bind(client);let count=0;
  12 |       client.bindingProbe=async params=>{
  13 |         const result=await probe(params);count+=1;
  14 |         return {...result,binding:{...result.binding,runtime_version:count===1?"0.154.0":null},installation:count===1?"verified":"timed_out"};
  15 |       };
  16 |     });
  17 |     await page.getByRole("button",{name:"설정",exact:true}).click();
  18 |     await page.locator(".settings-nav button").filter({hasText:/^AI 작업/}).click();
  19 |     if (layout === "zoom200") await page.evaluate(()=>{document.documentElement.style.zoom="2";});
  20 |     const settings=page.locator(".mission-settings");
  21 |     await settings.getByTestId("mission-model-advanced").locator("summary").click();
  22 |     await settings.getByLabel("표시 이름",{exact:true}).fill("Probe model");
  23 |     await settings.getByLabel("CLI 실행 파일의 전체 경로",{exact:true}).fill("/fixture/codex");
  24 |     await settings.getByLabel("모델 ID",{exact:true}).selectOption({label:"직접 입력…"});
  25 |     await settings.getByLabel("모델 ID 직접 입력",{exact:true}).fill("fixture-model");
  26 |     await settings.getByRole("button",{name:"모델 저장",exact:true}).click();
  27 |     const probe=settings.getByRole("button",{name:"설치 확인",exact:true});
> 28 |     await expect(probe).toBeEnabled();await probe.click();
     |                         ^ Error: expect(locator).toBeEnabled() failed
  29 |     await expect(settings.getByTestId("mission-settings-notice")).toContainText("설치된 CLI 파일과 버전을 확인하고 저장했습니다");
  30 |     await expect(settings.locator(".mission-settings-card").first()).toContainText("0.154.0");
  31 |     await settings.getByLabel("표시 이름",{exact:true}).fill("Unsaved change");
  32 |     await expect(probe).toBeDisabled();
  33 |     await expect(settings).toContainText("변경한 모델 설정을 먼저 저장한 뒤");
  34 |     await settings.getByLabel("표시 이름",{exact:true}).fill("Probe model");
  35 |     await expect(probe).toBeEnabled();await probe.click();
  36 |     await expect(settings.getByTestId("mission-settings-notice")).toContainText("버전 조회가 3초 안에 끝나지 않아 중단했습니다");
  37 |     await expect(settings.locator(".mission-settings-card").first()).not.toContainText("0.154.0");
  38 |     await settings.getByTestId("mission-settings-notice").scrollIntoViewIfNeeded();
  39 |     expect(await settings.evaluate(node=>node.scrollWidth-node.clientWidth)).toBeLessThanOrEqual(1);
  40 |     await page.screenshot({path:testInfo.outputPath(`installation-${layout}.png`)});
  41 |   });
  42 | }
  43 | 
```