# 09. 구현 보충 규칙과 예제

[명세 입구](../../ORCHESTRATION_SPEC.md) · [컴파일 가능한 DTO 예제](contracts.examples.ts)

이 문서는 구현 중 빈칸을 추측하지 않도록 저장·호출·화면 연결의 세부 규칙을 고정한다. 다음 명령은 참조 DTO와 예제의 TypeScript 정합성만 검사한다.

```sh
./node_modules/.bin/tsc --noEmit --strict --target ES2022 --module ESNext --skipLibCheck docs/orchestration/contracts.ts docs/orchestration/contracts.examples.ts
```

## 1. 최초 설정부터 실행까지

1. native credential UI에서 필요한 계정을 연결한다. API key 입력은 Tauri/native secret store로 직접 전달한다. React의 persisted store/localStorage/analytics에는 저장하지 않는다.
2. credential 값 자체 대신 opaque credential_ref를 binding.save에 넣는다. daemon이 같은 사용자 secret store에서 resolve 가능한지 probe한다. 불가능하면 AUTH_REQUIRED이며 raw credential을 RPC로 우회 전송하지 않는다.
3. runtime/provider/model/auth route/resource policy를 가진 Binding을 저장하고 probe한다. unsupported capability는 이유를 표시한다.
4. template에 lead/builder/reviewer binding과 초기 Policy를 저장한다. verifier resource policy는 기존 default LaunchPolicy(observe,2GiB,1slot)를 사용한다.
5. 새 작업 form에서 목표와 requirements, 검증 명령을 작성한다. 최초 command는 client가 UUID를 미리 배정해 requirement.verification_ids에 넣을 수 있다.
6. mission.create는 draft이므로 아직 등록되지 않은 command ID를 허용한다. 단 UUID/중복/참조 형태는 검증한다. 생성한 repository_id를 조회한 뒤 verification.save로 해당 명령을 등록한다.
7. mission.control(start)는 모든 command가 해당 repository에 실제 등록되어 있고 policy allowlist 안에 있는지 검사한다. 누락하면 INVALID_ARGUMENT과 해당 ID를 반환하고 draft를 유지한다.

endpoint_ref는 daemon 관리 endpoint registry를 참조한다. O1 preset은 공식 provider endpoint만 제공한다. custom endpoint 입력 UI는 O21까지 노출하지 않는다. endpoint 주소가 secret을 포함할 경우 저장을 거절한다. endpoint의 실제 값은 binding의 진단 화면에 hostname만 표시한다.

## 2. Request와 core/service 구분

```text
UI event handler
  -> typed client call(request_id 유지)
  -> existing bridge control connection
  -> mission service: read snapshot + duplicate request lookup
  -> core validate/reduce
  -> artifact 준비가 필요하면 별도 I/O 후 hash/ref 생성
  -> storage writer: expected_revision CAS + projections/event/request/outbox
  -> response
  -> outbox actor: 별도 start/send/cancel
```

UI는 실행 여부를 예측해 RUNNING 상태를 만들지 않는다. 클릭 중 표시만 로컬로 유지하고 영속 state는 snapshot에서 받는다. 실패한 client Promise가 daemon에서의 실행 실패를 뜻하지 않는다.

자동 내부 전이에는 user expected_revision을 사용하지 않고 service가 읽은 revision에 CAS한다. 충돌하면 최신 snapshot으로 core를 다시 계산한다. 같은 자동 동작은 `mission-id/task-id/attempt/operation` dedupe_key를 사용해 intent 중복을 막는다.

## 3. Exec 저장과 자원 원장

Run과 Exec는 1:0..1이다. provider가 remote session만 제공하면 Exec가 null일 수 있지만 O1 로컬 runtime들은 run별 소유 process를 가진다. ExecRecord는 `orch_execs`에 저장하고 snapshot에는 Entity(kind=exec)로 포함한다. process/OS resource의 실제 상세 지표는 기존 telemetry read 경로를 재사용하며 매 sample마다 mission revision을 올리지 않는다.

Exec 시작 순서: 입장 reservation → prepared Exec 저장 → WAITING launch helper 생성·소유 그룹 배정 → ProcessIdentity/그룹 저장 → release gate → target과 adapter protocol 시작. 플랫폼에 따른 gate 불가 사유는 기존 prefer/require 의미를 따른다. pipe stdout을 처리하기 전에 bounded spool을 등록한다.

exec cleanup과 원장 release는 finalized guard로 한 번만 수행한다. Run final event가 와도 소유 process가 계속 쓰거나 child cleanup이 끝나지 않으면 Exec를 exited로 만들지 않는다. daemon 종료 시 mission outbox dispatch를 먼저 막고 Exec 취소를 수행한 뒤 storage를 종료한다.

영속 supervisor는 첫 `refresh_recovery()` 성공 전에는 admission을 열지 않는다. 현재 daemon ID와 다른 소유자의 미종료 Exec를 저장소에서 읽어 원래 memory/CPU reservation을 복원한다. 미션이 종료·보관되어 있거나 Run이 terminal이어도 Exec 종료 증거 없이 제외하지 않는다. 복원된 정책이 현재 한도보다 커도 예약 자체를 버리지 않고 새 입장을 거절한다. 현재 supervisor의 실행은 기존 원장 항목을 유지하며 복원 대상에 중복 포함하지 않는다.

이후 조회는 미종료 집합과 이전에 복원한 ID의 현재 기록을 단일 SQL 스냅샷으로 읽는다. migration 0005의 부분 인덱스는 미종료 Exec를 조회하며, 이미 종료된 전체 이력을 매 tick 읽지 않는다. 열/JSON/Run 연결과 원래 binding resource policy를 대조하고, 조회 한도 초과·누락·변조·읽기 실패에서는 기존 예약을 유지하면서 새 시작을 막는다. 정상 조회가 복구되면 기존 준비 작업부터 이어갈 수 있다. 복구 실패 중에도 현재 실행의 결과 수집·취소·종료 저장은 진행한다.

복원된 예약의 일반 `release()`는 거절한다. 동일한 실행 명세·소유자·자원 정책·기존 process identity를 유지한 `Exec.exited`와 종료 시각을 확인해야 해당 예약을 해제한다. 주기적 host telemetry 갱신만으로 복구 보류를 해제할 수 없다. 이 조회 경로 자체는 OS 신호를 보내지 않는다. 별도 `mission-native-recovery` worker가 아래 native 계약을 수행한다.

Linux cgroup 실행은 RELEASE 전에 `Exec.group_identity = {kind: cgroup_v2, boot_id, kernel_id}`를 저장한다. `kernel_id`는 `name_to_handle_at(AT_EMPTY_PATH)`의 FILEID_KERNFS 64-bit 세대 포함 ID를 16자리 hex로 보존한다. [커널 구현](https://github.com/torvalds/linux/blob/master/fs/kernfs/mount.c)의 file handle 계약을 사용하며 PID나 경로 일치만으로 재소유하지 않는다. 기존 JSON에서 이 필드가 없으면 None으로 읽고 보수적으로 예약을 유지한다.

복구 worker는 한 번에 최대 8개를 순환하며 원본 manifest 해시·Run 연결·이전 owner·자원 정책·현재 Exec 스냅샷을 검증한다. cgroup v2 filesystem과 boot/kernel ID가 일치하는 디렉터리를 FD로 고정하고, 이후 조회·신호는 해당 FD 기준으로 수행한다. 그룹 경로 재사용, 다른 boot, ID 변조, 읽기 오류를 종료 증거로 취급하지 않는다. 단순 관찰 뒤에는 FD를 닫고 취소/미저장 종료에는 최대 64개 핸들을 보유한다. 한도에 걸린 실행은 예약을 유지한 채 대기한다. 그룹이 살아 있으면 관찰하고, 현재 fence의 Cancel intent 또는 MissionStopping이 있을 때만 TERM → monotonic 3초 → force를 적용한다. TERM과 구형 kernel의 force fallback은 pidfd를 연 뒤 멤버십을 재검증하며 bare PID kill로 후퇴하지 않는다. `cgroup.kill`과 `cgroup.events populated`는 하위 cgroup까지 포함한다.

그룹 empty 관측은 이전 owner를 유지한 `Exec.exited`와 관측 시각을 CAS로 저장한다. exit code와 제공자 성공을 추정하지 않으며 Run은 기존 Unknown/Interrupted 이력을 유지한다. 정상 종료와 복구 종료 모두 DB commit 전에는 native 디렉터리를 보존한다. CAS/DB 실패 시 열린 핸들·예약·디렉터리를 유지하며 재시도하고, 재시작하더라도 같은 커널 ID를 다시 검증할 수 있다. commit 뒤에는 최대 1024개/깊이 32 범위에서 빈 하위 cgroup과 원래 그룹을 정리한다. 일반 원장 release가 아니라 다음 일관된 저장소 조회가 복구 예약을 해제한다. 이름만 기준으로 빈 orphan을 지우던 startup sweep은 종료 증거를 잃지 않도록 제거했다.

macOS의 영속 pipe 실행은 `--exec-guardian` 독립 프로세스를 WAITING target helper보다 먼저 시작한다. 감독자는 비공개 0700 디렉터리/0600 Unix socket을 제공하고 `GroupRecoveryIdentity::MacosGuardian`에 endpoint와 감독자의 PID/start_token/boot_id를 저장한다. 모든 연결은 [Darwin LOCAL_PEERPID](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/un.h), peer UID, 실제 시작 시각과 per-boot UUID를 검증한다. Attach는 원래 생성 daemon만 할 수 있으며 한 helper identity로 고정된다. 복구는 그 helper identity와 Exec.identity도 대조한다.

감독자의 관찰 worker는 IPC와 분리되어 daemon 연결 대기·재시작 중에도 기존 observed-tree anchor/자식 관찰을 계속한다. 프로세스 조회는 확인된 ESRCH/zombie와 권한/불완전 응답을 구분한다. 이미 알려진 live anchor가 sysinfo 목록에서 빠져도 버리지 않는다. 종료 관측은 감독자가 durable ack까지 유지하며, 정상 Exec/복구 Exec의 DB exit commit 뒤 Retire를 보낸다. 명시적으로 수신한 empty 상태는 해당 그룹 핸들에 보관해 중복 정리와 ack 손실을 처리한다. 감독자가 사라졌다는 사실만으로 empty를 만들지 않는다. launcher가 강제 종료되어도 supervisor는 살아남지만 출력/provider 세션 재연결을 의미하지는 않는다.

이것은 macOS의 observed_tree/partial 계약을 재시작 사이에도 유지하는 경로다. OS가 강제하는 전체 자식 그룹/메모리 제한으로 바꾸지 않는다. UI는 snapshot Exec를 보존하고 그룹 종류에 따라 상세와 새 시도 결정에 관찰 범위를 표시한다. 원래 Run 결과/작업 공간/불명 외부 효과는 계속 보존한다. Windows Job의 재소유, 그룹 식별자 또는 독립 감독자가 없는 과거 기록, native 경로/감독자 자체가 사라진 실행의 추가 증명과 provider 결과 재조회는 별도 구현 범위다.

Binding.resource_policy는 기존 LaunchPolicy를 Rust에서 그대로 재사용한다. frontend reference의 ResourcePolicy는 문서용 이름이다. binding 저장 시 original LaunchProfile을 수정하지 않으며 과거 terminal profile에 O1 model 필드를 삽입하지 않는다.

결정적 integration은 runtime provider 없이 daemon 소유 helper Exec에서 Git 명령들을 순차 실행한다. helper와 자손은 한 OS 자원 그룹에 둔다. integration Task에는 충돌 해결에 사용할 integrator binding을 배정하지만 결정적 Run.binding_snapshot은 null이다. 충돌 시 그 Run을 failed로 마무리하고 동일 task의 새 Run에서 integrator binding을 사용한다. 기본 팀 설정 화면에서는 integrator를 Builder와 같은 binding으로 제안하고 저장되는 배정을 표시한다. capability 검사는 별도로 수행한다.

daemon 소유 `Task.integration`은 원래 계획 artifact와 Automatic/Resolving/Continuing 단계를 저장한다. planner 입력으로 설정할 수 없다. Resolving은 모델 cap·비용 정책을 적용하고, Automatic/Continuing은 전역 자원과 실행 예산을 사용한다. 해결 보고 뒤 소유 helper가 실제 파일을 검사·capture하고 남은 원래 입력을 적용해야 새 후보가 된다. 후보에는 채택된 해결 Run 출처도 남기며, 새 검증·리뷰를 거친다.

## 4. 정책 검증 범위

Policy 정수의 하한은 1이다. repair cycle은 0을 허용하여 자동 수정 반복을 끌 수 있다. max_cost_usd_micros는 null 또는 양의 U64다. null은 `금액 상한 미설정`으로 표시한다. defaults.policy_ceiling 밖의 값은 INVALID_ARGUMENT이다.

allowed_binding_ids/allowed_roles/allowed_verification_ids는 중복을 제거해 저장하지 말고 중복 입력 자체를 거절한다. role binding의 primary/fallback은 모두 allowlist 안에 있어야 한다. fallback 순서를 보존한다. 다른 auth route의 fallback은 사용자가 명시적으로 설정한 것만 사용 가능하며 default는 비어 있다.

지원되지 않는 hard cost cap을 supported로 표시하지 않는다. unknown_cost=block이면 실행 전 비용 추정/관측을 확보하지 못한 binding을 차단한다. allow_with_notice는 확인 불가 표시 후 다른 start/time/attempt 상한으로 운영한다.

현재 비용 예약은 `Binding.estimated_run_cost_usd_micros`의 null 또는 양의 U64를 사용한다. 이전 binding 문서에서 빠진 필드는 null로 읽는다. dispatch는 기존 Run의 불변 binding snapshot과 사용량을 합산하고 새 예상 금액을 더해 한도를 검사한 뒤, Run과 예약 snapshot을 한 CAS에 저장한다. 제공자 usage 지원 여부나 구독 quota 비율은 실행 전 예상 금액을 대신하지 않는다.

소유 실행은 보고액과 예상액 중 큰 금액을 유지한다. 종료가 확인되면 제공자 보고액으로 정산하고, 최종 보고액이 없으면 기존 예상액 또는 미확인 상태를 유지한다. 미전송 상태로 취소된 예약과 binding이 없는 결정적 검증은 제공자 비용에 포함하지 않는다. 합계는 u128/BigInt로 계산한다. 이미 시작한 실행이 예상액을 넘길 수 있으므로 이 검사를 provider hard cap으로 표현하지 않는다.

차단된 작업은 `cost_unknown` 또는 `cost_limit`과 작업 범위의 nonblocking Budget 결정을 함께 저장하며 Run/attempt를 만들지 않는다. 정책·binding 예상값·실행 결과가 바뀌면 다음 dispatch 검사에서 현재 조건을 만족하는 작업을 Ready로 바꾸고 해당 결정을 obsolete로 만든다. 실제 예약 직전에 비용을 다시 검사한다. 미확인 비용 허용 시에도 알려진 금액의 상한을 지키며, 현재 binding 편집으로 과거 Run을 다시 계산하지 않는다.

제공자의 알려진 제한 관측은 `Run.rate_limit`에 Run/event와 같은 트랜잭션으로 저장한다. migration 0004의 부분 인덱스는 미만료 reset만 조회하도록 돕고 보관된 미션의 이력도 포함한다. index-only DDL도 버전 기록과 같은 transaction에 적용한다. binding ID·runtime·program·provider·model·auth route·credential/endpoint 참조가 일치할 때 제한을 적용하며 label·예상 비용·probe 결과 변경은 범위를 바꾸지 않는다.

같은 연결의 Ready 작업은 `provider_rate_limited`와 `dispatch_after_unix_ms`로 대기한다. 해제 시각이 지나면 Ready로 되돌리고 기존 dispatch의 모든 조건을 다시 확인한다. 대기 중 Run/attempt/decision을 생성하지 않으며 다른 binding은 계속 진행한다. 관측 저장과 최종 dispatch 검사는 같은 guard로 직렬화하고 DB fence/CAS를 유지한다. 실패한 Run을 자동 재전송하는 계약과는 별개이며, 이미 예약된 실행은 해당 관측으로 취소하지 않는다. 절대 provider reset에는 UTC를 사용하고 실행 경과 시간에는 기존 monotonic 계측을 유지한다.

## 5. 메시지·결정의 조건표

| 조건 | 선택한 동작 |
|---|---|
| Lead task 없음, mission running | 새 plan task와 message context 생성 |
| Lead ready task 있음 | 해당 다음 context에 message 추가 |
| active Run + steer 지원 | 현재 turn에 send, ack 전까지 queued |
| active Run + steer 미지원 | 다음 run context로 queue; UI 즉시 전달 표시 금지 |
| task terminal | INVALID_STATE, 후속 mission 안내 |
| mission paused | message 저장만 하고 resume까지 dispatch 안 함 |
| mission stopping/terminal | 새 message 거절 |
| live provider approval | 같은 Run/정확한 request에 answer |
| final question 뒤 provider turn 종료 | task awaiting_input, 답변 뒤 새 attempt |
| answer 이후 provider ack 미확인 | Decision answered 유지, delivery unknown 복구 |

Decision 자체의 answered와 실제 전달 상태를 표시하려면 answer delivery를 Message row에 연결한다. Decision에 `answer_message_id`를 기록하며 answer_ref/선택값으로 daemon이 만든 system message는 원 provider request를 outbox에 binding한다. artifact 본문에 approval token을 넣지 않는다.

## 6. 결과 variant와 후속 동작

| Task kind | 정상 결과 | 다음 처리 |
|---|---|---|
| plan | plan | proposal 검증·적용 또는 plan decision |
| research/design/consult/diagnose | report | 본문/artifact/source 검사, task succeeded |
| implement/test_author/document | patch | 실제 capture→awaiting_review→succeeded |
| integrate | deterministic merge 또는 patch | 통합 candidate 고정 |
| review | review | candidate/finding 참조 확인, repair 또는 인수 대기 |
| verify | 모델 결과 없음 | Exec exit/환경/로그로 Verification 생성 |
| 모든 AI task | question | user decision, 완료로 취급 안 함 |
| 모든 AI task | blocked | 이유에 따라 재계획/decision; 임의 성공 금지 |

review에 findings가 있어도 review task 자체는 검토 산출 완료로 succeeded일 수 있다. mission 인수는 finding resolution을 별도로 검사한다. verify exit 실패는 verify task failed이며 후속 repair 경로가 열려 있으면 mission을 즉시 failed로 만들지 않는다.

## 7. Query 구현 규칙

mission.list는 updated_at DESC,id DESC keyset pagination이다. cursor는 필터/마지막 tuple을 포함하는 opaque token이고 limit≤50이다. 동시 갱신으로 목록 항목이 이동할 수 있으므로 UI는 ID로 중복 제거하고 재접속 시 처음부터 갱신한다. 일관된 상세 화면은 snapshot을 사용한다.

mission.events는 seq ASC이며 limit≤50, frame budget으로 더 줄일 수 있다. 반환 next_after_seq는 실제로 반환한 마지막 seq이며 빈 page면 요청 after_seq다. high_watermark는 해당 read transaction의 최신 seq다. notification은 hint이며 명령 완료의 유일한 증거로 사용하지 않는다.

대화 본문은 Message.body_ref를 artifact.read로 받는다. snapshot에 메시지 본문 전체를 추가하지 않는다. 화면은 메시지 ID별 body cache를 bounded LRU로 관리한다. 기본 본문 cache 상한은 8 MiB이며 초과 시 화면 밖의 본문부터 해제한다. 원 artifact는 보존 정책을 따른다.

## 8. Protocol fixture로 실행·재시작 검증

개발 E2E를 실행하기 전에 `cargo build -p iyagi-termd -p term-fixture`로 production daemon, protocol child, `mission-fixture-daemon`을 빌드한다. fixture daemon은 정확한 시험용 child 경로에만 서버 설치 관측/호환성 증거를 주입하고 production daemon library와 native helper를 사용한다. 재시작 시험도 같은 binary와 데이터 디렉터리로 수행한다. 이 binary는 배포 번들 대상이 아니며 production daemon에는 증거 주입 옵션을 제공하지 않는다. production daemon에서 미확인 실제 CLI 팀의 시작을 거절하는 IPC 시험은 별도로 유지한다.
