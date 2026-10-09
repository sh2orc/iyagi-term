# 오케스트레이션 실행 연결 작업

2026-09-15 코드 감사에서 확인한 실제 구현 상태와 완료 검증 목록이다.
설계 기준은 `ORCHESTRATION_SPEC.md` 및 이 디렉터리의 00–09 문서다.
아래 항목의 구현과 증거가 모두 갖춰지기 전에는 전체 기능을 완료로 표시하지 않는다.

## 완료 검증 목록

- [ ] 시작: Git 저장소·기준 OID·clean 상태·역할/모델 검증, 목표 artifact 소유권, 첫 Lead task의 원자적 생성.
- [ ] 스케줄링: 전체 미션/페이지를 대상으로 전역·미션·binding 상한, 공정성, 의존 관계, 예산, CAS 및 실행 intent 중복 방지.
- [ ] 실행: 데몬 루프 → durable outbox → 소유 프로세스/어댑터 → fencing된 이벤트 → 영속 상태와 UI 알림.
- [ ] 모델 연결: Codex/Claude/OpenCode 실제 transport, 명시적 모델·인증·권한·자원 설정, 지원 능력의 정직한 표시.
- [ ] 계획: provider 결과 검증, scope/의존 관계/요구사항 검증, 자동 적용 또는 사용자 결정, 수정 계획과 위임.
- [ ] 작업: 독립 Git worktree와 writer lease, bounded context/출력, 종료 확인 후 실제 변경 capture.
- [ ] 결과: 통합 → 후보 고정 → 실제 검증 명령 → 독립 리뷰 → 수정 사이클 → 사용자 인수.
- [ ] 제어: 메시지·승인·질문·일시정지·재개·취소·재시도·재배정, unknown 실행 및 재시작 복구.
- [ ] 저장 계약: artifact 소유권/보존, request 멱등성, immutable 증거, outbox 상태 전이.
- [ ] UI: 모델/팀/검증 설정, 실제 저장소 기준 선택, 새 AI 작업과 진행/결과 화면, 오류 복구 동선.
- [ ] 안내: 앱 내 사용법, 한국어/영어 시작 가이드, README와 명세의 실제 상태 정정.
- [ ] 검증: core/storage/adapter/RPC 회귀, 실제 데몬 기반 전체 흐름과 실패/복구 시험, UI 시각 검증, OS별 실제 호환성 증거 및 릴리스 판정.

## 최초 감사 근거

- `MissionService::dispatch_tick`은 정의만 있고 데몬 호출이 없다.
- 기존 dispatch는 미션을 각각 계산하고 첫 50개만 읽는다. 한 tick의 두 번째 commit은 첫 commit 전 revision을 재사용한다.
- prepared Run에 binding snapshot이 없고 automatic_start_count가 증가하지 않는다.
- `mission.control(start)`는 상태만 변경한다. pause/cancel은 live run 여부와 무관하게 즉시 완료한다.
- `mission.plan.apply`, `mission.policy.update`, `mission.finding.resolve`, `mission.activity`는 미지원 응답이다.
- `mission_e2e::standard_mission_goal_to_acceptance_over_real_stack`은 실제로 인수 성공을 검증하지 않고 후보 없음에 따른 거절을 검증한다.
- `MissionCreate`는 팀 템플릿 설정을 안내하지만 설정 화면에 해당 관리 기능이 없다.

## 작업 및 검증 기록

이후 구현 단위마다 변경 내용과 실제 실행한 검증 결과를 기록한다. 일부 통과를 전체 완료 증거로 사용하지 않는다.

### 1. 스케줄링·시작·제어의 저장 경계

- 전체 미션 페이지를 읽어 global 8 / mission 4 / binding 2 상한을 함께 계산한다. pausing/stopping/unknown 실행도 슬롯을 유지한다.
- tick 간 미션 순환, 중복 tick 직렬화, 작업 의존 관계 재검사, 최신 revision으로 여러 실행을 원자적으로 예약한다.
- Run에 실제 binding/model snapshot과 attempt를 기록하고 automatic_start_count를 같은 트랜잭션에서 증가시킨다. 검증 작업에 가짜 binding UUID를 생성하지 않는다.
- start는 Git/clean/HEAD 및 선언된 역할의 활성 모델을 확인하고 첫 Lead task를 생성한다. pause는 실행이 남으면 pausing을 유지한다.
- mission cancel은 미전송 Run만 즉시 취소하며, 시작된/결과 불명 Run은 소유권을 유지한 채 취소 intent를 남긴다.
- create의 목표 artifact를 실제로 귀속시키고 staging 소유자를 지운다. 최초 응답 재생은 상태 검증 전에 수행하며 참조 메타데이터 위조를 거절한다.
- 엔진용 artifact 읽기는 미션 소유권, 바이트 상한, 실제 파일 길이·SHA-256을 확인한다.
- outbox의 expected state/fencing token 조건부 변경과 Run 변경을 하나의 트랜잭션에서 처리하는 저장 API를 추가했다.
- task cancel과 mission cancel은 공통 Run 취소 전이를 사용한다. 같은 Run의 취소 intent ID를 재사용하여 작업 취소 후 미션 취소가 중복 interrupt를 만들지 않는다.
- create/control/message/task.control/decision.answer/accept의 요청 재생을 상태 검증 전에 처리하고 동일한 정규화 fingerprint를 사용한다.
- `mission_dispatch`에 실제 SQLite 기반 회귀 15개를 추가해 위 경계를 검증했다. 기존 `mission_e2e`의 dirty-start 시험을 실제 오류·revision·사용자 파일 보존 검증으로 바꿨다.
- 검증: `cargo test -p term-contracts -p term-core -p term-storage` 380개 통과. 미션 관련 9개 test binary(dispatch/claude/rpc/workflow/e2e/codex/opencode/exec/workspace) 77개 통과. dispatch의 마지막 멱등성 보강 후 15개 재검증 통과.
- 전체 `cargo test -p iyagi-termd` 시도에서 Claude 테스트가 Windows 버전의 호환성 증거를 macOS에 적용한다고 가정하여 실패했다. 추가 미션 검사에서 OpenCode 테스트에서도 같은 문제를 확인했다. 제품의 미검증 capability=false 동작은 유지하고 두 테스트를 OS/버전별 증거에 맞게 수정한 뒤 위 미션 검사 77개를 통과했다. 전체 daemon suite의 최종 재실행은 아직 남아 있다.

### 2. 데몬 actor와 실제 실행 파이프라인

- `Daemon::run`이 mission actor를 시작하고 종료 시 정리 완료를 기다린다. actor는 실행별 이벤트를 제한된 수만큼 처리하고, 검증 명령은 별도 worker에서 실행한다.
- worktree 소유 행·context·소유 표식을 실행 전에 저장하고, Run 및 outbox의 전송 claim을 원자적으로 갱신한 다음 provider를 시작한다.
- Plan 결과를 검증해 자동 적용하거나 Plan Decision으로 보관한다. 수동 적용과 결정 답변은 CAS 및 요청 재생을 지원한다.
- writer 종료 확인 후 실제 Git 변경을 수집한다. 후보 통합, 실제 검증 명령, 현재 후보에 연결된 독립 리뷰, 수정 계획, 새 후보 검증·리뷰 반복과 사용자 인수를 연결했다.
- 과거 후보의 검증·리뷰를 새 후보의 증거로 재사용하지 않는다. 이전 후보의 finding은 보존하지만 현재 후보의 인수를 무조건 막지 않는다.
- 승인 답변은 정확한 provider request ID에 연결된 Answer outbox로 전달한다. 전송 응답과 worker 종료 결과는 저장 CAS가 충돌해도 메모리에 유지해 다음 pass에서 기록한다.
- `term-fixture app-server`는 실제 stdio 프로토콜 자식 프로세스로 동작한다. 이 프로그램을 사용한 실제 데몬·IPC·Git·검증 명령·인수 전체 흐름 시험이 통과했다. 모델 추론은 deterministic fixture이므로 설치된 CLI의 추론 호환성 증거는 아니다.

### 3. 재시작·일시정지·취소

- startup은 pending outbox뿐 아니라 모든 소유 Run을 조회한다. acknowledged start도 복구 대상에 포함한다.
- may-have-sent/acknowledged Run은 Unknown과 격리 workspace로 남기고 DB fence를 증가시켜 이전 actor callback을 거절한다. Sending outbox는 Unknown으로 기록하며 자동 재전송하지 않는다.
- 이전 실행의 승인 질문을 obsolete로 바꾸고 복구 결정을 남긴다. 같은 상태를 다시 스캔해 새 결정·이벤트를 반복 생성하지 않는다.
- 미전송 실행의 복구를 정책이 금지하면 보류한다. 사용자가 해당 Run의 복구를 허용한 기록은 다음 재시작에도 유지된다.
- pause는 아직 전송하지 않은 Run을 그대로 보관하고 완료될 수 있다. cancel은 미전송 Run의 대기 intent도 정리한다. 시작된 실행은 종료 확인 전에 소유권을 해제하지 않는다.
- 실제 데몬을 강제 종료하고 같은 데이터 디렉터리에서 재시작해, Paused 미션의 전체 entity·revision·event 이력이 정확히 유지되고 새 실행이 생성되지 않는 시험이 통과했다.
- Unknown provider 실행의 native 조회·재연결·소유 프로세스 종료 증거를 통한 최종 조정은 아직 남아 있다. Unknown 슬롯을 임의로 해제하지 않는다.

### 4. 작업 입력과 Git 변경 경계

- 선행 작업의 텍스트 결과뿐 아니라 성공한 전이적 의존 작업들의 실제 패치를 후속 작업에 제공한다. 여러 의존 경로를 별도 임시 Git index에서 합쳐 입력 commit을 만들며 사용자 index·checkout·branch를 사용하지 않는다.
- 입력 commit은 고정된 메타데이터를 사용해 같은 Run의 준비를 재시도해도 동일하게 계산된다. 수정 작업은 현재 후보를 기준으로 시작한다.
- Git status/diff의 NUL 형식을 사용해 Unicode·따옴표·개행·끝 공백이 있는 경로를 보존한다. rename 양쪽과 provider가 이미 commit한 변경에도 허용 경로 검사를 적용한다.
- capture는 detached HEAD를 확인한다. provider가 다른 branch를 checkout한 경우 그 branch를 commit으로 전진시키지 않는다.
- symlink의 전체 연결이 작업 디렉터리 밖으로 나가면 거절한다. manifest는 worktree 파일을 따라 읽지 않고 고정된 Git blob에서 크기·해시를 읽는다.
- 구현 작업이 미래 검증/리뷰 단계에 의존해 DAG가 멈추는 계획은 거절한다.

### 5. 사용 안내·설정·활동 화면

- 설정에 AI 작업 그룹과 모델 연결·팀 템플릿·저장소별 검증 명령 저장 화면을 추가했다. 설정과 생성 화면 양쪽에 사용법을 노출한다.
- 생성은 실제 repository.inspect 결과의 저장소 ID·canonical path·HEAD를 사용한다. 완료 조건 ID는 UUID이고, 팀과 활성 모델 선택을 확인한다. USD는 정수 micro-dollar로 정확히 변환한다.
- 한국어/영어 가이드와 README 진입 링크를 추가했다. 모델 생성 → 팀 저장 → 새 미션 생성·시작 재시도를 실제 브라우저에서 검증했다.
- `mission.activity`를 연결했다. 실행별 1 MiB 표시 로그를 원자적으로 교체해 저장하고 논리 byte offset으로 immutable artifact 페이지를 반환한다. text delta별 mission revision 증가를 피하고 활동 시각은 초 단위로 갱신한다.
- 상세 화면은 증분 로그를 자동 조회하고 표시량을 제한한다. 공통 artifact 읽기는 UTF-8 문자가 chunk 경계를 넘는 경우를 올바르게 디코딩하며 전진하지 않는 cursor를 오류로 처리한다.

### 6. 모델 실행 요청과 호환성 증거

- 요청할 workspace 읽기/쓰기 권한을 binding의 capability 증거와 분리했다. Lead/Reviewer는 ReadOnly, writer는 Write를 요청한다. capability=false를 true로 바꿔 권한을 부여하지 않는다.
- Codex의 turn/start에 sandboxPolicy와 네트워크 허용 여부를 명시한다. workspaceWrite는 해당 작업 디렉터리만 쓰기 루트로 지정하고 임시 디렉터리의 암묵적 추가를 막는다.
- 공식 [App Server 문서](https://learn.chatgpt.com/docs/app-server)의 per-turn sandbox 설정과 설치된 `codex-cli 0.154.0`이 생성한 JSON schema를 대조했다. 로컬 schema의 thread sandbox는 kebab-case이고 turn policy는 camelCase다. 이 확인은 유료 추론을 수행하지 않는다.
- RunStart에 실제 mission ID와 daemon owner ID를 포함하고 Claude SpawnRequest에 연결했다. 기존의 임의 UUID 생성은 제거했다.

### 7. 사용자 정책과 리뷰 지적 처리

- `mission.policy.update`를 draft/running/paused 미션에 연결했다. 정책 상한, 역할 중복, 등록된 모델·저장소별 검증 명령을 검증한다. 원래 완료 조건의 검증 명령은 제거하지 못한다.
- 기존 소유 실행과 충돌하는 동시 실행 수·시간·네트워크·binding 등의 정책 축소는 거절한다. 이미 예약된 Run의 binding snapshot은 변경하지 않는다.
- 상한을 늘렸을 때 해당 예산으로 보류된 작업과 결정이 다시 진행할 수 있도록 처리한다.
- `mission.finding.resolve`는 현재 후보의 열린 지적만 dismissed로 변경한다. 소유권·실제 해시를 확인한 비어 있지 않은 사유가 필요하며, 사용자에게 fixed 판정 권한을 주지 않는다.
- 두 API 모두 상태 검증보다 먼저 기존 요청을 재생한다. 이전 후보 지적 변경 거절, 위조 사유 거절, 취소 후 정책 응답 재생 및 snapshot 보존 시험이 통과했다.

### 8. OpenCode 실제 프로토콜과 소유 서버 실행

- 저장된 OpenAPI와 공식 [서버](https://opencode.ai/docs/server/)·[SDK](https://opencode.ai/docs/sdk/) 문서로 기존 어댑터의 요청 형식과 완료 판정을 바로잡았다. session 생성은 `model.id`, prompt는 `model.modelID`와 `parts`를 사용한다.
- SSE의 `server.connected`를 확인한 뒤 `prompt_async`를 한 번만 보낸다. 세션 생성·프롬프트 전송의 불명확한 응답이나 SSE EOF 이후 같은 실행을 재전송하지 않는다.
- 세션 ID, 소유 user message의 parent ID, assistant message ID를 대조한다. 여러 assistant의 사용량 snapshot을 중복 없이 합산하고, 과거 message update가 현재 turn ID를 되돌리지 못하게 한다. 누락된 비용·토큰은 unknown으로 유지한다.
- 해당 실행의 final assistant 완료와 session idle을 확인한 뒤 저장된 메시지를 다시 조회한다. `info.structured`를 ProviderResult로 엄격히 역직렬화하며, 빈 HTTP 성공 응답이나 표시 텍스트만으로 성공 처리하지 않는다. 관측된 provider/model이 binding과 다르면 실패한다.
- 승인 요청은 소유 session의 정확한 request ID에만 답하며 한 번만 전송한다. 응답 손실은 새 `DeliveryReceipt::Unknown`으로 전달해 Message와 outbox를 원자적으로 Unknown으로 보관한다. 실제 actor 시험에서 제공자가 승인 후 완료해도 답변을 재전송하지 않는 것을 확인했다.
- HTTP transport는 127.0.0.1과 Basic 인증만 사용하며 proxy·redirect·POST retry를 비활성화한다. JSON/SSE의 개별 line과 frame을 4 MiB로 제한하고, 오류 응답 body·자격 증명은 진단 메시지에 넣지 않는다.
- 실제 server launcher는 주입된 ExecSupervisor와 Tokio runtime을 사용한다. run별 임의 인증 값을 환경으로 전달하고, 소유 stdout의 loopback 주소·health·binding 버전을 확인한다. 시작 실패와 버전 불일치는 자식 종료를 확인한 뒤 반환한다. `--pure` 및 자동 업데이트·공유·formatter·LSP 비활성화 설정은 사용자 global 설정 파일을 변경하지 않는다.
- AgentAdapter bridge는 일반 worker에서 서버 시작과 SSE를 처리한다. 기동 중 취소, 승인, 중단, 종료 확인을 공통 port에 연결했다. production factory는 아직 인증 참조 해석 연결을 기다리며, CLI/OS의 자동 실행 capability를 임의로 활성화하지 않았다.
- `term-fixture serve`는 Basic 인증을 실제 검사하는 HTTP/SSE 자식 프로세스다. 정상 구조화 결과, SSE 대기 중 취소, 기동 시간 초과, 버전 변경 및 실행 파일 부재를 시험한다. 추론은 fixture가 대신한다.
- 설치된 macOS OpenCode 1.18.30으로 별도 metadata smoke를 실행해 인증된 health와 OpenAPI 조회, 소유 서버 종료를 확인했다. 별도 XDG 디렉터리를 사용하고 session/prompt를 생성하지 않아 모델 추론은 수행하지 않았다. 이 증거는 구조화 추론·쓰기 격리의 실제 호환성 증거와 다르다.
- 위 실제 프로세스 시험에서 ExecSupervisor의 `spawn_on`이 명시된 Tokio reactor에 진입하지 않는 오류를 발견해 수정했다. 실행 파일이 없을 때 예약을 해제하고 미실행 종료 기록을 남기는 경로도 추가했다.
- verifier가 완료 결과를 저장한 직후 thread가 반환하기 전의 경합을 확인했다. actor가 소유 worker를 join하기 전에는 Pausing/Stopping 완료를 확정하지 않도록 수정했다.

### 9. 여러 필수 리뷰의 단계 완료 조건

- Reviewing에서 현재 후보의 리뷰 증거뿐 아니라 모든 활성 필수 작업의 성공 여부를 확인한 뒤 AwaitingAcceptance로 이동한다. superseded/cancelled 작업의 제외 규칙은 인수 검증과 같다.
- 첫 리뷰가 끝나고 그것에 의존하는 두 번째 필수 리뷰가 아직 대기 중인 계획을 실제 actor로 실행했다. 두 리뷰가 모두 성공한 뒤 인수 단계에 들어가는 것을 확인했다.

### 10. 공유 Exec 저장·실행 게이트와 Claude 연결

- `ExecPersistence`의 준비·상태 저장은 실패를 반환한다. 미션 구현은 명령·인자·cwd·환경 변수 이름만 포함한 실제 artifact를 저장하고 `Run.exec_id`와 Exec 행을 같은 트랜잭션으로 연결한다. 환경 변수 값은 기록하지 않는다.
- 저장 전에 미션/데몬/Run 소유권, binding snapshot의 실행 파일·자원 정책, 현재 worktree lease와 cwd를 확인한다. 같은 준비·관찰의 재전송은 기존 참조/기록을 반환하며, 프로세스 identity·group·시작 시각 변경과 Exited 상태의 역행을 거절한다.
- 실제 프로세스는 기존 launch-helper와 private gate를 거친다. 실행 전 Prepared 저장 → 대기 헬퍼 생성 → OS group 연결 → identity 저장 → RELEASE 순서를 지킨다. 취소가 준비와 RELEASE 사이에 들어오면 실행을 거절한다.
- 준비/소유권 저장 실패 시 target은 시작하지 않는다. 시작 실패는 헬퍼와 소유 group의 종료를 확인한다. 종료 기록을 저장하지 못하면 예약을 유지하고 재시도한다. 마지막 외부 handle이 해제되어도 감독 worker가 정리를 계속한다.
- macOS에서 이미 관찰·검증한 자식이 부모 종료 후 재귀속되어도 identity가 일치하는 동안 추적한다. 한 번도 관찰하지 못한 탈출 프로세스까지 격리했다고 주장하지 않으며 hard limit capability는 그대로다.
- Claude의 데몬 factory는 공유 영속 ExecSupervisor와 실제 host telemetry를 사용한다. 직접 생성자와 무저장 callback 경로는 기존 fixture/legacy 호출에 남아 있다. Codex의 양방향 pipe 전환과 OpenCode의 인증 참조 해석을 포함한 production factory는 아직 남아 있다.
- Claude stdout 전달을 256개 메시지/8 MiB로 제한하고 넘친 프로토콜을 실패로 처리한다. 파이프 EOF와 sink 수명을 분리하여 출력 후 무한 대기를 고쳤다. 저장 장애를 provider 실패나 종료 확인으로 바꾸지 않고 완료 저장을 기다린다. OpenCode의 일회성 close도 완료 저장을 재시도한다.
- provider의 `start`를 별도 worker에서 실행한다. 느린 시작이 actor의 다른 미션·제어 처리를 막지 않으며, 등록 전 들어온 취소를 등록 직후 다시 전달한다. worker가 반환하기 전에는 실행 소유권을 해제하지 않는다.
- 정상 시작, 단계별 저장 장애, handle drop, 부모보다 오래 사는 자식, 무관한 프로세스 보호를 실제 헬퍼/fixture로 시험했다. SQLite·실제 manifest·native gate를 함께 사용하는 통합 시험도 통과했다. `term-fixture`의 Claude print 모드는 유료 모델을 호출하지 않는 결정적 프로토콜 시험이다.

### 11. Codex 양방향 실행의 공유 감독기 전환

- Codex의 데몬 factory가 `SupervisedPeer`를 사용한다. `codex app-server`도 다른 provider와 같은 영속 Exec 저장·host admission·native gate·종료 확인을 거친다. 기존 `live`/`LivePeer`는 미션 dispatcher에서 사용하지 않는 standalone legacy 경로다.
- 공유 Exec에 interactive stdin을 추가했다. 입력은 순서대로 쓰고, 1 MiB 한도와 5초 시간 제한을 적용한다. 시간 초과/부분 전송은 결과 불명으로 처리하고 채널을 닫으며 자동으로 다시 쓰지 않는다.
- Codex와 Claude의 stdout 대기열은 공통 구현으로 256개 메시지/8 MiB를 제한한다. Codex outbound JSON도 제한된 writer로 직렬화하여 한도를 넘는 프레임을 파이프에 일부 기록하지 않는다.
- Codex의 protocol terminal과 `inspect`의 종료 확인을 분리했다. 실제 process/group 종료와 Exited 저장을 확인하기 전에는 예약과 Run 소유권을 유지한다. adapter 해제와 engine 반환 시 소유 transport 정리를 요청한다.
- 같은 Codex Run의 중복 start를 거절한다. 취소 의도를 stdin 전송 전에 기록하여 빠른 `turn/completed(interrupted)` 응답과의 경합을 제거했다. 승인 답변 전송이 불명확하면 Unknown을 반환하고 같은 승인 ID를 재전송하지 않는다.
- runtime이 pipe pump를 한 번도 poll하기 전에 종료되더라도 출력이 불완전했음을 기록하고 process 정리를 막지 않는다. stdin과 자식 프로세스의 drop 경로도 소유 입력/프로세스를 정리한다.
- 실제 fixture 자식으로 정상 결과, 승인 응답, steer, interrupt, 중간 단절, 출력 한도 초과, adapter drop, 저장 장애 중 종료 보류, 동시 Unicode 입력, stdin backpressure 시간 초과, oversized JSON 거절과 runtime 종료를 시험했다. 실제 daemon 전체 미션 시험은 네 provider Run 모두 영속 Exec에 연결되고 Exited가 저장되었는지도 검증한다.

### 12. OpenCode 인증 참조와 production factory

- `connections.rs`에 불변 endpoint 메타데이터와 OS 비밀 저장소 해석기를 추가했다. `connection add/list/revoke`는 로컬 명령이며 키는 stdin으로만 받는다. public metadata와 CLI 출력에는 opaque 참조만 있고, 키와 전체 목적지의 연결은 OS 저장소에서 다시 확인한다. 키 등록·해제 실패 시 평문 저장소로 대체하지 않는다.
- 네 preset을 지원한다: OpenCode의 OpenAI/Anthropic API, Z.ai 일반 API, Z.ai Coding 구독 endpoint. 제공자·인증 방식·endpoint를 함께 검증하고 모델 ID를 대체하지 않는다. 임의 endpoint나 다른 runtime으로 참조를 옮겨 사용하지 못한다. 키 회전은 새 immutable 연결을 만든다.
- 데몬의 OpenCode factory를 공유 영속 ExecSupervisor에 연결했다. 키 해석은 adapter worker에서 수행한다. 누락/폐기된 키는 AuthRequired, 연결 불일치는 PolicyDenied로 정규화하며 provider 프로세스 시작 전 거절한다.
- `SpawnRequest`와 private `GateTarget`에 환경 상속 차단을 추가했다. OpenCode production 경로는 필요한 host 변수만 선택하고 HOME/XDG/temp를 실행별 private 디렉터리로 바꾼다. 프로젝트 설정을 비활성화하고 `--pure`로 외부 plugin을 제한한다. 실제 `/config`를 읽어 key/baseURL/model/enabled_providers를 확인하고 예상하지 않은 provider/MCP/plugin 설정을 거절한 뒤 session을 만든다.
- 실행별 디렉터리는 Exec와 정리 worker가 함께 소유한다. transport가 만들어지기 전 실패해도 process 정리가 끝날 때까지 유지하며, 완료 기록까지 확인한 뒤 해제한다. sanitized launch manifest에는 환경 변수 이름과 `env_clear`만 추가하고 값은 기록하지 않는다. 기존 4필드 manifest도 읽을 수 있다.
- 알려진 제공자 키와 임시 서버 인증을 HTTP/SSE의 중첩 JSON 값·키, stdout/stderr의 sink·보관 tail에서 제거한다. 줄 크기 한도에서 비밀값이 잘린 prefix를 보관하지 않도록 해당 줄은 생략한다. 기존의 bounded protocol invalidation은 유지한다.
- 모델 설정에 인증/endpoint 참조 입력과 안내를 추가했다. 잘못 붙여넣은 키는 RPC 전 검증에서 차단하고, 서버도 opaque credential ref 형식을 검사하여 저장을 거절한다. 한국어·영문 가이드에 실제 CLI 명령, preset 표, 회전/폐기와 제한을 기록했다.
- 근거: [OpenCode provider 설정](https://opencode.ai/docs/providers/), [설정 우선순위](https://opencode.ai/docs/config/), [Z.ai 연결](https://docs.z.ai/devpack/tool/opencode), [OS credential backend](https://docs.rs/keyring/4.2.0/keyring/v1/index.html). 설치된 OpenCode의 조회 시험과 OS keychain 시험 범위는 아래 검증 기록과 같다.

### 13. Codex 인증 선택과 API 키 연결

- `codex-api` preset을 추가했다. Codex/OpenCode가 같은 OpenAI endpoint를 사용해도 runtime이 다른 인증 참조를 재사용할 수 없다. 데몬의 Codex factory는 OS 저장소 참조를 해석하고 API 키 누락·폐기를 프로세스 준비 전에 거절한다.
- API 실행은 실행별 private HOME/CODEX_HOME/XDG/temp와 명시적 환경을 사용한다. 공식 `cli_auth_credentials_store="ephemeral"`과 `forced_login_method="api"`를 요청하고, 실제 home과 `config/read`의 목적지·provider·저장 방식을 대조한 다음 `account/login/start`로 키를 보낸다. 키를 argv·환경·manifest에 넣지 않는다.
- 구독 실행은 사용자가 공식 Codex로 로그인한 CODEX_HOME을 사용한다. OAuth 토큰을 복사하거나 로그인 방식을 변경하지 않으며 갱신은 Codex에 맡긴다. API 참조가 섞였거나 계정이 ChatGPT 인증이 아니면 거절한다. 이 경로는 기존 Codex 홈을 공유하므로 API 실행과 같은 설정 격리를 주장하지 않는다.
- account/read의 실제 인증 방식, requiresOpenaiAuth, thread의 선택 provider를 확인한다. 인증이 확인된 뒤 account/updated가 다른 방식 또는 로그아웃을 보고하면 중단한다. upstream 오류 원문은 미션 오류에 노출하지 않는다. 사용자 지정/로컬 인증 및 임의 목적지는 아직 지원하지 않는다.
- 임시 API 디렉터리는 Exec 종료 기록 저장까지 유지한다. 어댑터가 완료된 실행 이력을 계속 가지고 있어도 정리 worker가 디렉터리를 해제한다. 실제 fixture와 저장 실패 주입으로 그 순서를 검증했다.
- 실제 Codex stdout의 비밀값 시험에서 공통 Exec의 stdout tap이 redactor를 받지 않는 누락을 확인해 수정했다. stdout/stderr 모두 scrubbed sink와 tail을 사용한다. JSON 안에 다시 인코딩된 구조화 결과도 재귀적으로 제거하며, 변경할 값이 없는 출력은 원래 포맷을 유지한다.
- 모델 설정의 Codex API 참조 입력과 구독 로그인 안내를 추가했다. 구독으로 변경하면 API 참조를 지워 숨겨진 이전 설정이 실행을 막지 않게 했다. 한국어/영문 가이드에 등록·선택·정리·지원 범위를 반영했다.
- 근거: [App Server](https://learn.chatgpt.com/docs/app-server), [Codex 인증](https://learn.chatgpt.com/docs/auth), [설정 참조](https://learn.chatgpt.com/docs/config-file/config-reference). 메타데이터 검사는 모델 추론이나 자동 실행 capability 승격의 증거로 사용하지 않는다.

### 14. Claude 인증 선택과 작업 전 초기화

- `claude-api`, `claude-subscription`, `claude-zai-coding` preset과 데몬의 authenticated Claude factory를 연결했다. OS 저장소의 credential·runtime·provider·auth route·고정 목적지를 함께 확인한다. 기존 OpenCode 연결을 Claude에 재사용하거나 폐기된 키로 Exec를 준비하지 못한다.
- 저장된 인증은 실행별 HOME/CLAUDE_CONFIG_DIR/XDG/temp와 상속을 차단한 환경을 사용한다. Anthropic API는 `ANTHROPIC_API_KEY`, 별도 구독 토큰은 `CLAUDE_CODE_OAUTH_TOKEN`, Z.ai Coding은 `ANTHROPIC_AUTH_TOKEN`으로 해당 자식에 전달한다. 키를 argv·RPC·manifest에 기록하지 않고 stdout/stderr와 구조화 결과에서 알려진 비밀값을 제거한다.
- `anthropic`·구독·두 참조 없음은 공식 CLI의 기존 홈과 로그인을 사용한다. 초기화 응답의 subscriptionType과 인증 출처를 검증해 Console API/profile 로그인을 구독으로 취급하지 않는다. 이 경로는 사용자 홈을 공유한다. 별도로 등록한 setup-token은 자동 갱신하지 않는다.
- 같은 감독 대상 프로세스에 stream-json initialize control request를 보내고 응답 ID·인증 출처·permission mode를 검증한 뒤 한 번의 user 프레임을 보낸다. 입력은 1 MiB, 초기화는 20초/256개 응답으로 제한한다. 불일치·취소·미확인 전송은 정적 typed 오류로 처리하고 작업을 재전송하지 않는다. 인증 직후 취소를 관측하면 프롬프트를 보내지 않는다.
- host-owned routing과 safe/restricted 모드를 사용한다. 읽기에는 Read/Glob/Grep, 쓰기에는 추가로 Edit/Write만 명시한다. 사용자 설정·hook/plugin/MCP와 대화형 승인을 사용하지 않으며 managed policy는 계속 적용된다. 성공은 구조화된 ProviderResult를 요구한다. production Claude 세션 저장/재개는 지원하지 않는다. 이 CLI 정책을 OS sandbox 증거로 간주하지 않는다.
- 어댑터 해제도 취소를 전달하고, 실제 process/group 종료·Exited 저장 후 임시 디렉터리를 해제한다. 완료된 실행 이력이나 sink가 남아 있어도 디렉터리 소유권을 해제한다. 인증 대기 중 취소·해제, 서로 다른 제공자의 동시 실행, 저장 장애 중 결과 보류와 뒤늦은 정리를 native gate/fixture로 검증했다.
- 모델 설정에 Claude 참조 입력과 기존 구독 로그인 선택법을 추가했다. 한국어·영문 가이드에는 세 preset의 등록 명령, 목적지, 토큰 교체와 실행 제한을 기록했다.
- 근거: [인증](https://code.claude.com/docs/en/authentication), [환경 변수 및 host routing](https://code.claude.com/docs/en/env-vars), [CLI 옵션](https://code.claude.com/docs/en/cli-reference), [공식 SDK control protocol](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/query.py), [Z.ai Claude 연결](https://docs.z.ai/devpack/tool/claude). 설치된 2.1.271의 인증 메타데이터 시험은 기존 2.1.263 text-print fixture의 capability 증거와 구분한다.

### 15. 실행 중 메시지 전달과 후속 Lead 계획

- `mission.message`가 소유 artifact의 크기·해시·UTF-8과 대상 작업 상태를 검증한다. Message, routing outbox, 미션 revision/event/request를 한 트랜잭션에 저장한다. 종전의 Message만 저장하는 경로에서 revision이 갱신되지 않아 연속 메시지가 event sequence 충돌을 일으키던 문제를 수정했다.
- routing intent와 실제 Run/fence에 묶인 전달 intent를 분리한다. 라우팅 완료는 사용자에게 전달됨으로 표시하지 않는다. 활성 Run의 immutable binding이 steer를 지원하면 해당 실행에 보내고, 미지원/일시정지는 queued로 둔다. 보내기 전에 대상이 끝났으면 새 실행 문맥으로 돌리며 동일 Run에는 다시 steer하지 않는다. 취소된 미션과 종료된 대상 작업의 미전송 메시지는 거절한다.
- delivery worker는 Run당 하나, 전체 최대 8개다. actor는 응답을 기다리며 멈추지 않는다. Sending claim을 먼저 저장하고 결과는 별도 commit한다. 저장 실패/CAS 충돌 중에는 수신한 receipt를 메모리에 유지해 결과 저장만 재시도한다. 응답 불명이나 worker panic은 Unknown으로 남기며 같은 effect를 다시 호출하지 않는다. 기존 승인 전달도 이 worker 경로로 연결했다.
- Codex `send_message`는 단순 stdin 쓰기 완료로 Delivered를 반환하지 않는다. 공식 pinned `TurnSteerResponse` schema의 응답 ID와 정확한 turnId를 확인한다. provider 거절은 Rejected, 잘못된 turn/응답 누락/연결 종료/5초 timeout은 Unknown이다. 승인 답변과 steer는 서로 다른 프로토콜 동작으로 유지한다.
- 활성/대기 Lead가 없으면 새 계획 작업을 생성하고 대기 중 계획에 연속 메시지를 합친다. 기존 목표·요구사항·계획 revision을 대화만으로 바꾸지 않는다. 일시정지 중에는 생성/전달을 보류하고 재개 후 진행한다. 기존 blocking decision을 우회하지 않으며, 계획 한도 때문에 진행할 수 없으면 결정 하나를 기록하여 다른 actor 작업을 막지 않는다.
- Git 준비 이후 최종 start claim에서 문맥을 다시 만들고 포함된 queued 메시지를 같은 트랜잭션에서 해당 Run에 묶는다. Started 수신 전에는 queued이고, 시작 확인 시 Delivered로 기록한다. Unknown/Rejected 메시지는 다음 실행에 자동 입력하지 않으며 Delivered는 이력으로 표시한다. context/prompt는 작업 파일 범위·완료 조건·도구 승인을 대화로 변경할 수 없음을 명시한다.
- startup은 Run이 이미 종료됐거나 미션이 보관돼 있어도 Sending 메시지/승인의 결과를 Unknown으로 보존한다. 오래된 actor의 늦은 receipt가 복구 상태를 덮어쓰지 못한다. 실제 native 실행 재조정과 사용자 주도 재전송 UX는 아래 남은 범위다.
- 앱 내 안내와 한국어/영문 가이드에 active/queued/unconfirmed 구분, 후속 계획, 불명 결과의 재전송 제한을 반영했다. 메모리 adapter 시험과 실제 데몬·native gate·Codex protocol fixture 시험은 설치된 제공자의 추론 호환성 증거와 구분한다.
- 화면 연결 감사에서 Lead/담당 입력창이 기존 미션 메시지를 staging으로 업로드하던 문제를 수정했다. 두 입력창은 해당 미션 ID로 업로드하고 완료 시점에 수신한 최신 revision으로 전송한다. 새 미션 목표는 staging을 유지한다. 상태 변경 중 대화 목록의 불안정한 참조가 반복 렌더링을 일으키던 문제도 목록 비교로 수정했다.

### 16. 확인된 실패의 복구 결정과 원자적 모델 변경·재시도

- 종료가 확인된 실패 Run에 Recovery 결정을 연결한다. Run 실패 기록 이후 결정 저장이 실패해도 다음 actor pass에서 재구성하고, 중복 결정이나 새 실행을 만들지 않는다. 계획·후보·작업 상태·시도 한도가 달라진 기존 결정은 obsolete로 바꾸고 필요한 현재 결정을 만든다.
- 실패 결정은 미션 전체를 막지 않는다. 실패한 작업의 직간접 의존 작업에는 `dependency_failed`를 기록하고 독립 작업은 계속 예약한다. 실패한 최신 Lead에 대기 메시지가 있어도 새 Plan을 반복 생성하지 않으며, 해당 작업의 명시적 복구를 기다린다. 실제 검증 결과가 있는 실패는 기존 bounded repair 경로를 유지하고 검증 준비/runner 실패는 복구 결정을 만든다.
- `retry_failed_task`는 정확한 실패 Run/Task, 다른 소유 실행 부재, 미션 상태, task attempt·mission 실행/시간 한도, 허용된 활성 binding을 다시 확인한다. 기존 Run은 보존하고 Task를 Ready로 돌린다. 새 Run과 attempt 증가는 기존 dispatch에서 이루어진다. 일시정지 중 선택한 재시도는 재개 전 실행하지 않으며 Unknown/Interrupted 실행에는 적용하지 않는다.
- `mission.task.control(retry)`의 선택적 `binding_id`로 모델 변경과 재시도를 한 CAS에 저장한다. 화면의 종전 reassign→retry 두 요청은 같은 revision을 재사용해 두 번째 요청이 실패할 수 있었으므로 단일 요청으로 바꿨다. 종료 작업의 재배정, 정책 밖/비활성 모델 선택, 소유 실행이 남은 재시도를 거절한다. 재시도/취소로 해결된 실패 결정은 같은 트랜잭션에서 obsolete로 바꾼다.
- 복구 결정과 task control은 각각 하나의 일관된 snapshot으로 검증·변경한다. 두 번의 snapshot 읽기 사이에 바뀐 미션 필드를 이전 projection으로 덮어쓰는 경계를 제거했다. 요청 재생은 이후 상태에서도 최초 응답을 반환한다.
- 사용자가 `stop_failed_mission`을 선택하면 다른 소유 실행을 취소하고 Stopping을 유지한다. 정리가 확인되면 Failed와 원래 failure code를 기록한다. 이전 실패 실행의 결과와 활동/Exec 증거는 변경하지 않는다.
- 화면에 한국어/영어 오류·시도 한도·독립 작업 안내와 복구 선택지를 추가했다. 모델 선택은 정책에 허용된 활성 연결만 표시하고, 종료/불명 실행과 종료 미션에 맞춰 재시도 제어를 막는다. 한국어/영문 사용 가이드를 갱신했다.

### 17. 실제 경과 시간과 시간 예산

- `mission/timing.rs`에 프로세스 내부 monotonic 시계를 추가했다. 미션이 Running/Pausing인 경과 시간을 한 번만 누적하며 병렬 Run 시간을 합산하지 않는다. 새로 시작한 각 Run은 시작 준비부터 확인된 종료까지 개별 시간을 기록하고, Prepared 예약 및 재시작 이전 Unknown 실행은 추정하지 않는다.
- RPC, actor, outbox, workflow의 production 상태 저장을 같은 시간 계측 경계에 연결했다. 시간과 상태를 한 트랜잭션으로 저장하며, CAS/저장 실패나 요청 재생은 시계 기준점을 전진시키지 않는다. 연속 checkpoint 사이의 1ms 미만 시간도 원래 기준점으로 유지한다. 미션 projection이 없던 엔진 변경에도 revision/시간 projection을 함께 저장한다.
- 조용한 미션도 약 1초마다 저장한다. 실제 데몬에는 Git 준비·통합으로 actor가 오래 걸리는 경우를 위한 별도 clock worker가 있고, 정상 종료에는 마지막 잔여 시간을 저장한다. 저장 실패 동안에는 메모리 기준점을 보존해 다음 성공에 반영한다. 비정상 종료 후에는 마지막 영속값부터 새로 계측하고 중단 시간을 더하지 않는다. 정상 저장 주기 외의 스케줄링/저장 지연을 엄격한 최대 손실 1초로 표현하지 않는다.
- dispatch/재시도/정책 변경은 checkpoint 사이의 실제 누적값으로 시간 한도를 검사한다. 시간/자동 시작 예산이 소진되면 새 dispatch를 막고 Budget 결정을 기록한다. 정책 확대는 조건에 맞는 결정과 작업 보류를 해제한다. 이미 진행 중인 Run은 기존 개별 timeout/정리 계약을 유지한다.
- `U64String`이 문자열 사전순으로 정렬되던 오류를 숫자 비교로 수정했다. 입력의 앞자리 0을 정규화해 Eq/Ord/Hash가 일치하도록 했고, 9/10/100 경계와 SQLite 최대 정수 부근을 검사한다. wire 타입은 기존 10진 문자열이다.
- 미션 상단에는 저장된 활성 시간/한도를, 실행 상세에는 저장된 개별 실행 시간을 표시한다. 한국어/영어 앱 안내에 합산 규칙, 약 1초 저장 주기, 비정상 종료·저장 장애 시 미저장 구간의 한계를 명시한다. UI는 로컬 wall clock으로 Unknown 시간을 늘리지 않는다.

### 18. 실행별 비용 예약과 비용 대기 복구

- binding에 선택적인 양수 `estimated_run_cost_usd_micros`를 추가했다. 이전 문서는 미확인 값으로 읽으며 실제 Run에는 예약 시점의 binding과 예상 비용을 불변 snapshot으로 저장한다. core의 비용 합산은 실패·취소·과거 시도를 포함하고, 미전송 종료 예약과 제공자를 사용하지 않는 결정적 검증은 제외한다.
- dispatch는 제공자 보고액·남은 예약·최종 비용 미보고 실행의 예상액을 합산하고 다음 예상 금액까지 확인한다. 소유 실행은 보고액과 예상액 중 큰 금액을 유지하고, 종료 보고가 오면 사용하지 않은 예상 금액을 해제한다. 전역 dispatch 직렬화와 Mission CAS로 병렬 작업의 중복 예산 사용을 막으며, 합계는 u128/BigInt로 계산한다.
- `unknown_cost=block`은 다음 모델의 예상 비용 부재 또는 과거 실행의 미확인 비용을 실제로 차단한다. usage 지원 capability만으로 실행 전 비용을 안다고 판단하지 않는다. `allow_with_notice`도 알려진 금액의 상한 검사를 유지한다. 현재 binding의 예상 비용 편집으로 기존 Run을 다시 가격 매기지 않는다. 구독 quota를 달러나 0원으로 바꾸지 않는다.
- 비용 차단은 Task와 작업 범위의 nonblocking Budget 결정을 한 트랜잭션으로 저장한다. Run·attempt·자동 시작 수는 증가하지 않는다. 정책·예상 비용·종료 사용량 변경 후 다음 검사에서 해당 보류를 재평가한다. 오래되거나 중복된 결정은 obsolete로 바꾸고, 조건이 같은 대기에 이벤트를 반복 기록하지 않는다. 독립적인 저비용 작업은 계속 진행할 수 있다.
- 비용 결정의 종료 선택은 정확한 현재 작업 상태를 검사하는 명시적 host 동작이다. 자유 입력으로 차단을 해제하지 않으며 요청 재생으로 시작·종료를 반복하지 않는다. 제공자가 부분 사용량이나 작은 누적값을 보내도 기존 토큰·비용 보고의 최대값을 지워 예산이 되살아나지 않도록 했다.
- 모델 설정의 예상 비용 입력, 생성 화면의 미확인 비용 정책, 결정 화면의 비용 정책 편집과 대기 작업의 모델 변경을 연결했다. 정책 저장은 최신 revision과 다른 정책 필드를 보존한다. 결과는 제공자 보고액·예약/추정·미확인 실행을 구분한다. 앱 안내와 한국어/영어 가이드에 사용법과 청구 상한의 한계를 추가했다.

### 19. 제공자 제한 해제 시각과 예약 대기

- 비종료 `AdapterEvent::RateLimited`와 `Run.rate_limit`을 추가했다. 관측/해제 시각은 10진 정수 문자열이며 과거 문서의 누락 필드는 null로 읽는다. Claude의 명시적 rejected/reset, Codex의 명시적 reached 상태와 소진 창, OpenCode 429의 Retry-After 초/HTTP-date를 정규화한다. 누락·만료·잘못된 값과 정보성 퍼센트는 거절 근거로 사용하지 않고 raw header/body도 저장하지 않는다.
- 관측은 DB fencing 검사 후 Run/event와 한 트랜잭션에 저장한다. 동일 Run의 짧아진 관측이나 희소한 정상 이벤트로 기존 해제 시각을 앞당기지 않는다. 관측 저장과 dispatch의 최종 검사/예약을 직렬화한다. provider가 이미 실행 중인 Run의 종료·재시도 여부는 기존 실행 계약을 유지한다.
- migration 0004에 Run JSON의 미만료 해제 시각을 조회하는 부분 인덱스를 추가했다. 보관·종료된 미션의 관측도 읽으므로 미션 보관이나 데몬 재시작으로 제한이 사라지지 않는다. 새 index-only migration이 DDL을 버전 기록보다 먼저 autocommit할 수 있는 기존 runner 경계를 수정하고 rollback 시험을 추가했다. 기존 migration 파일은 변경하지 않았다.
- binding ID·runtime·program·provider·model·auth route·credential/endpoint 참조가 같은 새 작업을 `provider_rate_limited` 및 `dispatch_after_unix_ms`로 대기시킨다. 이름·예상 비용·설치 확인 수정은 제한을 지우지 않는다. 해제 후 Ready로 전이하고 기존 예산·의존 관계·동시 실행 검사를 다시 통과해야 예약한다. 대기는 Run/attempt/decision을 생성하지 않고 같은 상태에 이벤트를 반복하지 않는다. 다른 연결은 진행하며 일시정지 중에는 새 실행을 시작하지 않는다.
- 대기 작업의 모델 변경을 단일 reassign RPC로 연결하고, Stopping 미션의 재배정을 backend에서도 거절한다. 비용/제공자 제한으로 대기 중인 Lead에 메시지를 보내면 원래 계획 작업 뒤에 보관한다. 기존 메시지 경로가 Blocked Lead를 진행 중인 계획으로 인식하지 못해 새 Plan을 중복 생성할 수 있던 누락을 수정했다.
- 참여 작업 목록과 상세에 제한 사유·현지 해제 시각·가능한 동작을 표시한다. 760px 미만에서 작업 선택 후 상세가 렌더되지 않던 누락을 수정했다. 좁은 화면에는 미션 영역 안의 전체 너비 상세를 열고 Escape/닫기 후 행 포커스를 복원한다. 중간 폭에서 같은 상세가 inline/side-sheet에 중복 렌더되던 문제도 수정했다. 확대 상태에서는 상세 헤더를 스크롤할 수 있다.
- 앱 내 안내, 한국어/영어 사용 가이드, wire reference와 adapter/구현 계약을 갱신했다. provider reset은 절대 UTC, 실행 경과 시간은 monotonic으로 구분한다. 계정 변경 재조회, 여러 binding에 걸친 계정 quota 매핑 및 안전한 transient failure의 자동 지연 재시도는 남은 범위다. 실제 제공자 추론 호환성을 fixture 성공으로 대체하지 않는다.

### 20. 결과 불명 실행의 영속 종료 증거와 새 시도

- `mission/reconciliation.rs`에서 Run에 연결된 감독기의 `Exec.exited`와 ended_at, mission/Run 일치, 단일 Exec 소유권, 원본 launch manifest의 소유권·해시를 검증한다. adapter 메모리/PID 조회 실패는 종료 증거로 쓰지 않는다. 정상 실행 중 disconnect 뒤 늦게 저장된 종료와 서비스 재시작 이후의 영속 종료 기록을 같은 경로로 처리한다.
- Run/fence/Exec 스냅샷을 담은 불변 artifact를 `reconciliation_ref`에 연결하고 Task active_run/workspace와 workspace writer를 해제한다. 기존 Run의 unknown/interrupted 상태, 결과, 시간, 모델 및 시도 기록은 보존하고 이전 작업 공간은 Quarantined로 남긴다. 슬롯 소비 판정은 Run의 증거를 확인하며, 제공자 비용의 미확인 부분은 별도로 유지한다. Interrupted도 증거 전에는 슬롯을 보유한다.
- 종료 증거·소유권 해제·기존 질문의 obsolete·새 복구 결정은 하나의 revision CAS 트랜잭션으로 저장한다. 정리된 이전 실행 때문에 중단 중이던 미션은 다른 소유 실행까지 끝난 뒤 중단을 완료한다. 반복 검사에서 no-op revision을 만들지 않는다.
- `retry_reconciled_task`는 명시적으로 선택해야 한다. 현재 task/최신 Run/증거 hash/fence와 시도·미션 예산·모델 allowlist를 다시 확인하고 Task만 Ready로 만든다. 기존 dispatch가 새 Run과 새 worktree를 생성하며 답변 재생은 중복 시도를 만들지 않는다. 일시정지 중에는 Ready로 대기한다. 복구 답변은 host에서 전달 완료로 기록하여 provider 지시로 재전송하지 않는다.
- 상세에 종료 미확인/확인 안내를 구분하고, 한국어·영어 결정 패널에 외부 영향 확인 후 새 시도/중단 선택지를 제공한다. 모델 변경은 종료 확인 후 허용된 binding에 재배정하며 새 시도 승인은 별도 결정으로 유지한다. 앱 내 사용법과 양 언어 가이드를 갱신했다.
- 여기서 확인하는 것은 **로컬 실행 종료**다. 제공자 결과나 외부 작업의 성공을 추정하지 않는다. 재시작 후 살아 있는 native 그룹의 재소유·종료, provider 재연결/결과 조회, 종료 기록을 남기지 못한 프로세스의 조정은 남은 범위다.

### 21. 재시작 시 provider Exec 자원 예약 복원

- 영속 `ExecSupervisor`는 최초 복구 조회 전 admission을 막는다. 이전 daemon 소유의 Prepared/Spawned/Stopping/Unknown Exec를 원래 memory/CPU 정책으로 원장에 복원한다. 종료·보관된 미션이나 terminal Run의 미종료 Exec도 포함하며, 현재 supervisor 실행과 중복 계산하지 않는다. 현재 설정으로는 예약할 수 없는 큰 과거 정책도 그대로 보유해 초과 실행을 막는다.
- `ExecPersistence.recovery_records`와 저장소의 단일 SQL 스냅샷을 연결했다. 미종료 집합과 이전 복원 ID를 함께 읽어 명시적 Exited 관측으로만 예약을 해제한다. 빠진 행·불일치한 열/JSON/Run 연결·원래 binding과 다른 자원 정책·동일 ID의 실행 명세/소유자 변경·상태 역행·중복은 거절한다. 반환 집합은 8192개로 제한하고 초과 시 부분 복구로 admission을 열지 않는다.
- migration 0005에 미종료 Exec의 부분 인덱스를 추가했다. 주기적 복구에서 전체 종료 이력을 다시 읽지 않는다. 기존 migration은 보존하고 버전/재실행/DDL rollback 기대값을 갱신했다. 실행 명세 동일성 검사를 기존 Exec 저장 경로와 공통 함수로 사용한다.
- 복구 조회 실패 시 기존 예약과 독립적인 admission 보류를 유지한다. 정상 host sample이 와도 해제되지 않는다. actor의 새 dispatch/준비만 멈추고 결과 수집·취소·종료 저장·중단 확정은 진행한다. 정상 조회 복귀 시 새 dispatch를 다시 허용하며 반복 오류 로그는 상태 변경 때만 남긴다.
- 이는 자원 원장 복원이다. native process/group handle을 재소유하거나 원격 제공자 결과를 재조회하지 않는다. 이전 프로세스의 종료 기록이 끝내 없으면 예약을 계속 보유한다. 이 단계에서는 결정적 검증·통합 실행의 영속 Exec 연결이 남아 있었다. 검증 실행은 아래 28절에서 연결했으며 통합 실행은 별도 범위다.

### 22. Linux native cgroup 복구와 영속 종료 확인

- Linux cgroup 실행은 launch gate RELEASE 전에 boot ID와 세대 포함 kernfs 그룹 ID를 Exec에 저장한다. 새 필드는 과거 JSON에서 None으로 읽는다. 준비·동일 전이·후속 전이·예약 복원에서 원래 그룹 증거를 변경할 수 없다.
- 복구는 OS worker에서 최대 8개씩 순환한다. manifest/Run/owner/policy/현재 Exec를 재검증하고 동일한 kernel 그룹을 FD로 고정한다. 이름 재사용·boot 변경·ID 변조·일반 디렉터리·symlink는 거부한다. 저장된 취소 요청 또는 MissionStopping이 있을 때만 TERM/3초/force를 수행하고, 그 외에는 살아 있는 그룹을 관찰한다.
- Linux cgroup 신호는 pidfd와 재확인된 membership 또는 cgroup.kill을 사용한다. 경로가 재사용돼도 열린 이전 그룹 FD로 읽고 신호를 보낸다. cgroup.events의 하위 그룹 포함 empty 관측을 사용하며, 사라진 PID/임의의 IO 오류를 empty로 바꾸지 않는다.
- 정상 provider 종료와 재시작 복구 모두 DB exit commit 전에는 그룹 디렉터리를 남긴다. 이전 owner를 유지한 ExecExited만 CAS로 저장하고, 다음 원장 복구 조회가 예약을 해제한다. 저장 실패 후 다시 재시작해도 남겨 둔 그룹을 확인할 수 있다. 오래된 empty 디렉터리를 이름/시간만으로 지우던 startup sweep을 제거했다. commit 뒤에는 빈 하위 그룹을 제한된 크기/깊이로 정리한다.
- 기존 취소·Unknown 종료 증거·새 시도 결정 흐름에 연결했다. Run의 불명 결과와 원래 작업 공간, 미확인 외부 영향/비용을 보존한다. macOS/Windows, 관찰 트리, 기존 식별자 없는 기록과 provider 결과 재조회는 이 변경의 지원 범위 밖이다.

### 23. macOS 독립 관찰자와 재시작 복구

- macOS provider pipe 실행은 별도 `--exec-guardian` 프로세스를 먼저 시작한다. 그 뒤 WAITING helper를 붙이고 기존 gate RELEASE 전에 guardian identity/endpoint와 helper identity를 저장한다. Guardian은 provider 환경/출력/DB를 읽지 않으며, daemon 종료와 별도로 원래 observed-tree 상태를 유지한다.
- 비공개 socket의 kernel peer PID/UID, 시작 시각·boot UUID, workload ID·protocol version을 검증한다. Attach는 원래 daemon만 한 helper에 수행할 수 있다. 새 daemon의 복구는 감독자가 추적한 helper와 저장된 Exec identity까지 대조한다. PID/endpoint만 일치하거나 감독자가 없다는 이유로 종료를 확정하지 않는다.
- 관찰 worker를 IPC 처리와 분리했다. 알려진 live anchor가 sysinfo 목록에서 빠져도 계속 보유하며, proc_pidinfo의 ESRCH/zombie와 읽기/권한/짧은 응답 오류를 구분한다. 명시적으로 관찰한 empty 상태만 유지하고 DB exit commit 후 retirement ack로 감독자를 끝낸다. 초기 시작/handshake/reaper 생성 실패 시 소유한 child와 새 private 디렉터리를 정리한다.
- Linux에서 검증한 기존 취소 의도·immutable owner/manifest/root 검증·CAS/DB 실패 재시도·예약 release 계약에 연결했다. 실행 launcher를 실제 SIGKILL한 뒤 guardian/target 생존, 새로운 controller에서의 재연결·소유 target 종료, 잘못된 birth/peer/root 거부, guardian 자체 손실 시 Unknown 유지를 검증했다. 유료 provider 결과 재연결 시험은 아니다.
- UI snapshot에 Exec를 포함하고 observed-tree 종료 범위를 상세와 재시도 결정에 표시한다. macOS는 관찰을 벗어난 자식 프로세스가 남을 수 있으며 전체 kernel group containment로 표시하지 않는다. Windows, legacy 기록과 provider 결과 재조회는 아래 남은 범위다.

### 24. 작업 요청 미전송 증거와 자동 지연 재시도

- 오류 코드의 transport retryable 플래그와 별도로 `FailedBeforeSubmission` 및 `Run.retry_evidence`를 추가했다. 기존 기록의 누락 필드는 null로 읽는다. 미전송이 확인된 ProviderUnavailable/ProviderRateLimited만 자동 경로에 들어가며, 접수된 실행·Unknown·인증/모델/정책/형식 오류는 자동 재시도하지 않는다.
- Codex는 turn/start를 쓰기 전에 생긴 transport 실패를 구분한다. 쓰기를 시도한 뒤 응답을 잃으면 Disconnected를 유지한다. Claude는 인증 stream이 user 프레임을 아직 꺼내지 않은 실패만 증명한다. OpenCode의 서버 준비 transient IO는 task session/prompt 전이고, production factory는 오류 반환 전에 process cleanup을 확인한다.
- 첫 두 실패에 대해 각각 2초/10초와 0..20% jitter를 계산한다. Run ID에서 고정 seed를 얻으며 Retry-After/관측한 provider reset이 더 늦으면 따른다. Task의 영속 대기 시각은 재시작·DB 재시도로 바뀌지 않는다. 이전 실패/Run/binding snapshot/workspace는 보존하고 기존 dispatch가 새 Run과 workspace를 만든다.
- attempt·전체 시작 수·활성 시간·허용된 enabled binding을 재검사하며 실제 dispatch의 비용/제한/동시 실행 조건을 그대로 적용한다. 일시정지·취소 동안 시작하지 않고, 모델 재배정은 기다리는 시각을 유지한다. 연결된 Exec가 있으면 올바른 Run/미션의 Exited와 종료 시각이 있어야 자동·명시적 재시도를 허용한다.
- 상세와 작업 목록에 재시도 시각/보존되는 실패/한도 안내를 추가하고 대기 중 모델 변경을 연결했다. 한국어·영어 사용 가이드와 wire/adapter/engine 명세를 갱신했다. 실제 데몬·IPC·Git·Codex protocol child에서 첫 initialize 실패 → 영속 종료 → 지연 → 새 Plan → 작업·검증·리뷰·인수 흐름을 검증했다. 모델 추론은 deterministic fixture다.

### 25. 검증 오류를 전달하는 제한된 계획 형식 수리

- 완료된 provider 답변의 schema 실패와 daemon 계획 validator의 수리 가능한 오류를 `InvalidResult`로 구분했다. `Run.retry_evidence`의 `plan_format_rejected`에 당시 revision·거절한 답변 artifact를, `result_ref`에 검증 오류를 저장한다. 일반 RESULT_INVALID나 초기화 오류만으로 수리하지 않으며 허용 목록 밖 역할·모델·검증 명령과 잘못된 경로는 POLICY_DENIED로 분리한다.
- 이전 실행 및 연결된 Exec 종료를 확인한 뒤 영속 `blocked:plan_format_repair`로 전환하고 Running일 때만 새 Run을 만든다. 동일 task에서 최대 두 번 수리하며 task/mission 실행·시간·모델·비용·제공자 제한을 유지한다. 일시정지·취소·저장 장애·서비스 재시작·revision 변경·누락된 Exec를 검증한다. 원래 계약, 실패 기록과 workspace를 변경하지 않는다.
- 다음 Lead에는 정확한 실패 Run ID·검증 오류·거절한 답변을 증거로 전달한다. 계획을 부분 적용하지 않고 완전한 수정 계획을 다시 검증한다. 중간 시도가 미전송으로 증명되면 다음 시도에도 같은 수리 문맥을 유지한다. 증거 artifact의 소유권/UTF-8/크기 및 전체 context 상한을 재검사하며 일반 중간 실패 뒤에는 과거 수리 지시를 되살리지 않는다.
- 세 adapter의 완료 답변과 protocol/초기화 실패를 구분하고 terminal 판정은 공통 메서드를 사용한다. UI 목록·상세·앱 내 안내와 한국어·영어 가이드에 수리 대기·중단·모델 변경을 추가했다. 실제 daemon·IPC·Codex protocol fixture에서 잘못된 첫 계획 → 진단을 포함한 새 Plan → 작업·검증·리뷰·인수까지 연결했다. 실제 모델 추론은 별도 검증 범위다.

### 26. 필수 작업의 Lead 대체 계획과 복구 소진 처리

- 종료가 확인된 필수 비계획 작업이 시도 한도를 소진하면 정확한 실패 Run을 `Task.failure_repair_run_ids`에 연결한 Lead Plan을 저장한다. 실패 진단·계약·요구사항을 문맥으로 전달하며 provider는 이 소유 필드를 제출할 수 없다. 기존 Run·workspace·attempt는 유지한다.
- 실패 작업의 retire와 요구사항을 모두 포함하는 required `replacement_of`가 없으면 계획을 거절한다. 기존 계획 승인·정책·DAG 검증과 제한된 형식 수리를 적용한다. 구현 중 독립 작업을 멈추지 않고, Lead와 대체 작업의 예약에도 시작·시간·모델·비용 한도를 적용한다.
- 계획 생성과 이전 실패 결정 폐기를 원자적으로 저장한다. DB 장애·재시작에 중복 생성하지 않고, 직접 취소한 계획도 같은 실패로 자동 재생성하지 않는다. Lead가 복구를 소유한 원래 작업의 별도 retry를 거절한다. 대기 중인 Lead는 모델 재배정만 할 수 있다.
- 필수 작업·필수 검증의 복구 한도 소진은 취소 의도를 저장한 뒤 모든 소유 Run·worker·연결 Exec 종료 확인을 기다려 Failed로 확정한다. 누락/불일치 Exec를 종료 증거로 보지 않는다. 결과 불명·종료 미확인·선택 작업·시도 잔여 실패·일시정지는 자동 대체하지 않는다.
- 리뷰 한도 소진은 사용자의 근거 해제 경로를 유지한다. 현재 후보/계획의 리뷰 Budget 결정을 만들고, 한도 확대 또는 마지막 주요 지적의 근거 해제 후 재개한다. 해제와 해당 결정 폐기는 한 트랜잭션이며 다른 비용 결정과 이전 후보의 결정은 보존한다. 명시적 중단도 지원한다. 앱 내 안내와 한국어·영어 가이드를 추가했다.
- 결과 화면에서 지적 근거와 저장된 사유를 읽고 사유를 올려 해제할 수 있다. 응답 불명 재시도는 같은 요청 ID·사유 ref를 유지하고, 업로드 중 후보가 바뀌면 이전 지적의 해제를 보내지 않는다. 해제 성공과 인수 완료를 구분해 표시한다.


### 27. 검증 프로세스 그룹의 신호 대상 고정

- Ubuntu 24.04 / procps-ng 4.0.4에서 외부 `kill -9 -<pid>`가 특정 PID를 옵션으로 해석해 실제 `kill(-1, SIGKILL)`을 호출하는 문제를 분리했다. private PID namespace의 컨테이너에서 신호 호출과 argv를 기록했으며 `kill -0 -162`의 대상이 -1로 바뀌는 무해한 재현도 확인했다. 메모리 부족 종료로 처리하지 않는다.
- 검증 runner의 Unix 그룹 정리는 검증 child의 숫자 그룹 ID를 C `kill`에 직접 전달한다. 0·1·i32 범위 밖 값은 거절하며 PATH의 외부 명령이나 옵션 파서를 거치지 않는다. 소유 그룹만 종료하고 독립 그룹은 유지하는 시험을 추가했다. 이 수정은 저수준 검증 helper의 그룹 신호에 적용된다. production 검증의 영속 native 소유권은 아래 28절에서 연결했으며 입력 강제 보호는 남은 범위다.

### 28. 결정적 검증의 영속 Exec·복구·증거 저장

- production actor의 검증 worker를 제공자와 같은 `ExecSupervisor`에 연결했다. 별도 검증 worktree를 만든 뒤 현재 후보·명령 전체·정규화한 실행 파일/cwd·Task/Run/workspace ID·실효 제한 시간·자원 정책을 미션 소유 `Run.context_ref`에 고정한다. context와 workspace Busy의 커밋 전에는 Exec를 만들거나 명령을 실행하지 않는다. 저수준 `workflow::run_verification`와 executor를 주입하지 않은 actor fixture는 기존 helper를 유지한다.
- `mission/exec_store.rs`는 binding 없는 실행을 일반적으로 허용하지 않는다. Verify Task의 단일 검증 ID와 daemon 소유 workspace, 고정된 계약·argv·환경 키·자원 정책을 검사하고 gate release 직전 현재 후보/허용 목록/정책을 다시 검사한다. 원본 manifest와 정확한 Run→Exec 연결은 기존 영속 계약을 따른다. 검증은 512 MiB·CPU 슬롯 1개의 admission 추정치를 사용하며 강제 메모리/CPU 상한으로 표시하지 않는다.
- 정상 종료·취소·시간 초과는 native group과 출력 reader 정리, Exited DB 커밋 후에만 Verification 결과로 이어진다. 종료 저장 실패 중 worker·자원 예약을 유지한다. 결과 저장 실패는 같은 Verification/로그로 재시도하고 새 명령을 실행하지 않는다. 실제 daemon SIGKILL 후에도 Linux cgroup/macOS 관찰자 소유권을 통해 남은 검증을 확인·중단하며 이전 결과는 Unknown/Interrupted로 남긴다.
- 로그 업로드 시작·청크 커서·본문 등록의 DB 장애를 각각 처리한다. 파일 append 뒤 커서 저장이 실패하면 기존 바이트와 재시도 바이트가 일치하는지 확인하고 빠진 바이트만 쓴다. upload UUID를 최종 artifact UUID로 사용해 rename 이후 DB 실패/저장 객체 재생성에도 같은 본문을 검증해 등록할 수 있다. 잘못된 해시와 이미 저장된 범위를 넘는 replay는 거절한다. 시작 실패는 SQL 중복 키와 실제 저장 오류를 구분하며 생성한 임시 파일을 정리한다.
- 절대 실행 파일 또는 절대 PATH 항목에서 찾은 bare name만 지원하고 cwd의 symlink 탈출을 거절한다. 환경 프로필은 명시적으로 미지원이며 현재는 데몬 환경 상속, 네트워크/입력 쓰기 미강제, InputIntegrity::Observed다. 재시작 결과 재조회·Windows 복구·통합 subprocess 전체의 영속 Exec 연결과 장기 저장 장애 분류는 여전히 남는다.

### 29. 전달 불명 지시의 명시적 대체 메시지

- 기존 `Message.supersedes_message_id`를 optional `mission.message` 입력과 연결했다. 같은 미션·수신자의 Unknown/Rejected 사용자 지시만 새 메시지로 대체할 수 있다. 승인/결정 답변, Queued/Delivered 메시지, 이미 후속 메시지가 있는 원본, 아직 Prepared/Sending intent가 남은 원본은 거절한다. 미션 상태·수신 작업·미션 소유 UTF-8 본문·byte 상한을 다시 검사한다.
- 원본 Message/Run/delivery/outbox를 수정하지 않고 새 Message와 route outbox를 atomic commit한다. snapshot revision을 입력 revision과 먼저 대조해 미래 revision으로 오래된 검증을 통과하지 못하게 한다. 동시 요청은 CAS로 하나만 저장하며, 최신 revision으로 다시 요청해도 직접 후속 메시지가 있으면 거절한다. None 필드를 직렬화에서 생략해 기존 요청의 fingerprint와 null/누락 입력의 재생을 유지한다.
- 재시작 이후에도 기존 불명 메시지는 새 실행 문맥에서 제외하며 사용자가 작성한 새 지시만 전달한다. 새 context 메시지는 `supersedes_message_id`로 출처를 보여 준다. 이 기능은 원본의 provider 접수 여부를 재조회하거나 외부 효과의 exactly-once를 보장하지 않는다.
- 대화에서 내용을 확인·수정하고 새 메시지로 보낼 수 있으며 원본/후속 메시지 이동과 키보드 포커스를 연결했다. 동일 원본에 이미 후속 메시지가 나타나거나 업로드 중 상태가 바뀌면 새 전송을 막는다. 저장 응답이 불명확하면 본문·artifact·request ID·payload를 유지하고 같은 요청으로 결과를 확인한다. 확인된 validation/CAS 거절 후에만 수정 또는 새 revision으로 다시 시도한다.

### 30. 일반 입력창의 저장 응답 불명 복구

- Lead/작업별 입력을 공통 `MessageComposer`와 `messageSubmission`에 연결했다. 초안과 업로드/저장 시도를 미션·수신자별 UI 세션 상태로 분리해, 화면 이동과 상세 창 닫기 및 전송 도중 다른 담당 선택에도 원래 요청을 유지한다. 동일 수신자의 여러 입력창과 연속 클릭은 하나의 동기 잠금으로 중복 시작을 막는다.
- 업로드 재시도는 같은 UUID를 유지한다. 저장 요청 직전에 최신 미션/수신 작업 상태와 revision을 다시 검사한다. 응답이 불명확하면 본문·artifact·request ID·수신자·revision 전체를 고정하며, 사용자가 같은 요청의 결과를 확인할 수 있다. 확인된 validation/CAS 거절 후에만 수정이나 새 요청을 허용한다. 종료 상태에서의 영수증 재생은 허용하고 성공 후 해당 수신자의 초안만 비운다.
- 두 입력창의 빈 내용·UTF-8 상한·IME 및 Enter 설정을 통일했다. 전달 불명 메시지의 명시적 대체 UI와도 서버 거절 판정을 공유한다. UI 세션 상태는 원본 snapshot과 별도로 보관한다. 이 단계의 메모리 전용 보관은 다음 31절에서 영속 요청 복구로 확장했다. 저장 확인을 provider 전달/수행 완료 증거로 표시하지 않는다.

### 31. UI 앱 재시작 뒤 메시지 전송 복원

- `messageJournal.ts`는 IndexedDB에 version과 정확한 `mission.message` params만 저장한다. 미션·수신 작업·대체 원본별 atomic slot을 readwrite transaction으로 예약하며 완료 전에는 RPC를 보내지 않는다. 다른 창의 요청이 있으면 덮어쓰거나 삭제하지 않고 그 요청을 복원한다. 이 창의 새 초안은 기존 요청의 결과 확인 뒤 다시 보여 준다.
- 일반 입력과 명시적 대체 입력을 같은 제출/복구 흐름으로 연결했다. 메시지 성공 또는 확인된 거절 뒤 영속 기록 삭제까지 성공해야 새 입력을 허용한다. 삭제 실패에도 원래 UUID·본문 참조·수신자·revision을 유지한다. 초기 기록 조회 실패는 입력을 잠그고 명시적 재조회를 제공하며, 이전 화면의 늦은 읽기가 새 상태를 덮어쓰지 못한다.
- 재시작 뒤 본문은 daemon artifact에서 다시 읽는다. 페이지 cursor·총 길이·256 KiB 상한·SHA-256·strict UTF-8을 확인하며 손상된 내용을 표시하지 않는다. 본문을 읽지 못해도 원래 정확한 요청으로 저장 결과를 확인할 수 있다. restore 자체는 message mutation을 호출하지 않는다. unknown version·다른 scope·잘못된 필드·추가 필드는 기록을 지우거나 임의 변환하지 않고 거절한다.
- 미전송 초안은 앱 메모리에만 보관하며, 영속 원장에는 본문·인증·전체 snapshot을 복사하지 않는다. 표준 저장소가 없거나 실패할 때 volatile fallback으로 전송하지 않는다. 서버의 provider 전달 상태나 기존 Run/outbox 의미는 바꾸지 않는다. 브라우저 프로필 초기화·원장/daemon DB 손상 및 본문 보존 만료는 복구 오류로 남으며 자동으로 새 요청을 만들지 않는다.

### 32. 통합 입력의 불변 후보 고정

- 통합은 단일 저장소 snapshot에서 후보의 ID·source run IDs·base/commit/tree OID를 읽는다. 누락된 후보를 mission base나 private ref로 대체하던 동작을 제거했다. 후보의 미션·성공한 실행 출처·중복을 검사하며, 전체 commit 종류와 tree 일치를 worktree 생성 및 첫 patch 적용 전에 확인한다.
- 각 patch는 저장된 commit과 해당 source의 base를 사용한다. private ref의 이동/삭제가 입력을 바꾸지 않으며, 원본 객체가 없어지면 실패한다. Git replacement object를 제외해 동일 OID의 다른 내용을 읽지 않는다. patch 생성은 외부 diff·textconv와 사용자 prefix·색상·변경 표시 문자 설정의 영향을 차단한다. 사용자 참조를 되돌리거나 삭제하지 않는다.
- 결과 manifest의 source마다 전체 OID와 실행 출처를 기록하고 통합 tree OID를 추가했다. 충돌 질문은 전체 입력 순서와 이미 적용한 입력을 함께 보존한다. 기존 workflow fixture도 capture 결과를 실제 Candidate로 저장한 뒤 통합하도록 맞췄다. 이 단계에서 남긴 영속 Exec 연결은 다음 33절에서 이어서 구현했다.

### 33. 통합 helper의 영속 Exec·취소·재시작 복구

- production actor는 `with_deterministic_exec`로 검증과 통합을 공유 감독기에 연결한다. 통합 입력 계획을 소유 artifact로 고정하고 모델 없는 내부 integrate Task를 예약한다. scheduler의 전역/미션 cap과 자동 실행 횟수 예산을 적용하며, claim에서 Run Starting·workspace Busy·독점 lease·Start outbox Sending·실행 계약을 원자적으로 저장한다. 저수준 workflow API와 집중 verifier fixture는 별도 동기 경로를 유지한다.
- 입력 artifact 경로·SHA-256·helper program·저장소 cwd·자원 정책을 launch manifest와 대조한다. helper는 입력 hash를 다시 확인한 뒤 worktree 준비부터 모든 통합 Git 명령과 manifest 생성을 수행한다. 입력/결과 각각 256 KiB 제한과 반환 결과의 입력 hash·source 순서·OID 일치를 확인한다. helper stdout 외부의 임의 결과 파일을 채택하지 않는다.
- 소유 그룹/stream 종료와 Exec Exited 저장 후 결과 artifact·Candidate 또는 Conflict decision·Run/Task 상태·workspace Retained·outbox 확인을 반영한다. 메모리 512 MiB/CPU 1 예약은 Observe 추정치다. 입장 대기는 같은 Exec ID를 유지하며 취소할 수 있다. 개별 실행 시간은 고정한 policy 제한을 따른다. 저장 장애 시 worker/lease를 유지하고 기록을 재시도하며 Git 작업을 다시 실행하지 않는다.
- 통합 충돌은 정상 helper exit와 구별하여 Run/Task failed로 기록하고 해당 Run을 질문에 연결한다. 열린 충돌 질문이 있으면 일반 실패 결정이나 단순 재시도를 중복 생성하지 않는다. 내부 통합의 취소는 확인된 종료 후 명시적으로 재시도할 수 있으며 새 Run/workspace를 만든다. UI와 메시지 API는 자동 단계에 모델 변경/추가 지시를 허용하지 않는다. 일시정지는 통합 worker 종료까지 기다린 뒤 확정하고, 후속 검증은 재개까지 보류한다. shutdown은 작업을 중단하고 종료 결과를 반영하며, worker panic은 종료 증거로 취급하지 않고 Unknown 경로로 보낸다.
- native recovery는 과거 통합의 task/source/workspace·실행 manifest·입력 artifact를 검증한 뒤만 재소유/취소한다. 재시작은 이전 결과를 Unknown으로 보존하며 자동 재실행·후보 채택을 하지 않는다. 이 단계에서 남긴 integrator 사전 배정과 동일 Task/workspace의 충돌 해결은 다음 34절에서 연결했다.


### 34. Integrator 배정과 같은 작업 공간의 충돌 해결

- 팀 설정에 Integrator 배정을 추가했다. 기본 선택은 Builder와 같으며 변경한 선택·저장된 배정을 표시한다. 새 미션은 네 역할을 요구한다. daemon 소유 `Task.integration`에 원래 계획 참조와 Automatic/Resolving/Continuing 단계를 보관하며 planner는 이 필드를 설정하지 못한다. 자동 Run의 binding snapshot은 null이고 실제 해결 Run만 모델 cap·비용 정책을 사용한다.
- 종료가 확인된 통합 충돌에 명시적으로 답하면 같은 Task의 새 Integrator Run이 같은 Retained workspace를 독점한다. 오래된 질문·다른 실행·미완료 소유권·역할/모델 정책·시도/시간/자동 시작 한도를 검사한다. 일시정지 중에는 결정만 저장하고 재개까지 기다린다. 열린 질문 동안 재배정은 미래 binding만 바꾸며 일반 재시도로 질문을 건너뛰지 못한다.
- 원래 후보 순서·base/commit/tree OID와 writer scope를 보존하고 base/ours/theirs 문맥을 제공한다. 모델의 Patch 보고만으로 후보를 만들지 않는다. 별도 소유 자동 Exec가 실제 파일의 충돌 표시·경로·symlink를 검사하고 capture한 뒤 남은 원래 입력을 이어서 적용한다. 미해결/범위 위반의 명시적 재시도는 Integrator로 돌아간다.
- 뒤쪽 후보의 추가 충돌도 같은 Task/workspace에서 별도 결정과 새 실행으로 처리한다. manifest와 Candidate source run IDs에 채택된 해결 실행을 남긴다. 이전 실패 Run은 불변이며 최종 후보는 검증·독립 리뷰·사용자 인수를 다시 거친다. Git 충돌 경로는 index의 NUL 구분 데이터로 읽어 따옴표·탭·개행·Unicode를 보존한다.
- UI에서 담당 모델과 자동 실행의 모델 없음 표시를 구분한다. 충돌 배너와 결정 버튼은 한국어·영어 안내를 사용하며, 질문 artifact의 파일 경로를 표시한다. 상세에서 해결 담당을 바꿀 수 있다. 모델 응답을 실제 해결 증거로 표시하지 않는다.
- 이 단계는 일반 파일·명시적 삭제의 해결을 연결한다. 해결/이어가기 도중 재시작으로 Unknown/Quarantined가 된 workspace의 복구는 다음 35절에서 연결했다. 특수 파일·후보 제외/재계획·모든 OS 및 실제 유료 CLI 조합의 완결성을 주장하지 않는다.



### 35. 불명 통합의 새 작업 공간 복구와 인수 영향 확인

- 종료 proof가 확인된 Integrator/Continuing Run의 명시적 복구는 같은 Task를 원래 계획의 Automatic 단계로 되돌린다. 입력 후보와 phase를 다시 검사하고 새 workspace에서 재구성한다. 기존 Quarantined 파일·Run·불명 결과는 변경하지 않는다. 이 동작은 04 §2의 불명 workspace 보존 규칙에 따른다.
- 통합 재구성 결정/Task 변경은 기존 CAS·멱등 요청에 포함된다. 종료 증거 없는 실행, 소유권이 남은 workspace와 변경된 원래 입력은 재구성을 거절한다. 새 충돌에는 별도 Integrator 결정이 필요하며 모든 새 Run은 기존 시도·시간·자동 시작 예산을 사용한다.
- 기존 인수 gate는 종료가 확인되고 새 후보가 완성돼도 과거 Unknown을 무조건 차단하고 있었다. 기본 차단을 유지하고 optional `acknowledged_reconciled_run_ids`를 연결했다. Unknown과 Interrupted 모두 사용자의 명시적 영향 검토가 필요하다. 정확한 termination proof/Exec/launch artifact, 답변된 복구 결정과 전달된 답변, 같은 Task의 성공한 새 Run·별도 Retained workspace, 기존 quarantine와 후보 출처를 재검증한다. 단순한 종료 ref만으로는 인수하지 않는다.
- 인수 이벤트의 changes_ref에 확인한 불명 Run·proof·복구 결정·대체 Run·quarantine의 ID를 불변 artifact로 보존한다. 기존 상태를 성공으로 바꾸지 않으며 비용·외부 효과를 확정하지 않는다. 필드 생략/null은 기존 직렬화와 요청 fingerprint를 유지한다.
- UI 인수 확인은 실행마다 미체크 checkbox로 시작한다. 종료 증거가 없으면 확인할 수 없고, 후보 또는 proof hash가 바뀌면 기존 확인을 사용하지 않는다. 확인 중 후보가 바뀐 경우 열린 확인창의 인수도 막는다. 통합 복구 안내는 새 workspace 재구성과 이전 파일 보존을 명시한다.

### 36. 개별 작업 취소의 필수 조건 보존·명시적 복구

- cancelled 필수 작업을 완료 분모에서 제외하던 인수/단계 전이를 수정했다. 취소는 실행을 멈추며 원래 요구사항을 유지한다. 검증·리뷰를 취소해도 자동으로 대체 실행을 생성하지 않는다. 아직 끝나지 않은 선택 작업도 통합 시점에 조용히 누락하지 않고 완료 또는 명시적 취소/대체를 기다린다.
- 취소된 작업의 명시적 retry는 현재 실행 단계, 미션/시도/시간/시작 예산과 binding 정책을 검사한다. 미시작 작업 또는 정확한 Cancelled Run·종료된 Exec·Retained workspace와 lease 해제가 필요하며 Unknown, active owner, 누락된 Exec는 거절한다. 자원 입장 대기 중 취소는 실행기가 시작 전 취소 증거를 저장하고, 이 증거와 Exec 부재를 함께 확인한다. 단순히 Exec가 없다는 이유로 실행 종료를 추정하지 않는다.
- 종료된 cancelled 작업을 계획으로 retire할 수 있게 했다. 살아 있는 실행과 미종료 소유권은 거절하고, 기존 요구사항 coverage·의존 작업·대체 계약 검증은 유지한다. 취소 작업을 superseded로 바꾸는 변경과 새 작업/plan revision은 하나의 트랜잭션이다. 이전 Run/Exec/workspace는 보존한다.
- 상세와 취소 확인창에 한국어·영어 완료 조건 보존 안내를 추가했다. 종료 후 같은 단계에서 재시도/모델 변경 후 재시도를 제공하고, 지나간 단계는 Lead 대화의 새 계획을 안내한다. 취소 후 재시작, 명시적 retry, Lead 메시지의 대체 계획 모두 별도 실행을 통해 검증·리뷰·인수로 이어진다.

### 37. 충돌 후보 제외·필수 대체 계획·새 통합

- production 충돌 결정에 후보 제외 선택지를 연결했다. 원래 helper 입출력·최신 통합 Run·종료 Exec·workspace 소유권을 다시 검증하고 실제 충돌 후보를 결정한다. 직접/전이적 의존 작업과 제외된 결과를 포함한 후보를 base로 사용한 작업도 새 입력에서 제외한다.
- 성공 Task/Run/Candidate/workspace는 그대로 남긴다. 기존 실패 통합과 비성공 의존 작업의 superseded 전이, 새 필수 Lead Plan, 결정 답변을 하나의 CAS 트랜잭션에 저장한다. system 답변 artifact에 결정·원래 후보·제외 Task/Run·사용자 답변 참조를 남기고 새 계획의 objective로 연결한다. 재시작과 같은 요청 재생에서도 원래 기록을 유지한다.
- 제외된 이력은 DAG 의존성과 요구사항 coverage에 쓰지 않지만 작업 수와 ordinal에는 계속 포함한다. 제외된 필수 산출 작업마다 원래 조건을 유지하는 필수 대체가 필요하며 대체 이력의 연결도 검사한다. 남은 필수 작업이 요구사항을 충족하면 선택 작업만 제외한 빈 계획은 허용한다. 계획 적용·단계 전이·인수에서 근거와 대체 완료를 재검증한다.
- 현재 후보를 새 작업 base로 재사용하지 않고 새 workspace에서 통합한다. 제외 Run이 포함된 후보는 게시/인수할 수 없으며 manifest에 exclusion_decision_ids를 남긴다. 통합 후보 revision과 supersedes 이력은 유지하고 새 후보의 검증·리뷰를 수행한다. 수정 사이클·계획 revision·작업 수·실행 예산을 계속 적용한다.
- UI에 두 언어의 선택지와 기록 보존·의존 결과 제외·필수 대체·재검증 안내를 추가했다. 실패한 선택 작업도 명시적 취소할 수 있게 상세 버튼을 맞췄다. 이전 36절의 Cancelled Task 명시적 retry/대체 허용을 Rust 상태표와 states.json에도 반영했다. 이 변경은 기존 Cancelled Run의 상태를 되돌리지 않는다.

### 38. 수정 사이클의 후보 제외와 검증·리뷰 대체

- 이전 통합 후보를 구성한 입력을 제외하면 그 후보에서 수행한 수정 작업·검증·리뷰도 제외 대상이다. 이 경우 모든 필수 대체의 성공을 통합 전에 요구하던 조건 때문에, 새 후보가 있어야 실행 가능한 검증·리뷰가 통합을 막는 문제를 실제 Git/native 실행 시험으로 재현했다.
- 제외 대체 검증을 계획·통합·인수 단계로 구분했다. 계획에는 필요한 모든 대체 계약이 있어야 하고, 통합에는 구현 단계의 대체 성공이 필요하다. Verify/Review 대체는 계약을 유지한 채 새 후보가 나온 뒤 각 단계에서 실행한다. 기존 Task/Run/Candidate/검증/finding 이력은 보존한다.
- 필수 Verify/Review를 일반 작업으로 바꾸거나 원래 검증 명령을 제거하는 계획을 거절한다. 구현 대체를 뒤 단계의 검증·리뷰로 바꾸어 순환 대기를 만드는 계획도 거절한다. 인수는 대체 검증의 현재 candidate Passed 및 연결된 성공 Run, 대체 리뷰의 실제 결과 artifact에 기록된 candidate ID를 확인한다. 공통 리뷰 완료 판정도 같은 결과 판독 함수를 사용한다.
- 연속 두 번 후보 제외 시 최종 대체가 전체 replacement_of 연결을 충족하는지, 검증·리뷰가 끝난 이전 후보의 입력 제외 시 repair-base 작업까지 새 작업으로 수행되는지 확인한다. 구체적인 검증 수치와 제한은 아래 최신 검증 기록에 남긴다.

### 39. 설치 조회 상한·관측 저장·호환성 증거 범위

- 공통 CLI 버전 조회는 고정 `--version`, 닫힌 stdin, 3초 timeout, stdout/stderr 각각 4 KiB 상한을 사용한다. 무제한 `Command.output()`과 실패 시 예전 버전을 재사용하던 경로를 교체했다. 조회에 인증·모델 목록·추론 요청은 포함하지 않는다. missing/failed/timeout/output limit/version 형식 오류를 별도 wire 상태로 반환한다.
- Unix는 별도 프로세스 그룹을 만들고 WNOWAIT로 root를 회수하기 전까지 PID를 보존한 뒤 소유 그룹을 정리한다. Windows 구현은 Job Object에 연결해 정리한다. Windows의 spawn 이후 job 연결, 그룹을 벗어난 자식 및 OS별 실제 실행 검증 한계를 전체 격리 보장으로 확대하지 않는다.
- binding.probe가 관측 버전·확인 시각·capability를 저장하고 새 revision을 반환한다. 실패 시 예전 버전과 capability를 제거하면서 사용자 enabled는 보존한다. 조회 중 모델 설정 변경은 revision CAS로 보호한다. 모델·프로그램·provider·인증 대상 변경은 이전 관측을 무효화하며 단순 이름/비용 편집은 서버의 관측을 유지한다.
- capability는 정확한 runtime/version/OS와 provider/auth 경로·인증 참조 유무가 일치하는 기존 증거에만 연결한다. prerelease/build suffix를 보존하고 별도 키·토큰 경로에 구독 로그인 증거를 옮기지 않는다. 이 단계 당시 남았던 신규·미조회 binding의 서버 소유 증거 처리는 다음 40절에서 연결했다. 실행 직전 binary identity 재확인은 남아 있다.
- 화면은 미저장 변경을 먼저 저장하도록 안내하고 설치 상태를 활성화 상태와 분리한다. 반환된 서버 revision을 폼과 목록에 함께 반영한다. 두 언어 가이드·계약과 700px/200% 확대 시나리오를 갱신했다. 구체적인 최종 검사 결과는 아래 최신 검증 기록에 남긴다.

### 40. 설치 관측의 서버 소유와 실행 snapshot 검증

- 실제 CLI의 binding.save는 클라이언트가 보낸 버전·확인 시각·capability를 지원 증거로 사용하지 않는다. probe의 비공개 관측을 같은 binding document/CAS에 저장해 별도 migration 없이 원자성을 유지한다. 관측은 형식 버전·정확한 연결 대상 해시·OS·관측 버전·시각에 귀속된다. RPC Binding에는 이 내부 필드를 노출하지 않으며 클라이언트의 추가 필드는 관측으로 채택하지 않는다.
- binding.list/save/probe 응답과 dispatch의 새 binding snapshot은 현재 registry로 지원을 다시 계산한다. 확인 시각만 있는 과거 DB, 다른 OS나 다른 연결의 관측, public capability 수정은 unknown으로 처리한다. 역할·실행 이력은 삭제하지 않는다. 실행 준비에서도 Run snapshot의 대상·버전·확인 시각과 같은 서버 관측을 요구해 예전 Run의 지원 주장이 새 프로세스로 직접 넘어가지 않게 한다.
- fake runtime의 시험 입력은 명시적 주입 adapter에만 사용하며 기존 production factory의 거절을 유지한다. 실제 코드의 증거 공급자와 CLI 조회는 서비스 생성 시 주입할 수 있지만 배포 데몬은 고정 구현을 쓰고 RPC·환경 변수로 교체할 수 없다.
- 기존 실제 IPC 메시지 시험 두 개는 client capability=true를 지원 증거처럼 사용하고 있었다. 이 가정을 제거하고 시험 코드 안에서만 protocol fixture의 서버 관측·registry를 주입했다. IPC·SQLite·Git·native gate/Exec·Codex 프로토콜 자식은 실제 구현이며 production binary의 실제 provider 호환성 증거로 확대하지 않는다. 다른 daemon binary 시나리오는 그대로 유지한다.
- 이 단계 당시 남았던 역할별 필수 capability gate와 launch 직전 버전 재확인은 다음 41절에서 연결했다. binary identity 고정과 관측 이후 업그레이드 경쟁 조건은 남아 있다.

### 41. 역할별 필수 기능 검사·시작 전 버전 재확인

- core의 공통 규칙은 모든 모델 작업에 structured_result/events/cancel을 요구한다. Implement/TestAuthor/Integrate/Document에는 scoped_write, 나머지 역할에는 read_only가 필요하다. 선택 기능인 resume/steer/approval_reply/model_listing/usage/native_terminal_attach는 실행을 막지 않는다. 결정적 Verify와 모델 없는 Git 통합은 이 검사 대상이 아니다.
- mission start·정책 배정·계획 적용·재시도·모델 변경과 dispatch/실행 준비에 검사를 연결했다. 예약 전 부족한 기능은 capability_* 대기로 남겨 Run·attempt·비용 예약을 만들지 않는다. 반복 tick은 같은 대기 상태를 다시 저장하지 않으며, 지원되는 서버 관측이나 모델 재배정 후 Ready로 복구한다. 일시정지·취소 상태의 실행 허용 조건은 유지한다.
- 시작 worker는 adapter.start 전에 서버 관측/필수 기능을 다시 검사하고 상한이 있는 로컬 CLI 버전 조회를 수행한다. 버전 불일치·파일 없음·조회 시간 초과는 CAPABILITY_UNSUPPORTED와 작업 미전송 증거로 기록한다. 실제 adapter.start와 모델 요청을 하지 않고 자동 재시도하지 않는다. 저장된 관측과 과거 Run 버전/attempt는 보존하며 설치 재확인 후 명시적 재시도를 요구한다. 동일 버전의 파일 교체 및 조회/시작 사이 교체를 막는 executable identity 고정은 별도 남은 범위다.
- 생성 화면은 필수 역할의 지원을 확인하고 부족하면 제출을 막는다. 작업 상세는 호환성 대기·미전송 실패를 구분하고 지원되지 않는 모델의 재배정을 막는다. 미전송 안내는 오류 코드뿐 아니라 저장된 request_not_submitted 증거도 요구한다. 이 단계의 registry에는 scoped_write의 실제 증거가 없어 전체 쓰기 팀의 자동 시작을 지원한다고 표시하지 않았다. macOS의 제한된 실제 검증 조합은 아래 42절에서 추가했다.
- 재시작 E2E의 protocol fixture는 별도 `mission-fixture-daemon` binary에서 서버 증거를 주입한다. production daemon에 RPC·환경 변수·실행 인수 우회를 추가하지 않았다. 시험용 binary는 앱 번들에 포함하지 않는다. 실제 IPC·SQLite·Git·native gate/Exec·프로세스 재시작을 사용하지만 실제 제공자 추론이나 설치된 CLI 호환성 증거로 확대하지 않는다.

### 42. macOS 실제 Codex 구독 실행·쓰기 범위·즉시 취소

- Apple Silicon macOS(Darwin 25.6.0), Codex 0.154.0, openai, 관리 구독 로그인, gpt-5.6-luna의 production adapter와 native Exec에서 실제 모델 응답·승인된 파일 생성·시작 직후 취소를 확인했다. 각 실행의 선택 모델과 관측 모델 일치, native 정리와 종료 레코드를 검사했다. 기록 저장소는 시험용 메모리 저장소이며 실제 SQLite/전체 미션 검증을 대신하지 않는다.
- 실제 실행에서 발견한 구독 모델 요청의 API-key endpoint 오지정(401), 닫힌 root schema에 properties가 없어 정상 결과까지 거절하는 문제, turn/start 응답과 turn/started 사이 취소 경쟁을 수정했다. 구독 모델 endpoint와 계정 metadata endpoint를 구분하고, 공통 결과를 닫힌 result envelope로 감쌌다. 세 어댑터는 envelope와 과거 직접 DTO를 엄격하게 해석한다. 독립 JSON Schema validator로 여섯 결과 DTO와 잘못된 입력을 검사한다.
- 취소 의도를 먼저 저장하고 thread/turn이 일치하는 turn/started 이후 한 번만 전송한다. 다른 turn 알림과 중복 취소는 전송을 만들지 않는다. 실제 turn/interrupt 응답과 interrupted 종료를 확인했다. 취소 접수와 native 종료 확인은 구분한다.
- 실제 command/exec에서 읽기 전용 안/밖 쓰기, 허용 디렉터리 쓰기, sibling/symlink/.git/.codex/.agents/TMPDIR//tmp 쓰기 및 두 모드의 네트워크 차단 등 12개 경계를 확인했다. 모델 호출 없는 sandbox 증거와 실제 adapter 세 실행의 증거를 별도로 보존한다. 쓰기 실행은 존재하는 절대 workspace가 없으면 peer 생성 전에 거절한다.
- 관리 로그인 설정을 병합할 때 빈 mcp_servers가 기존 서버를 제거하지 않는 것을 확인했다. effective server를 thread별 disabled로 지정하고 해당 thread의 실제 MCP 상태와 tool 목록을 작업 전 검사한다. 앱·플러그인·hooks·하위 에이전트·브라우저·shell snapshot·notify·login/profile shell을 끄고 확인한다. 네트워크 불허 시 web search도 끈다. 사용자 전역 설정과 untrusted 승인 정책은 유지한다.
- registry는 macOS/aarch64/정확한 버전/제공자/인증/모델/빈 저장 참조를 대조한다. 구조화 결과·이벤트·취소·파일 변경 승인·읽기·쓰기 범위·모델 목록만 지원으로 등록했다. resume/steer/usage/native_terminal_attach, 다른 모델·버전·인증 방식 및 다른 OS의 쓰기 능력으로 확대하지 않는다. 앱 안내와 두 언어 시작 가이드에 실제 설정값을 명시했다.
- 재현 명령, 기록 digest와 한계: [Codex macOS 검증](CODEX_MACOS_01540.md). 이 단계의 증거는 adapter까지이며 전체 미션 증거는 44절을 참고한다. 릴리스 판정은 남아 있다.

### 43. 조회 도중 저장되는 미션 상태의 일관성

- 42절의 실제 daemon 재시작 시험에서 복구 결정은 존재하지만 연결된 Run의 reconciliation_ref는 비어 있는 응답을 발견했다. Storage::read_mission은 read pool 연결만 빌리고 여러 SELECT를 하나의 읽기 트랜잭션으로 묶지 않았다. WAL writer가 조회 중 commit하면 mission revision·Run·Decision이 다른 시점의 값으로 섞일 수 있었다.
- 모든 mission 읽기 closure를 deferred 읽기 트랜잭션으로 감싼다. snapshot의 revision/event watermark/모든 entity와 이벤트 목록/상한이 같은 저장 시점을 사용한다. 성공하면 commit하고 오류·panic이면 RAII로 rollback한 뒤 연결을 반환한다. writer는 WAL에서 계속 commit할 수 있다.
- 새 결정적 시험은 첫 조회 직후 실제 writer thread의 mutation을 완료한 뒤 같은 연결에서 다시 materialize하고 event tail을 읽는다. 수정 전 revision 1→2 혼합으로 실패하는 것을 확인했다. 수정 후 같은 읽기는 이전 revision과 entity/event를 유지하고 다음 읽기는 새 commit을 관측해야 한다. 별도 오류·panic 검사로 반환된 연결의 다음 조회도 확인한다. 기존 재시작 시험의 종료 증거 assertion은 완화하지 않았다.

### 44. 실제 Codex 전체 미션·작업별 출력·승인 상세

- Apple Silicon macOS의 Codex 0.154.0/openai/관리 구독/gpt-5.6-luna에서 production daemon·IPC·SQLite·Git·native Exec로 목표→Lead 계획→독립 Builder 두 개→자동 통합→실제 검증 명령→독립 Reviewer→인수와 같은 요청 재생을 통과했다. 모델 실행 네 개와 자동 통합/검증 두 개가 모두 성공했다. 원래 Git HEAD/clean 상태, 실제 요청/관측 모델 일치와 모든 실행의 종료 기록/슬롯 해제를 확인했다. fixture 모델·capability 주입·승인 우회는 사용하지 않았다. [보존 증거·digest·재현](CODEX_MACOS_MISSION_01540.md).
- 실제 Lead가 복잡한 객체 union 스키마에서 계획 대신 불가 응답을 반환하는 문제를 확인했다. Plan/Review에는 평탄한 nullable 필드를 가진 명시적 v2 envelope를 사용한다. Question/Blocked를 유지하고 kind별 활성 필드와 빈 배열/null 규칙을 스키마·문맥에 제공한다. canonical DTO로 재검증하며 비활성 payload를 버리거나 null을 빈 배열로 자동 변환하지 않는다. Codex·Claude·OpenCode에 같은 작업별 스키마를 연결했지만 Claude/OpenCode의 실제 추론 증거로 확대하지 않는다.
- Codex의 thread 모델/실행 중 reroute 관측을 fenced 이벤트로 SQLite Run에 저장한다. 지정 모델 불일치, 인증된 thread의 모델 확인 누락은 MODEL_UNAVAILABLE로 종료한다. stale/종료 후 이벤트는 과거 관측을 바꾸지 않는다.
- Codex 파일 변경 승인에 일치하는 thread/turn/item의 실제 변경을 연결했다. 캐시는 item 64 KiB·전체 256 KiB·32 item·item당 64 change로 제한하며 잘못된 갱신에서 과거 내용을 재사용하지 않는다. 화면은 파일 경로·변경 종류·이동 경로·diff·추가 root 권한을 표시한다. 상세 누락 시 승인을 막고 거절은 유지한다. 배너에도 내부 요청 JSON 대신 사용자 질문을 표시한다.
- 실제 모델 시험은 소유 파일 추가 두 건과 후보의 제한된 읽기 명령 세 건만 검토 후 승인했다. 초기 형식 실패와 하니스가 거절한 복합 읽기 명령 시도는 성공 증거에 포함하지 않았다. 모델을 이용한 자동 상태 변경은 별도 임시 Git 저장소 안에서만 수행했으며 릴리스 및 다른 CLI/OS/인증 경로는 미완료로 유지한다.

### 45. CLI entrypoint 파일 지문·관측 revision·실행 준비 대조

- 기존 사전 검사는 버전 문자열만 비교해 같은 버전을 출력하는 다른 파일을 구분하지 못했다. 설치 관측 format 2에 canonical 경로·바이트 길이·SHA-256과 관측 저장 binding revision을 추가했다. public Binding과 RPC에는 내부 증거를 노출하지 않으며 클라이언트가 보낸 값은 사용하지 않는다. 기존 지문 없는 관측은 unknown으로 투영하므로 다시 설치 확인해야 한다.
- 실제 파일을 연 뒤 64 KiB 청크로 읽고 regular file·최대 512 MiB·길이·조회 전후 metadata·path 대상의 일치를 확인한다. Unix는 O_NONBLOCK으로 열어 FIFO에서 대기하지 않는다. 파일 검사와 --version 자식 조회에는 하나의 3초 예산을 사용한다. 자식 출력/정리 상한은 기존 동작을 유지한다. 파일시스템 I/O 자체를 OS 수준에서 취소하거나 느린 네트워크 파일시스템의 응답 시간을 보장하는 구현은 아니다.
- 시작 worker는 저장된 지문과 다른 파일이면 그 파일의 --version도 실행하지 않는다. 조회 도중 바뀐 파일은 verified로 저장하지 않는다. 검사가 실패하면 이전 관측을 덮어써서 새 파일을 승인하지 않으며 기존 Run·attempt를 보존한다. 재검사는 새 binding revision에 귀속된다. checked_at이 우연히 같거나 시계가 뒤로 가도 이전 Run이 새 파일 지문을 상속하지 못한다. 이름만 편집하는 작업은 원래 관측 revision을 유지한다.
- 주입한 프로토콜 fixture 관측은 명시적 Rust 생성자에서만 파일 지문 없이 사용할 수 있다. production daemon의 기본 생성자에는 이 경로를 활성화하는 RPC·환경 설정·실행 인수를 추가하지 않았다. capability registry나 모델/인증 지원 범위는 넓히지 않았다.
- 실제 210 MB Claude 파일을 검사할 때 비최적화 SHA-256 때문에 개발 빌드의 3초 상한을 넘었다. 시간 제한을 늘리지 않고 development/test의 sha2만 opt-level 3으로 빌드한다. release 설정과 digest 알고리즘은 그대로다. 이후 설치된 Codex·Claude·OpenCode의 지문+버전 및 같은 지문 재확인 smoke를 통과했다.
- 이 변경은 지정한 entrypoint의 교체를 검출한다. Node 등 interpreter, Codex launcher가 고르는 native child, admission 대기 뒤 launch gate/실제 OS exec에 연결되는 파일 고정은 다음 단계로 남긴다. 파일 검사만으로 검사→exec의 모든 경쟁이나 전체 실행 이미지의 신원을 보장한다고 표시하지 않는다.

### 46. Builder 결과 종류와 전체 목표/담당 작업의 구분

- 파일 지문 검사 적용 후 실제 Codex 전체 미션을 다시 실행하자 Builder가 전체 목표의 계획 지시를 자기 역할에 적용하고 report를 반환했다. 엔진은 ResultInvalid로 거절했지만 Builder의 출력 스키마에는 여전히 여섯 결과 종류가 모두 허용되어 있었다. 해당 시험은 실패로 보존하며 이전 성공 기록으로 대체하지 않는다. 소유 미션 취소 후 모든 Exec 종료와 슬롯 정리를 확인했다.
- TaskKind가 지정된 일반 작업의 성공 결과를 공통 writer 분류에 따라 patch/report로 제한했다. question/blocked는 유지하고 native Verify는 모델 보고로 성공시킬 수 없다. Plan/Review의 기존 flat envelope와 종류 없는 과거 호출의 여섯 결과 종류는 유지한다. 사용자 목표는 전체 미션이며 현재 작업은 task_kind·objective·task_contract만 수행한다는 지시를 공통 문맥에 추가했다. 엔진의 최종 결과 검증을 완화하지 않았다.
- 아홉 일반 작업 종류의 맞는/다른 성공 결과, question/blocked, 과거 호출 스키마를 독립 JSON Schema validator로 대조했다. 실제 시험 목표나 승인 허용 목록은 바꾸지 않았다.

### 47. 검증 명령의 OS 권한·별도 환경·후보 입력 대조

- 새 macOS 검증을 `/usr/bin/sandbox-exec`의 기본 거절 profile로 실행한다. 후보와 다른 호스트 경로의 쓰기를 막고 별도 소유 output과 /dev/null만 허용한다. 자식도 같은 권한을 받으며 network는 명령과 미션 양쪽의 허용을 요구한다. TCP·UDP·Unix 소켓, 직접/자식 쓰기와 link 우회, 허용된 output 및 명시적 network 연결을 실제 OS에서 검사했다. 다른 OS의 새 검증은 지원 오류로 중단하며 조용히 비격리 실행으로 대체하지 않는다.
- env_clear로 데몬의 키·SSH agent·런타임 주입 변수를 제거하고 frozen PATH·전용 HOME/temp/XDG/Cargo 출력 경로만 전달한다. command/profile/env/output/candidate commit을 v2 context로 저장한다. 환경은 비밀을 포함하지 않는 고정 allowlist이며 값의 SHA-256을 profile argv에 결합해 key만 같은 다른 환경으로 복구할 수 없다. 기존 v1 실행은 원래 manifest와 대조해 조회/종료 복구한다.
- 검증 workspace는 hook·fsmonitor를 끈 `worktree add --no-checkout`과 index 초기화 후 candidate의 raw blob을 직접 기록해 만든다. 조건부/worktree 설정에서 뒤늦게 활성화되는 clean/smudge/process filter와 줄바꿈 변환도 실행하지 않는다. 파일당 64 MiB·합계 512 MiB의 원본 입력 상한을 적용한다. 전후 실제 파일은 candidate의 raw blob hash·mode·symlink target·전체 파일 집합과 비교한다. 변환 필터의 clean 주장, ignored/untracked 파일, 누락·외부 link·unsupported tree entry를 거절한다. 명령이 exit 0이어도 입력이 변하면 Failed/Unknown이다. 새 샌드박스 성공과 입력 일치가 확인된 경우에만 Enforced이며 엄격한 인수도 같은 증거를 사용한다.
- output은 workspace 옆의 새 0700 디렉터리다. 이미 존재하는 경로를 채택하지 않으며 CAS 재시도는 처음 준비한 경로를 재사용한다. 출력 및 환경/로그 증거는 보존하며 DB 장애 때 명령을 재실행하지 않는다. host file read의 비밀 격리, 출력 디스크 강제 한도, 다른 OS backend, 준비용 Git의 전체 자원/timeout 경계는 남아 있다. 이 범위를 전체 OS/릴리스 완료로 표시하지 않는다.

### 48. Git 공통 디렉터리의 저장소 등록과 별칭

- repository.inspect와 mission.create는 연결된 worktree·하위 디렉터리·symlink의 Git common directory를 대조해 같은 repository_id를 반환한다. 실제 작업 경로와 HEAD는 선택한 worktree에 그대로 속하며, 같은 커밋을 가진 별도 저장소는 별도 ID를 유지한다. create의 Git 경로는 worktree root로 정규화한다.
- 기존 경로 기반 행 하나가 같은 Git 저장소로 확인되면 ID·생성 시각·기존 canonical_path를 보존해 common_dir/object_format을 CAS로 보완한다. 여러 legacy ID가 같은 저장소로 해석되면 참조를 임의 병합하지 않고 InvalidState로 알린다. 해당 과거 데이터의 명시적 병합/복구 UI는 남아 있다.
- common_dir의 중복 검사와 INSERT/UPDATE는 동일한 SQLite IMMEDIATE 트랜잭션 안에서 수행한다. 별도 service·Storage writer가 경쟁해도 새 ID를 중복 저장하지 않고 먼저 저장된 ID를 반환한다. 저장한 common_dir의 변경·제거도 거절한다. mission start는 저장된 repository_id와 현재 Git 식별을 다시 대조한다.
- 데몬에서 Git으로 전달되는 저장소 선택 환경 변수도 제거해 선택한 cwd 대신 다른 Git 디렉터리·worktree·index를 읽지 않도록 한다. 사용자 프로세스의 환경 자체는 변경하지 않는다. [Git 환경 변수 정의](https://git-scm.com/docs/git#_environment_variables).

### 49. 역할 지시문 원본 읽기와 실행 도구 지시 파일 격리(부분)

- [10 역할 지시문](10-role-instructions.md) 명세를 추가했다. 저장소 `.iyagi/roles/<role>.md`를 미션 base commit에서 읽어 시작 때 고정하고 Run 문맥으로 전달한다. md는 지시 내용만 담당하며 권한·쓰기 경로·모델·완료 조건·결과 형식은 바꾸지 못한다.
- `workspace/role_files.rs`는 base commit tree에서 정확한 경로의 항목만 찾아 blob ID로 읽는다. working tree와 index는 읽지 않는다. symlink·submodule·상한 초과·비 UTF-8은 오류로, 경로 없음·공백 파일은 지시문 없음으로 구분한다. 파일 이름과 Role wire 값의 일치도 시험한다. `MissionLimits.max_role_instruction_bytes`와 defaults.json 상한(32 KiB)을 추가하고 parity 시험을 갱신했다.
- Codex: app-server 실행에 `-c project_doc_max_bytes=0`을 추가했다. 설치 버전 0.154.0 소스에서 이 값이 0이면 프로젝트 AGENTS.md 탐색이 중단되는 것을 확인했다. `thread/start` 응답의 `instructionSources`에 작업 공간 안 경로가 있으면 prompt 전에 POLICY_DENIED로 끝낸다. 구독 경로의 사용자 `CODEX_HOME` AGENTS.md와 skill 탐색은 관리 로그인 때문에 끌 수 없어 명세에 남은 노출로 기록했다.
- OpenCode: `OPENCODE_DISABLE_CLAUDE_CODE`와 `OPENCODE_DISABLE_EXTERNAL_SKILLS`를 추가했다. 설치 버전 1.18.30 번들에서 두 설정 이름과, 기존 `OPENCODE_DISABLE_PROJECT_CONFIG`가 프로젝트 지시 파일·설정 탐색을 끄는 조건을 확인했다.
- 연결 보류: 같은 시각 다른 작업이 Mission 계약·service·execution·Git helper를 변경 중이어서 `RoleInstruction` wire 타입, `Mission.role_instructions`, start 고정, Run 문맥 전달, TS 생성 타입은 넣지 않았다. 따라서 현재 제품에서 역할 지시문은 아직 적용되지 않는다.
- 검증: 사용자 요청에 따라 cargo 빌드·시험과 명세 검사 스크립트를 실행하지 않았다. 추가한 단위 시험(role_files 7개, Codex instructionSources 1개)과 parity 시험은 미실행이다.

## 아직 완료되지 않은 경로

1. 세 CLI의 저장된 인증과 Codex/Claude 관리 구독 로그인 확인을 연결했다. Claude의 실제 구독 로그인·유료 추론 및 각 CLI/OS/인증 조합의 호환성 검증은 남아 있다. 임의 endpoint·로컬 모델·다른 OAuth 연결, 인증 등록 중 충돌/중단 복구와 연결 관리 UI도 필요하다. probe의 provider/auth별 증거 구분·정확한 버전 조회와 실패/대상 변경 시 관측 무효화는 위 39절에서 연결했다. 신규·미조회·레거시 binding과 준비된 Run의 서버 소유 증거 처리는 위 40절에서 연결했다. 역할별 필수 기능과 실행 직전 버전 재확인은 위 41절에서 연결했다. entrypoint의 내용 지문·관측 revision과 시작 준비 대조는 45절에서 연결했다. interpreter/launcher child 및 admission/gate→OS exec의 파일 고정·원자적 교체 방지는 더 보강해야 한다. macOS aarch64의 Codex 0.154.0/openai/구독/gpt-5.6-luna는 아래 42절에서 실제 모델 일치·읽기·쓰기 범위·승인·취소를 확인해 네 역할 배정을 허용했다. 이 조합의 실제 모델 전체 미션은 44절에서 확인했다. 다른 조합의 scoped_write와 실제 모델의 장애·복구 경로는 별도 미검증이다. 해석기가 없는 참조를 무시한 채 실행하지 않는다.
2. Unknown/Interrupted의 영속 Exec 종료 증거 검증·새 시도 결정과 재시작 시 provider Exec 예약 재구성을 연결했다. 식별자가 저장된 Linux cgroup의 상태 조회·재소유·취소·종료 저장까지 연결했다. macOS의 새 실행은 독립 관찰자를 통해 재연결·취소·종료 기록 저장을 지원한다. Windows, 독립 관찰자 없는 legacy 트리와 식별자 없는 과거 기록, 이미 사라진 native 그룹/관찰자의 추가 증명, provider 재연결·결과 재조회는 남아 있다. 영속 저장이 장기간 실패하면 Exec worker/예약을 유지하며 재시도한다. 이 상태의 복구와 고정적인 저장 계약 오류의 별도 분류가 필요하다.
3. 일반 메시지의 active steer outbox·응답 확인·다음 실행 문맥·새 Lead 계획 생성과 실패한 Lead의 명시적 복구 대기는 연결했다. 전달 불명/거절 사용자 지시의 명시적 대체 메시지 API·UI를 연결했다. 일반 입력창의 저장 응답 불명은 같은 요청으로 확인하며 화면 이동에도 유지한다. UI 앱 재시작 뒤 확인 중인 일반/대체 요청을 같은 입력창에서 복원한다. provider 상태 재조회, 큰 문맥의 정리, 미확인 전송의 통합 목록·손상 기록 관리와 다른 수신자로 옮겨야 하는 복구 동선은 남아 있다. 메시지 본문만으로 기존 계약을 확대하거나 불명 요청을 자동 재전송하지 않는다.
4. 실제 active_time 계정, 시간 예산 차단, 실행별 비용 예약, 미확인 비용 정책과 알려진 provider reset의 연결별 예약 대기를 연결했다. 계정 변경 후 제한 재조회·여러 binding의 계정 quota 매핑·해제 시각 미확인 제한의 복구와 provider/검증 timeout 단계의 일관된 영속 기록은 남아 있다. 예상 비용 검사는 이미 실행 중인 제공자의 실제 청구 상한을 강제하지 않는다.
5. 확인된 작업/계획/인증 실패의 사용자 재시도·실패 종료 결정, 의존 작업 차단, 작업 요청 미전송이 증명된 transient failure의 지연 재시도와 잘못된 계획의 제한된 형식 수리, 필수 작업 실패의 Lead 대체 계획과 복구 소진 후 종료 확인을 연결했다. 충돌 후보 제외와 필수 대체 계획은 위 37절에서 연결했다. 그 밖의 이미 접수된 실행의 외부 영향 평가를 통한 자동 재시도, 임의 통합 입력 변경과 나머지 phase 전이의 완결성은 남아 있다. 선택 작업 대기/취소와 필수 취소의 명시적 재시도·대체 계획은 위 36절에서 연결했다. 통합의 불명 workspace는 명세에 따라 재사용하지 않으며 위 35절의 새 workspace 재구성과 명시적 영향 확인을 연결했다. 여러 필수 리뷰의 완료를 기다리는 조건과 기존 검증/리뷰의 수정 사이클은 유지한다.
6. 검증 명령과 통합 subprocess는 영속 Exec에 연결했다. macOS verifier의 환경·네트워크·입력 쓰기 차단은 47절에서 연결했다. 신규 저장소 identity 중복 등록은 48절에서 막았다. 모호한 legacy ID의 명시적 병합, LFS/특수 파일 지원, 대용량 출력/patch의 전체 자원 상한, 다른 OS의 격리 실행기와 검증 준비 Git·출력 디스크의 전체 강제 한도는 남아 있다. 통합 helper의 결과 byte 제한은 Git patch와 manifest 생성 중 전체 메모리 상한을 보장하지 않는다.
7. 설정 편집/오류 복구의 나머지 UX, 활동 로그의 장기 artifact 보존 비용, OS·CLI·인증 조합별 실제 호환성과 릴리스 검증.
8. 역할 지시문(49절, 10 명세): Mission 연결(wire 타입·start 고정·Run 문맥·TS 생성 타입·UI 표시), Codex 구독 경로의 사용자 지시·skill 관측 기록, OpenCode 작업 공간 skill 탐색 확인, 세 runtime의 실제 격리 증거가 남아 있다.

위 항목이 남아 있으므로 전체 기능을 완료 또는 production-ready로 판정하지 않는다. 자동 시험 통과와 실제 provider 호환성 검증은 별도 증거다.

## 이번 연결 작업의 검증 기록

- `mission_actor` 15개, `mission_dispatch` 15개, `mission_rpc` 8개 통과. 설치된 provider의 실제 추론 대신 주입한 어댑터를 사용하는 범위를 포함한다.
- `mission_codex` 12개, Claude/OpenCode/exec adapter 시험 각각 8개 통과. Codex 요청 권한과 capability 증거 분리를 검증했다.
- 프런트엔드 전체 단위/DOM 시험 1,306개 통과, TypeScript 검사 통과.
- 브라우저 기존 전체 시나리오 14개 통과. 이후 설정 화면 700px/200% 확대 시험을 추가해 생성·설정 4개 재검증 통과하고 스크린샷을 직접 확인했다.
- 전체 Rust 회귀 검사에서는 이전의 “미전송 실행도 pausing 유지” 가정이 실패했다. 전송하지 않은 Run을 보관한 채 Paused가 되는 계약으로 해당 시험을 수정했고 dispatch 15개를 재검증했다. 실행 중 verifier를 기다리는 pause 시험은 별도로 유지한다.
- 최종 `cargo test -p term-contracts -p term-core -p term-storage -p iyagi-termd`는 677개 통과·0개 실패·2개 기존 ignored로 종료했다. ignored는 실제 Claude CLI 탐지 시험과 기존 대형 terminal journal 재attach 미해결 시험이다.
- 마지막 Git capture를 commit-tree/update-ref plumbing으로 바꿔 repository hook 실행과 사용자 branch 갱신을 피했다. 이후 actor/e2e/workflow/workspace 41개를 집중 재검증해 모두 통과했다. dependency 입력 재계산과 사용자 staged index 보존, commit hook 미실행을 추가로 검증했다.
- `git diff --check` 통과. 생성된 기존 반응형 스크린샷은 테스트 전 파일로 복구했고, 새 설정 화면의 시각 검증 산출물만 별도로 남겼다.

### OpenCode 연결 작업 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd`: 315개 통과, 0개 실패, 3개 ignored. 이 시점의 전체 데몬 회귀 결과이며, 마지막 인증 부정 시험과 여러 필수 리뷰 수정은 아래 집중 검사로 추가 검증했다.
- ignored는 기존 실제 Claude 탐지·대형 journal 재attach 시험과 새 설치형 OpenCode metadata smoke다. OpenCode smoke는 명시적인 설치 경로와 버전을 지정해 별도로 실행했고 통과했다.
- OpenCode HTTP/SSE unit 8개, adapter/process 22개 통과. 모델 호출 없는 설치된 OpenCode 1.18.30 smoke 1개도 재검증 통과했다. 유효하지 않은 인증이 실제 서버에서 거절되는지 확인한 뒤 인증된 health/OpenAPI를 읽었다.
- 승인 결과 불명 저장과 여러 필수 리뷰를 포함한 actor 17개, workflow 9개, 실제 daemon e2e 4개: 총 30개 집중 회귀 통과.
- 공유 실행 감독기의 변경 이후 Codex 12개, Claude 8개, exec 8개 통과. 더 넓은 daemon 검사에도 포함된다.
- 이번 변경 경로의 `git diff --check` 통과. 동시에 수정된 다른 terminal/session 파일은 이 작업에서 되돌리거나 정리하지 않았다.

### 공유 Exec 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-platform`: 397개 통과, 0개 실패, 기존 3개 ignored. 저장소/게이트 연결과 별도 시작 worker를 포함한 전체 데몬·OS 플랫폼 회귀 결과다.
- native group 종료가 interrupt grace를 건너뛰지 않도록 마지막 순서를 보강했다. 이후 exec 17개, Claude 8개, OpenCode 22개: 47개 집중 회귀 통과, 설치형 smoke 1개 ignored. 이 마지막 변경은 앞의 397개 전체 검사 이후 적용했고 관련 실행 경로를 재검증했다.
- actor 22개에는 실제 SQLite·artifact·native gate를 함께 실행하는 시험, 저장 불일치/취소 경합, 느린 시작 중 제어 처리와 등록 후 취소 재전달이 포함된다.
- 변경한 코드·문서 경로의 `git diff --check` 통과. 프런트엔드 동작 변경은 없으며, 동시에 수정된 terminal/session 파일 및 생성물은 보존했다.

### Codex 양방향 공유 실행 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd`: 340개 통과, 0개 실패, 기존 3개 ignored. 마지막 입력/출력 정리와 승인 결과 불명 처리까지 포함한 전체 데몬 회귀 결과다.
- exec 25개는 Codex/Claude 실제 fixture 프로세스, native gate, 저장 실패, 입력 backpressure·동시 Unicode frame, oversized JSON 차단과 unpolled pump 종료를 포함한다.
- Codex 13개는 기존 pinned schema 기반 회귀와 승인 쓰기 실패 후 Unknown/단일 전송 검증을 포함한다. 실제 daemon 전체 미션 4개는 provider Run 네 개의 영속 Exec 연결과 종료 증거도 검증한다.
- 이번 프로토콜 시험은 모델 추론을 fixture로 대체한다. 설치된 CLI·인증 경로별 유료 추론 및 OS별 sandbox 호환성을 증명한 것으로 취급하지 않는다.
- 변경 경로의 `git diff --check` 통과. 다른 terminal/session 변경과 생성물은 보존했다.


### OpenCode 인증·설정 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-pty -p term-contracts`: 688개 통과, 0개 실패, 4개 ignored, 49개 test/doc binary. 공통 gate의 `env_clear`, 영속 manifest 호환성, 실제 daemon RPC의 원문 키 저장 거절까지 포함한다.
- 위 검사에는 connection 6개, exec 26개, OpenCode 24개가 포함된다. 서로 다른 Z.ai 연결의 동시 실행·출력 redaction·키 폐기 후 시작 차단·native helper를 거친 실제 환경 상속 차단을 시험했다. 이후 추가한 줄 한도에서의 비밀값 prefix 생략 시험 1개도 통과했다.
- 설치된 OpenCode 1.18.30의 인증된 `/config` 조회 1개를 별도 실행해 통과했다. 더미 키가 지정 endpoint에 연결되고 프로젝트 설정이 덮어쓰지 않는지 확인했다. session 생성이나 모델 추론은 실행하지 않았다.
- 실제 macOS OS credential store 시험 1개 통과. 임시 디렉터리 namespace에 시험용 키 한 개를 생성·조회·해제하고 다시 조회가 거절되는지 확인했다. 사용자의 기존 credential을 읽거나 수정하지 않았다. 이 시험은 일반 회귀에서는 추가 ignored로 두며 Windows/Linux backend 실행 검증은 남아 있다.
- 미션 프런트엔드 단위/DOM 51개, TypeScript 검사 통과. 최종 DOM의 잘못 붙여넣은 키 차단/정상 참조 저장 시험도 재검증했다. 브라우저 700px·200% 확대 시나리오 2개 통과, 스크린샷을 직접 확인했다.
- 변경 경로 `git diff --check` 통과. 동시에 변경 중인 terminal/session 및 다른 UI 파일·생성물을 보존했다. 전체 기능 및 실제 paid-provider/OS sandbox 호환성 완료 판정은 하지 않는다.

### Codex 인증·설정 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-pty -p term-contracts`: 694개 통과, 0개 실패, 6개 ignored, 49개 test/doc binary. 공통 stdout redactor 연결 수정과 중첩 JSON 비밀값 제거까지 포함한 최종 결과다.
- Codex 인증 경계 시험은 home·API/ChatGPT 목적지·저장 방식·custom provider 변경, 로그인 실패, account 인증 불일치, requiresOpenaiAuth, thread provider 불일치, 실행 중 인증 변경과 정상 경로를 검사한다. 거절 위치까지 전송한 요청을 대조해 키·프롬프트를 너무 일찍 보내지 않는지 확인했다.
- 실제 native gate/fixture는 API 키가 argv·환경에 없고 구조화 결과에서 제거되는지, 종료 기록 저장 장애 중 임시 디렉터리를 보존한 뒤 해제하는지, 폐기된 키로 Exec를 준비하지 않는지 검증했다. 이 시험의 최초 실패로 stdout tap의 redactor 누락을 찾아 수정했고 최종 전체 회귀에서 통과했다.
- 설치된 Codex 0.154.0의 metadata smoke 1개를 명시적으로 실행해 통과했다. 실제 production peer·native gate를 통해 initialize/config/read/account/login/start/account/read와 정리를 확인했다. 임시 메모리 credential backend의 더미 키만 사용했으며 auth.json·모델 목록·thread·turn·유료 추론은 생성하지 않았다. 일반 회귀에서는 ignored로 둔다.
- 미션 프런트엔드 단위/DOM 52개와 TypeScript 검사 통과. Codex API 참조 저장 후 구독으로 바꿀 때 이전 참조가 제거되는 동작을 포함한다. Codex/OpenCode의 700px·200% 확대 브라우저 시나리오 4개 통과, Codex 스크린샷을 직접 확인했다.
- CLI 도움말의 codex-api preset과 변경 경로 `git diff --check`를 확인했다. 설치·인증 조회를 실제 모델 추론·구독 권한·OS sandbox 호환성 증거로 사용하지 않으며, 다른 terminal/session 변경과 생성물을 보존했다.

### Claude 인증·설정 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-pty -p term-contracts`: 703개 통과, 0개 실패, 7개 ignored, 49개 test/doc binary. 세 Claude 인증 preset, 인증 후 취소, 어댑터 해제와 구조화 결과 검증까지 포함한 최종 결과다.
- Claude 인증 native gate/fixture 6개는 세 인증 환경의 분리, 목적지 고정, 중복 Run 거절, 비밀값 제거, 폐기된 키 차단, 초기화 응답 ID/인증/권한 불일치, 초기화 중 취소, 어댑터 해제, 서로 다른 Anthropic/Z.ai 연결의 동시 실행과 구조화 결과 누락을 검사한다. 저장 장애 동안 결과·임시 디렉터리를 보류한 후 종료 기록 저장과 함께 정리하는 동작도 확인했다.
- 별도 unit은 관리 구독에 API/profile/token 메타데이터가 섞인 경우를 거절하고, 인증 초기화 직후 취소하면 실제 fixture가 프롬프트를 받지 않는지 검증한다. connection 시험은 runtime·provider·auth route를 바꿔 같은 credential을 재사용할 수 없는지 확인한다.
- 설치된 Claude Code 2.1.271의 metadata smoke 1개를 명시적으로 실행해 통과했다. 실제 production source의 initialize 단계에서 세 저장 인증 출처와 권한 모드를 확인하고 process/임시 디렉터리 정리를 기다렸다. 더미 키와 메모리 credential backend를 사용했으며 user 프레임·모델 추론은 보내지 않았다. 이 시험의 supervisor는 observer 경로이며 native gate 증거는 위 fixture 시험에서 확보했다. 기존 CLI 구독 로그인 및 유료 추론 권한을 검증한 것으로 취급하지 않는다.
- 미션 프런트엔드 단위/DOM 53개, TypeScript 검사 통과. Claude 구독 참조 저장과 두 참조를 비워 기존 로그인으로 바꾸는 동작을 포함한다. Codex/OpenCode/Claude의 700px·200% 확대 브라우저 시나리오 6개 통과, Claude 스크린샷을 직접 확인했다.
- CLI 도움말에서 세 새 preset을 확인했고 이번 변경 경로의 `git diff --check`를 통과했다. 전체 경로 검사에는 기존 `src/generated/AgentSessionListParams.ts`의 trailing whitespace 두 곳이 남는다. 해당 파일과 동시에 수정 중인 terminal/session/UI 변경은 보존했다.

### 메시지 전달·후속 계획 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-core -p term-storage`: 503개 통과, 0개 실패, 기존 7개 ignored, 55개 test/doc binary. 메시지 outbox·Codex 응답 대조·actor 전달 worker·실제 daemon IPC fixture를 포함한다. 패키지 범위가 위 인증 작업 검사와 다르므로 총수를 직접 비교하지 않는다.
- actor/Codex/exec/RPC 집중 검사 87개 통과, 1개 ignored. actor 메시지 시험 9개는 전달 receipt 저장 장애 후 단일 전송, 불명 결과의 자동 재전송 금지, 미지원 steer의 후속 문맥, 일시정지/재개, 연속 메시지 revision, 후속 계획 병합, 취소 경합과 계획 상한을 검사한다.
- 전체 Rust 검사 후 소유권/복구 시험을 보강하고 메시지 9개를 다시 통과했다. staging 본문 및 다른 미션이 소유한 본문을 거절하고, 종료 Run의 Sending 메시지는 미션의 보관 여부와 관계없이 Unknown으로 남으며 이전 receipt가 이를 덮어쓰지 못한다.
- 실제 데몬·IPC·Git·native gate·Codex stdio fixture에서 추가 지시가 정확한 소유 turn에 도착하고, 응답 및 Exec 종료가 저장되며, 동일 RPC 재생이 메시지나 Run을 늘리지 않는 시험이 통과했다. 이 시험은 위 전체 회귀에도 포함된다. 모델 추론은 fixture로 대체했다.
- 최종 미션 프런트엔드 단위/DOM 55개와 TypeScript 검사 통과. 새 시험은 Lead/담당 두 입력창의 미션 귀속 업로드, 업로드 중 revision 변경과 성공 후 입력 비우기를 검사한다. 최초 추가 시험에서 반복 렌더링으로 멈추는 문제를 재현해 수정한 뒤 재검증했다.
- 설정 가이드의 700px/200% 확대 브라우저 2개 통과, 스크린샷을 직접 확인했다. 메시지 관련 기존 브라우저 시나리오 4개와 새 두 입력창 전송/대화 갱신 시나리오 1개도 통과했다. 브라우저는 mock daemon 기반 UI 증거이며 실제 프로세스 전달은 별도 Rust IPC 시험으로 확인했다.
- 변경한 26개 경로의 tracked/untracked whitespace 검사를 통과했다. 다른 terminal/session/UI 변경과 생성물은 보존했다. 실제 제공자 추론·native 복구와 남은 경로의 완료 판정은 하지 않는다.

### 확인된 실패·재시도 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 772개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 마지막 Rust 동작 변경을 포함한 전체 회귀다. 이후 Rust 필드 주석을 일반 주석으로 정리하고 해당 TypeScript export 시험 1개를 재검증했다.
- actor 42개에는 새 실패/복구 시험 11개가 포함된다. 인증 실패 후 지시 대기, 큰 문맥 준비 실패의 계획 생성 반복 차단, 요청 재생과 이전 Run 보존, 일시정지 중 재시도, 시도/미션 한도 및 비활성 binding 재검사, 원자적 모델 변경, 의존 관계 차단과 독립 작업 성공, 다른 실행 정리 후 Failed 확정, 결정 저장 장애 후 복구, Unknown 실행 재시도 차단을 검증했다.
- 메시지 시험 중 daemon shutdown으로 만든 Failed Lead를 성공한 Lead처럼 사용하던 fixture를 바로잡았다. 실패 뒤 문맥 전달은 명시적 task retry를 거치고, 성공한 Lead 뒤 새 계획 생성/상한 시험은 실제 actor 파이프라인이 인수 단계까지 진행한 상태에서 검증한다. 요구 계약을 완화해 시험을 통과시키지 않았다.
- 실제 데몬·IPC·Git·native gate·Codex protocol fixture에서 첫 Plan의 잘못된 결과 종류를 실패로 저장하고 Exec Exited를 확인한 뒤, 정확한 Recovery 결정에 답해 정상 계획·작업·검증·리뷰·인수까지 진행했다. 과거 실패 Run은 그대로이고 같은 답변 RPC 재생으로 실행이 늘어나지 않는다. 이 시험은 별도 1회와 위 전체 회귀 모두 통과했다. 설치된 제공자의 모델 추론 대신 deterministic fixture를 사용한다.
- 최종 미션 단위/DOM 64개와 TypeScript 검사 통과. 새 UI 시험 9개는 허용 모델만 표시하는 단일 retry 요청, Unknown/Interrupted/실행 중 제어 차단, 종료 작업 표시, 한국어/영어 복구 문구를 확인한다.
- 브라우저 3개 통과: 기존 결정 배지의 포커스 유지와 새 복구 결정의 700px/200% 확대 표시·답변. 갱신한 배너와 선택지 스크린샷을 직접 확인했다. 브라우저는 mock daemon UI 증거이며 실제 상태 전이는 위 Rust/IPC 시험으로 검증한다.
- 이번 변경 23개 경로의 tracked/untracked whitespace 검사를 통과했다. 다른 terminal/session/UI 변경과 생성물을 보존했다. native Unknown 복구와 나머지 구현 항목은 계속 진행 대상이다.

### 실제 시간 계측·예산 연결 이후 검증 (2026-09-15)

- daemon/core/storage/contracts/platform/pty 6개 패키지의 최종 검사 범위는 중복을 제외해 926개 통과, 기존 7개 ignored, 68개 test/doc binary다. 최초 전체 명령은 기존 macOS 프로세스 시험 한 개에서 실패했다. `sh`가 `sleep`으로 exec된 뒤 이름은 `bash`, 실제 exe는 `/bin/sleep`으로 관찰된 경우다. 해당 시험이 이름 외에 실행 파일도 확인하고 실패 전에도 자식을 정리하도록 수정했다. production 프로세스 감지 코드는 변경하지 않았다.
- 최초 명령에서 통과한 daemon/contracts/core 검사 이후, platform/pty/storage 220개와 나머지 3개 패키지 doc 시험을 재검증해 모두 통과했다. 따라서 최초 전체 명령 자체가 성공한 것으로 기록하지 않는다. 변경과 관계없는 terminal/session/UI 작업 및 생성물은 보존했다.
- actor/dispatch/workflow/RPC 집중 검사 80개 통과. actor 47개에는 새 시간 시험 5개가 포함된다. 1초 checkpoint·1초 미만 최종 저장, pause/resume와 CAS 실패·요청 재생, 병렬 Run 두 개와 Pausing 시간, SQLite 저장 실패 후 단일 누적, 데몬 재시작의 중단 시간 제외와 Unknown Run 보존, checkpoint 사이의 시간 예산 차단 및 정책 확대를 검사한다.
- 실제 daemon IPC 미션 6개가 통과했다. 조용히 지시를 기다리는 Codex protocol fixture에서도 Mission/Run 시간이 늘어 저장되는 것을 관찰하고, 정상 전체 흐름 및 실패 계획 복구 흐름에서 provider/verifier 종료 Run의 실제 계측값을 확인했다. 모델 추론은 기존 deterministic fixture이며 유료 제공자 호환성 검증은 아니다.
- 프런트엔드 미션 단위/DOM 64개, TypeScript 검사 통과. Unknown/Interrupted/Running 상세가 로컬 현재 시각을 더하지 않고 저장된 시간과 한계 안내를 표시하는 것을 기존 제어 시험에 추가했다.
- 브라우저 3개 통과. 활성 시간/한도를 추가한 700px/200% 확대 화면의 넘침·결정 선택지와 기존 포커스 유지 동작을 확인했고, 두 스크린샷을 직접 검토했다.
- 명세 검사: 문서 14개·로컬 링크 36개·참조 사례 40개와 SQL 제약/rollback 통과. 변경 경로의 tracked diff 및 범위 내 untracked 27개 파일 whitespace 검사 통과. 실제 제공자/OS 호환성과 남은 제어·비용·복구 경로의 완료 판정은 하지 않는다.

### 비용 예약·대기 복구 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 789개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 재검증 첫 시도는 테스트용 `term-fixture` 실행 파일 부재로 중단됐다. `cargo build -p term-fixture`로 준비한 뒤 전체 명령을 다시 실행해 통과했다.
- actor 50개·dispatch 22개·실제 daemon IPC 7개가 위 전체 회귀에 포함된다. 새 비용 시험은 병렬 예약의 중복 지출 방지, 실제 보고에 따른 잔여 예약 해제, 저비용 독립 작업 진행, 정확한 상한 경계와 초과 보고, Unknown/Stopping 예약 유지, 과거 미확인 비용 보존, 큰 정수 합산, 결정 저장 실패의 rollback, 부분 사용량 누적 보존, 비용 종료 선택과 요청 재생을 검사한다.
- 실제 데몬·SQLite·Git·native gate·Codex protocol fixture에서 1달러 예상 Plan 이후 두 Builder가 비용 때문에 대기하는 것을 확인했다. 실제 정책 RPC로 상한을 4달러로 바꾼 뒤 작업·검증·리뷰·인수까지 성공했고, 이전 Plan Run과 요청 멱등성이 유지됐다. 금액은 사용자가 설정한 예상값이며 모델 추론은 deterministic fixture다. 실제 유료 제공자의 청구나 호환성 시험으로 취급하지 않는다.
- 최종 미션 단위/DOM 69개와 TypeScript 검사 통과. 모델 예상값의 정수 변환과 빈 값 처리, 미확인/부분 사용량 표시, Number 정밀도를 넘는 합산, 최신 revision·다른 정책 필드 보존, 비용 대기 작업의 단일 모델 변경 요청을 검사했다.
- 브라우저는 실패 복구 2개와 비용 정책 편집 2개, 총 4개 시나리오가 통과했다. 최초 환경에는 Playwright Chromium이 없어 설치 후 실행했다. 700px와 200% 확대 화면을 직접 확인하고 비용 패널의 높이와 내부 스크롤을 제한해 대화 입력창을 유지했다. 비용 시험은 최종적으로 저장 메시지와 입력창이 보이는지도 검사한다. 200% 스크롤의 소수 픽셀 반올림에는 1% 가시성 오차만 허용한다.
- 명세 검사에서 문서 14개·로컬 링크 36개·참조 사례 40개와 SQL 제약/rollback이 통과했다. 변경 범위의 tracked diff와 새 비용/안내 관련 13개 파일 whitespace 검사도 통과했다. 구독 quota 자동 대기, native Unknown 복구와 실제 CLI/OS 호환성은 남은 구현·검증 범위로 유지한다.

### 제공자 제한·좁은 화면 상세 연결 이후 검증 (2026-09-15)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 801개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 이 전체 검사 뒤 Blocked Lead의 메시지 분기를 보강했고, `cargo test -p iyagi-termd --test mission_actor` 51개를 별도로 통과했다. 전체 명령이 마지막 메시지 수정까지 포함한 것으로 표현하지 않는다.
- 최종 dispatch 27개에는 제한의 영속 저장·보관 미션/서비스 재시작 후 보존·정확한 해제 경계·시도 미소비·no-op·다른 연결로 재배정·요청 재생·DB fence·저장 실패 rollback·이름/비용 변경의 제한 유지·일시정지 중 해제와 Stopping 재배정 거절이 포함된다. 저장소 73개에는 새 인덱스와 DDL/버전 원자성 시험이 포함된다.
- 실제 daemon IPC 8개가 통과했다. 새 provider reset 시험은 Codex protocol fixture의 실제 stdout 알림을 받아 Plan 결과에 시각을 저장하고 Builder 두 개를 대기시킨다. 알려진 시각 이후에만 다음 provider Run이 시작하며, 이전 Plan Run을 유지한 채 검증·리뷰·인수까지 성공한다. 정상 모델 추론은 deterministic fixture로 대체한다.
- Claude recorded stream은 allowed/warning을 거절로 취급하지 않고 rejected window 하나만 내보내며 현재 Run의 성공 결과를 유지한다. OpenCode session.error 시험은 429의 Retry-After 관측이 정규화된 실패보다 먼저 전달되고 raw credential/body가 이벤트에 섞이지 않는지 확인한다. 순수 파서는 희소한 Codex window, 과거/누락/잘못된 시각, 큰 정수, Retry-After의 대소문자·HTTP-date·중복 헤더를 검사한다.
- 최종 미션 단위/DOM 73개와 TypeScript 검사 통과. 한국어/영어 해제 시각, 종료된 작업의 과거 대기 숨김, 비용/제공자 제한 작업의 모델 변경과 실패 재시도 구분을 검사했다.
- 브라우저 6개 통과: 기존 1440/1024/700/200% 반응형 4개와 새 제한 대기 상세 700/200% 2개. 최초 시험에서 좁은 화면의 상세 누락을 재현해 수정했다. 중간 폭의 상세 단일 렌더, 미션 영역 내 패널 경계, 해제 시각과 모델 변경 버튼 가시성, Escape 닫기와 원래 행 포커스 복원을 확인했다. 최종 두 화면도 직접 검토했다.
- 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback)와 변경 범위 tracked diff 및 새/untracked 14개 파일의 whitespace를 확인했다. 기존 terminal/session/layout 변경과 생성물은 보존했다.
- 새 브라우저 시험을 포함한 별도 TypeScript 검사도 통과했다. 검사 중 드러난 공통 helper의 미사용 빈 함수와 부정확한 missionMessage 선언은 실제 DaemonClient 타입을 사용하도록 정리했다. 브라우저 동작은 변경하지 않았다.

### 영속 종료 증거·새 시도 연결 이후 검증 (2026-09-16)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 810개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 이후 Interrupted의 취소가 이력을 Stopping으로 바꾸지 않도록 보강하고 actor 59개를 통과했다. 복구 시험의 시계를 고정한 뒤 해당 8개도 다시 통과했다. 전체 810개가 마지막 취소 보강까지 포함한다고 표현하지 않는다.
- 실제 native gate/supervisor/SQLite 시험에서 프로세스 그룹 종료와 stream 정리 뒤 Exec 종료를 저장하고 provider 결과를 잃게 했다. 새 MissionService가 메모리 실행 핸들 없이 종료 증거를 연결하고 소유권을 해제하면서 이전 Run을 Unknown으로 보존하는 것을 검사했다. 설치된 제공자에 대한 유료 추론 시험은 아니다.
- 복구 8개는 미확인 소유권 유지, manifest 변조 거절, 저장 실패 시 Run/Task/workspace/decision rollback, no-op, 이전 Run/격리 디렉터리 보존, 새 workspace·단일 시도 생성, 답변 재생, 늦은 callback 무시, 예산·오래된 질문·증거 불일치 거절, paused 새 시도와 resume, 중단 완료, Interrupted 이력 보존을 검사한다. 로컬 종료로 미확인 제공자 비용을 축소하지 않는 것도 확인했다.
- 미션과 i18n 단위/DOM 93개 및 TypeScript 검사가 통과했다. 종료 미확인/확인 안내, 한국어·영어 새 시도 결정, 모델 변경과 재시도 승인의 분리를 검사한다. 새 브라우저 시험을 포함한 별도 TypeScript 검사도 통과했다.
- Playwright 4개 통과: 기존 실패 복구와 새 종료 확인 결정 각각 700px·200% 확대. 안내·선택지 가시성, 수평 넘침 없음, 답변 뒤 버튼 비활성화를 확인했고 새 결정 화면 두 장을 직접 검토했다. 브라우저는 mock IPC를 사용하며 Rust의 실제 종료 증거 검사와 구분한다.
- 명세 검사에서 문서 14개·링크 36개·참조 사례 40개 및 SQL 제약/rollback이 통과했다. 변경 범위의 whitespace도 확인했다. 살아 있는 native 그룹의 재소유/중단, provider 결과 재조회와 재시작 시 Exec 예약 재구성은 계속 구현할 항목으로 남겼다.

### provider Exec 예약 복원 이후 검증 (2026-09-16)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 821개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 이번 예약 복원과 actor 보류, 저장소 조회/정책 검증, migration 0005를 포함한다. 이후 변경은 사용 가이드와 소스 주석이다.
- 새 원장 시험 4개는 복원된 예약의 concurrency/CPU/memory/headroom 차단, 현재 설정보다 큰 과거 예약 유지, 반복 복원의 단일 계정, 종료 증거 없는 일반 release 거절, 누락·중복·변조·상태 역행 시 원자적 보류, 현재 supervisor의 예약 보존을 검사했다.
- actor 66개 중 새 예약 복원 시험 7개는 최초 admission 차단, 영속 조회 재시도, 종료/보관 미션의 미종료 Exec 포함, 현재 owner 제외, 행 삭제·Run 링크·열/JSON·자원 정책 변조 거절, 복구 보류 중 새 시도 미소비와 기존 실행 취소 완료를 검사했다.
- 실제 native gate로 장시간 실행하는 fixture 프로세스를 시작하고 새 MissionService/supervisor가 그 예약을 복원했다. 원래 감독기의 종료·stream 정리·Exec 종료 저장 후에만 새 원장의 예약이 해제되고, 기존 결과 불명 복구 결정으로 이어지는 것을 확인했다. 실제 제공자 추론이나 살아 있는 프로세스 핸들의 재소유를 검증한 것으로 표현하지 않는다.
- 실제 daemon IPC 8개와 공유 Exec 33개 통과를 전체 회귀에 포함한다. 생산 actor가 첫 복구 조회를 수행하도록 연결한 뒤 기존 정상 미션·인증·메시지·비용·quota 흐름이 유지되는지 확인했다.
- 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback)와 변경 범위 tracked diff 및 새 파일 whitespace 검사도 통과했다. 화면 동작은 변경하지 않았다. native 그룹 재소유/중단과 제공자 결과 조회는 남은 항목이다.


### Linux native 실행 복구 이후 검증 (2026-09-16)

- macOS에서 `cargo test -p iyagi-termd -p term-platform -p term-contracts -p term-storage -p term-core`: 889개 통과, 0개 실패, 기존 7개 ignored, 61개 test/doc binary. 마지막 worker 순환/핸들 제한/로그 변경 뒤 actor 66개와 공유 Exec 33개(기존 1개 ignored)를 다시 통과했다.
- Ubuntu 24.04/aarch64의 실제 Linux kernel에서 컨테이너 전용 private cgroup namespace를 사용했다. `IYAGI_CGROUP_REQUIRE_DELEGATION=1`로 native 검사가 조용히 생략되지 않도록 강제하고, 새 테스트 프로세스/그룹만 생성·종료했다. daemon lib 166개·mission actor 68개·공유 Exec 33개·platform 73개 총 340개 통과(기존 2개 ignored). 마지막 변경 뒤 native 복구 3개와 정상 gate 종료 저장 장애 1개를 다시 통과했다.
- 실제 OS 검증은 boot/세대 ID 불일치, 동일 이름의 새 cgroup, FD로 고정된 이전 그룹, 하위 cgroup의 종료, 무관한 프로세스 보존, 일반 디렉터리/symlink 거부를 포함한다. 서비스 검증은 이전 native 핸들 폐기 후 재소유, 취소 전 무신호, DB exit commit 실패/재시작/재시도, 원본 manifest 변조 거부, 변경된 durable identity의 신호 차단, 종료 확인 뒤 명시적 새 시도 결정을 포함한다. 이는 실제 Linux 그룹에 대한 검증이며 유료 제공자 추론이나 Windows/macOS native 재소유 검증은 아니다.
- Linux 회귀 중 기존 FakeAdapter 취소 시험의 가상 시계 race가 재현됐다. Started 수신 뒤 worker가 Delay를 늦게 등록하면 이미 전진한 시각보다 뒤의 deadline에서 영구 대기했다. 시험을 포화 종점까지 전진하도록 고쳐 두 실행 순서를 모두 처리했고 lib 166개 재실행이 통과했다. 생산 재시도 정책을 변경하지 않았다.
- Windows의 contracts/core/platform cross-check는 통과했다. 전체 daemon cross-check는 이 macOS 환경에 Windows C SDK 헤더가 없어 bundled SQLite의 `stdlib.h` 단계에서 실패했다. 실제 Windows 실행/인증 호환성으로 간주하지 않는다.
- 생성 계약 재검증 260개, TypeScript 검사, 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback) 및 변경 범위 whitespace 검사를 통과했다. Linux 그룹의 복구 가능한 소유권과 종료를 연결했으며 전체 오케스트레이션 완료 판정은 하지 않는다.

### macOS 독립 관찰자 복구 이후 검증 (2026-09-16)

- `umask 022`에서 `cargo test -p iyagi-termd -p term-platform -p term-core -p term-storage -p term-contracts`: 895개 통과, 0개 실패, 기존 7개 ignored, 61개 test/doc binary. 마지막 제품 코드 변경을 포함한 전체 회귀다.
- 첫 전체 검사의 guardian 시작 실패 2개와 정리 시점 검사 실패 1개를 수정했다. 임시 디렉터리는 기본 권한에 의존하지 않고 생성 시 `0700`을 지정한다. 시작 실패의 원인을 daemon 로그에 남기며, retirement 응답 이후 관찰 thread 종료와 socket 제거가 완료되는 시점은 별도로 기다린다. 수정 후 actor 69개·exec 36개 집중 회귀도 통과했다.
- 실제 launch helper/gate를 사용하는 별도 launcher 프로세스를 SIGKILL해 guardian과 target이 남는 경계를 시험했다. 새 controller의 재연결·원래 target 종료·종료 증거의 retirement 전 보존, 잘못된 birth/root와 다른 socket peer 거부, guardian 자체 손실을 종료로 취급하지 않는 동작을 확인했다. 전체 검사와 별도로 socket 시험의 parent 권한을 보강해 거절이 실제 kernel peer PID 검사에서 발생하는지 확인했으며 guardian 시험 3개가 다시 통과했다. 3개 중 하나는 자식 launcher fixture 진입점이고 실제 경계 시나리오는 2개다. 전체 daemon IPC 재시작이나 유료 provider 세션 재연결 증거로 확대하지 않는다.
- 실제 macOS 그룹을 사용하는 서비스 복구 시험 3개는 취소 전 무신호, DB exit 저장 장애와 그 사이의 supervisor 교체, manifest/소유권 변조 차단, 저장 성공 이후 예약 해제와 명시적 새 시도를 검증했다. Linux의 private cgroup namespace에서도 native 복구 3개와 platform 73개가 통과했다. Windows는 `term-platform`의 `x86_64-pc-windows-msvc` 컴파일만 통과했으며 실제 실행 복구를 검증하지 않았다.
- 미션·i18n 단위/DOM 95개와 TypeScript 검사, 브라우저 시나리오의 추가 TypeScript 검사, 700px·200% 확대 브라우저 시험 2개가 통과했다. 상세와 복구 결정에 관찰 범위가 표시되고 다른 미션/cgroup에는 잘못 노출되지 않는지 확인했으며 스크린샷을 직접 검토했다. 브라우저는 mock daemon UI 시험이다.
- 한국어·영어 사용 가이드에 OS별 복구 동작과 구현 위치를 추가했다. 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback)와 변경 범위 28개 파일 whitespace 검사를 통과했다. macOS의 observed-tree 한계, Windows/legacy 복구 및 provider 결과 재조회는 남은 범위로 유지한다.

### 미전송 일시 오류의 자동 재시도 이후 검증 (2026-09-16)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 840개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 제품의 retry evidence·fencing·영속 대기·종료/예산 검증을 포함한 전체 회귀다. 이후 모델 재배정 시험과 명시적 재시도 거절 시험의 조건을 보강하고 자동 복구 시험 9개를 다시 통과했다.
- 복구 시험은 실제 SQLite와 Git workspace를 사용한다. 고정 wall/monotonic clock으로 2초/10초 jitter 범위, deadline 직전/직후, 재시작 뒤 동일 deadline, 새 Run/독립 workspace와 이전 기록 보존, 두 재시도 뒤 결정 전환, pause/cancel, DB commit 장애, task/mission 시작 한도, 비활성 모델, 모델 재배정 멱등성과 다음 Run의 binding snapshot을 확인했다. missing/unfinished Exec에서는 자동 재시도를 막고, Task 자체는 재시도 가능한 Failed 상태로 만들어 명시적 재시도도 Exec 검증에서 거절되는지 확인했다. 증거 누락·접수 확인·Unknown·비 transient code·legacy JSON도 검사했다.
- Codex protocol 시험은 initialize 쓰기 실패와 turn/start 부분 쓰기를 구분해 후자에서 작업을 재전송하지 않고 Disconnected로 남기는지 확인했다. 실제 Claude fixture 자식은 initialize 응답 전에 종료하고 user 프레임 수신 marker를 만들지 않았다. DB exit 저장 장애 동안 결과를 보류하고, 저장 성공 후에만 미전송 실패 증거와 정리를 확인했다.
- 실제 daemon·IPC·native gate·SQLite·Git·Codex protocol fixture에서 첫 initialize 실패가 자동 대기로 들어간 뒤 예정 시각 이후 새 Plan으로 작업·검증·리뷰·인수까지 완료됐다. 첫 Run의 Failed/evidence/종료 Exec/작업 공간을 보존하고 다음 Run의 시각·attempt·별도 workspace를 확인했다. 이 시나리오는 집중 1개와 위 전체 회귀에서 모두 통과했다. 유료 모델 추론과 설치된 CLI 호환성은 이 증거의 범위가 아니다.
- 최종 미션·i18n 단위/DOM 97개, TypeScript 및 브라우저 시나리오 TypeScript 검사, 700px·200% 확대 브라우저 2개 통과. 대기 시각/실패 보존 안내와 모델 변경 버튼을 확인하고 스크린샷을 검토했다. 최초 브라우저 실패로 대기 이유가 모델 변경 허용 목록에서 빠진 것을 찾아 수정했다. 마지막 앱 내 사용법 변경 후 설정·생성·i18n 집중 검사도 통과했다.
- 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback)와 이번 변경 범위 44개 파일 whitespace 검사를 통과했다. 전체 목표는 유지하며, 이미 접수된 실행의 외부 영향 평가·잘못된 계획의 제한된 수리·Windows 재시작 복구 등은 남은 범위다.

### 제한된 계획 형식 수리 이후 검증 (2026-09-16)

- `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts`: 850개 통과, 0개 실패, 기존 7개 ignored, 59개 test/doc binary. 이후 미전송 중간 시도에 수리 문맥을 유지하도록 보강하고 계획 수리 집중 검사 11개를 최종 통과했다. 전체 명령 자체가 이 마지막 문맥 변경까지 포함했다고 표현하지 않는다.
- 실제 SQLite/Git actor 검사는 오류·거절 답변의 정확한 다음 실행 전달, 별도 workspace, 과거 Run/계약 보존, 계획 부분 적용 없음, 충분한 task attempt 예산에서도 자동 수리 두 번 상한, 일시정지/서비스 재시작/단일 재개, 저장 장애 중 미실행, 취소·attempt/start·binding·revision 변경과 missing Exec 차단을 확인했다. generic RESULT_INVALID와 정책 밖 경로는 자동 수리하지 않는다. staging artifact의 문맥 유입 거절, 거절 답변/합산 context byte 상한, 미전송 시도 후 동일 수리 문맥, 수동 계획 적용 정책의 승인 유지도 확인했다.
- Codex의 malformed 완료 답변은 원문이 있는 InvalidResult이고 완료되지 않은 protocol 실패는 일반 실패로 남는다. 실제 Claude 자식의 성공 envelope에 structured output이 없으면 종료 정리 후 InvalidResult를 확인한다. OpenCode는 소유 session/parent/message/model·영속 완료 검증을 통과한 schema 오류에만 InvalidResult를 만든다. 최초 회귀에서 새 terminal 유형을 포함하지 않은 Claude/OpenCode 시험 helper가 실패해 공통 terminal 판정을 사용하도록 수정했고 전체 회귀에서 통과했다.
- 실제 daemon·IPC·native gate·SQLite·Git·Codex protocol child에서 첫 잘못된 Plan 뒤 진단과 거절 답변을 포함한 새 Plan으로 작업·검증·리뷰·인수까지 완료했다. fixture는 다음 context에 정확한 진단/이전 답변이 없으면 실패한다. 종료 Exec·새 attempt/workspace와 복구 결정 없이 정상 진행을 검증했다. 기존 명시적 실패 복구 시나리오는 정책 밖 경로 거절로 유지했고 실제 daemon 시나리오 10개가 통과했다. 유료 모델 추론·설치된 CLI 호환성 증거로 확대하지 않는다.
- 미션·i18n 단위/DOM 100개, 소스 TypeScript 및 새 브라우저 시나리오를 포함한 TypeScript 검사가 통과했다. 계획 수리 대기/중단, 한국어·영어 문구, 다른 task/결과 불명/종료 작업에서 잘못된 안내 숨김을 확인했다. mock daemon 브라우저 700px·200% 확대 2개가 통과했고 안내·모델 변경 버튼 가시성·수평 넘침·Escape 포커스 복귀와 스크린샷을 직접 확인했다.
- 앱 내 사용법과 한국어·영어 가이드, engine/adapter/wire 문서를 갱신했다. 명세 검사(문서 14개·링크 36개·참조 사례 40개·SQL 제약/rollback)와 변경 범위 38개 파일 whitespace 검사를 통과했다. 기존 terminal/session/UI 변경은 보존했다. 필수 작업 실패의 자동 Lead 수정 기회, Windows 복구와 실제 provider 호환성 등 전체 목표의 남은 범위는 유지한다.

### 필수 실패 대체 계획·리뷰 판단·그룹 신호 수정 이후 검증 (2026-09-16)

- 필수 실패 집중 actor 시험 15개가 macOS에서 통과했다. 정확한 실패 진단·대체 작업, 독립 실행 병행, 수동 계획 승인, DB 장애/재시작 중복 방지, 취소 후 재생성 방지, 원래 작업의 중복 retry 거절, 필수 검증 한도 소진, Exec 종료 증거 누락, 리뷰 지적별 근거 해제·한도 확대·명시적 중단을 포함한다. SQLite/Git/실제 검증 명령과 주입 provider를 사용한다.
- 전체 `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts -- --test-threads=2`는 59개 바이너리에서 870개 통과·0개 실패·7개 기존 ignored로 종료했다. 이는 마지막 Unix 검증 그룹 신호 변경 전 결과다. 앞선 기본 병렬도 실행은 호스트 프로세스 시작 지연 중 24개 actor 시험이 시간 초과로 실패했다. 제한 시간을 늘리거나 검사를 삭제하지 않고 제한된 병렬도로 재검증했다.
- Ubuntu 24.04 private PID/cgroup namespace에서 새 actor 묶음을 실행하다 SIGKILL을 재현했다. 해당 컨테이너 memory.events의 oom/oom_kill은 0이고 Docker OOMKilled도 false였다. 진단용 신호 인터포저로 외부 kill의 argv와 실제 `kill(-1, 9)` 호출을 확인했다. 숫자 그룹 ID 직접 신호로 변경한 뒤 같은 필수 복구 15개가 통과했다. macOS에서도 소유 그룹 종료와 독립 그룹 생존 시험 1개를 통과했다. 진단 컨테이너는 종료 후 제거했다.
- 마지막 그룹 신호 수정 후 macOS의 actor/실제 daemon E2E/workflow 125개를 다시 실행해 모두 통과했다. E2E에는 필수 실패의 Lead 대체 후 인수, 수정 소진 뒤 모든 provider Exec 종료 확인이라는 새 2개 시나리오가 포함된다. 실제 데몬·SQLite·Git·프로토콜 child를 사용하며 모델 답변은 fixture다.
- UI/DOM/i18n 112개·18개 파일 통과. 최종 리뷰 중단 문구와 배너 번역 후 관련 UI/i18n 29개를 재검증했다. 사유 업로드·응답 불명 시 동일 요청 재시도·업로드 중 후보 변경 차단·다른 미션/후보/종료 상태 보호를 포함한다. TypeScript는 src와 새 브라우저 시나리오를 함께 검사해 통과했다.
- Playwright 4개 통과: 700px/200% 확대에서 실패 작업과 Lead 연결·원래 retry 차단·대기 Lead 모델 변경·리뷰 한도 안내·사유 입력/저장·저장된 해제 근거 표시를 검사했다. 스크린샷으로 입력과 버튼을 직접 확인했다. 브라우저는 mock IPC이며 실제 데몬의 리뷰 결정 해제/인수 전이는 별도 actor 시험으로 검증한다.
- 문서 14개·로컬 링크 36개·참조 사례 40개와 SQL 제약/rollback 검사를 통과했다. 이번 범위 41개 경로의 tracked diff 및 untracked 19개 파일 whitespace 검사도 통과했다. 다른 terminal/session/UI 변경은 보존했다. 설치된 유료 provider 추론·Windows 복구·나머지 실행/검증 강제 경계를 완료로 판정하지 않는다.


### 검증 명령 영속 실행·로그 저장 복구 이후 검증 (2026-09-16)

- 최종 Linux 전체 `cargo test --offline -p iyagi-termd -p term-platform -p term-core -p term-storage -p term-contracts -- --test-threads=2`는 61개 test/doc 바이너리에서 953개 통과·0개 실패·기존 7개 ignored로 종료했다. 마지막 제품 코드와 추가한 취소/바이트 충돌 검사까지 포함한다. 이는 Linux 실행 증거이며 중단한 macOS 전체 회귀를 대체해 그 플랫폼까지 전체 통과했다고 표시하지 않는다.
- 최종 macOS 집중 검사 9개와 실제 daemon IPC 시나리오 13개가 통과했다. 새 검사는 고정된 명령과 단일 Exec, contract 저장 전 미실행, 변경된 argv/program/cwd/소유 ID/자원 정책 거절, timeout·취소 정리, Exited 저장 실패 중 예약 유지, 로그 begin/chunk/commit 및 Verification 결과 저장 장애를 확인한다. 결과 저장 중 취소는 Cancelled로 기록하며 명령 실행 횟수는 1회다. 업로드 재시도는 변경된 바이트와 committed 범위를 넘는 replay를 거절하고 rename 후 DB 장애에도 같은 본문을 재검증한다.
- Ubuntu 24.04 private PID/cgroup namespace에서도 같은 최종 집중 검사 9개와 실제 daemon 시나리오 13개가 통과했다. `IYAGI_CGROUP_REQUIRE_DELEGATION=1`로 native 검사의 생략을 막았다. 데몬만 SIGKILL한 뒤 기존 검증 프로세스 생존, 새 daemon의 취소 전 무신호, 원래 native identity에 의한 종료, Unknown 결과와 실행 횟수 보존을 두 OS에서 확인했다. provider 답변은 deterministic protocol fixture이며 유료 모델 추론의 증거가 아니다.
- macOS 전체 회귀는 host syspolicyd의 약 285% CPU 사용과 일반 프로세스 시작 지연 중 actor 시간 초과가 발생해 중단했다. 테스트 제한 시간을 늘리지 않았고 이 실행을 통과로 집계하지 않는다. 호스트 지연 해소 후 위 집중/daemon 검사를 순서대로 다시 실행했다. 종료 후 해당 actor/guardian 프로세스가 남지 않은 것도 확인했다.
- 미션·i18n 단위/DOM 112개와 TypeScript 검사를 통과했다. 앱 내 사용법과 한국어·영어 가이드에 검증 실행·자원 예약·취소·저장 재시도·재시작 처리 및 미지원 강제 경계를 반영했다. 명세 문서 14개·로컬 링크 36개·참조 사례 40개·SQL 제약/rollback과 변경 범위 18개 경로의 whitespace 검사를 통과했다.


### 전달 불명 지시의 명시적 대체 이후 검증 (2026-09-16)

- 최종 Linux 전체 `cargo test --offline -p iyagi-termd -p term-platform -p term-core -p term-storage -p term-contracts -- --test-threads=2`는 61개 test/doc 바이너리에서 960개 통과·0개 실패·기존 7개 ignored로 종료했다. Ubuntu 24.04 private PID/cgroup namespace에서 native delegation을 필수로 실행했다. 해당 컨테이너는 종료 후 제거됐다.
- macOS 메시지 actor 검사 16개와 실제 daemon IPC 시나리오 14개, contract/export 검사 261개가 통과했다. Unknown 원본 불변성, 원자적 successor/outbox 저장, 동시 요청의 단일 후속 메시지, 저장 장애 rollback, 같은 UUID 재생, 이전 fingerprint 호환성, 수신자/본문/결정 답변 경계와 재시작 후 새 지시 문맥을 확인했다.
- 새 daemon 시나리오는 실제 SQLite·Git·native gate·Codex protocol child에서 첫 steer의 응답 손실을 재현한다. 같은 요청 재생은 새 메시지를 만들지 않고, 명시적 후속 지시는 같은 실행에 한 번 저장·전달돼 정확한 새 본문으로 결과를 만든다. 이는 fixture 프로토콜 증거이며 설치된 CLI나 유료 provider 추론의 증거가 아니다.
- 미션·mock client·i18n 단위/DOM 137개, 마지막 안내 문구 변경 후 집중 검사 24개, 소스와 새 브라우저 시나리오를 포함한 strict TypeScript 검사를 통과했다. 응답 손실 후 본문·UUID·payload 유지, 확인된 CAS 거절 후 새 revision, 업로드 중 다른 후속 메시지 생성 차단을 포함한다.
- 700px·200% 확대 브라우저 시나리오 2개가 통과했다. 실제 mock artifact 업로드, 원본 Unknown/새 메시지 Queued 유지, 두 메시지 사이 포커스 이동, 버튼 가시성과 수평 넘침을 검사하고 스크린샷을 직접 검토했다. 사용 가이드와 앱 안내를 갱신했으며 명세 및 변경 범위 21개 경로의 whitespace 검사를 통과했다.


### 일반 입력창의 저장 응답 복구 이후 검증 (2026-09-16)

- 최종 미션·i18n·mock client·실제 bridge 단위/DOM 검사 182개, 22개 파일이 통과했다. 새 입력 복구 검사 14개는 저장 후 응답 손실과 화면 재마운트, 종료 상태에서 같은 요청의 결과 확인, CAS 거절 뒤 수정, StorageUnavailable 동안 UUID/revision 보존, 업로드 응답 손실, 업로드 중 작업 선택/종료, 수신자별 초안 분리와 동시 입력창 잠금을 확인한다. 정상 메시지가 하나만 저장되고 원래 요청 전체가 유지되는지 실제 mock 원장과 대조했다.
- 소스와 새 브라우저 시나리오를 포함한 strict TypeScript 검사를 통과했다. source tsconfig를 상속한 임시 검사 설정에 절대 경로의 `vite/client` 타입을 사용했으며 제품 설정을 완화하지 않았다.
- 최종 Playwright 7개가 통과했다. 기존 IME 1개, 명시적 대체 2개, 새 일반 입력의 Lead/Task × 700px/200% 확대 4개를 함께 검사했다. 결과 확인 전 화면을 닫았다 열어도 같은 본문과 요청을 유지하며, 기존 메시지 불변성과 정확히 하나의 추가 메시지를 확인했다. 최초 시험은 harness가 미리 만든 목표 메시지까지 새 전송 수에 포함해 실패했으므로 기존 메시지 집합과 새 메시지를 구분하도록 고쳤다.
- 브라우저 스크린샷에서 버튼·안내문·줄바꿈을 검토했다. 응답 불명에 기존 `전송 실패` 문구가 적용되는 것을 확인해 `저장 응답 확인 필요`로 수정했고, 확대 화면에서도 안내문과 버튼이 함께 보이는 검사를 추가한 뒤 위 최종 회귀를 통과했다. 이 브라우저 증거는 mock daemon이며 provider 전달/추론 증거가 아니다.
- 사용 가이드 두 언어, 앱 안내, UI 명세와 구현 현황을 갱신했다. 명세 14개 문서·36개 로컬 링크·40개 사례·SQL 제약/rollback, 이번 누적 범위 28개 경로의 tracked diff 및 untracked 15개 파일 whitespace 검사를 통과했다. 이 단계는 Rust 제품 코드를 변경하지 않았으며 앞 절의 Linux 960개와 macOS 메시지 16개/실제 daemon 14개 결과를 유지한다. UI 앱 재시작 복원과 전체 목표의 남은 경로는 완료로 표시하지 않는다.


### UI 전송 원장·프로세스 재시작 복원 이후 검증 (2026-09-16)

- 최종 미션·i18n·mock client·실제 bridge 단위/DOM 검사 216개, 24개 파일이 통과했다. 새 원장 codec/본문 검사 24개와 복구 수명 검사 10개를 포함한다. 기존 입력 시험도 초기 비동기 복구가 끝난 뒤 입력하도록 갱신하고 이전 전송·IME·수신자·revision 단언을 유지했다.
- 복구 수명 검사는 일반/대체 지시의 UI 상태 재생성, 저장 전 대기/실패와 미전송, 성공 또는 서버 거절 후 기록 삭제 실패, 원래 UUID/revision 재생, 다른 창의 기존 요청 우선 복원과 새 초안 보존, 손상 기록 재조회, 늦은 응답 차단을 확인한다. 완료된 미션의 거절 결과가 후속 작업 버튼에 가려지는 문제도 고쳐 오류·입력 본문·후속 작업 동선을 함께 검증했다.
- 본문 검사는 여러 페이지로 나뉜 Unicode, 정확한 총 길이/SHA-256/UTF-8, 잘못된 cursor·정수·초과/미완료 페이지를 다룬다. 잘못된 해시의 본문은 표시하지 않으며 기존 요청 참조는 유지한다. 원장 version·scope·추가 필드·범위 오류는 복원과 전송을 거절한다. 초기 저장소/정리 실패 검사는 주입 저장소를 사용하며 아래 브라우저 시험은 실제 IndexedDB를 사용한다.
- 최종 Playwright 21개가 통과했다. Chromium과 WebKit 각각에서 Lead/Task/명시적 대체 × 700px/200% 확대의 브라우저 프로세스를 완전히 종료하고 같은 독립 프로필로 다시 실행하는 12개, 두 엔진의 별도 창 간 원자적 저장·다른 요청 삭제 거절 2개, 기존 IME/대체/응답 손실 7개다. 복원 자체에는 mutation이 없고 결과 확인에는 원래 params가 그대로 쓰이며 추가 업로드·메시지·daemon 원장 변경이 없음을 대조했다.
- 재시작 브라우저 시험의 daemon 데이터는 test driver가 보존한 MockDaemonClient fixture이며, 제품의 복구 기록은 실제 디스크의 IndexedDB다. 실제 Tauri 앱 패키지/OS 전체, 전원 손실, 유료 CLI 추론을 검증한 것으로 확대하지 않는다. 생성한 브라우저 프로세스를 닫고 임시 프로필이 0개 남았음을 확인했다.
- 최종 소스와 새 브라우저 시나리오를 포함한 strict TypeScript 검사, 문서 14개·로컬 링크 36개·참조 사례 40개·SQL 제약/rollback 및 이번 17개 경로의 tracked diff와 untracked 12개 파일 whitespace 검사가 통과했다. Chromium/WebKit의 복원 본문·수신자·버튼과 확대 화면의 안내문을 스크린샷으로 검토했다. 두 언어 가이드·앱 안내·계약·구현 현황을 갱신했다. Rust 제품 코드는 이번 단계에서 바꾸지 않았으며 이전 단계의 Linux/daemon 결과를 새 검증으로 중복 집계하지 않는다.


### 통합 입력 고정·Git 설정 독립성 이후 검증 (2026-09-16)

- 최종 macOS `cargo test -p iyagi-termd --lib --test mission_actor --test mission_e2e --test mission_workspace --test mission_workflow -- --test-threads=2`: 331개 통과, 0개 실패, 기존 1개 ignored. 라이브러리 167개·actor 119개·실제 daemon IPC 14개·workflow 11개·workspace 20개이며 전체 저장소의 모든 테스트를 실행한 수치로 확대하지 않는다.
- 새 통합 검사 9개는 이동/삭제된 private ref, 실제 Git replacement object, 뒤쪽 source의 잘못된 tree·symbolic/missing/non-commit OID·중복, source별 repair base, dirty/다른 base workspace, 외부 diff/textconv·prefix·강제 색상·변경 표시 문자 설정을 다룬다. 잘못된 source 전에 첫 patch가 적용되지 않고 사용자 checkout/index/기존 refs가 보존되는 것을 확인했다. 일반 Git 읽기에는 replacement가 실제로 반영되는 것도 별도로 대조했다.
- 서비스 검사는 저장된 Candidate의 커밋과 실행 출처 사용, manifest의 정확한 OID, 후보 누락/다른 실행으로 재표시/빈 실행 출처/중복의 worktree 생성 전 거절과 무변경을 확인한다. 기존 충돌 시험은 전체 입력과 적용된 입력의 OID가 질문 artifact에 남는지 추가 검증했다. 색상 설정의 ANSI patch를 임시 저장소에서 재현한 뒤 출력 형식을 고정하고 위 최종 회귀를 다시 통과했다.
- 명세 문서 14개·로컬 링크 37개·참조 사례 40개·SQL 제약/rollback, 변경 범위 9개 파일의 whitespace 검사가 통과했다. 사용 가이드 두 언어와 workspace 명세·구현 현황을 갱신했다. daemon 검사에서 모델은 프로토콜 fixture이며 실제 유료 CLI 추론·Windows/Tauri 앱 검증 증거는 아니다. 통합 subprocess 영속 Exec와 충돌 복구의 남은 범위를 완료로 표시하지 않는다.


### 통합 helper의 영속 실행 연결 이후 검증 (2026-09-16)

- macOS 중간 전체 회귀 `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts -- --test-threads=2`는 59개 test/doc 바이너리에서 907개 통과·0개 실패·기존 7개 ignored로 종료했다. 이후 충돌 상태 반영과 일시정지 정리를 보강했으므로 최종 변경의 증거는 다음 집중 검사와 구분한다.
- 최종 `mission_actor` 130개·`mission_e2e` 15개·`mission_workflow` 11개·`mission_workspace` 20개, 총 176개가 통과했다. 통합 Exec 집중 검사 11개는 실제 Git·SQLite·native gate에서 단일 실행 연결, 입력 변조 거절, admission 대기 취소, Git 실행 중 취소·시간 초과, 새 Run/workspace의 명시적 재시도, 일시정지 후 재개, 충돌 실패와 단일 질문, 종료 기록·후보 저장 장애 및 복구 계약 변조 거절을 확인한다.
- 새 daemon 시나리오는 통합 Git hook 실행 중 실제 데몬을 강제 종료한다. 재시작 후 같은 Exec 소유권으로 프로세스를 관찰하고 명시적 취소에 따라 종료 기록을 저장하며, 기존 Run을 자동 재실행하거나 후보로 채택하지 않는지 확인한다. 모델은 프로토콜 fixture이며 설치된 유료 CLI의 실제 추론 증거가 아니다.
- 최종 미션·i18n·mock client·bridge 단위/DOM 221개, 25개 파일이 통과했다. 자동 통합의 한국어·영어 안내, 모델/지시 입력 미제공, 취소 후 명시적 재시도와 열린 충돌 결정 중 재시도 차단을 포함한다. 소스와 새 브라우저 시나리오를 포함한 strict TypeScript 검사도 통과했다.
- Playwright 4개가 통과했다. 자동 통합과 기존 종료 확인 복구를 700px·200% 확대에서 검사했다. 최종 자동 통합 스크린샷을 직접 검토했고 안내·버튼 가시성, 수평 넘침 없음, 재시도 후 이전 Run 불변성을 확인했다. plain browser의 mock daemon 검사이며 Tauri 앱 검증은 아니다.
- 사용 가이드 두 언어와 workspace 명세·구현 현황을 갱신했다. 명세 문서 14개·로컬 링크 37개·참조 사례 40개·SQL 제약/rollback 및 변경 범위 32개 경로의 whitespace 검사를 통과했다. integrator 사전 배정과 같은 Task/workspace의 충돌 해결 완료, Windows 복구 및 OS 강제 경계는 미완료로 유지한다.


### Integrator 배정·충돌 해결·후속 통합 이후 검증 (2026-09-16)

- macOS 중간 전체 회귀 `cargo test -p iyagi-termd -p term-core -p term-storage -p term-contracts -- --test-threads=2`는 59개 test/doc 바이너리에서 916개 통과·0개 실패·기존 7개 ignored로 종료했다. 이후 재시도와 literal path 검사를 추가한 집중 회귀는 `mission_actor` 134개·`mission_e2e` 16개·`mission_workflow` 11개·`mission_workspace` 21개, 총 182개가 통과했다. 마지막 symlink 경로 검사와 실제 재배정 증거는 아래 최종 통합 검사로 구분한다.
- 최종 통합 Exec 집중 검사 15개가 통과했다. 같은 Task/workspace의 자동→Integrator→자동 실행, 실패 Run 불변, 실제 binding 재배정과 같은 요청 재생, 일시정지 중 결정 저장, 결정/작업 변경의 DB 장애 rollback, 남은 세 번째 입력 적용, 두 번의 연속 충돌, 새 후보 검증·리뷰·인수, 미해결 결과의 명시적 Integrator 재시도, scope 위반과 상위 symlink의 읽기 거절을 확인했다. 외부 파일과 사용자 checkout은 그대로이며 종료 후 예약 원장이 비어 있다.
- 추가 core 검사는 미래 Integrator 모델의 cap이 가득 차도 자동 실행은 모델 슬롯을 쓰지 않고, Resolving 실행은 같은 cap을 적용하는지 확인했다. 실제 Git 검사는 Unicode·따옴표·탭·개행·뒤쪽 공백이 포함된 충돌 파일 경로를 정확히 보존한다. 새 daemon IPC 시나리오는 실제 Exec·Git·SQLite와 Codex 프로토콜 자식에서 충돌 해결 후 검증·리뷰·인수를 완료한다. 최초 시나리오 실행은 오래된 term-fixture 바이너리를 사용해 실패했으며 명시적 fixture 빌드 후 통과했다. 설치된 유료 CLI 추론의 증거로 확대하지 않는다.
- 최종 미션·i18n·mock client·bridge 단위/DOM 검사 225개, 25개 파일이 통과했다. Integrator 기본/명시 배정 저장, 과거 자동 Run의 모델 없음 표시, 열린 충돌 중 일반 재시도 차단과 재배정/결정 RPC를 포함한다. 소스와 U24/U25 브라우저 코드를 포함한 strict TypeScript 검사를 통과했다.
- 팀 생성 4개와 기존 자동 통합 2개 브라우저 시나리오가 통과했으며, 최종 충돌 UI 2개를 700px·200% 확대에서 다시 통과했다. 최종 스크린샷에서 한국어 배너·실제 충돌 파일 목록·해결 버튼·수평 넘침 없음을 검토했다. 모델 배정 후에도 이전 Run이 바뀌지 않고 결정은 새 Resolving 상태로 이어지는지 확인했다. 브라우저는 mock daemon 검사이며 실제 Tauri 앱·OS 전체의 증거가 아니다.
- 사용 가이드 두 언어, 앱 내 안내, workspace/실행 계약과 구현 현황을 갱신했다. 명세 14개 문서·37개 로컬 링크·40개 참조 사례·SQL 제약/rollback이 통과했다. 새 Task 타입은 Rust 원본에서 재생성했다. 해결 도중 Unknown/격리된 workspace 복구, 특수 파일·후보 제외/재계획과 OS별 강제 경계는 미완료로 유지한다.


### 불명 통합 재구성·명시적 영향 확인 이후 검증 (2026-09-16)

- 최종 macOS daemon 집중 회귀는 `mission_actor` 134개·`mission_e2e` 18개·`mission_workflow` 11개·`mission_workspace` 21개, 총 184개가 통과했다. 계약·core·storage의 21개 test/doc 바이너리에서도 393개가 통과했다. 두 묶음 모두 실패와 ignored는 0개다. 마지막으로 기본 인수 차단 시험을 Unknown/Interrupted 및 종료 ref 유무 조합으로 보강한 뒤 workflow 11개를 다시 통과했다. 전체 저장소 또는 모든 OS의 검사 수치로 확대하지 않는다.
- 새 실제 daemon 시나리오 2개는 Integrator 해결 중과 자동 이어가기 중 데몬을 강제 종료한다. 재시작 뒤 자식의 생존과 예약 유지, native 종료 기록 이후 복구 결정, 원래 입력의 새 workspace 재구성, 새 충돌 해결·검증·리뷰·인수까지 확인했다. 기존 Run과 격리 파일은 그대로이며 불명 실행의 파일은 최종 후보에 포함되지 않는다. 실제 SQLite·Git·native 프로세스와 프로토콜 fixture를 사용했으며 유료 모델 추론은 검증하지 않았다.
- 복구 결정/Task 저장 실패는 원자적으로 rollback되고 같은 요청으로 재시도된다. 인수는 명시적 확인 누락·중복 ID·변경된 Exec 종료 증거·활성 Task에서 거절된다. Accepted 이벤트 저장 장애 후 미션 상태를 보존하고 같은 UUID 재시도에서 한 번만 완료한다. 이벤트가 가리키는 불변 artifact의 기존 Run·종료 proof·대체 Run 참조를 실제 저장소와 대조했다. 필드 생략/null의 기존 요청 직렬화도 유지된다.
- 최종 미션·i18n·mock client·bridge 단위/DOM 230개, 25개 파일과 소스 및 U17/U24/U25/U26의 strict TypeScript 검사가 통과했다. 종료 증거 없는 확인 차단, proof/후보 변경 시 확인 무효화, 언어별 복구 안내와 선택한 실행 ID의 전송을 검사했다.
- Playwright 6개가 통과했다. 일반 종료 복구·통합 재구성·불명 실행 확인 후 인수를 각각 700px와 200% 확대에서 검사했다. 새 재구성과 인수 스크린샷 4개에서 안내·checkbox·버튼 가시성과 줄바꿈을 직접 검토했다. 브라우저는 mock daemon이며 실제 인수 증거 검사는 위 native 시나리오로 구분한다.
- 한국어·영어 가이드와 실행·workspace·UI·wire 계약을 갱신했다. 명세 14개 문서·37개 로컬 링크·40개 참조 사례·SQL 제약/rollback을 통과했다. 이번 범위 27개 경로의 tracked diff와 untracked 10개 파일 whitespace 검사도 통과했다. Windows native 복구·설치된 CLI/인증 조합·특수 파일·나머지 phase와 OS 강제 경계는 미완료로 유지한다.


### 필수 작업 취소·명시적 재시도·대체 계획 이후 검증 (2026-09-16)

- macOS daemon 회귀 `mission_actor` 141개·`mission_dispatch` 27개·`mission_e2e` 18개·`mission_workflow` 11개, 총 197개가 통과했다. 이후 workspace 소유권과 시작 전 취소 증거를 보강하고 최종 취소 관련 actor 23개 및 검증 Exec 10개를 통과했다. 최종 제품 변경의 근거는 후자의 집중 검사로 구분한다. 별도 `term-core` 전체 57개도 통과했다. 모두 실패/ignored는 0개이며 저장소 전체 또는 모든 OS 검사로 확대하지 않는다.
- 새 취소 시나리오 7개는 실제 SQLite/Git과 주입 provider를 사용한다. 필수 writer 취소 후 actor 재생성·독립 작업 진행·단계 보류, 명시적 retry의 멱등성과 시도 보존, 대체 계획 저장 장애 rollback과 같은 요청 재생, Lead 메시지로 대체 후 인수, 검증/리뷰 취소의 자동 재생성 차단, 선택 작업 대기/취소와 지나간 단계 retry 거절을 확인했다. 종료/Exec 누락·Unknown·active owner·workspace lease가 남은 상태에서 retry와 retirement를 거절한다. 이 검사의 Exec 종료 경계는 영속 레코드를 주입한 증거다.
- 별도 실제 native 검증 명령은 실행 중 취소 후 종료 저장을 기다리고, 명시적 retry에서 새 Run/Exec/workspace를 사용한다. 명령 호출 수가 두 번이며 이전 Cancelled 검증·Run은 그대로이고 새 검증은 Passed다. 기존 통합 자원 대기 취소 시험에서 Exec 없이 MayHaveSent인 기록을 발견했다. 실행기의 시작 전 취소를 RequestNotSubmitted 증거로 전달하고 이를 저장하도록 수정한 뒤, 실행 전 취소와 실제 그룹 종료·재시도 시험을 다시 통과했다. 오류 문자열이나 Exec 부재만으로 종료를 허용하지 않는다.
- 최종 미션·i18n·mock client·bridge 단위/DOM 234개, 25개 파일이 통과했다. 소스와 U17/U24/U25/U26/U27 브라우저 코드의 strict TypeScript도 통과했다. 중간 UI 재실행 한 번은 timeout과 뒤이은 DOM 실패 11개가 발생했다. 제품 timeout을 변경하지 않고 worker 2개로 같은 전체 범위를 재실행해 통과했으며, 시간 지연의 호스트 원인은 확정하지 않는다.
- 최종 Playwright 4개가 통과했다. 필수 취소와 기존 자동 통합 취소를 700px·200% 확대에서 검사했다. 안내·재시도 버튼의 가시성/줄바꿈·수평 넘침 없음과 이전 Run/요구사항 보존을 확인했다. 브라우저는 mock daemon이며 새 provider/OS 호환성 증거가 아니다.
- 두 언어 사용 가이드와 실행·workspace·UI 명세, 구현 현황을 갱신했다. 문서 14개·로컬 링크 37개·참조 사례 40개·SQL 제약/rollback 및 변경 범위 26개 경로의 tracked diff/untracked 15개 파일 whitespace 검사를 통과했다. 후보 제외·통합 입력 재계획, Windows 복구, 실제 CLI/인증 조합과 나머지 OS 강제 경계는 남은 범위로 유지한다.


### 충돌 후보 제외·필수 대체 계획 이후 검증 (2026-09-16)

- macOS daemon 회귀는 mission_actor 145개·mission_dispatch 27개·mission_e2e 18개·mission_workflow 11개·mission_workspace 21개, 총 222개가 통과했다. 계약·core의 별도 8개 test/doc 바이너리에서 322개가 통과했다. 실패/ignored는 0개다. 마지막 Lead 지시 문구를 대체 연결과 non-Plan 작업에 맞춘 뒤 후보 제외 집중 3개를 다시 통과했다. 전체 저장소·모든 OS 검증 수치로 확대하지 않는다.
- 후보 제외 검사는 실제 Git·SQLite·native 통합/검증 Exec와 주입 provider를 사용했다. 필수 후보와 의존 작업의 대체, 선택 후보/의존 작업 생략 및 빈 계획, 이전 성공 작업·Run·후보·workspace 보존, 제외한 파일이 없는 새 후보, 검증·리뷰·인수까지 확인했다. 일시정지 중 실행 보류, actor 재생성 후 복원, 결정/새 Plan 저장 장애 rollback과 같은 요청 재생도 통과했다.
- 잘못된 계획의 필수 대체 누락·선택 작업으로 변경·제외 성공 이력에 대한 의존·성공 작업 retirement를 거절했다. 대체가 다시 교체된 경우 원래 요구사항을 유지하는 연결을 허용한다. 변경된 현재 입력, 종료 Exec 누락, 소진된 수정 예산은 결정 저장 없이 거절한다. core 검사는 제외 이력의 coverage 배제·선택 작업만 제외한 빈 계획·ordinal 및 작업 수 한도 보존을 확인했다. 모든 다중 제외/repair-base 조합을 실제 시나리오로 검증했다는 주장은 하지 않는다.
- 최종 미션·i18n·mock client·bridge 단위/DOM 236개, 25개 파일과 소스/U17/U24/U25/U26/U27/U28 strict TypeScript가 통과했다. 브라우저의 기존 충돌 해결 2개와 새 후보 제외 2개를 통과했다. 새 시나리오 최초 실행은 fixture에 Lead binding이 없어 거절됐다. fixture 배정을 수정한 뒤 700px·200% 확대에서 계획 전환, 이전 Run/요구사항 보존, 버튼 가시성과 수평 넘침 없음을 확인하고 스크린샷을 직접 검토했다. mock daemon 화면 검사이며 실제 Tauri 앱의 증거는 아니다.
- 두 언어 사용 가이드·앱 내 안내·engine/workspace/UI 명세와 구현 현황을 갱신했다. 명세 14개 문서·37개 로컬 링크·40개 참조 사례·SQL 제약/rollback 및 변경 범위 24개 경로의 whitespace 검사를 통과했다. 실제 유료 CLI/인증 조합, Windows 복구, OS 강제 경계와 나머지 명세 경로는 미완료로 유지한다.


### 수정 사이클의 후보 제외·검증 순서 이후 검증 (2026-09-16)

- 최종 macOS 회귀는 mission_actor 147개·mission_dispatch 27개·mission_e2e 18개·mission_workflow 11개·mission_workspace 21개, 총 224개가 통과했다. 실패/ignored는 0개다. 실제 daemon IPC·native 실행·중단/재시작 회귀도 포함하지만 설치된 유료 모델 추론이나 Windows/Tauri 앱 검증으로 확대하지 않는다.
- 새 실제 Git/SQLite/native 통합·검증 시나리오 2개를 추가했다. 수정 전 연속 제외는 통과했으나 이전 통합 결과의 입력 제외 후 통합 단계가 ResultInvalid로 중단되는 것을 재현했다. 단계별 대체 조건을 수정한 뒤 후보 제외 집중 5개가 통과했다. 제공자 모델은 주입 fixture이며 유료 CLI 호환성의 증거는 아니다.
- 첫 시나리오는 후보를 두 번 연속 제외하고 최종 대체가 원래 작업과 중간 대체 작업의 replacement_of 연결을 충족한 뒤 검증·리뷰·인수되는 것을 확인했다. manifest에 두 제외 결정이 남고 이전 Run은 바뀌지 않는다.
- 두 번째 시나리오는 충돌 해결→검증→중요 리뷰 finding→이전 후보 기반 수정→원래 입력 제외를 수행한다. 제외 집합에 수정 작업과 과거 Verify/Review가 포함되는지 확인했다. 일시정지 중 결정과 계획을 저장하고 actor를 재생성한 뒤, 새 통합 후보 revision/supersedes 유지·새 검증·새 리뷰·인수까지 완료했다. 이전 성공 Task/Run과 finding은 그대로 보존됐다.
- 필수 Verify/Review 누락, 일반 구현 작업으로 변경, 원래 검증 명령 삭제의 5가지 계획을 원자적으로 거절했다. 최종 대체 검증과 리뷰에 각각 과거 후보의 증거를 연결하면 인수를 거절하고 미션 상태를 보존한다. 증거를 복구한 뒤 정상 인수된다. 검증에는 해당 Task/Run의 성공 연결을, 리뷰에는 해당 Task의 결과 artifact와 정확한 candidate ID를 요구한다.
- 한국어·영어 가이드와 engine/workspace 명세를 갱신했다. 명세 14개 문서·37개 로컬 링크·40개 참조 사례·SQL 제약/rollback, 이번 범위 11개 경로의 whitespace와 새 파일/제외 모듈의 rustfmt 검사를 통과했다. 별도로 검사한 기존 generated AgentSessionListParams.ts에는 이번 변경과 무관한 trailing whitespace 2개가 남아 있어 수정하지 않았다. UI·wire·DB schema는 이번 단계에서 변경하지 않았다.


### 설치 조회·관측 저장 이후 검증 (2026-09-16)

- 최종 macOS daemon 회귀는 lib 171개·mission_actor 147개·mission_binding_probe 3개·mission_claude 9개·mission_codex 15개·mission_dispatch 27개·mission_e2e 18개·mission_opencode 24개·mission_rpc 9개, 총 423개가 통과했다. 실패 0개·명시 실행용 ignored 4개다. 별도 term-contracts/term-core 검사 323개도 실패/ignored 없이 통과했다. 전체 저장소나 모든 OS의 검증 수치로 확대하지 않는다.
- 실제 shell fixture로 stdout/stderr 버전, 비정상 exit, 없는 파일, 잘못된 버전, 출력 초과, timeout, root 종료 후 파이프를 유지하는 자식과 소유 자식 종료를 검사했다. 초기 150ms 시험 한도가 macOS의 새 script 실행 지연보다 짧아 실패했다. 시험 한도를 2초로 조정하고 통과했으며 제품 한도는 3초다.
- 실제 SQLite 서비스 검사는 성공 관측 영속화·재시작 복원, CLI 버전 변경과 prerelease 증거 무효화, 실패 후 과거 버전 제거, 사용자 enabled 보존, 대상 변경 무효화, 같은 binding.save 요청 재생, stale client 관측 차단과 조회 중 동시 수정의 CAS 충돌을 확인했다.
- 설치된 Codex 0.154.0·Claude 2.1.272·OpenCode 1.18.30에 공통 helper의 `--version`만 수행하는 smoke 1개를 별도 실행해 통과했다. 인증·모델 이용 권한·유료 추론 호환성 검증은 아니다. 이전 Claude 2.1.271의 인증 초기화 증거를 새 버전으로 승격하지 않는다.
- UI 단위/DOM 238개(25개 파일), 기존 연결 화면과 새 설치 확인 Playwright 8개, source 및 U07/U29를 포함한 TypeScript 검사가 통과했다. 두 언어 상태 안내, 미저장 보호, 서버 revision 사용, 실패 후 버전 제거를 검사했다. 700px와 200% 확대의 오류 안내·설정 배치를 스크린샷으로 확인했다. plain browser/mock daemon 증거이며 Tauri 앱 실행 증거는 아니다.
- 실제 새 installation.rs를 참조하는 독립 Rust crate에서 Windows MSVC와 Linux GNU target의 타입 검사를 통과했다. 최초 독립 crate의 Tokio macros feature 누락을 수정한 후 통과했다. 전체 Windows daemon cross-check는 SQLite C 빌드에서 Windows SDK의 stdlib.h가 없어 중단됐다. 전체 Windows 빌드·실제 Job Object 실행은 미검증으로 유지한다.
- 문서 14개·로컬 링크 37개·참조 사례 40개와 SQL 제약/rollback 검사가 통과했다. 이번 범위 28개 경로의 tracked diff 및 untracked 9개 파일 whitespace 검사도 통과했다. 설치 조회 보강을 전체 오케스트레이션 완료나 출시 판정으로 취급하지 않는다.


### 서버 소유 설치 관측·실행 snapshot 검증 이후 검증 (2026-09-16)

- 최종 macOS Rust 회귀는 lib 172개·mission_actor 147개·mission_binding_probe 6개·mission_dispatch 28개·mission_e2e 18개·mission_rpc 9개, 총 380개가 통과했다. 실패 0개·명시 실행용 ignored 2개다. 이번 단계에서는 UI·설치된 CLI 추론·다른 OS 실행을 다시 검사하지 않았으며 이전 결과를 새 검사로 합산하지 않는다.
- 새 서비스 검사는 실제 SQLite와 CLI shell fixture를 사용한다. 새 연결의 위조 버전/확인 시각/지원 flag, 서버 비공개 필드 복사, 다른 연결 ID/프로그램, 같은 요청 재생, 레거시 public 기록과 다른 OS 관측을 검사했다. 지원 flag만 DB에 바꿔도 현재 registry가 다시 계산하며 DB UPDATE 장애에서 새 관측이 일부 저장되지 않는 것을 확인했다.
- 실행 준비 단위 검사는 증거 없는 Run snapshot의 주장 거절, 유효한 서버 관측의 허용, 다른 버전·확인 시각·모델·프로그램 거절과 서버 registry 철회 후 지원 제거를 확인했다. 원래 저장 문서와 이력은 그대로다. dispatch 서비스 검사는 레거시 client capability를 새 Run snapshot에 승격하지 않는지 확인했다.
- 기존 IPC 메시지 시나리오 2개는 시험 코드의 daemon library와 server-side fixture evidence를 사용하도록 수정한 뒤 active steer 및 응답 불명 지시의 명시적 대체를 통과했다. 클라이언트의 steer=true만 저장했을 때는 false이고, 시험 서버의 설치 관측 이후에만 true인지 추가 확인한다. 실제 IPC·영속 Exec·native gate·프로토콜 자식은 유지하되, 설치된 CLI의 실제 호환성 증거는 아니다. 나머지 16개 E2E는 기존 daemon binary를 사용한다.
- 새 embedded harness는 최초에 복구 예약 초기화를 호출하지 않아 실행 시작을 기다리는 시험이 실패했다. production과 같은 supervisor.refresh_recovery와 dispatch 허용 확인을 넣은 뒤 집중 검사와 위 최종 회귀를 통과했다. 최초 dispatch 회귀의 메서드 이름 오류도 수정한 뒤 최종 회귀를 수행했다.
- 명세 문서 14개·로컬 링크 37개·참조 사례 40개·SQL 제약/rollback 검사가 통과했다. 이번 범위 14개 경로의 tracked diff와 untracked 8개 파일 whitespace 검사 및 새 Rust 파일의 rustfmt 검사를 통과했다. 역할별 자동 실행 gate·binary 변경 감지와 전체 목표의 나머지 미완료 항목은 그대로 유지한다.

### 역할별 필수 기능·시작 전 버전 재확인 이후 검증 (2026-09-16)

- macOS의 daemon lib 172개·actor 150개·binding probe 6개·dispatch 28개·E2E 19개·RPC 9개, 총 384개 통과·기존 ignored 2개다. core는 단위/통합 60개를 통과했다. 최초 추가 E2E의 snapshot을 wire enum과 다른 JSON 형태로 읽던 시험 오류를 수정하고 E2E/RPC 28개를 재실행했다. 마지막 제품 코드에서 실패한 회귀는 남아 있지 않다.
- 실제 SQLite/Git과 주입 adapter 검사는 미검증 작업의 무예약 대기·새 설치 관측 이후 실행, 읽기만 검증된 연결의 쓰기 계획 거절을 확인했다. 버전 변경·파일 없음·조회 시간 초과에서 adapter.start 호출이 0회이고 이전 Run 버전과 미전송 증거를 보존하며 자동 재시도하지 않는지 검사했다. production daemon의 실제 IPC 시험은 클라이언트가 지원 flag와 버전을 위조해도 시작을 거절하고 Draft·Run/Exec 없음·시작 횟수 0·원래 Git HEAD를 유지하는지 확인했다.
- 프로토콜 fixture의 전체 흐름·재시작은 별도 시험 daemon에서 서버 증거를 제공한다. production daemon은 같은 주입을 허용하지 않는다. 실제 provider 추론·쓰기 sandbox·Windows 실행 호환성을 증명하는 시험은 아니다.
- 미션 단위/DOM 193개, 소스와 새 브라우저 시나리오 TypeScript 검사가 통과했다. 브라우저 설치 확인/호환 모델 선택 4개는 700px와 200% 확대에서 미검증 선택 차단·호환 모델 재배정·과거 Run 보존·수평 넘침 없음을 확인했고 스크린샷도 직접 검토했다. 브라우저는 mock daemon이다. 사용 가이드와 앱 내 안내에 전체 쓰기 팀의 실제 호환성 미검증 상태를 명시했다.
- 마지막 앱 내 안내 변경 후 설정·생성·i18n 집중 검사 29개를 다시 통과했다. 명세 문서 14개·로컬 링크 37개·참조 사례 40개·SQL 제약/rollback과 이번 범위 40개 경로(미추적 22개 포함)의 whitespace 검사도 통과했다. 전체 목표의 미완료 항목과 실제 CLI/OS 호환성 검증은 남아 있다.


### 실제 Codex 구독·sandbox·조회 일관성 이후 검증 (2026-09-16)

- 실제 macOS/aarch64 Codex 0.154.0/openai/관리 구독/gpt-5.6-luna의 production adapter 세 사례가 통과했다. 읽기 구조화 결과, 승인한 소유 파일 생성, 즉시 취소의 RPC 수락·interrupted 종료와 각 native 정리/선택 모델 일치를 확인했다. 기록은 [고정 digest 증거](CODEX_MACOS_01540.md)에 보존했다. 실제 SQLite 전체 모델 미션을 수행했다는 뜻은 아니다.
- 모델·인증 호출 없는 command/exec sandbox 검사 12개가 통과했다. 구독 설정·인증 목적지·스키마·조기 취소의 실제 실패를 진단해 수정했다. 최신 인증 설정의 실제 API metadata smoke 1개도 통과했다. 더미 키와 ephemeral 인증 저장소만 사용하며 API 모델 추론은 하지 않았다.
- 회귀 중 기존 fixture binary와 새 취소 adapter가 섞인 실행은 소유한 test process만 종료하고 최종 fixture를 다시 빌드했다. 그 실행을 통과로 기록하지 않는다. 이후 실제 daemon 재시작 E2E 1개에서 Run과 복구 결정의 조회 시점 혼합을 발견했다. 읽기 트랜잭션 회귀는 수정 전 revision 불일치로 확실하게 실패했고 수정 후 storage 전체 75개가 통과했다. 테스트의 종료 증거 조건을 완화하지 않았다.
- 설정/생성/지원 capability/configuration/i18n 검사 35개와 소스 TypeScript가 통과했다. 추가된 안내가 화면 전체 문자열 assertion에 걸린 테스트는 실제 인증/endpoint 입력란의 부재를 확인하도록 수정했다.
- 최종 daemon 회귀는 중복 실행을 제외해 473개가 통과했다: lib 174, actor 150, binding probe 6, Claude 9, Codex 17, dispatch 28, E2E 19, Exec 37, OpenCode 24, RPC 9. 일반 실행의 명시적 환경/실모델 검사는 ignored로 유지한다. 조회 수정 후 actor 150·dispatch 28·E2E 19·RPC 9를 다시 통과했으며 최초 실패한 continuation 재시작도 포함한다.
- 명세 검사(문서 14개·로컬 링크 42개·참조 사례 40개·SQL 제약/rollback), Python 구문 검사, 이번 범위 29개 경로의 tracked diff 및 untracked 14개 파일 whitespace 검사를 통과했다. 중단된 과거 회귀의 관찰자 1개는 PID/명령/시험 로그 소유권과 소켓 경로 제거를 확인한 뒤 정리했다. 릴리스 gate는 그대로 유지하고 전체 목표의 남은 항목을 완료로 표시하지 않는다.


### 실제 Codex 전체 미션·승인 상세 이후 검증 (2026-09-16)

- 설치된 Codex의 production daemon 전체 미션 1개가 통과했다. 정확한 macOS/aarch64·0.154.0·openai·구독·gpt-5.6-luna 조합이며 실제 모델 실행 네 건, 자동 Git 통합과 Python 검증, 독립 리뷰, 인수의 동일 요청 재생을 확인했다. 원래 checkout 보존과 모든 Exec 종료/슬롯 해제를 확인했다. [고정 결과와 범위](CODEX_MACOS_MISSION_01540.md). 일반 회귀에서는 명시 실행용 ignored다.
- 이번 범위의 daemon 회귀는 중복을 제외해 472개가 통과했다: lib 178, actor 151, Claude 9, Codex 17, dispatch 28, E2E 19, Exec 37, OpenCode 24, RPC 9. 전체 저장소·모든 OS의 검증 수치가 아니다. actor는 기존 150개 통과 뒤 새 저장 시험의 기대값을 수정해 해당 1개를 다시 통과했다. 코드는 nonterminal 이벤트를 저장해도 반환값이 false이며, 테스트는 재조회한 SQLite 모델 값·revision 불변성으로 저장과 거절을 판단한다.
- 추가한 ModelObserved 때문에 terminal만 기다리던 인증 테스트 helper와 승인/steer fixture의 사전 이벤트 가정을 수정했다. 기록된 model_mismatch는 이미 thread/start부터 불일치하므로 최초 불일치와 정상 시작 후 reroute를 별도로 검사한다. 두 경로 모두 MODEL_UNAVAILABLE, 결과 미채택, 관측 보존과 turn/start 전송 여부를 확인한다. Plan/Review/Question/Blocked의 새 envelope와 inactive payload·null 배열·unknown/잘못된 ID를 검사했다.
- 기존 single-thread 시험이 사전 이벤트 assertion으로 panic한 뒤 종료를 기다리며 멈춰 소유한 test process와 사라진 임시 socket의 관찰자만 식별해 정리했다. 시험은 별도 runtime worker로 정리가 진행되도록 바꾸고 승인/steer 동작과 전체 Exec 37개를 다시 통과했다. 해당 중단 실행은 통과로 집계하지 않았다. 동시에 수정된 터미널 코드의 AtomicBool import 누락 한 곳만 보완한 뒤 daemon/fixture를 재빌드했다. 다른 터미널 변경의 전체 검증을 주장하지 않는다.
- 미션·i18n 단위/DOM 194개와 소스 및 새 브라우저 시나리오를 포함한 strict TypeScript 검사가 통과했다. 파일별 변경 내용·이동 경로·root 권한·HTML 비실행·상세 누락 시 승인 차단을 검사했다. 700px·200% 확대 Playwright 2개가 통과했고 최종 스크린샷에서 긴 경로/내용 줄바꿈, 버튼 가시성과 수평 넘침 없음을 확인했다. 처음 시각 확인에서 배너에 내부 승인 JSON이 남는 것을 찾아 공통 파서로 수정하고 다시 검사했다. mock daemon/plain browser 증거이며 실제 Tauri 앱 패키지 시험이 아니다.
- 최신 재빌드 뒤 production daemon의 목표→인수 protocol 시나리오 1개도 다시 통과했다. 위 472개에 중복 집계하지 않았다. 명세 문서 14개·로컬 링크 48개·참조 사례 40개·SQL 제약/rollback과 범위 33개 경로(미추적 15개 포함)의 whitespace 검사가 통과했다. 두 언어 가이드·adapter/UI 계약·실제 증거 문서를 갱신했다. 다른 CLI/OS/인증, 실제 모델의 장애·재시작, executable identity 및 릴리스 검증은 완료로 표시하지 않는다.


### CLI 파일 지문·담당 작업 결과 제한 이후 검증 (2026-09-16)

- 파일 지문 변경의 macOS 회귀는 lib 188개·actor 151개·binding probe 7개·dispatch 28개·E2E 19개·RPC 9개, 중복 제외 402개를 통과했다. 첫 actor 묶음은 150개 통과·통합 timeout 1개 실패였고 실패 사례는 단독 재실행에서 통과했다. 500ms 제한에서 Git hook 시작 파일이 없던 실패였으므로 자식 종료 자체를 검증할 수 없었다. 시험 예산만 3초로 바꾸고 15초 hook 대기·시작 marker·BudgetExceeded·Exited·workspace 보존·예약 해제 assertion은 유지했다. 제품 timeout 정책은 변경하지 않았다.
- 설치된 Codex 0.154.0(8,790 bytes), Claude 2.1.272(210,702,192 bytes), OpenCode 1.18.30(144,272,354 bytes)에 실제 파일 검사·버전 조회·예상 지문 재대조를 수행해 1개 opt-in smoke를 통과했다. 최초 debug 빌드의 Claude 해시 계산은 3초를 초과했다. sha2만 dev/test에서 최적화하고 동일한 예산으로 다시 통과했다. 로그인/권한/추론 호환성을 증명하는 시험이 아니다.
- 같은 버전·길이의 다른 내용, 같은 내용의 다른 symlink 대상, 상대 경로·디렉터리·과대 파일·FIFO·만료 예산, 버전 조회 중 자기 변경, SQLite 재개방, 지문 없는 과거 관측, 같은 checked_at의 새 관측과 과거 Run 분리를 검사했다. 바뀐 파일은 --version도 호출하지 않는 marker와 새 설치 확인 후 새 Run만 통과하는 결과를 확인했다.
- 설정·생성·capability·i18n 단위/DOM 33개와 소스 및 U29 strict TypeScript, 700px·200% 확대 브라우저 2개가 통과했다. 스크린샷의 안내 줄바꿈과 수평 넘침 없음을 검토했다. 최초 브라우저 검사의 전체 화면 버전 문자열 assertion은 사용법에 적힌 지원 버전까지 읽고 있었다. 모델 카드로 대상을 좁혀 다시 통과했다. 브라우저는 mock daemon이다.
- 이후 실제 미션에서 발견한 Builder 결과 종류 문제를 수정하고 최종 lib 189개·Claude 9개·Codex 17개·E2E 19개·OpenCode 24개, 258개를 다시 통과했다. 위 회귀와 중복 집계하지 않는다. 최초 실제 재시험의 ResultInvalid와 정리 기록, 수정 후 75.53초에 성공한 전체 미션 기록을 각각 보존했다. 새 설치 관측 format 2/revision 2와 네 Run의 binding을 실제 SQLite에서 대조했다. [실패·수정·최종 성공 증거](CODEX_MACOS_MISSION_01540.md). 다른 CLI의 실제 모델 추론이나 전체 목표 완료로 확대하지 않는다.
- timeout 시험 예산 보강 후 실제 Git/native 통합 집중 20개를 worker 2개로 다시 통과했다. 최종 명세 문서 14개·로컬 링크 49개·참조 사례 40개·SQL 제약/rollback, 이번 범위 19개 경로(미추적 13개 포함)의 whitespace 검사가 통과했다. 지정 entrypoint 밖의 실행 이미지 고정, 다른 OS·CLI·인증, 릴리스 검증은 미완료로 유지한다.

### 검증 OS 권한·원본 후보 입력 이후 검증 (2026-09-16)

- 최종 코드의 macOS daemon 회귀는 중복을 제외해 267개가 통과했다: lib 195, 검증 actor 12, E2E 19, RPC 9, workflow 11, workspace 21. 최초 lib 묶음은 194개 통과·설치 probe 1개 시간 초과였고, 동일 코드·동일 2초 제한으로 실패한 사례를 단독 재실행해 통과했다. 제품이나 테스트의 시간 제한을 늘리지 않았다. 실제 모델 및 설치 파일 smoke의 일반 ignored 설정도 유지했다.
- 실제 Seatbelt 시험은 후보/호스트 파일 쓰기·삭제·이동·hardlink·symlink 우회·자식 쓰기와 TCP/UDP/Unix socket을 거절하고 별도 출력 및 명시적으로 허용한 연결을 확인했다. 실제 Git 시험은 hook·필수 clean/smudge/process filter·검증 worktree에만 활성화되는 조건부 filter·줄바꿈 변환을 실행하지 않는 원본 생성과 변경 바이트·실행 비트·누락·ignored 파일·외부 링크 거절을 확인했다. 조건부 설정 시험의 최초 실패는 gitdir 패턴 끝 슬래시로 설정이 활성화되지 않은 fixture 오류였다. 패턴을 수정하고 해당 worktree에서 필터 설정이 실제 조회되는 assertion을 유지한 뒤 통과했다.
- actor 회귀는 strict policy에서 Enforced 증거만으로 인수, 외부에서 입력을 바꾼 경우 exit 0이어도 Failed/Unknown, 환경 값/출력/후보/네트워크 계약 변조 거절, 저장 장애 중 재실행 없음, 취소·시간 초과·재시도와 예약 해제를 확인했다. production daemon E2E에는 강제 종료 후 복구도 포함하며 fixture 제공자와 실제 Git/native Exec의 범위다.
- 설치된 Codex 0.154.0/openai/구독/gpt-5.6-luna의 실제 미션도 `require_enforced_verification=true`로 60.63초에 인수까지 통과했다. 검증 Passed/Enforced, 네 모델 요청/관측 일치, 원래 checkout 보존과 모든 Exec 종료/슬롯 해제를 확인했다. 이 실제 모델 실행 이후 추가한 raw blob 직접 생성 경로는 위 최종 회귀로 검증했으며 실제 추론 기록과 구분한다. [보존 결과와 정확한 범위](CODEX_MACOS_MISSION_01540.md).
- 앱 내 한국어·영어 사용법의 오래된 비강제 안내를 수정하고 출력 폴더·OS 지원 범위를 설명했다. 생성·설정·인수 화면과 i18n 36개가 통과했다. React act 환경 경고가 있었지만 실패는 없었다. 실제 Tauri 앱 조작이나 다른 OS/CLI/인증 조합, 릴리스 완료의 근거로 확대하지 않는다.
- 명세 문서 14개·로컬 링크 50개·참조 사례 40개·SQL 제약/rollback, 이번 범위 18개 경로(미추적 10개 포함)의 whitespace 검사와 새 검증 Rust 모듈의 rustfmt 검사를 통과했다. 기존 공유 작업 공간의 다른 변경과 사용자 데몬은 보존했다. 호스트 읽기 격리·출력 디스크 한도·다른 OS backend·준비용 Git 자원 경계와 전체 목표의 나머지 경로는 미완료로 유지한다.


### Git 저장소 식별·상속 환경 경계 이후 검증 (2026-09-16)

- 같은 저장소의 linked worktree/하위 경로/symlink가 별도 ID를 얻는 문제를 먼저 세 개의 실패 시험으로 재현했다. 수정 후 새 등록 시험 5개가 통과했다. 별도 Storage writer 두 개의 동시 등록, legacy 단일 ID 보존·CAS 보완, 중복 common_dir 거절, 기존 common_dir 변경/제거의 무변경 거절, 모호한 과거 ID 보존을 확인했다.
- 실제 production daemon에 다른 소유 저장소의 GIT_DIR/GIT_WORK_TREE/GIT_COMMON_DIR/GIT_INDEX_FILE을 주입한 시험은 수정 전 선택 경로 대신 다른 경로를 반환해 실패했다. Git 공통 실행 생성기에서 저장소 선택 환경을 제거한 뒤 선택 경로·HEAD·draft의 root/ID와 다른 저장소 보존을 확인했다. 생성 후 .git 연결을 교체한 경우 시작을 InvalidState로 막는 assertion도 추가했다. 모든 저장소와 데몬은 시험용 임시 경로다.
- 이번 변경 범위의 Rust 회귀는 중복 제외 320개가 통과했다: term-storage 75, 새 repository 5, RPC 10, E2E 19, mission_workspace 21, workspace lib 9, actor 153, dispatch 28. 마지막 Git 연결 교체 assertion을 추가한 RPC 한 사례도 별도로 다시 통과했다. 일반 E2E 회귀의 모델은 protocol fixture이며 아래 추가 실모델 시험과 구분한다.
- 한국어/영어 가이드·workspace 계약·구현 현황을 갱신했다. 명세 문서 14개·로컬 링크 50개·참조 사례 40개·SQL 제약/rollback과 이번 9개 경로의 whitespace 검사를 통과했다. 모호한 legacy 저장소 ID를 병합하는 UI와 다른 OS/CLI의 실제 호환성·나머지 격리/복구·릴리스 항목은 완료로 판정하지 않는다.
- 저장소 식별 보강 이후 실제 Codex 전체 미션 1개도 별도로 통과했다. macOS/aarch64·0.154.0·openai·구독·gpt-5.6-luna, enforced verification 필수 조건에서 계획·두 Builder·통합·검증·독립 리뷰·인수·동일 요청 재생을 확인했다. 요청/관측 모델 일치·원래 checkout 보존·모든 Exec 종료/슬롯 해제가 true다. [실모델 재검증 증거](CODEX_MACOS_MISSION_01540.md)에 결과와 digest를 추가했다. 이 증거 파일과 보고서를 포함한 이번 최종 범위는 11개 경로다.
- 실모델 기록 추가 후 최종 명세 검사는 문서 14개·로컬 링크 51개·참조 사례 40개·SQL 제약/rollback이 통과했고, 최종 11개 경로의 whitespace 검사도 통과했다.

### 첫 설정·생성 흐름 단순화 (2026-09-17, 미검증)

- 읽기 전용 `runtime.detect`를 추가했다. codex·claude·opencode의 절대 경로(데몬 PATH와 사용자 설치 디렉터리), 로컬 `--version`, 로그인 흔적(존재 여부만, keyring/auto 저장소 codex는 unknown), 설정된 기본 모델, 이 OS·아키텍처·버전에 실제 증거가 있을 때만 고정 모델, 시작 gate와 같은 함수로 계산한 역할을 반환한다. 모델을 알 수 없으면 역할은 빈 목록이다. Tauri 브리지 허용 목록에 `runtime.detect`와 누락돼 있던 `repository.inspect`를 추가했다.
- 설정 → AI 작업 첫 카드에 빠른 설정을 두었다. `이 모델로 팀 만들기`는 binding 재사용/저장 → probe → 네 역할 capability 확인 → 네 역할 template 재사용/저장이며 capability gate를 우회하지 않는다. 기존 모델 폼은 `직접 입력(고급)`으로 접고, 팀에 `모든 역할에 같은 모델`, 검증 명령 인수에 공백 구분 입력을 추가했다.
- 새 AI 작업: pane 오른쪽 클릭 `이 폴더에서 AI 작업…`과 상단 `새 AI 작업` 버튼·팔레트·네이티브 메뉴가 초점 pane의 project ?? cwd를 채우고 자동 inspect한다. 첫 template 자동 선택, 팀이 없거나 역할이 부족하면 대화상자 안 빠른 설정, 완료 조건 선택 사항(비우면 human check 조건 1개), 실행 설정 접힘·요약, 시간 제한 분 단위(1–120), 제출 비활성 사유와 binding 조회 실패 재시도를 표시한다.
- 네 역할·Lead 시작·리뷰 후 인수라는 엔진 규칙은 바꾸지 않았다. Builder 하나만 쓰는 단일 에이전트 모드는 인수 규칙 변경이 필요한 별도 결정으로 남긴다.
- Rust·TS 단위/dom 테스트와 Playwright 스펙(u02 빠른 설정 경로, u07·u29 선택자)을 작성·수정했지만 **빌드·타입체크·테스트를 실행하지 않았다.** `src/generated`의 새 타입 3개는 ts-rs 형식에 맞춰 손으로 작성했으므로 `verify:contracts`로 재생성 결과와 비교해야 한다. 이 항목은 검증 완료 근거가 아니다.

### 실행·결과·복구 사용성 전면 개선 (2026-09-18, 미검증)

- 사용자 결정: 용어 "할 일·확정·결과", 미검증 CLI는 연결별 명시 동의 시 실험적 실행 허용(어댑터 미구현 기능은 계속 차단, 결과에 표시), 리뷰 생략 옵션(기본은 리뷰 포함), 종료 증거 없는 실행은 사용자 확인 기록 후 정리, 작업 공간은 사용자가 누를 때만 정리.
- 데몬: `Mission.semantic_revision`으로 시간·활동 기록 커밋 사이의 사용자 mutation CAS 완화, 확정 대기 중 활성 시간 제외, 오류 reason_code 표(01 §7)와 결정 option id·`decision_answer` 형식(01 §8), 에이전트 Blocked 결과를 결정으로 전환, 결정 자유 입력(answer_ref)을 다음 실행 문맥에 전달, `adjust_limits`, `RepositoryInspectResult.verification_supported`, `Binding.experimental_version`과 어댑터 구현 기능 표(03 §6), `runtime.detect`의 grade·experimental_roles, 리뷰 생략 시 필수 역할·통합 충돌 선택지, `mission.run.attest_exited`(user_attested), `workspace.usage`/`workspace.cleanup`(04 §8 보존 정책), `follow_up_of`(이전 확정 결과 commit을 base로, 후속 작업은 dirty checkout 허용). 브리지 허용 목록에 새 RPC와 누락됐던 `repository.inspect`를 추가했다.
- 화면: 오류 문장·행동 버튼 공용화, REVISION_CONFLICT 재동기화 후 1회 재시도, 결정 패널(한국어 선택지·계획 목록·자유 입력·모델 변경·한도 편집기), 실행 상세(내부 값 라벨화, 로그 따라가기, notice 한 줄, 비활성 사유, 멈춘 실행 직접 확인), 결과 화면(manifest `entries` 파싱 수정, 확정 체크리스트, 직접 확인 체크, 결과 가져오기 명령·터미널 열기, 버리기, 검증 로그, 작업 공간 정리 제안), 작업 목록 모달·보관 해제, 상태 우선 탭 배지, 알림 센터 연동, 팔레트 검색어·단축키 `Cmd/Ctrl+Shift+M`, 프로덕션 빌드에서 프로토콜 없으면 진입점 숨김, 결과 자동 전환, 연결 끊김/동기화 지연 구분, 실패 요약 카드, 후속·같은 목표 새 작업, draft 시작·삭제, 생성 중복 방지, base_changed 재생성, 검증 미지원 OS 표시, 실험적 연결 동의·배지, 앱 내 사용법 5줄, 사용자 가이드 짧은 문서/상세 문서 분리.
- Rust·TS 단위/dom 테스트와 Playwright 스펙을 작성·수정했지만 **빌드·타입체크·clippy·테스트를 실행하지 않았다.** `src/generated` 변경은 손으로 작성했으므로 `verify:contracts`로 재생성 결과와 대조해야 한다. 스크린샷 기준 이미지는 갱신하지 않았다. 이 항목은 검증 완료 근거가 아니다.

### 로컬 호환성 증거와 신뢰 등급 (O22)

- 2026-09-19, 이 항목은 문서 전용 작업분이다(코드는 직접 작성하지 않았다). [11-local-evidence.md](11-local-evidence.md)에 새 계약을 쓰고 그 위에서 이 문서 묶음([ORCHESTRATION_SPEC.md](../../ORCHESTRATION_SPEC.md), [01-contracts.md](01-contracts.md), [03-adapters.md §6](03-adapters.md#6-capability登録と権限), [05-ui.md](05-ui.md), [07-tickets.md](07-tickets.md), [contracts.ts](contracts.ts), 사용자 가이드 4종)를 갱신했다. `crates/term-contracts`·`crates/iyagi-termd`·`src/generated`·`src/features/missions`의 실제 구현은 같은 시점에 별도로 진행돼(예: `agent_runtime/local_probe.rs`, `mission/run_evidence.rs`, `src/generated/{CompatibilityGrade,LocalEvidence,LocalProbeReport,LocalRunEvidence}.ts` 신설, `Binding.ts`의 `local_evidence` 추가) 이 작업 범위 밖이며 직접 작성·리뷰·실행하지 않았다.
- 증거 층 분리: 출시 fixture 하나가 정확한 버전만 인정하던 것을 출시(`null`)/같은 라인(`version_line`, 이 버전의 로컬 프로토콜 진단 통과가 조건)/로컬 자가 진단(`local_probe`)/실행 관측 승격(`observed_runs`)/사용자 동의(`experimental_opt_in`) 다섯 층으로 나누고 `Support.reason_code`로 구분했다(11 §2–§4).
- 런타임별 로컬 자가 진단을 정의했다: 모델 호출·인증 토큰 없이 Codex는 app-server handshake+`model/list`+12개 sandbox 경계 사례, Claude는 `--help` 플래그 존재, OpenCode는 소유 서버 health+경로+provider 조회로 프로토콜·격리·모델 목록 존재를 증명한다(11 §6). 성공 실행 횟수가 임계치를 넘으면 `observed_runs`로 추가 승격한다(11 §4). 두 층이 출시 증거 없이 4역할을 통과시키면 새 등급 `CompatibilityGrade::VerifiedLocally`다(11 §3.2, §7).
- 실험적 연결 동의를 연결당 한 번으로 바꿨다: CLI 버전이 바뀌어도 재동의를 요구하지 않고(`experimental_version_mismatch` 분기 제거), 자가 진단이 실패로 증명한 기능은 동의로도 열리지 않는다(11 §3.4, §7).
- UI 계약에 연결별 신뢰 칩(`출시 검증됨`/`이 PC에서 확인됨`/`실험적`/`미확인`) + 근거 한 줄, `설치 확인` 버튼 문구 `지금 확인`(자가 진단 포함), 재동의 UI 제거, 필수 기능 기준 `실험적 연결` 배지(`isExperimentalBinding`)를 반영했다(11 §8).
- **검증 결과(2026-09-19, macOS arm64).** `cargo check --workspace --all-targets`에서 O22가 만진 모든 타깃이 통과했다: iyagi-termd lib(유닛 테스트 포함)·bins·통합 테스트(runtime_detect·binding_probe·actor·codex·opencode·e2e·rpc·workflow·dispatch·agent_detect_smoke), term-contracts·term-core·term-storage 테스트 타깃. `cargo test -p term-contracts`(291 통과)로 ts-rs가 `src/generated`를 재생성했고 손으로 쓴 미러(`Binding.ts`·`CompatibilityGrade.ts`·`LocalEvidence.ts`·`LocalProbeReport.ts`·`LocalRunEvidence.ts`)와 바이트 단위로 같은 형태가 나왔다(`npm run verify:contracts`의 `git diff --exit-code`는 다른 작업의 미커밋 생성 파일 때문에 쓰지 않았다). 프론트는 `tsc --noEmit` 통과, vitest `src/features/missions`·`daemon/mockClient.test.ts`·`i18n` 619건 중 O22 관련은 모두 통과했다. 읽기 전용 리뷰 3건으로 잡은 것: `runtime.detect`의 `'static` 수명 위반, `RunDetail.dom.test.tsx` 닫는 괄호, OpenCode 프로브 시간 상한 누락, detect가 12초 예산을 그대로 넘기던 것(→ `DETECT_BUDGET` 2초), 재동의를 철회로 오판하던 `consent_withdrawn`, 네트워크 sandbox 판정의 로케일 의존 — 모두 수정했다.
- **O22와 무관하게 HEAD에서 이미 실패하던 것(그대로 둠).** Rust: `tests/mission_claude.rs`·`tests/support/claude_auth.rs`가 `ClaudeAdapterConfig.secrets_root`(secret-store 이전 커밋) 이후 갱신되지 않았고 참조하는 `claude/fixtures/streams/*.iyagi.jsonl`이 저장소에 없다. 프론트 vitest 4건: `Costs.dom.test.tsx`(예상 비용 저장), `MissionCreate.dom.test.tsx`(후속 작업 base), `MissionSettings.dom.test.tsx` 2건(추론 강도 저장, OpenCode 다른 모델 목록) — HEAD worktree에서 같은 4건이 실패함을 확인했다. 한 줄 수정으로 풀린 기존 컴파일 오류 2건(`claude/auth_tests.rs`의 `build_launch_plan` 경로, `term-pty/src/flow.rs`의 test 가변 차용)과 `MissionSettings.tsx`의 기존 tsc 오류 2건(미사용 상수, `hintKey` null 좁힘)은 고쳤다.
- 동의 철회는 `experimental_consent_withdrawn`(CAPABILITY_UNSUPPORTED) reason_code로 거절한다(`experimental_version_mismatch` 대체; 01 §reason_code 표·11 §3.4). UI 근거 줄은 자가 진단을 "안 함"(runs만 있음)과 "실패"(`local_probe_failed`)로 구분해 후자는 `프로토콜 확인 실패`로 표시한다.
- `python3 docs/orchestration/verify_spec.py`만 실행해 문서 링크·07-tickets.md의 O01–O21 존재·06-verification.md의 E/W/U 사례·SQL 제약을 검사했고 통과했다(`git diff --check`도 통과). 이 검사는 문서 정합성만 확인하며 코드나 UI 동작을 검증하지 않는다.
- 이 PC(macOS)에 설치된 실제 Codex 0.155.0·Claude 2.1.276·OpenCode 1.18.30으로 로컬 자가 진단·실행 관측을 아직 한 번도 실행하지 않았다. 라이브 probe 실행과 결과 확인은 남은 작업이다.
