# 02. 실행 엔진과 복구

[명세 입구](../../ORCHESTRATION_SPEC.md) · enum/전이: [states.json](states.json).

## 1. 구현 모양

`term-core::mission::reduce(snapshot, command, observed_time) -> Result<Transition, DomainError>`는 순수 함수다. Transition에는 projection 변경, event 설명, side-effect intent가 있다. OS·Git·HTTP·SQLite·현재 시각을 직접 호출하지 않는다. daemon service가 입력을 읽고 core 결과를 동일 writer transaction으로 저장한다. expected_revision race 시 core 입력부터 다시 검증하되 사용자 mutation은 자동 재적용하지 않는다.

run별 actor가 adapter 통신을 직렬화한다. mission command queue도 직렬 처리하되 adapter/파일 I/O를 기다리며 전체 mission lock을 잡지 않는다. callback에는 mission_id/task_id/run_id/fencing_token이 반드시 포함된다. 옛 run/token callback은 state 변경 없이 진단 counter만 증가시킨다.

## 2. Mission 수명

| 전이 | 허용 조건과 동작 |
|---|---|
| draft → running | 목표·binding(Lead·Builder 필수, 독립 리뷰일 때만 Reviewer, Integrator 선택)·깨끗한 Git base(후속 작업은 확정 commit 도달 가능성)·정책 검사; bootstrap plan task 생성 |
| draft → cancelled | 실행 없이 사용자 취소 |
| running → pausing | 새 dispatch 차단; 현재 run은 완료할 수 있음 |
| pausing → paused | live run 0; unknown은 recovery decision으로 남기고 dispatch 금지 |
| paused → running | 사용자 resume; 재검증 후 큐 재개 |
| running/pausing/paused → stopping | 미전송 outbox 취소, live run에 cancel intent, 하위 작업 전파 |
| stopping → cancelled | 모든 실행이 종료 또는 원격 중단이 확인됨 |
| running → completed | phase=awaiting_acceptance, 현재 candidate 인수, 필수 검증/리뷰 통과 |
| running/pausing/stopping → failed | 실패 확정; 더 실행할 수 없고 소유 실행 종료 확인 |

stopping에서 종료 결과가 불명하면 stopping을 유지하고 OUTCOME_UNKNOWN decision을 표시한다. 종료된 척 cancelled로 바꾸지 않는다. stopped process 확인 불가 상태에서 workspace lease를 해제하지 않는다.

개별 필수 작업의 cancel은 실행을 멈추며 원래 요구사항을 면제하지 않는다. cancelled 필수 작업은 재시도해 성공하거나 검증된 대체 계획으로 superseded가 되기 전까지 단계 전이와 인수를 막는다. 취소한 검증/리뷰를 자동 생성하여 사용자 취소를 되돌리지 않는다. 명시적 retry는 같은 실행 단계에서, 미시작 또는 취소 Run의 종료·소유 Exec Exited·workspace Retained/lease 해제를 확인한 뒤 허용한다. 시도·시간·시작·binding 예산은 그대로 적용하고 과거 Run/Exec/workspace는 보존한다. 다음 단계로 넘어간 선택 작업은 Lead 재계획으로 변경한다. 미완료 선택 작업을 통합 시점에 조용히 누락하지 않으며 완료 또는 명시적 취소/대체를 기다린다.

pause는 새로운 실행 시작을 멈추는 동작이다. OS SIGSTOP이나 실행 중인 작업의 즉시 정지를 의미하지 않는다. UI 버튼은 `새 실행 일시정지`, pausing은 `진행 중 실행이 끝나면 일시정지`다. 즉시 중단은 cancel이다.

phase는 현재 orchestration stage다. 기본 순서는 planning → implementing → integrating → validating → reviewing → awaiting_acceptance → done. 읽기 전용 조사 mission도 O1 첫 버전에서는 candidate를 base_oid로 생성하여 같은 인수 루프를 사용한다. 실패 후 수정 계획을 적용하면 implementing으로 돌아갈 수 있다. 사용자 질문은 state나 phase를 강제로 바꾸지 않고 open_decision_count로 표시한다.

terminal mission에는 새 메시지를 전송하지 않는다. UI는 결과에 대한 후속 작업을 **새 mission**으로 만들며 goal에 원 결과 ref를 명시적으로 채택한다. `mission.create.follow_up_of`로 확정 완료 미션을 지정하면 새 base는 그 확정 candidate commit이고, start는 사용자 HEAD 일치와 checkout clean 대신 그 commit의 private ref 도달 가능성을 검사한다([01 §3](01-contracts.md#3-rpc와-전송)). archive는 archived_at만 변경한다.

## 3. Task와 Run 전이 조건

- planned → ready: 모든 depends_on task가 succeeded, 입력 artifact 유효, 필요한 결정 answered, binding/policy 검사 통과.
- planned/ready → blocked: dependency 실패·context 만료·capability/자원 외 정책 차단. 단순 global slot 부족은 ready 유지와 queue reason만 표시.
- ready → running: 새 Run prepared 생성과 outbox start, attempt_count 증가를 같은 transaction에 기록. provider가 실제 시작했다는 뜻은 Run.state에서 별도 표시한다.
- running → awaiting_input: 사용자 결정 또는 provider approval 필요. 나머지 독립 task는 계속된다.
- awaiting_input → running: 유효한 답 전달이 확인됨. 연결 결과 불명이면 그대로 두고 delivery unknown 표시.
- running → awaiting_review: 유효한 patch 결과 수신, 실제 파일 capture·범위 검사 대기.
- awaiting_review → succeeded: 실제 변경 capture·범위 검사·쓰기 종료 확인 완료. Builder가 수행했다고 보고한 local test는 참고 자료이며 최종 검증 통과가 아니다. 필수 command는 통합 candidate에서 verifier가 별도 실행한다.
- running → succeeded: report/plan/review/verify 결과의 타입별 완료 조건 충족.
- running/awaiting_input → blocked: unknown/interrupted run 또는 비자동 retry 사유; active_run_id를 유지하여 중복 실행 차단.
- running/awaiting_input/awaiting_review → failed: 검증 가능한 실패. failed → ready는 retry 승인/정책과 attempt 상한 검사 후에만 가능.
- blocked → ready: 차단 원인이 해결되고 기존 run이 더 쓰지 않는다는 증거가 있음.
- superseded: 미시작/실패 작업을 새 계획이 대체. 성공한 task를 수정·되돌리지 않는다.

Run.succeeded 조건은 provider final success **및** typed AgentResult 유효성, 필요한 프로세스 종료/turn 종료·쓰기 정지 확인이다. exit 0만으로는 충분하지 않다. verifier Run은 exit/입력 무결성/로그 capture로 판정한다. API transport disconnection은 성공 신호가 아니다.

Run.unknown/interrupted는 terminal 기록이다. recovery 후 새 run을 만들며 기존 Run에 `reconciliation_ref`(확인 증거 artifact)를 붙인다. unknown resolution 전에는 동일 task의 새 run을 생성할 수 없다. binding 재배정도 동일 규칙을 따른다.

## 4. 시작과 기본 workflow

1. mission.create: clean repo HEAD와 expected_base_oid 일치 확인, goal staging artifact 채택, draft 저장. 아직 모델 실행 없음.
2. mission.control(start): binding availability/권한 확인, policy snapshot, 기본 lead `plan` task 1개 생성.
3. Lead context에는 목표, 사용자가 지정한 requirements, 코드 조사 포트, 사용 가능한 역할·binding ID, 계획 schema만 넣는다. 임의 실행 명령 권한은 부여하지 않는다.
4. Lead가 PlanProposal을 제출하면 schema·DAG·scope 검증. 자동 계획 허용 시 적용, 아니면 plan decision을 생성한다. 단순 정보 부족은 product decision으로 질문한다.
5. 채택된 implement/test_author/document task를 dispatch한다. 각 writer는 독립 workspace를 가진다.
6. 필수 writer task의 capture가 완료되면 엔진이 integration task를 하나 만든다. 충돌 없으면 deterministic Git 조합; 충돌 시 integrator 역할을 실행한다.
7. 통합 후보를 고정하고 verification command별 verify task 생성. 후보 기준 전체 필수 command를 실행한다.
8. verification pass 후 독립 reviewer에게 candidate와 requirement matrix 전달. blocking/major finding이면 repair proposal을 요청한다. `require_independent_review=false`(빠른 미션: allowed_roles·role_bindings에 Reviewer 없음)면 review task를 만들지 않고 필수 작업이 끝나면 바로 awaiting_acceptance로 간다. 진행 중 policy.update로 이 값을 끄면 아직 시작하지 않은(Run 없는) Review task는 superseded가 되고, 실행 중이거나 끝난 Review는 그대로 남는다. Integrator 역할 연결이 없어도 결정적 통합은 진행하며, 충돌 결정은 `resolve_and_reintegrate`를 내지 않는다.
9. repair는 새 task와 candidate를 만든다. 기존 증거를 변경하지 않는다. O1은 새 candidate에서 모든 필수 검증을 재실행한다.
10. 검증·리뷰가 만족되면 phase=awaiting_acceptance. 사용자의 mission.accept가 candidate ID와 human requirement를 확인한 뒤 completed.

Lead 대화는 mission.messages로 유지한다. provider thread는 adapter가 resume를 증명한 범위에서만 재사용한다. 새 Lead run은 항상 정식 context 묶음을 받는다. 대화 세션이 사라져도 mission 계약은 남는다.

## 5. Plan 적용 알고리즘

PlanProposal.tasks는 **추가할 작업**이며 기존 작업을 암묵적으로 덮지 않는다. retire_task_ids는 미시작/실패/blocked 및 종료가 확인된 cancelled task를 지정한다. running task를 바꾸려면 먼저 cancel 완료가 필요하다. 취소한 작업의 Run/Exec 종료와 workspace lease 해제를 재검사하며 종료 불명 기록을 계획으로 건너뛰지 못한다.

ProviderTaskSpec의 local_key는 신규 작업 이름이다. depends_on_keys/parent_key는 같은 proposal의 local_key 또는 이미 전달된 기존 task UUID만 참조한다. daemon이 proposal 수신 때 신규 key마다 UUID v4를 한 번 생성하여 변환한 PlanProposal artifact에 저장한다. 저장한 proposal 재전송 시 같은 UUID를 재사용한다. result variant와 task kind가 맞지 않으면 RESULT_INVALID: plan→plan, review→review, writer→patch, research/design/consult/diagnose→report. question/blocked는 모든 AI task에서 허용한다.

검증 순서:

1. based_on_plan_revision 일치, IDs 유일성, mission scope와 artifact 소유권.
2. task kind/role 조합: verify만 role=null/binding=null; 나머지는 00 역할 표에 맞고 allowed_roles에 포함.
3. 모든 binding은 사용자 allowlist 안에 있고 모델·권한 capability가 있음.
4. depends_on 존재, self edge 금지, mission 밖 edge 금지; accepted 기존 task+신규 task에서 Kahn topological sort.
5. parent chain cycle 없음, delegation depth≤3, task≤256, plan revision≤20.
6. retired 작업에 의존하는 live/nonretired task가 없어야 한다. 대체는 새 ID와 replacement_of로 표현하고 미시작 dependent도 새 계약으로 대체한다.
7. 필수 requirement마다 담당 task 또는 최종 human_check가 존재. verify command는 allowlist의 ID만 사용.
8. writable path는 repository-relative, `..`/절대 경로/NUL/.git 접근 금지. 경로 교집합이 있는 병렬 writer는 허용하되 integration 위험 경고; 같은 workspace는 금지.
9. 위임/재시도/비용 한도와 외부 side-effect 범위 검사.
10. task insert·dependency insert·retire·plan_revision+1·event를 한 transaction에 기록.

실패 시 plan 부분 적용 없음. 잘못된 계획의 형식 수리는 Lead에게 최대 2회(총 task attempt 상한 안에서) 요청하고 이후 사용자 decision. plan의 자연어 설명으로 검증을 생략하지 않는다.

완료된 답변의 schema 오류 또는 daemon 계획 검증의 수리 가능한 오류에만 `InvalidResult`를 사용한다. `Run.retry_evidence.basis=plan_format_rejected`에 당시 plan revision과 거절한 답변의 미션 소유 artifact를 기록하고 `result_ref`에는 검증 오류를 보존한다. 일반 RESULT_INVALID, 초기화/transport 실패, stale revision, 정책 밖 경로·역할·모델·검증 명령, 저장 오류와 Unknown에는 형식 수리 증거를 만들지 않는다. 답변이 context byte 상한을 넘으면 자동 수리하지 않는다.

종료 Run과 연결된 Exec의 Exited를 확인한 뒤 `blocked:plan_format_repair`를 저장한다. Running 미션만 다음 스케줄에서 Ready로 바꾸며 일시정지·재시작·DB commit 실패로 추가 시도를 만들지 않는다. 같은 task의 거절 증거가 세 번째면 자동 수리를 중단하고 정확한 마지막 Run의 Recovery 결정을 만든다. task attempt·전체 시작·시간·binding을 재검사하며 실제 시작에는 비용·제공자 제한도 적용한다. 기존 Run/계약/workspace는 보존한다.

다음 Lead context의 `plan_repair`는 실패 Run ID, 검증 오류와 거절한 답변을 포함한다. 이것은 신뢰할 수 없는 증거이며 권한이나 원래 요구사항을 바꾸지 않는다. 이후 시도가 미전송으로 확인된 경우에는 같은 수리 지시를 다음 시도까지 유지한다. 다른 중간 실패에는 과거 수리 지시를 되살리지 않는다. 소유권·본문 무결성과 전체 context 한도를 다시 검사하며 누락/만료/범위 밖 증거를 건너뛴 채 provider를 실행하지 않는다.

### 충돌 후보 제외 후 계획

열린 Conflict의 `exclude_candidate` 답변은 기록된 helper 입력·결과·종료 Exec·workspace lease와 현재 통합 Run을 다시 확인한다. 사용자 입력으로 임의 후보를 지정하지 않는다. 제외 대상은 실제 충돌 후보의 source Run과 전이적 의존 작업, 해당 결과를 포함한 후보를 base로 사용한 작업이다. 성공한 Task/Run/Candidate/workspace 기록은 보존한다. 기존 실패 통합과 실행되지 않을 비성공 의존 작업은 superseded로 바꾸고 새 필수 Lead Plan과 결정 답변을 한 CAS 트랜잭션으로 저장한다. 현재 candidate 포인터를 비워 제외된 결과를 새 작업의 base로 재사용하지 않는다.

검증된 system 답변 artifact는 원래 결정·후보·제외 Task/Run ID와 사용자 답변 참조를 담는다. 계획·단계 전이·인수 때 이 근거를 다시 읽는다. 제외된 이력은 새 DAG의 dependency와 요구사항 coverage에 사용할 수 없지만 작업 수·ordinal 한도에는 계속 포함한다. 제외된 필수 실행 작업마다 원래 요구사항을 유지하는 필수 대체 작업이 필요하며, 대체 작업을 다시 교체한 경우에는 replacement_of 연결을 따라 최종 대체를 확인한다. Plan 작업 자체는 산출물 대체 대상이 아니다. 구현 단계의 필수 대체가 성공하기 전에는 통합하지 않는다. 제외된 Verify/Review도 같은 종류와 원래 검증 명령을 유지하는 필수 대체 계약이 필요하지만, 실행은 새 후보 통합 후 각 단계에서 진행한다. 최종 인수에는 각 필수 대체의 성공과 새 후보에 대한 검증/리뷰 근거를 확인한다. 과거 후보의 성공 결과를 대체 완료로 재사용하지 않는다. 남은 필수 작업이 모든 요구사항을 충족하면 선택 작업만 제외한 빈 계획을 허용한다. 수정 사이클·계획 revision·작업 수·시작·비용 한도는 유지한다.

## 6. Scheduling

tick은 기존 250ms scheduler 주기를 공유할 수 있지만 O1 ready 선택은 별도 순수 함수로 둔다. 실행 순서는 다음과 같다.

1. running mission 중 일시정지·전체 중단·전역 차단이 없는 것.
2. mission 순환 round-robin. 같은 mission 안에서는 사용자가 답해 재개 가능한 작업 → 오래된 ready 순서, 동률 ordinal.
3. dependency/decision/context/policy/binding 검증.
4. global 8, mission 4, binding 2 run cap 검사. provider approval 대기 Run도 slot 사용.
5. workspace writer lease 확인, OS resource admission과 원장 reservation.
6. 동일 task live Run 없음 확인, prepared Run/outbox/start_count를 atomic 생성.
7. actor가 durable outbox를 읽어 실행.

현재 구현은 Run·attempt·비용 예약을 만들기 전에 서버 관측에서 계산한 필수 capability를 검사한다. 부족한 작업은 blocked:capability_*로 남기고 독립 작업은 계속 진행한다. 같은 상태의 반복 tick은 revision/event를 증가시키지 않는다. 설치 재확인 또는 호환 모델 재배정 후 조건이 충족되면 Ready로 돌리며, paused/pausing 미션은 재개 전 실행하지 않는다. mission start와 정책 배정은 역할, 계획 적용·재시도·재배정·dispatch는 실제 TaskKind에 필요한 기능을 검사한다. 준비 이후의 변경은 시작 worker에서도 검사한다. 버전 재조회 실패/불일치는 adapter.start 전에 FailedBeforeSubmission으로 기록하며 자동 재시도 대상이 아니다. 이미 예약된 이 시도의 기록과 횟수는 보존한다.

lead/spec consultation도 run cap과 budget에 포함한다. deadlock 방지를 위해 consult 요청자는 계속 run slot을 보유한 채 worker를 기다리지 않는다. 결과를 checkpoint하고 현재 run을 종료한 뒤 `blocked:awaiting_consult`로 바꾸며 자문 task를 실행한다. 자문 완료 후 새 attempt로 이어가고 재개용 attempt는 상한에 포함한다. 명세의 기본 3회 상한으로 부족하면 사용자 정책 변경을 요구한다.

quota 관측이 stale이면 알 수 없음으로 처리하고 새 실행 실패 응답을 기다릴 수 있다. quota 퍼센트만으로 강제 모델 교체하지 않는다. 알려진 rate-limit reset까지 새 dispatch를 대기시키고 나머지 binding은 진행한다.

## 7. Outbox와 결과 불명

외부 provider 호출의 exactly-once를 주장하지 않는다. local intent 중복 방지와 불명 상태에서의 자동 재전송 금지가 계약이다.

사용자가 원래 지시의 영향을 확인한 뒤 `mission.message.supersedes_message_id`를 명시하면 새 Message와 route outbox를 같은 트랜잭션에 생성한다. 원본은 같은 미션·수신자의 Unknown/Rejected 사용자 메시지여야 하고, 승인/결정 답변과 미완료 전송 intent가 없어야 한다. 이미 직접 후속 메시지가 있으면 새 요청 ID여도 거절한다. 원본의 body/delivery/Run/outbox는 수정하지 않는다. 같은 request ID 재생은 최초 응답을 반환한다. 새 필드를 생략하거나 null로 보낸 기존 요청은 기존 fingerprint를 유지한다. 다음 실행 문맥에는 새 메시지의 원본 연결 ID도 포함한다. 이 경로는 원본의 불명 intent를 다시 실행하지 않으며 provider 접수 여부를 확인한 것으로 표시하지 않는다.

```text
DB: prepared run + prepared outbox commit
DB: outbox sending, run starting/may_have_sent commit
IO: adapter start/send
DB: provider IDs + acknowledged commit
IO: events/final
DB: validated result + terminal run + next action commit
```

- 첫 commit 전 crash: 아무것도 시작하지 않음.
- prepared/unsent만 남음: allow_recovery_of_unsent=true이고 pause/cancel/budget 검사 통과 시 실행 가능.
- sending commit 후 crash: 실제 전송 전이었어도 unknown 취급. native status 조회로 확인 전까지 재전송 금지.
- acknowledgment 후 연결 상실: provider session/turn read로 조정. 재개가 동일 turn 재전송인지 별도 turn 시작인지 구별한다.
- final 수신 후 DB 기록 실패: stream의 final을 재조회하거나 immutable result artifact와 provider ID로 조정. 성공을 추측하지 않는다.

새 actor는 DB fencing_token을 증가시켜 이전 actor callback을 무효화한다. token만 증가했다고 이전 프로세스가 종료된 것은 아니다. writer lease 재할당에는 별도 소유 process 종료 확인이 필요하다.

현재 연결된 종료 조정은 감독기가 group/stream 정리 후 저장한 `Exec.exited` 증거를 사용한다. Run/Exec 연결과 mission 소유 실행 명세의 해시를 확인하고, 정확한 Run/fence와 Exec 스냅샷을 담은 불변 `reconciliation_ref`를 기록한다. 증거 연결·Task의 active_run 해제·Workspace writer 해제·복구 결정은 단일 CAS 트랜잭션이다. Run의 unknown/interrupted 상태와 기존 결과·시간은 바꾸지 않고 이전 workspace는 Quarantined로 보존한다. `Run.holds_execution_slot()`은 이 증거 이후에만 불명 실행의 로컬 슬롯을 해제하며, 비용 계산은 제공자 결과 미확인을 별도로 유지한다.

**사용자 확인 정리.** 재소유에 필요한 native 신원이 없는 이전 daemon의 Exec(식별·시작 시각·그룹 신원/참조와 일치하는 그룹 종류 중 하나라도 없음, 예: 관측 트리만 있는 macOS·Windows Job·Prepared)나 Exec 없이 불명이 된 Run은 감독기가 Exited를 쓸 수 없어 실행 자리와 자원 예약을 계속 잡는다. 사용자가 프로세스가 없음을 확인하면 `mission.run.attest_exited`가 같은 해제(증거 연결·Exec exited·workspace Quarantined·active_run 해제)를 단일 CAS 트랜잭션으로 수행하되 증거 종류를 `Run.reconciliation_kind="user_attested"`로 남긴다. 현재 daemon이 소유한 Exec, 이미 Exited인 Exec, stopping 등 live Run에는 적용하지 않는다(`attestation_not_applicable`). native recovery가 재소유·관측할 수 있는 신원을 가진 이전 daemon Exec(`exec_store.rs` `validate_recovered`와 같은 조건: identity·started_at·group_reference가 있고 group_identity가 cgroup v2+`cgroup` 또는 macOS guardian+`observed_tree`)도 거절한다: 현재 daemon의 recovery가 그 그룹을 감시하고 취소 시 멈추며 비었음을 관측해야만 닫는다. 영구적인 recovery 실패를 기록하는 표시는 없으므로, 그런 그룹이 끝내 복구되지 않는 경우에도 사용자 확인으로 대신하지 않는다(미션 중단·daemon 로그 확인). 확인은 종료 관측이 아니므로 Exec의 exit_code는 null이고, 이후 복구 결정과 인수 확인은 관측 증거와 같은 절차를 따른다. 확인이 틀려 프로세스가 살아 있으면 그 외부 영향은 감독 밖에 남는다는 점을 UI가 확인 문구로 알린다.

복구 결정의 `retry_reconciled_task`는 현재 작업·최신 Run·증거 hash/fence·시도/예산/모델 정책을 다시 검사한 뒤 Task만 ready로 만든다. 실제 새 Run과 새 workspace는 기존 dispatch가 생성한다. 답변 재생은 같은 결과를 돌려주며 전달 불명 메시지와 이전 실행은 재전송하지 않는다. 원래 중단 중인 미션은 로컬 소유권이 모두 정리되면 중단을 완료할 수 있다. native 그룹 재소유와 provider 결과 재조회는 별도 경로이며, adapter/PID 미조회는 종료 증거로 인정하지 않는다.

통합의 Resolving/Continuing 실행이 불명으로 끝났다면 복구는 원래 통합 계획을 다시 확인하고 Automatic 단계부터 새 workspace에서 구성한다. 같은 Task의 과거 실행과 격리 파일은 보존한다. 인수 시에는 종료 증거·명시적 복구 결정·완료된 새 실행이 있어도 과거의 불명 영향을 자동 확인하지 않는다. 사용자가 해당 Run ID를 `acknowledged_reconciled_run_ids`로 확인해야 하며 daemon이 근거를 재검증해 인수 이벤트에 기록한다. 누락/중복/다른 미션 ID와 변조된 증거는 거절한다. 필드 생략 또는 null은 기존 직렬화와 요청 fingerprint를 유지한다.

## 8. 메시지와 결정

mission.message는 message row+delivery outbox를 저장한다. target_task_id=null은 Lead다. active target에 steer capability가 있으면 현재 turn으로 전달, 없으면 queued로 두고 다음 run context에 포함한다. UI가 queued를 즉시 전달됨으로 표시하지 않는다.

Lead에게 보낼 활성 plan run이 없으면 엔진이 새 plan task를 생성하고 해당 메시지를 첫 context에 포함한다. 같은 ready plan task가 있으면 메시지를 합쳐 중복 plan task를 만들지 않는다. 최종 question 결과로 provider turn이 끝난 경우 Run은 succeeded, Task는 awaiting_input이다. 답변 후 해당 task를 blocked:resume_pending→ready로 전이하여 새 attempt를 scheduling한다. 살아 있는 approval 대기 Run은 같은 Run에 답변하며 새 attempt를 만들지 않는다.

전송 대상이 이미 terminal task이면 INVALID_STATE. 사용자가 작업 전체에 요구사항 변경을 보내면 Lead는 plan proposal을 만든다. 기존 실행 파일 범위를 메시지 하나로 몰래 확대하지 않는다.

O1에서 mission의 goal/requirements는 start 후 불변이다. 같은 요구사항을 만족하는 구현 전략 변경은 replan으로 처리한다. 인수 조건 자체를 추가/삭제하는 요청은 명시적 후속 mission 생성으로 안내한다. 현재 mission의 완료 기준을 대화 해석만으로 낮추지 않는다.

decision.answer는 ID/state/plan_revision/candidate binding을 확인한다. 선택지 답 또는 answer artifact 중 적어도 하나, option은 원래 ID만 허용. 답변 commit과 provider 전달은 분리한다. 전달 실패 때 사용자가 같은 결정을 새로 답하도록 만들지 않고 delivery를 복구한다. 승인 요청은 정확한 provider request/action에 binding; product 답변을 tool 승인으로 재사용하지 않는다.

answer_ref 본문은 다음 실행 문맥에 실제로 들어간다. product 답변은 대상 작업 다음 Run의 queued 메시지, plan `revise`는 새 plan task objective(`plan_revision_request`: 사용자 요청+거절된 제안), provider 차단 작업의 `retry_with_instruction`/`change_model`은 route intent가 붙은 queued 메시지, `replan`은 Lead objective(`provider_blocked_replan`)다. 선택지와 함께 embed되는 답변은 UTF-8 64 KiB 이하만 받는다. 기록 메시지 형식은 01 §8이다.

provider가 `blocked` 결과를 내면 Run은 succeeded, Task는 `blocked:provider_blocked:<code>`다. 엔진은 실패 결정과 같은 reconcile 흐름에서 그 Run에 묶인 non-blocking Recovery 결정(`retry_with_instruction`·`change_model`·`replan`·`stop_mission`)을 연다. 연결 Exec 종료를 확인한 뒤에만 열고, 재시도 선택지는 시도 한도 안에서만 제시한다. 답변 시 시도·시작·시간 예산, binding 허용/활성, capability를 다시 검사한다. `change_model`은 사용자가 먼저 task.control reassign으로 고른 다른 연결만 사용하며 엔진이 모델을 대체하지 않는다. `replan`은 차단 보고서와 사용자 지시를 담은 Lead plan task를 만들고, 차단 작업은 그 계획이 retire/대체할 때까지 유지한다. 계획이 끝났는데도 같은 Run으로 차단돼 있으면 결정을 다시 연다. running/pausing/paused 미션의 provider 차단 작업은 결정 없이 조용히 멈추지 않는다.

다른 사용자의 응답 race는 expected_revision으로 한 명만 성공한다. 이미 obsolete인 질문은 STALE_DECISION. 동일한 질문 텍스트라는 이유만으로 approval을 병합하지 않는다.

## 9. 실패·retry·budget

자동 retry는 요청 미접수 확인 또는 이전 실행 종료와 side effect 평가가 완료된 transient failure만 가능하다. 지연은 2초/10초 + 0..20% jitter; fake clock 시험에서는 jitter seed 고정. retry_after가 있으면 더 늦은 시각을 사용한다. 재시도는 새 Run이며 실패 기록을 보존한다.

현재 자동 경로는 adapter의 `FailedBeforeSubmission` 증거가 있는 PROVIDER_UNAVAILABLE/PROVIDER_RATE_LIMITED에 연결한다. `Run.retry_evidence`에 작업 요청 미전송과 관측 시각을 저장하고, Run ID 기반의 고정 jitter와 Retry-After/저장된 reset 중 더 늦은 시각을 `Task.dispatch_after_unix_ms`에 기록한다. 첫 두 실패에 각각 최대 한 번의 새 시도를 예약하며 task attempt/mission 시작·시간/모델·비용 정책을 다시 적용한다. 오래된 기록의 증거 누락, Acknowledged와 충돌하는 미전송 주장, Unknown/Interrupted, AUTH/MODEL/POLICY/invalid output에는 자동 재시도하지 않는다.

기존 Run과 workspace를 보존한 채 Task를 blocked:transient_retry로 대기시키고, 시각이 지난 Running 미션만 Ready로 돌린다. 연결된 Exec가 있으면 같은 미션/Run의 Exited와 종료 시각을 확인해야 하며, missing/unfinished Exec에서는 자동·명시적 재시도 모두 막는다. 저장 실패 중에는 기존 상태/시각을 유지하고 새 provider를 시작하지 않는다. 일시정지·취소·재시작·모델 변경은 기다리는 시간을 없애지 않는다. 모델 변경은 다음 Run의 binding snapshot에만 반영한다. 이미 접수된 실행의 외부 영향 평가를 통한 자동 재시도는 별도 구현 범위다.

AUTH/MODEL/POLICY/invalid plan/unknown outcome에는 blind retry 금지. 실패해도 독립 task는 계속된다. required task가 최종 실패하면 dependent는 blocked:dependency_failed; Lead에 허용된 repair 기회를 준 뒤 더 이상 경로가 없으면 live run 정리 후 mission failed.

필수 비계획 작업이 작업별 시도 한도에 도달하고 최신 Failed Run과 연결된 Exec의 종료가 확인되면 daemon은 Lead Plan을 한 번 생성한다. `Task.failure_repair_run_ids`로 정확한 실패 Run을 연결하며 이 필드는 provider가 제출할 수 없다. 원래 진단·계약·요구사항·Run/workspace를 보존해 Lead 문맥에 넣고, 계획은 실패 작업을 retire하며 그 요구사항을 모두 포함하는 required 대체 작업을 `replacement_of`로 연결해야 한다. 누락된 대체 계획은 적용하지 않고 제한된 형식 수리 조건을 검사한다. 정책·경로·모델·의존 관계 검증과 자동 계획 승인 설정은 그대로 적용한다.

새 계획과 원래 실패 결정의 obsolete 처리는 같은 트랜잭션이다. 저장 실패나 재시작 때 같은 Run의 계획을 중복 생성하지 않으며, Lead가 담당 중인 원래 실패 작업의 별도 retry는 막는다. 수정 계획 취소 후에는 같은 실패로 자동 재생성하지 않는다. 구현 단계의 독립 작업은 계속 실행하며 새 계획과 대체 작업도 기존 시작·시간·모델·비용·동시 실행 한도를 적용받는다. Unknown/Interrupted·종료 미확인·선택 작업·아직 시도가 남은 실패는 이 자동 대체 대상이 아니다. 실패한 수정 Plan 자체는 기존 계획 복구 결정을 사용한다.

필수 작업 또는 필수 검증의 수정 사이클/작업 수 한도가 소진되면 Stopping과 취소 의도를 저장한다. 모든 소유 Run, actor worker 및 연결된 Exec의 종료 증거가 확인된 뒤 Failed로 확정한다. 누락·불일치 Exec는 종료 증거로 취급하지 않는다. 리뷰의 주요 지적은 사용자 근거 해제라는 경로가 남으므로 현재 후보에 묶인 Budget 결정을 만든다. 사용자는 수정 한도 확대·근거를 남긴 지적 해제·중단을 선택할 수 있다. 마지막 major/blocking 지적을 해제할 때 해당 후보/계획의 리뷰 한도 결정만 같은 트랜잭션에서 obsolete로 바꾸고 인수 조건을 다시 검사한다. 다른 비용/예산 결정과 이전 후보의 지적은 해제하지 않는다.

repair cycle≤3, task attempts≤3, automatic starts≤64, active time≤4시간을 기본 적용한다. 시작·시간 등 입장 budget 초과는 새 dispatch를 막고 budget decision을 만든다. 진행 중 Run은 사용자가 설정한 run timeout까지 안전 종료 가능; hard token/dollar cap이 provider에서 지원되지 않으면 **추정 입장 제어**라고 표시한다. 초과분 0을 보장하지 않는다.

위 수치는 기본 policy다. 사용자는 defaults.policy_ceiling 범위 안에서 policy.update로 확대할 수 있다. global/binding cap, task 수, frame/artifact/depth 상한은 policy로 확대하지 못한다. ceiling도 도달하면 새 mission으로 이어가야 한다. Run 경과 시간은 monotonic으로 계산하고 1초마다 저장한다. mission active_time은 Run 시간을 합산하지 않고 mission이 running/pausing인 실제 경과 시간으로 계산한다. paused/draft 시간과 phase=awaiting_acceptance(사용자 인수 대기, dispatch 없음) 시간은 제외한다. 한도 판정도 같은 누적값을 쓴다. 시간 checkpoint와 활동 시각 커밋은 housekeeping이라 semantic_revision을 올리지 않으며 사용자 mutation을 REVISION_CONFLICT로 만들지 않는다(01 §2). 저장 전 crash로 잃을 수 있는 최대1초를 표시한다.

executor를 주입하지 않은 저수준 검증 helper의 Unix 종료 신호는 `process_group(0)`으로 만든 child 그룹 ID를 숫자로 직접 전달한다. Production actor는 아래 영속 Exec 경로의 native 소유권을 사용한다. 외부 kill 프로그램의 음수 인수 파싱과 PATH 교체에 의존하지 않으며 0/1/범위 밖 ID는 거절한다.

run timeout 후 interrupt→10초 grace→소유 process terminate→5초 grace→확인된 소유 process kill. 종료 확인 전 lease 해제 금지. 출력 없음 60초는 stale activity 표시만 하며 교착 또는 실패로 자동 판정하지 않는다.

### 검증 명령의 영속 실행

Production actor는 후보별 독립 worktree와 Verify Run을 예약한 뒤 `mission/verification_exec.rs`에서 명령·후보·workspace·실행 파일/cwd·실효 시간 제한·자원 정책을 미션 소유 context artifact로 고정한다. 이 기록과 Busy lease를 커밋해야 공유 Exec supervisor의 gate를 준비할 수 있다. Exec 저장소는 binding 없는 Verify Run의 고정 계약을 대조하고 release 직전에 후보·허용 목록·workspace fence를 재검사한다.

검증은 admission 추정치 512 MiB와 CPU 슬롯 1개를 예약한다. 실제 사용량 강제 상한은 없다. 정상 종료·취소·시간 초과 후 native group과 출력 정리, Exited 커밋이 확인돼야 검증 결과를 쓴다. DB 장애에는 기존 log upload와 결과 커밋만 재시도한다. 데몬 재시작으로 결과를 잃은 검증은 Unknown/Interrupted로 보존하며 지원되는 native 소유권 복구 후 명시적 복구 결정으로 처리한다. 새 검증 계약 v2는 macOS Seatbelt profile·분리된 출력 경로·허용 환경·후보 commit을 고정한다. 환경 값 자체를 Exec manifest에 넣지 않고 profile argv의 SHA-256으로 연결하므로 재시작 때 같은 key의 다른 값도 거절한다. 원래 v1 계약은 과거 실행 조회/복구용으로 유지한다. 샌드박스 적용에 성공한 명령이 exit 0이고 전후 raw input 검사도 통과해야 Enforced다. 실패/취소/시간 초과 또는 입력 변경은 Unknown으로 기록하며 통과시킬 수 없다. 명령과 미션 모두 network를 허용해야 해당 권한을 열며, 다른 OS의 새 검증은 지원 오류로 중단한다. 별도 환경 프로필과 디스크 강제 상한은 남아 있다.
