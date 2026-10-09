# Codex 0.154.0 — 실제 모델 전체 미션

2026-09-16, Apple Silicon macOS(Darwin 25.6.0/aarch64), OpenAI 관리 ChatGPT 구독, `gpt-5.6-luna`.
production `iyagi-termd`의 IPC·SQLite·Git·native Exec와 설치된 Codex를 사용해 목표부터 인수까지 통과했다. fixture 모델이나 capability 우회는 사용하지 않았다. 이 결과는 아래 한 조합과 정상 완료 경로의 증거이며 릴리스 판정은 아니다.

## 수행한 흐름

1. 소유한 임시 Git 저장소와 모델 연결을 만들고 실제 설치 확인을 수행했다.
2. Lead가 두 개의 독립 필수 Builder 작업을 계획했다. 각 작업의 쓰기 범위는 해당 파일 하나다.
3. 두 Builder가 별도 worktree에서 `sum.txt`와 `label.txt`를 작성했다. 실제 파일 추가 요청 두 건의 경로·작업 범위·writer 소유권·grantRoot 부재를 확인한 뒤 IPC로 승인했다.
4. 데몬이 변경을 통합하고 고정된 후보를 만들었다. Python 검증 명령은 파일 바이트가 각각 `42\n`, `verified\n`인지 검사해 통과했다.
5. 별도 Reviewer가 후보 파일의 실제 바이트와 Git 변경 경로를 읽고 해당 candidate ID에 대한 리뷰를 완료했다. 하니스는 정확한 workspace에서 `od` 두 건과 저장된 base/candidate OID의 `git diff --no-ext-diff --name-only` 한 건만 승인했다.
6. 인수 직전에 두 파일 내용·변경 경로 집합·현재 후보의 Passed 검증·성공 리뷰를 확인했다. `mission.accept` 성공과 같은 요청의 멱등 재생을 확인했다.
7. 모델 실행 네 건이 모두 성공했고 요청/관측 모델이 일치했다. 자동 통합·검증을 포함한 여섯 Run이 성공했다. 모든 Exec의 Exited와 실행 슬롯 해제, 원래 저장소의 HEAD·clean 상태 보존을 확인했다.

[보존한 결과 JSON](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-mission.macos-01540.json), SHA-256 `191ae000a90140e182c4fae7a9b5716a48bc329eb590fdf9427c2361748cc6b5`.
결과에는 실행 상태, 요청/관측 모델, 소유 임시 경로와 승인 내용만 담았다. 비밀 인증값과 숨겨진 추론은 보존하지 않았다.

## 발견한 문제와 수정

- 복잡한 객체 `anyOf` 출력 형식에서 실제 Lead가 계획 결과 대신 불가 응답을 반환했다. Plan/Review에는 `iyagi-result-v2`의 평탄한 nullable 필드를 사용하고, kind별 활성 필드·비활성 null·빈 배열 규칙을 프롬프트의 스키마에도 제공한다. Question/Blocked 응답을 유지한다. 디코더는 비활성 데이터, 누락 필드, 활성 배열의 null, 잘못된 ID와 알 수 없는 필드를 거절하며 canonical ProviderResult로 다시 검증한다. 다른 작업과 기존 보관 결과 형식도 지원한다.
- Codex thread와 reroute의 모델 관측이 메모리에만 남았다. fenced ModelObserved 이벤트로 SQLite Run에 기록한다. 지정 모델과 다르거나 인증된 thread에서 모델이 확인되지 않으면 작업을 중단한다.
- 파일 변경 승인 요청에 경로와 diff가 없어 사용자가 변경 내용을 검토할 수 없었다. 일치하는 thread/turn/item의 제한된 변경 캐시에서 상세를 연결한다. 화면은 실제 경로·작업 종류·이동 경로·diff·추가 root 권한을 표시하며, 상세를 확인할 수 없으면 승인을 비활성화한다.

초기 실패 시도는 통과 수치에 포함하지 않는다. 객체 union 제거만으로 성공시키는 진단 중간본은 채택하지 않았다. 최종 형식은 질문·중단 응답과 엄격한 검증을 유지한다. 리뷰의 복합 읽기 명령을 하니스가 거절한 시도도 종료·정리했고, 최종 시험에서는 사전에 명시한 세 개의 개별 읽기 명령만 사용했다. 제품의 `untrusted` 승인 정책은 유지했다.

## 재현

실제 모델 추론을 사용하므로 일반 회귀에서는 ignored다. 정확한 Codex 실행 파일 경로를 지정한다. 하니스가 모르는 승인은 자동 허용하지 않으며 실패 시 소유 미션을 취소하고 종료 기록을 기다린다.

```sh
cargo build -p iyagi-termd
IYAGI_CODEX_BIN=<absolute-codex-path> \
IYAGI_CODEX_MISSION_EVIDENCE_OUT=/tmp/codex-mission-live.json \
cargo test -p iyagi-termd --test mission_e2e \
  codex_mission_live::installed_codex_mission_reaches_verified_reviewed_acceptance \
  -- --ignored --exact --nocapture
```

자동 시작 8회, 작업별 시도 2회, 수정 1회, 동시 실행 2개, 실행 시간 120초, 하니스 대기 360초로 제한했다. UI의 인수 클릭은 IPC 요청으로 수행한 것이므로 실제 Tauri 앱 조작 증거로 확대하지 않는다. 최초 시험 당시 verifier OS 격리는 미완료였으며 아래 별도 재검증 기록과 구분한다. 다른 CLI·OS·모델·인증, 실제 모델의 충돌/장애/재시작 경로와 릴리스 검증은 별도로 남아 있다. [기존 adapter·sandbox 검증](CODEX_MACOS_01540.md), [구현 현황](../../docs/orchestration/IMPLEMENTATION_STATUS.md).


## 파일 지문과 담당 작업 결과 제한 적용 후 재검증

2026-09-16 같은 macOS/aarch64·CLI·모델·구독 조합에서 설치 확인부터 다시 실행했다. 설치 관측 format 2, verified_at_revision 2에 지정 entrypoint의 SHA-256을 저장하고 네 모델 실행 시작 전 실제 파일/버전을 다시 대조했다. 저장된 entrypoint는 `codex.js`, 8,790 bytes, SHA-256 `61b0194f3bb6534439c8d26a3ed57d0805f84b884588b761795323eeb92fcf70`이었다. SQLite에서 네 Run의 binding revision과 저장된 관측의 연결을 확인했다.

첫 재시험은 Builder가 전체 목표의 계획 지시를 자기 작업으로 해석해 patch 대신 report를 반환하면서 ResultInvalid로 중단됐다. 이전의 일반 작업 스키마는 여섯 결과 종류를 모두 허용했다. [실패와 정리 기록](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-mission.macos-01540-role-mismatch.json), SHA-256 `51fb9c49307dc18c7f8296a087d3f8e311629229e11937000191d5e18101402a`. 소유 미션 취소 후 원래 checkout 보존과 모든 Exec 종료/슬롯 해제를 확인했다. 이 실행은 성공으로 집계하지 않는다.

담당 TaskKind의 성공 결과만 허용하도록 스키마를 제한하고, 전체 목표와 현재 작업의 범위를 공통 문맥에서 구분했다. 엔진의 결과 검증, 실제 시험의 목표, 승인 명령/파일 허용 목록은 유지했다. 최종 재시험은 75.53초에 계획·두 Builder·통합·검증·독립 리뷰·멱등 인수까지 통과했다. 여섯 Run 모두 성공, 네 모델 요청/관측 일치, 원래 Git HEAD/clean 상태와 종료/슬롯 정리를 다시 확인했다. [수정 후 성공 기록](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-mission.macos-01540-entrypoint.json), SHA-256 `f20cbcdc22449e1c01e7f5dd3516029ba639c81e4932b2d262ab3c81d5c0dbfa`.

파일 검사는 지정 entrypoint에 한정된다. Node interpreter, wrapper가 선택한 native 자식, 로드된 실행 이미지와 최종 OS 실행 사이의 원자적 고정까지 검증한 것은 아니다. 다른 CLI/OS/인증과 릴리스 판정은 별도 범위로 유지한다.

## 검증 입력 쓰기 차단을 필수로 한 실제 미션

2026-09-16 같은 조합에서 `require_enforced_verification=true`로 실제 미션을 다시 실행해 60.63초에 통과했다. 계획·두 Builder·자동 통합·검증·독립 리뷰의 여섯 Run이 성공했고, 최종 검증은 exit 0·Passed·Enforced다. 입력 쓰기 차단 미보장 결과에 대한 별도 확인 없이 인수와 동일 요청 재생이 성공했다. 네 모델의 요청/관측 일치, 원래 checkout 보존, 모든 Exec 종료와 슬롯 해제를 다시 확인했다.

검증 명령에는 macOS Seatbelt의 쓰기 제한과 네트워크 차단, 데몬 환경을 지운 전용 HOME/temp/출력 경로를 적용했다. 입력은 실행 전후 후보의 원본 바이트·실행 비트·링크와 대조했다. [보존한 엄격 인수 결과](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-mission.macos-01540-enforced.json), SHA-256 `55bf491d9fc1a3cd71061f1406709d4d70aaf2811780bf74c204535b4c0b21ba`.

이 실제 모델 시험 이후 검증 입력 생성은 `--no-checkout`과 raw blob 직접 기록으로 추가 보강했다. 조건부 Git 필터·줄바꿈 변환 우회와 최종 입력 생성 경로의 근거는 실제 Git 단위 시험 및 production daemon/protocol 회귀이며, 위 실제 모델 기록과 구분한다. 호스트 파일 읽기의 비밀 격리, 출력 디스크 강제 한도, 다른 OS 실행기와 릴리스 검증은 이 결과의 범위에 포함하지 않는다.


## 저장소 식별 보강 후 재검증

2026-09-16T14:30:52Z에 같은 macOS/aarch64·Codex 0.154.0·openai·구독·gpt-5.6-luna 조합으로 production daemon 전체 미션을 다시 통과했다. Git common directory 등록·시작 시 ID 재대조·상속 Git 경로 환경 제거를 적용한 코드다. 계획·두 Builder·자동 통합·격리된 검증·독립 리뷰·인수가 성공했고, 요청/관측 모델 일치, 원래 checkout 보존과 종료/슬롯 정리를 확인했다. 일반 회귀의 protocol fixture와 구분되는 추가 실모델 시험 1개다.

[보존 결과](../../crates/iyagi-termd/src/agent_runtime/codex/fixtures/streams/live-mission.macos-01540-repository.json), SHA-256 `e02a56a7cb5b9cfa754c12d9f78074bdba4c6c57b676bede1c79f4a2ea599b0b`. 재현 명령은 위와 같으며 별도 임시 Git 저장소와 데이터 디렉터리를 사용했다. linked worktree·동시 등록·환경 오염·Git 링크 교체의 경계는 새 결정적 시험으로 검증했다. 다른 CLI/OS와 미완료 릴리스 항목으로 범위를 확대하지 않는다.
