# 07. 한글 IME 입력 경로 (WKWebView × xterm 6.0)

이 문서는 macOS WKWebView에서 한글(두벌식) 입력이 PTY에 **사용자가 친 그대로** 도달하도록 하는 브리지(`src/features/terminal/imeBridge.ts`, `xtermSetup.ts::attachWebKitImeBridge`, `hangulAutomaton.ts`)의 계약이다. 모든 규칙은 실기 트레이스(`node_modules/.cache/iyagi-ime-trace.log`, 개발 서버에서 `iyagi-ime-trace` vite 플러그인이 기록)에서 확인된 이벤트 순서에 근거한다. 추측으로 규칙을 바꾸지 않는다 — 새 증상은 먼저 트레이스로 확정한다.

## 1. 관측된 이벤트 모델

### 1-1. WebKit 폴백 경로(기본)

macOS WebKit(WKWebView)의 한글 IME는 표준 조합 이벤트 대신 다음 순서로 hidden textarea를 편집한다.

| 단계 | 이벤트 | 비고 |
|---|---|---|
| 첫 자모 | `beforeinput/input insertText "ㅇ"` → `keydown keyCode=229 key="ㅇ"` | **input이 keydown보다 먼저** 온다 |
| 조합 진행 | `input insertReplacementText "아"/"안"` → `keydown 229` | 마지막 글자 교체 |
| 확정(커밋) | `input insertReplacementText` **값 불변** | 다음 키가 조합에 붙지 못할 때(새 음절·Space·Enter·기호·Backspace로 조합이 빌 때) 그 키 직전에 온다. 유휴만으로는 오지 않는다 |
| 커밋 뒤 실제 키 | `keydown 32 / 13 / 8 …` | 마커 직후 2–3ms |
| Backspace(조합 중) | `input insertReplacementText "아"` → `keydown 229 key="Backspace"` | 자소 분해, DEL을 보내면 안 된다 |
| Backspace(조합 없음) | `keydown 8` | xterm이 DEL |
| Space | `keydown 32` → `keypress 32` → `input insertText` (NBSP U+00A0일 수 있음) | keydown이 먼저 |
| Shift 자모 | `keydown 229 key="ㅆ" shift` | `keydown 16 Shift`는 조합 중에도 온다 |
| 입력 소스 전환 | `keydown 20 CapsLock` → (+16ms) `keydown 0 key="Unidentified"` → `keyup 20` | `kd 0`은 양방향 전환 모두에서 발화하며, 안 올 때도 있다 |

간헐적으로 표준 조합 경로(`compositionstart/update/end` + `insertCompositionText/deleteCompositionText/insertFromComposition`)가 나타나며, 같은 세션 안에서 다음 입력부터 폴백으로 되돌아갈 수 있다. 따라서 브리지는 `compositionupdate~compositionend` 구간에만 물러나고 영구 전환하지 않는다.

### 1-2. 시작·전환 직후의 활성화 경주 (핵심 결함)

앱 시작 직후(수 초~수십 초), 그리고 한/영 전환 직후 한동안 WebKit은 한글 입력 소스의 키를 IME에 넘기지 않는다. 실기에서 세 가지 형태가 관측됐다.

| 형태 | 관측 | 종전 결과 |
|---|---|---|
| A. 우회 자모 keydown | `keydown 68 key="ㅇ"` (229 아님, input 없음) | xterm이 단일 문자 keydown을 즉시 전송 → 셸에 `ㅇㅏㄴㄴㅕㅇ…` 자모 열 |
| B. 라틴 keydown + 늦은 IME 삽입 | `keydown 83 key="s"` → `keyup 83 key="ㄴ"` → (+130ms) `input insertText "ㄴ"` | xterm이 `s`를 보낸 뒤 IME가 ㄴ까지 넣어 `ㅇㅏs녕…` |
| C. 229 + 라틴 key, 삽입 없음 | `keydown 229 key="r"` → `keyup 82 key="ㄲ"`, input 없음 | 첫 자모(ㄱ) 유실 — IME가 키를 삼킴 |

형태 A는 4번의 앱 시작에서 각각 줄 전체가, 다른 시작에서는 첫 2–3키가 관측됐다. B의 keyup은 레이아웃이 전환됐음을 알려 주는 결정적 신호다.

### 1-3. xterm 6.0의 전송 규칙(브리지가 막지 않은 이벤트에 한함)

| 이벤트 | xterm 6.0 동작 |
|---|---|
| keydown, 수정키 없는 단일 문자(`keyCode ≥ 48`, `key.length === 1`) | **keydown에서 즉시 전송 + preventDefault**(`Keyboard.ts` default 분기, `cancel(ev, true)`). 자모 `key`도 예외가 아니다 |
| keydown, A–Z 대문자 | macOS IME HACK으로 전송하지 않고 keypress에 넘김. keypress가 전송하되 `cancelEvents=false`라 preventDefault 하지 않아 textarea에도 삽입된다(그 input은 `_keyDownSeen`으로 무시) |
| keydown 229 | `CompositionHelper.keydown` → `_handleAnyTextareaChanges`: setTimeout(0) 뒤 textarea 값 차이를 전송(브리지가 insert 이벤트를 전부 소유하므로 **중복 전송 위험만** 있다) |
| keydown Enter/Ctrl+C | 전송 후 `textarea.value = ""` |
| textarea blur | `_handleTextAreaBlur`가 `textarea.value = ""` (거울 데스싱크 원인 — 피어 핫픽스에서 자가 복구) |
| input insertText | `!_keyDownSeen`일 때만 data 전송 |

## 2. 불변식

1. **단일 전송 주체.** 모든 `insertText/insertReplacementText`는 브리지가 소유하고 xterm에는 전파하지 않는다(`stopPropagation`). xterm은 브리지가 통과시킨 keydown(비-IME 실제 키: 라틴·숫자·기호·제어키)만 보낸다. 229 keydown은 조합 여부와 무관하게 항상 차단한다.
2. **거울 불변식.** `shellValue`/`textValue`는 hidden textarea 값(NBSP→공백 정규화)의 거울이다. 브리지가 textarea 밖에서 만든 텍스트(오토마타 출력, 홀드 해제 라틴)는 거울에 넣지 않는다 — 양쪽이 모두 모르는 텍스트는 꼬리 diff가 건드리지 않는다.
3. **꼬리 편집만.** 셸 라인은 `DEL×n + 삽입`으로만 고친다. 앞쪽은 절대 건드리지 않는다(커서 이동 뒤 입력도 셸이 알아서 처리).
4. **textarea를 프로그램적으로 비우지 않는다.** WebKit IME는 커밋 뒤에도 조합을 내부에 들고 있어 비우면 다음 Backspace가 엉뚱한 자모를 복원한다(안→안아). 줄 리셋은 xterm의 CR/ETX와 blur에서만 일어나고 브리지는 `resync`로 따라간다.
5. **확정은 이벤트 신호로.** 새 음절 insertText·값 불변 마커·실제 키 keydown·compositionstart가 확정 신호다. 2초 백스톱은 안전망일 뿐이며, 백스톱 뒤 분해는 되돌린 글자만큼 DEL을 즉시 보낸다(`retractCommitted`).

## 3. 상태기계

브리지는 네 가지 소유 모드를 오간다.

| 모드 | 진입 | 소유자 | 이탈 |
|---|---|---|---|
| IME 폴백 | 기본 | 브리지(insert 이벤트 → 보류·확정) | — |
| 네이티브 조합 | `compositionupdate` | xterm CompositionHelper | `compositionend`(값으로 resync) |
| 오토마타(우회) | 229가 아닌 keydown의 `key`가 호환 자모(U+3131–U+3163) | 브리지 `HangulAutomaton` | 자모가 아닌 keydown·IME insert·compositionstart·resync에서 commit |
| 홀드 창 | attach 직후 · CapsLock(양방향) · `kd 0 Unidentified` · Shift+Space 강제 전환 | 브리지(라틴 keydown 보류) | 아래 표 |

### 3-1. 홀드 창(입력 소스 전환 불확실 구간)

CapsLock의 전환 방향은 알 수 없고(`imeMode` 추정은 풍선 표시용일 뿐) 시작 직후 활성화는 1초 이상 걸릴 수 있다. 그래서 창은 **양방향 모두** 열고, 판정은 시간이 아니라 **keyup의 `key`** 로 한다. Shift+Space 강제 전환(앱이 OS 입력 소스를 직접 바꾼다 — `hangulToggle.ts`·`bridge/ime.rs`)은 방향을 알지만 전환 직후 WebKit이 IME를 거치지 않는 경주는 같으므로 같은 창을 연다(`inputSourceToggled`: 보류 키·조합을 먼저 확정한 뒤 창을 다시 연다).

| 창이 열린 동안의 이벤트 | 처리 |
|---|---|
| 라틴 인쇄 keydown(수정키 없음, Space/Tab 제외) | 보류 목록에 넣고 `stopPropagation + preventDefault`(xterm 전송·textarea 삽입·keypress 모두 차단) |
| 보류 키의 keyup `key`가 **라틴** | 영문 확정 — 보류 전부 순서대로 방출, 창 닫힘 (영문 타이핑 지연 ≤ 첫 키 누름 시간) |
| 보류 키의 keyup `key`가 **자모** | 레이아웃이 한글로 바뀜 — 계속 보류, IME 삽입을 350ms 기다린다. 복원 시 이 keyup 자모(Shift 자모 포함)를 자판표보다 우선한다 |
| IME insert(insertText 한글 / insertReplacementText) | 마지막 보류 키는 이 삽입의 키이므로 버리고, 그 앞 보류 키들은 두벌식 자판표로 자모 복원 후 오토마타에 넣는다. 창 닫힘 |
| 우회 자모 keydown | 보류 전부 자모 복원 → 오토마타, 이어서 이 자모. 창은 유지(라틴 keydown이 오면 다시 보류) 하며 타이머를 연장 |
| 229 keydown | 보류 전부 자모 복원(IME가 삼킨 키) 후 창 닫힘 |
| 비인쇄 키(Enter·방향키·Backspace…) | 보류를 먼저 해소(자모 keyup이 있었으면 자모 복원, 아니면 라틴 방출) 후 키를 정상 처리 |
| 타이머 만료 | 자모 keyup이 있었으면 자모 복원, 아니면 라틴 방출. 창 닫힘 |
| `Unidentified`/keyCode 0/수정키 keydown | **투명** — 어떤 상태도 바꾸지 않는다(종전엔 `kd 0`이 홀드를 즉시 해제해 홀드가 무효했다) |

자모 복원으로 오토마타에 넣은 텍스트는 "잠정"으로 표시한다. 곧이어 IME가 같은 자모를 삽입하면(늦은 처리) 오토마타의 마지막 자모를 되돌리고(DEL) IME 텍스트를 따른다.

### 3-2. 오토마타(`hangulAutomaton.ts`)

표준 두벌식 규칙(초성·중성·종성, 복합 모음 7종, 복합 종성 11종, 받침 이동, ㄸ/ㅃ/ㅉ 종성 불가). 출력은 즉시 반영형 꼬리 편집이다: 자모마다 `DEL×(직전 조합 중 음절 길이) + 새 상태`, 앞 음절이 그대로 확정되면 새 음절만 덧붙이고, 받침 이동으로 앞 음절이 바뀌면 그것부터 다시 쓴다. Backspace는 현재 음절의 마지막 자모를 되돌리고(복합도 한 단계), 조합이 비어 있으면 xterm의 DEL에 맡긴다. IME 개입 경계에서는 오토마타 음절을 확정하고 IME에 넘긴다 — IME가 삼킨 키까지 복원할 수는 없어 경계 음절 하나가 갈라질 수 있다(`아녕` 형태). 이는 종전의 줄 전체 자모 유출·라틴 유출과 비교할 문제다.

## 4. 시그널 경로(제어키)

| 키 | 조합 보류 중 | 홀드 창 중 | 평상시 |
|---|---|---|---|
| Enter | 마커(커밋)가 먼저 와 보류분 전송 → `kd 13`은 xterm이 CR, textarea 비움 → `resync("")` | 보류 해소 후 CR | CR |
| Shift+Enter | 조합 먼저 확정 → LF 한 번(`shiftEnterAction`), keypress의 CR 억제 | 동일 | LF |
| Ctrl+C / Esc | 실제 키 keydown이 보류분 확정 → xterm ETX/ESC → `resync` | 보류 해소 후 처리 | ETX/ESC |
| Space | 마커 → `kd 32`는 브리지가 keydown/keypress만 차단(preventDefault 없음) → 기본 삽입 input을 브리지가 한 번 전송 | 홀드 대상 아님 | 동일 |
| Shift+Space(한/영 강제 전환이 켜졌을 때) | 브리지 keydown(capture)이 **코어보다 먼저** 판정 → 토글이 `preventDefault`(공백 삽입·keypress 없음)·전파 차단 → 조합 확정 → OS 입력 소스 전환(`ime_toggle_hangul`, 비동기) → 홀드 창 다시 엶. xterm custom key handler에만 두면 WebKit에서는 위 Space 차단에 막혀 불리지 않는다(실기 트레이스 회귀) | 보류 해소(비인쇄 키처럼) 후 동일 | OS 전환 + 홀드 창. 꺼져 있으면 Space와 같다 |
| Backspace | 229(분해)는 차단, 조합이 비면 마커 뒤 `kd 8`을 xterm이 DEL | 보류 해소 후 DEL | DEL(오토마타 조합 중이면 자모 되돌림) |
| Tab / 방향키 | 실제 키 → 보류분 확정 후 xterm | 홀드 대상 아님(Tab) / 보류 해소 후 처리 | xterm |
| 숫자·기호(`.` `/` `=` `~` …) | 마커 뒤 실제 keydown → xterm이 keydown에서 즉시 전송 | 라틴처럼 보류 | xterm |
| 대문자(Shift+영문) | — | 보류 | xterm keypress가 전송, 뒤따르는 textarea 삽입 input은 브리지가 실제 키 소유로 판정해 삼킨다(중복 없음) |

## 5. 검증

- 단위: `hangulAutomaton.test.ts`(조합 규칙·Backspace·꼬리 편집), `imeBridge.test.ts`(코어 상태기계: 확정 신호·Space 단일 소유·롤오버·홀드/keyup 판정·우회 자모·229 차단·투명 키·늦은 IME 중복 제거), `xtermSetupIme.test.ts`(attach 계층: preedit·캐럿 숨김·타이머·blur·Shift+Space 강제 전환), `hangulToggle.test.ts`(전환 판정·단일 진행·HUD·결과 콜백).
- 리플레이(`xtermSetupImeReplay.test.ts`): 실기 fixture 3개를 attach 계층 + **xterm 6.0 규칙을 흉내 낸 가짜 xterm**에 흘려 PTY 복원 줄이 사용자가 친 문장과 같아야 한다.
  - `webkit-ime-trace-hangul-sentence.tsv`: 1,922 이벤트 문장(마침표·Backspace 수정·NBSP·CapsLock·Shift 자모).
  - `webkit-ime-trace-launch-bypass.tsv`: 시작 직후 우회 자모 열 → `안녕하세요`.
  - `webkit-ime-trace-launch-mixed.tsv`: 우회 → 라틴 keydown+늦은 IME → IME 조합 → `아녕하세요`(라틴 유출 없음).
  - `webkit-ime-trace-capslock-hold-session.tsv`: 새 브리지가 부착된 실사용 세션(부착 홀드·CapsLock 홀드·Unidentified·blur·Shift 자모·명령어) → 자모 유출 없이 `계속`·`남은것도 끝내`.
- 새 증상 확인 절차: 개발 서버로 앱을 **재시작**(HMR은 이미 부착된 브리지를 교체하지 못한다) → 재현 → `node_modules/.cache/iyagi-ime-trace.log`의 마지막 run(작은 ts의 `attach` 줄부터)을 `node scripts/ime-trace-to-fixture.mjs <log> <from> <to>`로 fixture로 변환해 리플레이 테스트에 추가한다. 웹 인스펙터에서는 `window.__imeEvents`(최근 400개).

## 6. 알려진 한계

- 형태 C(229 + 라틴 key, 삽입 없음)는 IME가 키를 삼킨 것이라 복원하지 않는다(어떤 자모였는지는 알 수 있으나 IME의 이어지는 조합과 합칠 수 없다).
- IME 개입 경계에서 음절 하나가 갈라질 수 있다(§3-2).
- 커서 이동(방향키·Home/End·PgUp/PgDn) 뒤 Backspace는 WebKit IME가 마지막 음절을 여전히 조합 중이면 분해로 온다 — 네이티브 터미널의 "통째 삭제"와 다르다. textarea 비우기는 불변식 4 위반이므로, 이동 키에서 **선택 범위만 0→끝으로 흔든다**(`nudgeImeSelection`): WebKit은 선택이 바뀌면 입력 컨텍스트에 마킹 폐기를 알리므로 IME가 새 음절부터 시작한다. 실기 트레이스로 아직 확인되지 않은 완화다 — 효과가 없어도 종전 동작(분해)으로 남을 뿐 부작용은 없다. 확인 절차: 이동 키 뒤 Backspace가 `kd 8`(실제 키)로 오면 성공, `insertReplacementText`+`kd 229 Backspace`면 실패.
- 브리지는 모든 플랫폼에 부착된다. Chromium(WebView2)에서는 표준 조합 이벤트가 오므로 네이티브 조합 모드가 대부분을 맡고, 홀드 창은 CapsLock(실제 CapsLock)에서도 열리지만 keyup 판정으로 첫 키 누름 시간만큼만 지연된다.
