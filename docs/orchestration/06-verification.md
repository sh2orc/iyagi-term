# 06. 시험과 출시 판정

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. 검증 단계

| 단계 | 목적 | 유료 모델 필요 |
|---|---|---|
| 명세 검사 | 링크·defaults·참조 SQL·전이 사례 | 없음 |
| core/contract/storage | 순수 규칙·CAS·transaction·schema | 없음 |
| fake E2E | 생성부터 인수/실패/복구까지 | 없음 |
| adapter compatibility | 특정 CLI/provider/auth/OS의 실제 동작 | 명시적인 실제 연동 시험에서만 |
| GUI/부하 | 화면·포커스·IME·bounded rendering | 없음 |
| 제품 품질 비교 | 실제 개발 결과·개입·시간·사용량 | 별도로 승인한 평가 예산 |

fake와 live 결과를 다른 보고서로 기록한다. 한 모델만 성공한 결과를 모든 binding 지원으로 표시하지 않는다.

명세 SQL 검증기는 baseline 0001에서 O1 migration 0002 파일을 항상 검사한다. 별도로 개발 중인 0002 파일이 checkout에 있으면 0001+0002에서도 같은 검사를 추가로 수행하고 적용한 baseline을 출력한다. 명세만 커밋된 checkout도 독립 검사가 가능하며, 실제 O03 migration 검증에서는 배포 대상의 모든 migration을 반드시 포함한다.

## 2. 필수 core/storage 사례

| ID | 입력/사건 | 반드시 확인할 결과 |
|---|---|---|
| E01 | 동일 request_id/payload 2회 | mission/run/event 하나, 같은 최초 응답 |
| E02 | 동일 request_id 다른 payload | REQUEST_CONFLICT, 추가 side effect 없음 |
| E03 | 같은 revision의 두 plan 적용 | 하나만 commit, 다른 요청 REVISION_CONFLICT |
| E04 | dependency cycle/self/missing/cross-mission | plan 전체 거절, task 0개 추가 |
| E05 | 완료 task를 plan에서 수정/retire | 거절, 기존 결과 보존 |
| E06 | 동시에 두 scheduler tick | task당 live Run 하나 |
| E07 | 두 mission ready, 한쪽 대량 task | round-robin으로 다른 mission도 시작 |
| E08 | slot/resource/quota 차단 | 원인 표시, runnable 다른 binding 진행 |
| E09 | pause 중 run 완료 | 새 dispatch 없음, 마지막 run 뒤 paused |
| E10 | cancel 직후 late result | cancelled task 부활 없음, 결과는 이력만 |
| E11 | old fencing token callback | 새 Run/projection 변경 없음 |
| E12 | prepared/unsent 상태에서 crash | 정책 허용일 때만 복구 실행 |
| E13 | sending 직후 crash | unknown, 자동 POST/turn 재전송 없음 |
| E14 | final 수신 후 DB failure | 성공 오표시 없음, 재조회/복구 |
| E15 | cancel ack지만 process 살아 있음 | stopping/lease 유지 |
| E16 | 동일 workspace writer 두 개 | 하나만 lease 획득 |
| E17 | obsolete approval에 answer | STALE_DECISION, provider 승인 없음 |
| E18 | reply commit 후 connection loss | answer 보존, delivery unknown, 중복 승인 금지 |
| E19 | partial stream + exit0 | typed final 없으면 RESULT_INVALID |
| E20 | 모델/인증 미지원 | 임의 fallback/유료 API 전환 없음 |
| E21 | task attempts/repair/start budget 경계 | 상한 도달 시 새 실행 0, decision 생성 |
| E22 | 오래된 quota/null usage | 0으로 표시/계산하지 않음 |
| E23 | artifact chunks 재전송/offset/hash 오류 | 중복 bytes 없음, 실패 body 채택 안 함 |
| E24 | 타 mission artifact/decision/run ID | scope 오류, 원문 유출 없음 |
| E25 | 계획 UTF-8/JSON escaping으로 frame 초과 | frame 전송 전 거절 |
| E26 | snapshot pagination 중 mutation | page 기준점 동일, 전체 적용 뒤 갱신 |
| E27 | notification drop/dup/gap/reconnect | snapshot으로 동일 최종 상태 |
| E28 | migration 각 statement에서 fault | DDL/version 함께 rollback, 기존 R1 rows 보존 |
| E29 | title/path/log에 token/HTML/control chars | secret redaction, unsafe HTML 실행 없음 |
| E30 | consult가 마지막 slot 사용 | 부모 checkpoint 종료 후 자문 시작, deadlock 없음 |

이 사례는 해당 reducer branch를 그대로 재구현한 테스트가 아니라 observable storage/event/side-effect count를 검증한다. mock now/UUID/adapter를 주입해 sleep 없이 실행한다.

## 3. Workspace와 결과 시험

| ID | 시나리오 | 합격 기준 |
|---|---|---|
| W01 | dirty/staged/untracked 원 repo | 기본 시작 차단, stash/commit/reset 없음 |
| W01b | 같은 repo에 `include_uncommitted` 시작 | private snapshot base, 사용자 index/checkout/branch/HEAD 불변, ignored 제외 |
| W02 | API/UI 서로 다른 worktree 편집 | 원 repo 불변, source run별 manifest |
| W03 | 같은 파일 상충 변경 | integration conflict 기록, 묵시적 ours 선택 없음 |
| W04 | 허용 path 밖/외부 symlink 변경 | 결과 거절, 사용자 데이터 삭제 없음 |
| W05 | 후보A 검증 후 후보B 생성 | A 증거를 B 통과로 재사용하지 않음 |
| W06 | verifier가 입력 수정 후 복원 | observed 유지, enforced 오표시 없음 |
| W07 | timeout/로그 유실/unknown input | 검증 passed 금지 |
| W08 | open blocking/major finding | 인수 거절 |
| W09 | strict 검증에 observed만 있음 | 인수 거절; 일반 policy는 명시 확인 요구 |
| W10 | 현재 candidate와 다른 ID 인수 | STALE_CANDIDATE |
| W11 | 작업 중 archive/close | archive 거절, close는 실행 유지 |
| W12 | 만료 artifact 참조/retained dirty workspace | CONTENT_EXPIRED/자동삭제 없음 |

Git fixture는 임시 저장소에서 수행한다. 사용자의 실제 저장소를 테스트 대상으로 수정하지 않는다. tmp path는 mktemp 또는 language tempfile API로 생성하고 fixture 소유가 확인된 경로만 정리한다.

## 4. GUI 승인 시나리오

실제 browser rendering 시험은 Vitest renderToString만으로 대체하지 않는다. O16에서 Playwright를 dev dependency로 추가하고 `test:mission-ui` script로 실행한다. Tauri 실제 OS 시험은 별도 manual/automation 결과를 남긴다.

| ID | 화면과 조작 | 합격 기준 |
|---|---|---|
| U01 | 기존 v1 workspace→v2 | terminal ID/분할/포커스 보존 |
| U02 | mission 생성·start timeout | draft/동일 request 복구, 중복 mission 없음 |
| U03 | 1→4→12 agents 자동 생성 | 상위 탭/현재 선택/입력 focus 유지 |
| U04 | 한글 조합 중 Enter/Shift+Enter | 전송 0, 조합 완료 후 명시 전송 1 |
| U05 | task 클릭 후 Lead composer 전송 | 수신자 Lead 유지 |
| U06 | 담당에게 추가 지시 | task ID/label 고정, 별도 draft |
| U07 | queued/unknown delivery | 전달됨으로 오표시 없음 |
| U08 | 두 개 고정/세 번째 추가 | 교체 선택, 새 run/PTY spawn 없음 |
| U09 | mission/agent-view 탭 닫기 | 실행 cancel count 0 |
| U10 | 일반 terminal 닫기 | 기존 종료 확인/동작 유지 |
| U11 | 타 탭에서 결정 요청 발생 | badge만, 강제 전환/모달 없음 |
| U12 | 위로 scroll 중 text stream | 읽는 위치 보존, 새 활동 버튼 |
| U13 | 1440/1024/700px와200% zoom | 겹침/본문 손실 없음, focus 복원 |
| U14 | keyboard/screen reader/reduced motion | 모든 제어 접근 가능, token 낭독 없음 |
| U15 | reconnect/snapshot expiry | draft와선택 보존, mutation 재활성은 sync 후 |
| U16 | 결과 화면에서 candidate 교체 | 이전 인수 버튼 disable |
| U17 | 5번째 숨김 terminal view | LRU detach, run 유지, 재표시 replay |
| U18 | quit/close all tabs | mission Exec 포함, 일반 terminal과 이중 집계 없음 |

각 U03/U11/U13/U16은 screenshot artifact를 남긴다. screenshot만으로 행동을 검증하지 않고 DOM/IPC spy assertions와 함께 제출한다.

## 5. 결정적 통합 fixture

표준 mission은 plan→API 구현/UI 구현 병렬→통합→검증→독립 review→인수다. fake adapter는 fixture repo의 두 파일을 서로 다른 workspace에서 변경하고 verifier는 실제 로컬 프로세스로 결과를 확인한다. adapter 결과를 흉내내기만 하고 Git/DB/Exec를 전부 mock하지 않는다.

변형: API 실패1회 후retry, review finding→repair 후보2, plan cycle 거절, 결정 질문 중 pause, start crash, cancellation 미확인, 디스크 상한, provider 교체 인계. 매 fixture마다 raw event 순서/row 수/최종 candidate hash/외부 실행 횟수를 검증한다.

## 6. 성능 목표

다음은 측정 전 목표이며 고정 장비·빌드·fixture·5회 반복으로 median/p95를 보고한다. 외부 모델 지연은 UI/daemon 지연과 분리한다.

- 12개 fake run, 총 activity 200 events/sec에서 화면 입력 반응 p95≤100ms.
- 선택 상세 전환 p95≤150ms(이미 받은 metadata), cold artifact 로딩 시간은 별도 보고.
- pause/cancel RPC 접수 p95≤250ms(local daemon); 실제 provider 중단 시간은 별도.
- terminal foreground echo는 기존 기준 p95≤50ms/p99≤100ms를 유지.
- 30분 activity flood에서 cache 상한 도달 후 지속 선형 메모리 증가 없음. 12개 run의 UI metadata 증분 RSS 목표≤150MiB; provider child RSS와 webview를 구분해 보고.
- hidden mission view cache≤4, rendered activity≤200, snapshot≤connection당2×8MiB.
- event flood가 state/cancel/answer control path를 굶기지 않음.

## 7. 제품 평가

대표 실제 작업을 유형별로 최소20개 고정한다: 단순 버그, API+UI 기능, 여러 파일 refactor, test failure 진단, 불명확 요구, 보안 관련 변경. 가능한 동일 base/요구/예산에서 단일 강한 agent, 수동 다중 CLI, O1을 비교한다. 작업·설정·실패도 모두 기록하고 좋은 사례만 선별하지 않는다.

핵심 측정: 독립 인수 통과율, 사용자 수정/설명 시간, 재작업 횟수, blocking defect, 전체 경과 시간, 관측 사용량/미관측 비율, 장애 후 중복 실행/유실. agent 자기평가를 정답으로 쓰지 않는다. 역할 배정 학습용 task와 평가용 task를 분리한다.

출시 필수: E/W/U 필수 case 통과, serious regression 0, 주장한 모든 runtime/auth/OS capability의 evidence, 유료 fallback 0, unknown outcome 자동 재시작 0. 품질 우위가 측정되지 않으면 `최고 수준 입증` 문구를 쓰지 않고 개선 과제를 기록한다.

## 8. 실행 명령과 증거

현재 명세 검사는 입구의 명령을 사용한다. 구현 후 필수 명령:

```sh
npm run typecheck
npm run test:unit
npm run test:mission-ui
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p term-contracts -p term-core -p term-storage --locked
cargo test -p iyagi-termd --test mission_e2e --locked
cargo test --workspace --locked
python3 scripts/smoke_e2e.py
npm run build:frontend
```

`test:mission-ui`와`mission_e2e`는 O16/O17에서 추가할 대상이며 현재 존재한다고 가정하지 않는다. ts-rs 생성물 비교는 dirty worktree의 전체 git diff가 아니라 실행 전후 해당 생성 파일 차이로 판정한다. 기존 변경을 stale 결과로 오판하지 않는다.

티켓별 증거는 구현 상태 문서(IMPLEMENTATION_STATUS.md)의 해당 절: 구현 범위, 대상 OS/runtime 버전, 실행 명령/exit code, 실패·skip 이유, fixture/screenshot 경로, 알려진 한계. 실제 secret/prompt 전체를 보고서에 복사하지 않는다.
