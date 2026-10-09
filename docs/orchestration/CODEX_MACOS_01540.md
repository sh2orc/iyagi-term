# Codex 0.154.0 — Apple Silicon macOS 검증

2026-09-16, macOS Darwin 25.6.0/aarch64, OpenAI 관리 ChatGPT 구독 로그인, `gpt-5.6-luna`.
이 문서는 이 조합의 읽기·쓰기 역할에 필요한 adapter 기능을 검증한 기록이다. 이후 production daemon·SQLite·실제 모델의 전체 미션 검증은 [별도 기록](CODEX_MACOS_MISSION_01540.md)에 보존했다. 어느 기록도 릴리스 판정을 의미하지 않는다.

## 실제 실행과 증거

| 검사 | 결과 | 범위 |
| --- | --- | --- |
| 읽기 전용 구조화 응답 | 통과 | production CodexAdapter → SupervisedPeer → native Exec → 실제 구독 추론 |
| 승인된 파일 생성과 구조화 응답 | 통과 | 소유한 임시 workspace의 `allowed.txt` 추가 1건만 승인; 외부 canary 보존 |
| 시작 직후 취소 | 통과 | matching turn/started 후 한 번 전송; RPC 수락과 interrupted 종료 확인 |
| 세 실행의 모델·정리 | 통과 | 요청/관측 모델 일치, 종료 레코드, native cleanup, 예약 해제 |
| sandbox 경계 12건 | 통과 | 별도 command/exec; 모델·인증 호출 없음 |

실제 모델 시험의 기록 저장소는 메모리 FaultStore다. production adapter, 인증 경로, native gate/Exec와 스트림을 사용하며 DiagnosticPeer는 관측만 추가한다. SQLite 영속성이나 목표→인수 전체 미션을 이 시험으로 주장하지 않는다. 승인 하니스는 item/started의 실제 변경이 단일 Add이고 소유한 `allowed.txt`를 가리키며 grantRoot가 없을 때만 응답한다. 다른 변경·명령은 거절한다. 제품의 untrusted 승인 정책은 그대로다.

Sandbox 검사는 읽기 전용 안/밖 쓰기 거절, workspace 안 쓰기 허용, sibling·외부 symlink·`.git`·`.codex`·`.agents`·TMPDIR·`/tmp` 쓰기 거절, 읽기/쓰기 모드의 loopback network 거절을 확인했다. app-server의 시작 cwd도 production처럼 workspace여야 한다. 초기 하니스가 상위 임시 경로에서 시작했을 때 쓰기 범위가 넓어지는 것을 확인하고 하니스를 수정했다. 제품은 기존 workspace cwd를 유지하며 절대 경로의 실제 디렉터리가 없는 writer 실행을 추가로 거절한다.

민감한 원문·숨겨진 추론을 담지 않은 기록:

- [실제 모델 세 실행](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-session.macos-01540.json), SHA-256 `f38e26477653e3a6eb40eaf40968e10c83eb35c0a5849cfae68566931cdebe16`.
- [sandbox 12개 경계](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/sandbox.macos-01540.json), SHA-256 `5ef4840bb4f1431068fc2b856a0801c1a23d5a6067fe543418d8463b38803b3c`.

## 발견한 문제와 수정

- 구독 로그인에 API-key 모델 endpoint를 강제해 account/read 성공 뒤 실제 추론이 401로 실패했다. 구독 모델 endpoint와 API-key endpoint, 계정 metadata endpoint를 구분했다.
- 기존 schema는 additionalProperties=false인 root에 properties가 없어 모든 DTO 필드를 거절했다. `{result: <typed DTO>}`의 닫힌 object로 바꾸고 내부 anyOf로 여섯 결과를 표현했다. 실제 모델 시험과 독립 JSON Schema validator를 사용했다. 세 adapter의 공통 parser는 새 envelope와 과거 직접 DTO를 엄격하게 처리한다. [Structured Outputs root 요구사항](https://developers.openai.com/api/docs/guides/structured-outputs#root-objects-must-not-be-anyof-and-must-be-an-object)
- turn/start 응답 직후의 interrupt는 turn/started보다 먼저 도착해 -32600으로 거절됐다. 의도를 먼저 보존하고 일치하는 시작 알림 후 한 번 전송하도록 수정했다. fixture에는 실제 순서의 시작 알림을 추가하고 지연/다른 turn/중복 취소 회귀를 추가했다.
- 빈 mcp_servers 설정은 사용자 설정과 병합되므로 기존 서버를 제거하지 못했다. thread별 서버 비활성 설정과 실제 상태·tool 목록 조회를 함께 검사한다. apps/plugins/hooks/하위 에이전트/브라우저/notify/shell profile 등의 비활성 설정도 작업 전 검증한다. [App Server](https://learn.chatgpt.com/docs/app-server), [설정 항목](https://learn.chatgpt.com/docs/config-file/config-reference)

## 재현

설치된 정확한 Codex 경로를 사용한다. 실제 구독 모델 시험은 유료 추론을 수행하며 일반 회귀에서는 ignored다. 아래 `<absolute-codex-path>`를 실제 실행 파일 경로로 바꾼다. 모델을 자동 대체하지 않는다.

```sh
python3 scripts/codex_sandbox_probe.py --program <absolute-codex-path> --out /tmp/codex-sandbox.json
```

```sh
IYAGI_CODEX_BIN=<absolute-codex-path> \
IYAGI_CODEX_VERSION=0.154.0 \
IYAGI_CODEX_MODEL=gpt-5.6-luna \
IYAGI_CODEX_EVIDENCE_OUT=/tmp/codex-live.json \
cargo test -p iyagi-termd --test mission_exec \
  codex_live::installed_subscription_runs_and_cancels_through_production_adapter \
  -- --ignored --exact --nocapture
```

`IYAGI_CODEX_LIVE_CASE=cancel`은 취소만 진단할 때 사용한다. 단일 사례 결과로 전체 증거를 갱신하지 않는다. 기록 수집은 registry를 자동 변경하지 않는다.

## 적용 범위

Registry가 runtime, macOS, aarch64, 정확한 CLI 버전, openai, 구독, 정확한 모델과 빈 저장 인증/endpoint 참조를 대조한다. 구조화 결과·이벤트·취소·파일 변경 승인·읽기·쓰기 범위·모델 목록만 지원으로 등록했다. resume/steer/usage/native_terminal_attach는 미검증으로 유지한다. 다른 OS·버전·모델·인증 방식으로 쓰기 증거를 옮기지 않는다.

설정 → AI 작업에서 이 연결을 저장하고 설치 확인을 누르면 Lead/Builder/Reviewer/Integrator에 배정할 수 있다. 이 조합의 전체 모델 미션은 위 별도 기록에서 확인했다. 다른 CLI/OS 조합, executable identity 고정, 조회/시작 사이 교체, 릴리스 검증 등은 [구현 현황](../../docs/orchestration/IMPLEMENTATION_STATUS.md)에 남아 있다.
