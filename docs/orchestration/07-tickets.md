# 07. 구현 티켓과 작업 지시

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. 구현자가 따를 공통 절차

한 번에 한 티켓을 맡는다. 해당 티켓의 선행 완료 증거가 없으면 없는 API를 mock으로 숨겨 완료 처리하지 않는다. 문서에 정한 파일 경로가 이후 변경됐으면 실제 symbol을 검색해 대응 경로를 확인하고 증거에 기록한다.

1. `git status --short`로 기존 변경을 확인하고 담당 범위 밖 변경을 보존한다.
2. 명세 입구, 해당 주제 문서, 09 구현 보충, contracts.ts/defaults/states의 관련 항목을 읽는다.
3. 아래 입력·출력·실패 조건을 구현한다. provider flag/SDK 호출을 기억에 의존해 추측하지 않는다.
4. 지정된 E/W/U 사례와 해당 package 검사를 실행한다.
5. changed path만 검토하고 구현 상태 문서(IMPLEMENTATION_STATUS.md)의 해당 절에 증거를 작성한다.
6. 하위 구현에서 발견한 규칙 변경은 원본 계약과 fixture를 함께 수정한다. placeholder가 production 경로로 남아 있으면 완료가 아니다.

이 티켓들은 추론을 최소화하도록 나눈 **구현 단위**다. 모든 task/role을 별도 AI agent에 동시에 맡기라는 지시는 아니다. 공유 파일 충돌이 있으므로 병렬 구현은 별도 조정이 있을 때만 한다.

## 2. 의존 순서

```text
O01 → O02 → O03 → O04
               ├→ O05 → O06
               └→ O07
O04+O06+O07 → O08/O09/O10
O04+O06+O07 → O11 → O12 → O13 → O14
O02+O04 → O15 → O16
O08..O16 → O17 → O18
O18 → O19 / O20 / O21 / O22
```

## O01. 기반 목록과 feature gate

- 입력: 00 문서, 현재 코드, 기존 R1 동작.
- 변경: `crates/iyagi-termd/src/config.rs`, contract hello capability, UI feature discovery.
- 구현: `mission_protocol:1`의 선언과 실제 기능 활성화를 분리한다. 기능 미구현이면 false/absent. 런타임 탐지 목록과 dirty 파일을 기록한다. feature gate default는 development에서만 on 가능, release는 O18 전 off.
- 기존 terminal start/attach/cancel smoke를 baseline으로 기록한다. 시간/메모리 기준 장비도 기록한다.
- 완료: gate off일 때 기존 화면·RPC가 그대로 동작하고 O1 자동 모델 호출이 0이다.

## O02. Rust 타입과 순수 validator

- 선행: O01. 변경: `term-contracts/src/mission/{mod,types,rpc,validation}.rs`, `src/generated/`.
- contracts.ts의 모든 public DTO/enum/Rpc params/result를 Rust로 옮기고 serde/ts-rs export한다. ID/U64는 기존 newtype을 재사용한다. MissionState와 기존 WorkloadState는 다른 타입이다.
- ProviderResult와 wire AgentResult를 별개 enum으로 구현한다. provider local_key→UUID 변환은 O11 service 책임으로 둔다.
- UUID, UTF-8 byte, path, enum, result variant, null, unknown field 정책을 검사한다. 입력 DTO는 serde deny_unknown_fields; 응답 forward fields는 허용한다.
- E04/E20/E24/E25의 계약 부분, 모든 상태 edge의 양성·음성 table test를 작성한다.
- 완료: TS 생성물이 참조 계약과 필드/enum/nullability가 일치, hand-written duplicate wire type 없음.

## O03. Migration과 영속 원자성

- 선행: O02. 변경: `term-storage/src/{migration,storage,writer}.rs`, `term-storage/src/mission/`, 새 migration, tests.
- 먼저 기존 migration DDL+version row를 한 transaction에 넣는다. 0001의 자체 version insert와 충돌 없이 처리하고 0002 upgrade를 확인한다.
- 참조 SQL을 다음 번호 migration으로 등록한다. artifact body는 SQLite에 넣지 않는다. projection/event/request/outbox mutation API를 writer command 하나로 제공한다.
- `apply_mission_transition(expected_revision, fingerprint, transition)`와 read snapshot/event/request APIs를 구현한다. nested transaction 금지.
- E01/E02/E03/E06/E28, duplicate SQL constraints, rollback 후 재오픈, 기존 R1 row preservation 시험.
- 완료: side-effect intent와 상태가 부분 저장되지 않으며 schema/version crash gap이 없다.

## O04. RPC·Snapshot 동기화

- 선행: O03. 변경: daemon dispatch의 작은 O1 분기, `mission/service.rs`, realClient/DaemonClient/MockDaemonClient.
- contracts.ts Rpc의 mission/artifact/binding/template/verification 메서드를 typed stub가 아닌 storage+domain handler에 연결한다. 아직 실행 미구현 메서드는 CAPABILITY_UNSUPPORTED로 응답한다.
- snapshot materialize/page/cache/expiry와 mission.changed hint를 구현한다. frame encoder를 공용 사용한다.
- mutation timeout→request.get→동일 ID 재전송 흐름, error.details를 TS client에 보존한다.
- E01/E02/E25/E26/E27. snapshot 중 mutation/연결 종료/cache release도 시험한다.
- 완료: fake client와 real IPC가 같은 DTO/오류를 주고 대용량 목록이 프레임 한도를 넘지 않는다.

## O05. Artifact 저장과 인계 자료

- 선행: O03. 변경: `workspace/artifacts.rs`, storage upload/artifact commands, context loader.
- begin/write/commit/read, offset 중복 확인, fsync/rename/hash, staging client scope, mission 채택을 구현한다.
- body size/quota/expiry와 private path 검사, content expired/corrupt 상태를 구현한다. secret redaction은 adapter 저장 경로에 삽입 가능한 포트로 제공한다.
- E23/E24/E29, W12. zero-byte/multibyte/중간 chunk 실패/commit 재호출을 포함한다.
- 완료: 큰 목표를 작은 RPC frame으로 보낼 수 있고 잘못된 bytes/소유권은 채택되지 않는다.

## O06. Workspace·lease·candidate

- 선행: O05. 변경: `workspace/{git,leases,capture,integration}.rs`.
- clean repo/base 검사, detached worktree 생성, ownership marker와 독점 writer lease, private snapshot ref를 구현한다.
- actual diff/manifest와 binary/delete/untracked/symlink/허용 path 검사. candidate는 새 ID로만 생성한다.
- deterministic integration과 충돌 결과를 구현하고 AI conflict resolver 실행은 O13에 위임한다.
- W01–W05/W12, E16. 임시 Git repo에서만 시험한다.
- 완료: 원 사용자 checkout 불변, 동시 writer 거절, source→candidate 추적 가능.

## O07. Exec supervisor와 Fake adapter

- 선행: O03/O05. 변경: `exec/{mod,process,ownership,output}.rs`, `agent_runtime/{mod,fake}.rs`.
- PTY 없는 pipe process lifecycle, OS resource admission/원장, bounded stdout/stderr, process identity/취소를 구현한다. 같은 process를 기존 workload와 중복 집계하지 않는다.
- adapter port와 normalized events, fake clock/scripted results, fencing token을 구현한다.
- fake child가 file write/output flood/ignore interrupt/late exit를 재현하도록 term-fixture를 확장한다.
- E11/E15/E19/E29, 기존 terminal resource smoke.
- 완료: UI 없이도 fake run을 시작/관찰/중단하고 종료 확인까지 lease와 resource reservation이 유지된다.

## O08. Codex adapter

- 선행: O04/O06/O07. 변경: `agent_runtime/codex/`, versioned sanitized protocol fixtures.
- 03 문서의 initialize→account/model→thread→turn→events→result→interrupt 흐름을 구현한다.
- 설치 버전 schema를 fixture로 고정하고 required capability만 opt-in한다. model mismatch/null usage/approval correlation 처리.
- recorded stream 기반 offline test를 먼저 완료한다. 실제 계정 시험은 별도 승인된 workload로 수행하고 auth route/OS별 evidence를 남긴다.
- 완료: 지원하는 조합에서 계획 결과·취소·재개 확인, 미지원 조합은 이유 있는 disabled. activity viewer 기본.

## O09. Claude Code adapter

- 선행: O04/O06/O07. 변경: `agent_runtime/claude/`, print stream fixtures.
- run별 config/auth route, stdin prompt, stream/result 변환, owned resume, interrupt/cleanup을 구현한다.
- bare/subscription 차이, unsupported steer/approval_reply, Anthropic/Z.ai 설정 간섭을 시험한다.
- permission 범위를 enforce하지 못하는 binding은 자동 builder에 노출하지 않는다. all-permission flags로 시험 통과 금지.
- 완료: print adapter가 실제 지원하는 기능만 capability로 광고하고 queued message를 정확하게 표현한다.

## O10. OpenCode adapter와 binding 설정

- 선행: O04/O06/O07. 변경: `agent_runtime/opencode/`, daemon provider observer, settings mission bindings.
- run별 authenticated local server, health/session/message/SSE/abort/status 확인, typed result normalization.
- Z.ai coding/general API 경로 구분, 명시 model ID, quota freshness, credential ref 주입과 redaction.
- binding/template/verification command 설정 CRUD와 CAS를 연결한다. capability test evidence 없으면 unsupported 표시.
- 완료: Codex/Claude/OpenCode binding을 별도 모델·provider로 저장하고 실행 간 설정 누출이 없다.

## O11. 계획 검증과 DAG scheduler

- 선행: O04/O06/O07. 변경: `term-core/src/mission/{reducer,plan,scheduler}.rs`, `mission/{planner,dispatch}.rs`.
- bootstrap plan task, ProviderResult local_key mapping, artifact 등록, PlanProposal 검증·적용을 구현한다.
- task dependencies, retire/replacement, round-robin/global/mission/binding caps, resource admission, prepared outbox.
- 기본 fixed team과 사용자 선택 모델을 사용한다. O19 adaptive score를 먼저 구현하지 않는다.
- E04–E08/E21/E30, fake plan→parallel builder scenario.
- 완료: 계획은 원자 적용되고 cycle/limit/권한 위반으로 실행되지 않는다.

## O12. 메시지·결정·전문가 요청

- 선행: O11. 변경: `mission/{messages,decisions,delegation}.rs`, adapter answer/send 연결.
- queued/delivered/unknown, Lead/target task 구분, exact approval correlation, stale decision CAS.
- 08 문서의 전문 자문 요청과 checkpoint 후 대기, 결과의 원 task 인계를 구현한다. agent-facing 도구는 authenticated run scope만 사용한다.
- policy.update와 사용자 finding dismissal은 별도 mutation이며 audit에 남긴다.
- E17/E18/E24/E30. 한 사용자의 answer와 다른 창 answer race 포함.
- 완료: 질문이 모이고 독립 작업은 계속되며 승인 또는 user message가 중복 적용되지 않는다.

## O13. 통합·검증·리뷰·인수

- 선행: O11/O12. 변경: `mission/{workflow,verification,review,acceptance}.rs`.
- 필수 writer 완료 뒤 integration, 충돌 시 integrator, 후보 고정 후 command runner와 independent reviewer 실행.
- typed findings/repair proposal/candidate replacement, requirement matrix, observed/enforced 구분, accept predicate를 구현한다.
- W03/W05–W10, fake review finding→repair→새 검증→인수 시나리오.
- 완료: UI 없이 목표부터 completed까지 실제 Git/command/store를 사용하는 통합 시험 통과.

## O14. Outbox 복구·취소·예산

- 선행: O13. 변경: `mission/{outbox,recovery,budgets,cancel}.rs`, daemon startup/shutdown.
- prepared/sending/acknowledged crash 구분, native inspect, stale actor fencing, unresolved workspace quarantine.
- pause drain/cancel propagation/timeout escalation과 rate-limit backoff, attempt/repair/start/time/cost 상한.
- E09–E15/E18/E20–E22, crash injection 각 boundary.
- 완료: unknown을 자동 retry하지 않고 닫힌 UI/daemon 재시작에서도 영속 이력을 복구한다.

## O15. 탭 union과 UI store

- 선행: O02/O04. 변경: workbenchStore/workspaceStorage/TabItem/Workbench/nativeMenu/Modals, `features/missions/{store,uiStore,selectors}.ts`.
- terminal/mission/agent-view union, v1→v2, kind guard, snapshot 원자 적용, badge selectors, hidden mission list.
- 기존 controller의 close/quit와 mission close를 분리한다. snapshot change가 기존 terminal output render를 반복시키지 않게 한다.
- U01/U09/U10/U15/U18, 기존 terminal tests.
- 완료: 일반 terminal에 회귀 없이 mission 탭을 열고 숨기고 재복원한다.

## O16. 완성 UI와 browser 시험

- 선행: O15. 변경: `features/missions/{MissionPage,LeadConversation,TeamList,RunDetail,DecisionPanel,ResultReview,MissionCreate}.tsx`, CSS/i18n.
- 05의 레이아웃·입력·정렬·고정·responsive·accessibility·오류 상태를 모두 구현한다.
- fake scenario controls는 dev/test build에만 제공한다. test:mission-ui에 실제 browser 테스트 runner를 추가한다.
- U02–U08/U11–U17 screenshot+interaction assertions. 1/4/12 agents와 unknown/failed/completed fixtures.
- 완료: 화면을 보고 모든 상태와 제어를 이해할 수 있고 IME/keyboard/좁은 화면 시험 통과.

## O17. 수직 통합과 장애·부하 시험

- 선행: O08–O16. 변경: `iyagi-termd/tests/mission_e2e.rs`, GUI fixture, benchmark harness, CI.
- fake mission을 real IPC/Git/process/store로 수행하고 app disconnect/daemon crash/provider timeout/디스크 상한을 주입한다.
- 06의 E/W/U 전체와 30분 flood를 실행한다. provider child와 app RSS를 분리한다.
- 완료: 모든 필수 deterministic case, 성능 목표 또는 공개된 실패 분석/수정 증거. skip을 pass로 집계하지 않는다.

## O18. 실제 호환성과 출시 증거

- 선행: O17. 변경: compatibility fixtures/report, packaging/설정 migration, 기능 flag.
- 실제 승인된 계정/예산에서 runtime/auth/OS별 end-to-end와 06 제품 평가를 수행한다.
- macOS/Windows/Linux의 지원 기능 표를 완성하고 capability 없는 조합은 UI에서 차단한다.
- 비용·개입·완료율과 baseline 비교, 알려진 한계, rollback/기존 workspace 복원 확인.
- 완료: O1 필수 기준 통과 후에만 release flag 활성화. 최고 수준 성능을 주장하려면 별도 비교 증거 필요.

## O19. 적응형 배정과 비교 실행

- 선행: O18. 입력: 08 문서. 변경: `mission/{routing,evaluation}.rs`, 팀 전략 UI.
- 사용자 preset→설명 가능한 routing→별도 evaluation split을 순서대로 구현한다. shadow proposal은 실제 배정을 바꾸지 않는다.
- quality-only/latency/budget 전략과 unknown metric 처리, provider fallback은 allowlist+auth route 정책을 지킨다.
- 완료: frozen baseline 대비 평가, config version/rollback, task leakage 없는 holdout 시험.

## O20. 원격·외부 결과 반영

- 선행: O18. 입력: 기존 원격 명세와 새 ADR. 변경: executor adapter/remote journal/외부 action outbox.
- 원격 실행 lease/heartbeat/동일run inspect/네트워크 단절 시 unknown, artifact hash 회수부터 구현한다.
- PR/push/merge/deploy는 사용자 설정된 외부 action scope와 별도 idempotency를 요구한다. 로컬 accept를 publish 승인으로 간주하지 않는다.
- 이 티켓 착수 전 transport/provider별 하위 명세를 추가해야 한다. O1에서 지원한다고 표시하지 않는다.
- 완료: 원격 장애와 외부 중복 동작 시험, 서비스별 권한·복구 증거.

## O21. 외부 adapter 확장

- 선행: O18. 입력: 03 port와 별도 adapter protocol ADR.
- 외부 plugin manifest에 protocol/version/capabilities/auth route/실행 경로를 선언하고 sandbox/서명/업데이트 정책을 정의한다.
- fixture conformance suite 통과 전 자동 작업 지원을 허용하지 않는다. 임의 plugin 코드를 daemon 내부에 동적 로딩하지 않는다.
- 이 티켓 착수 전 wire protocol과 배포/검증 명세를 추가한다. 모든 코딩 서비스를 동일 수준으로 지원한다고 미리 주장하지 않는다.
- 완료: 독립 sample adapter가 install→probe→run→cancel→recover conformance를 통과.

## O22. 로컬 호환성 증거와 신뢰 등급

- 선행: O18. 입력: [11 문서](11-local-evidence.md). 변경: `crates/term-contracts/src/mission/{types.rs,rpc.rs}`, `src/generated/{Binding,CompatibilityGrade,LocalEvidence,LocalProbeReport,LocalRunEvidence}.ts`(ts-rs 출력 형식을 손으로 미러), `docs/orchestration/contracts.ts`; `crates/iyagi-termd/src/agent_runtime/{capability_evidence.rs,local_probe.rs,mod.rs,detection.rs}`; `crates/iyagi-termd/src/mission/{service.rs,binding_evidence.rs,run_evidence.rs,execution.rs,actor.rs,mod.rs}`; `src/features/missions/{bindingSupport.ts,QuickSetup.tsx,MissionSettings.tsx,MissionCreate.tsx,RunDetail.tsx,ResultReview.tsx,TeamList.tsx,configuration.ts}`, `src/i18n/sections/missions.ts`, `src/features/daemon/mockClient.ts`, 관련 `*.test.ts(x)`; 이 문서 묶음(`ORCHESTRATION_SPEC.md`, `01-contracts.md`, `03-adapters.md §6`, `05-ui.md`, `IMPLEMENTATION_STATUS.md`, 사용자 가이드).
- 출시 fixture 하나로 묶던 capability 증거를 출시(`null`)/같은 라인(`version_line`)/로컬 자가 진단(`local_probe`)/실행 관측(`observed_runs`)/사용자 동의(`experimental_opt_in`) 다섯 층으로 나누고 `Support.reason_code`로 구분한다.
- 모델 호출·인증 없이 이 PC에서 프로토콜 handshake·OS sandbox 경계·모델 목록 존재를 확인하는 런타임별 자가 진단과 성공 실행 관측 승격을 추가해 새 등급 `CompatibilityGrade::VerifiedLocally`를 도입한다.
- 실험적 연결 동의를 연결당 한 번으로 바꿔 CLI 업데이트마다 재동의를 요구하던 `experimental_version_mismatch`를 제거하고, 자가 진단이 실패로 증명한 기능은 동의로도 열리지 않게 한다.
- UI에 연결별 신뢰 칩(출시 검증됨/이 PC에서 확인됨/실험적/미확인)과 근거 한 줄, `지금 확인` 버튼의 자가 진단 통합, 필수 기능 기준 실험적 연결 배지를 반영한다.
- 완료: [11 §9](11-local-evidence.md#9-시험-작성만-실행은-사용자가-요청할-때)의 결정적 시험(버전 라인 경계·KNOWN_BREAKS, capability 층 우선순위, `local_probe_failed`가 동의를 이기는지, runs 임계치와 모델 불일치 무시, `version_line`이 protocol_ok 없이는 미적용, Claude `--help` 파서와 Codex sandbox 매핑, `runtime.detect`의 `verified_locally`/`same_line_unverified` 등급, `binding.probe`의 local_evidence 저장·보존·초기화와 `binding.save`의 클라이언트 값 폐기, Run 관측 카운터·CAS 재시도·Fake 미기록, 프론트 등급 문구·신뢰 칩·재동의 UI 부재·필수 기능 기준 배지) 작성·통과와 `npm run verify:contracts`의 `src/generated` 재생성 diff 없음, 실제 CLI(Codex/Claude/OpenCode)에서 자가 진단·실행 관측 1회 이상 확인.

## 3. 티켓 완료 보고 형식

```text
티켓: Oxx
선행 증거:
구현한 계약/요구사항:
변경 파일:
검사 명령과 결과:
E/W/U case IDs:
실제 runtime/OS 시험 여부:
남은 제한과 미구현 항목:
명세 변경이 있었다면 근거:
```

모델 성능이 낮더라도 입력·출력·검증 기준을 바꾸어 티켓을 끝내지 않는다. 모순은 숨기지 않고 최소 정정안을 작성하며, 실제 지원 여부가 외부 상태에 달린 경우 evidence 없는 capability를 false로 유지한다.
