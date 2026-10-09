# 00. 제품과 아키텍처

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. 고정한 제품 결정

1. 상위 탐색 단위는 `Mission`(사용자에게는 작업)이다. 한 작업에 여러 에이전트가 참여한다.
2. 기본 화면은 왼쪽 Lead 대화, 오른쪽 참여 작업 목록과 선택한 실행 상세다. 자동으로 상위 탭을 늘리지 않는다.
3. 일반 터미널은 계속 제공한다. 기존 임의 CLI 세션의 대화를 자동 수입하거나 제어 가능한 세션으로 간주하지 않는다.
4. runtime, provider, model, account/auth route, role을 독립 식별한다. 같은 Claude Code 실행 도구도 Anthropic과 Z.ai에 별도 binding을 가질 수 있다.
5. 영속 작업 상태·실행 소유권·검증 결과는 daemon이 소유한다. React mount/unmount는 실행 시작·종료 명령이 아니다.
6. Lead는 특별한 무제한 권한의 프로세스가 아니다. 구조화된 계획·결정 요청을 제출하고 엔진이 검증·적용한다.
7. 실행 실패, 미확인 결과, 사용자 결정 대기, 자원 대기는 구별한다. 프로세스 종료와 요구사항 충족도 구별한다.
8. 사용자 구독을 유지하는 경로와 별도 API 과금 경로를 표시한다. 한도 소진 시 유료 API로 자동 전환하지 않는다.
9. 사용자는 기본 팀으로 시작할 수 있지만 최초 설정에서는 유효한 Lead/Builder/Reviewer binding을 선택해야 한다. 연결되지 않은 모델을 대신 선택하지 않는다.
10. O1의 편집 실행은 Git 저장소와 daemon 소유 작업 공간을 요구한다. 비 Git 경로는 일반 터미널로 계속 사용한다.

## 2. 역할

| role | 입력과 결과 | 기본 권한 |
|---|---|---|
| lead | 사용자 목표/진행 상태 → 계획·재계획·진행 보고 | 읽기, 제한된 위임 도구 |
| researcher | 질문/코드 기준 → 출처 있는 조사 결과 | 읽기; 네트워크는 정책에 따름 |
| architect | 요구/조사 → 인터페이스·결정·제약 | 읽기 |
| builder | 작업 계약/작업 공간 → 코드 변경·결과 선언 | 할당 작업 공간 편집·검증 명령 |
| test_author | 요구/실패 조건 → 테스트 변경 | 할당 작업 공간 편집 |
| reviewer | 고정 후보/요구 → 근거 있는 findings | 읽기 |
| specialist | 분야/질문/관련 코드 → 자문·findings | 읽기 |
| diagnostician | 실패 로그/후보 → 재현·원인·수정 제안 | 기본 읽기; 실험은 별도 작업 공간 |
| integrator | 후보들/기준 → 충돌 해결 변경 | 통합 작업 공간 편집 |
| documenter | 검증된 결과 → 문서 변경 | 할당 작업 공간 편집 |

Verifier는 LLM role이 아닌 결정적 명령 실행기다. 인수 평가는 reviewer의 `review` 결과와 사용자 `mission.accept`로 구성한다. 필요 없는 역할은 생성하지 않는다. 하나의 binding을 여러 역할에 사용할 수 있지만 reviewer run은 구현 run의 대화 세션을 재사용하지 않는다.

## 3. 계층과 파일 배치

```text
React Mission UI / 기존 terminal UI
       ↓ DaemonClient + 생성된 Rust DTO
Tauri bridge: 기존 사용자 전용 IPC를 운반
       ↓
iyagi-termd::mission (새 orchestrating service)
  ├─ term-core::mission (순수 reducer, DAG, scheduling 결정)
  ├─ term-storage::mission (동일 SQLite writer, snapshot/event/outbox)
  ├─ iyagi-termd::agent_runtime (Codex/Claude/OpenCode/Fake adapter)
  ├─ iyagi-termd::exec (pipe/HTTP lifecycle와 OS resource ownership)
  └─ iyagi-termd::workspace (Git, artifacts, verification)
```

새 crate는 O1에서 추가하지 않는다. 기존 `orchestrator.rs`는 terminal workload 수명 관리자로 남긴다. 이름이 같다는 이유로 mission engine 전체를 이 파일에 추가하지 않는다.

| 현재 파일/구조 | 재사용/수정 지점 |
|---|---|
| `crates/term-contracts/src/rpc.rs` | 65,536 bytes **prefix 포함** 제한 유지; O1 RPC/event enum 추가 |
| `crates/iyagi-termd/src/dispatch.rs` | mission 분기만 위임; Git/adapter I/O를 직접 작성하지 않음 |
| `crates/iyagi-termd/src/state.rs` | `MissionService` handle 추가; 기존 workload lock 재사용 금지 |
| `crates/iyagi-termd/src/scheduler.rs` | 기존 resource admission 재사용을 위한 포트; DAG는 새 core 모듈 |
| `crates/term-storage/src/{storage,writer,ops,queries}.rs` | 동일 writer에 O1 transaction command 추가 |
| `crates/term-storage/src/migration.rs` | 적용 SQL과 version 기록을 한 transaction에 넣은 후 새 migration 등록 |
| `src/features/daemon/{client,mockClient}.ts` | O1 typed methods와 fake 이벤트 추가 |
| `src/features/bridge/realClient.ts` | O1 params/result/event 연결, 오류 details 보존 |
| `src/store/{workbenchStore,workspaceStorage}.ts` | 탭 union과 workspace v2, 실행 상태는 새 mission store |
| `src/features/terminal/{registry,sessionController}.ts` | 기존 terminal semantics 유지; mission view에 한해 attach/detach 연결 |
| `src-tauri/src/bridge/subscriptions.rs` | 기존 usage 조회 유지; scheduling은 daemon 소유 provider 관측기로 이동/공유 |

새 코드는 `crates/term-contracts/src/mission/`, `crates/term-core/src/mission/`, `crates/iyagi-termd/src/{mission,agent_runtime,exec,workspace}/`, `src/features/missions/`에 둔다. TS 모델 타입은 `src/generated/`에서만 import한다. 로컬 UI 선택 상태만 TS 수기 interface를 사용한다.

## 4. 자원과 서비스 경계

PTY가 없는 JSONL 프로세스도 자원을 사용한다. 새 `ExecRecord`가 program process/OS ownership/admission reservation을 연결한다. 기존 workload 또는 새 Exec 중 **한 곳만** 같은 프로세스를 자원 원장에 등록한다. OpenCode 서버 공유는 O1에서 하지 않고 run별 process를 사용한다. 이는 초기 격리 정책이며 최적화는 증거가 있을 때 별도 변경한다.

OS 제한 없는 플랫폼에서 실행 자체를 금지하지는 않지만 `require` 정책을 충족할 수 없으면 시작을 거절한다. 작업 공간 경로 분리는 보안 sandbox와 동일하지 않다. 권한 계약을 enforce할 수 없는 adapter는 해당 자동 편집 역할에 배정하지 않는다.

## 5. 구현 단계와 장기 기능

- O01–O07: 계약·영속성·fake runtime·workspace 기반. 실제 사용 가능한 오케스트레이션으로 표시하지 않음.
- O08–O10: 공식 runtime 세 가지와 provider/auth binding, 버전별 호환 시험.
- O11–O14: 목표에서 인수까지 엔진·질문·검증·복구.
- O15–O18: 최종 UI, 접근성·IME·부하·세 OS 검증, O1 출시 판정.
- O19: 프로젝트 근거 기반 자동 배정과 비교 실행.
- O20: 원격 executor와 외부 앱/PR/배포 연동. 로컬 인수 이후의 별도 권한 범위.
- O21: 검증된 외부 adapter 확장 계약과 서명·버전 관리.

범용 agent loop, 전체 IDE, 임의 서비스 GUI 자동화는 O1에서 만들지 않는다. 검증된 native runtime의 기능을 adapter로 연결한다. 이를 이유로 O1의 상태 복구·검증 추적·협업 UI 품질을 축소하지 않는다.
