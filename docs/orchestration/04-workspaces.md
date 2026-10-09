# 04. 작업 공간, 맥락, 변경, 검증

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. Git 기준과 작업 공간

O1은 local Git repository를 대상으로 한다. daemon은 canonical path와 Git common directory로 repository_id를 식별하고 HEAD OID/object format을 기록한다. 다른 경로의 같은 repository를 별개 저장소로 중복 등록하지 않는다. 등록은 canonical Git common directory를 기준으로 하며 연결된 worktree·하위 경로·symlink는 같은 ID를 사용한다. SQLite 쓰기 트랜잭션에서 중복과 기존 common_dir 변경을 거절한다. 하나의 과거 경로 행은 기존 ID를 보존해 보완하지만 여러 legacy ID가 같은 저장소로 해석되면 자동 병합하지 않는다. 시작 시 저장된 ID와 현재 Git 식별을 재대조한다. 사용자 작업 디렉터리에 tracked/untracked 변경이 있으면 mission start를 기본적으로 DIRTY_WORKTREE로 거절한다. 사용자가 커밋하지 않은 변경을 base에 포함하겠다고 고른 경우(`include_uncommitted`)에만 daemon이 그 작업 트리를 private snapshot commit으로 기록하고 그 commit을 mission base로 쓴다(`base_snapshot`). 어느 경우에도 staged/unstaged/새 파일을 자동 commit/stash하지 않는다 — staging은 저장소 index의 별도 복사본에서만 이뤄지고(stat cache·skip-worktree 보존), 움직이는 ref는 `refs/iyagi/missions/<id>/inputs/base` 하나이며 사용자의 index·checkout·branch·HEAD는 쓰지 않는다. ignored 파일은 capture와 같은 규칙으로 제외하고, untracked 파일은 포함한다. commit 저자·시각을 고정해 같은 tree·같은 HEAD면 같은 commit OID가 나오므로 재기록은 멱등하다. unmerged path가 있거나(merge/rebase 진행 중) 커밋하지 않은 집합이 항목 수·파일 크기·합계 한도를 넘으면 snapshot을 거절한다. `base_snapshot`이 있는 mission의 start는 clean 검사 대신 기록한 HEAD가 그대로인지 확인하고 작업 트리를 다시 기록한다 — 그 사이 HEAD가 움직였으면 base_changed다. UI는 포함 여부 선택과 정리 후 재시도를 안내한다. 후속 작업(`follow_up_of`)의 base는 이전 미션의 확정 candidate commit이며, start는 사용자 HEAD 일치와 clean 대신 그 commit이 private candidate ref로 도달 가능한지 검사한다. 모든 작업 공간은 daemon 경로에 그 commit으로 만들어지고 사용자 checkout을 읽거나 쓰지 않으므로 커밋하지 않은 사용자 변경이 base를 바꾸지 않는다.

소유 경로는 OS app data root 아래 `missions/<mission-id>/workspaces/<workspace-id>`와 `artifacts/<id-prefix>/<artifact-id>`다. 경로 문자열에 user title/provider session ID를 넣지 않는다. 디렉터리 0700, private 내용 0600을 사용하고 Windows에서는 사용자 ACL을 적용한다.

workspace 생성은 Git argv 배열로 `worktree add --detach <owned-path> <base-oid>`에 해당하는 작업을 수행한다. 현재 사용자 branch를 checkout/reset하지 않는다. ownership marker에는 repository ID, mission ID, workspace ID, 생성 시 OID를 기록한다. 등록 실패/중단 시 임의 경로를 자동 삭제하지 않고 preparing/quarantined로 복구 목록에 남긴다.

## 2. Writer lease

workspace당 writer run은 하나다. DB lease 획득 → OS/adapter 권한 설정 → 실행 순서를 지킨다. 같은 workspace를 공유하는 두 writer는 WORKSPACE_BUSY다. read-only reviewer도 mutable worker workspace 대신 고정 candidate를 읽는다.

lease token은 논리적 fencing이다. 예전 프로세스가 여전히 쓸 수 있다면 새 token만으로 안전하지 않다. process tree/remote state 종료 확인 후 lease를 해제한다. 불명 workspace는 quarantined, 새 시도는 읽기 검토 후 새 workspace에서 수행한다.

worktree는 port/DB/캐시/secret 격리가 아니다. 실행 프로필은 임시 출력 경로와 필요한 service fixture를 명시한다. shared production DB 자격증명을 기본 주입하지 않는다. repository hooks/tool config 실행 권한도 실행 정책에 포함한다.

## 3. 변경 capture

모델의 patch 결과는 변경을 완료했다는 주장이다. daemon이 실제 Git diff/status를 조회한다.

1. writer 중단/종료와 미완료 tool process 없음을 확인한다.
2. 허용 path 밖 변경, .git 변경, 경로 traversal, 허용 범위 밖을 가리키는 symlink, 예상하지 않은 submodule/LFS 변경을 검사한다. O1은 submodule pointer/LFS 객체 변경을 자동 통합하지 않고 명시적 unsupported 결과를 낸다.
3. tracked 삭제/수정과 허용된 untracked 파일을 포함해 manifest를 작성한다. ignored 파일은 기본 산출물에서 제외하고 explicit output allowlist에 있는 것만 포함한다.
4. 파일 내용 크기/hash, base OID, source run IDs, 실제 변경 path를 artifact로 저장한다.
5. private Git ref 아래 commit을 만들어 객체를 GC로부터 보존한다. user branch/ref는 움직이지 않는다. 생성 commit은 daemon의 local snapshot이며 push하지 않는다.
6. 허용 범위 위반은 RESULT_INVALID, workspace retained로 처리하고 검토할 diff는 보존한다. 자동으로 사용자 파일을 되돌리지 않는다.

## 4. 통합

integration workspace는 mission base에서 생성한다. source candidate 순서는 plan topological order, 동률이면 task ordinal이다. daemon은 각 변경을 차례로 적용하고 충돌 없으면 다음으로 진행한다. 충돌 시 partial integration을 기록하고 integrator에게 base/ours/theirs와 계약을 전달한다. 충돌 해결도 새 run과 동일 통합 workspace의 독점 lease가 필요하다.

통합 입력은 한 번 읽은 저장소 snapshot의 candidate ID·source run IDs·base/commit/tree OID로 고정한다. 후보가 없거나 성공한 source run과 일치하지 않으면 worktree 생성 전에 거절하며 mission base나 private ref로 대체하지 않는다. 전체 입력의 commit 종류·tree 일치·중복을 첫 patch 적용 전에 검사한다. repair patch는 해당 후보의 base를 기준으로 적용한다. private ref가 이동·삭제되어도 남아 있는 원래 객체를 사용하며 Git replacement object를 적용하지 않는다. 객체가 사라졌으면 통합을 거절한다. diff의 외부 프로그램·textconv·사용자 prefix·색상·변경 표시 문자 설정이 patch 내용을 바꾸지 않도록 한다.

새 manifest에는 통합 결과의 tree OID와 적용된 각 입력의 전체 OID를 남긴다. 충돌 질문에는 순서가 고정된 전체 입력과 적용 완료 입력을 함께 기록한다.

production 통합은 Task/Run·독점 workspace·Start outbox를 저장하고 공유 ExecSupervisor의 native gate를 거친 helper에서 실행한다. worktree 생성부터 patch 적용·private commit·manifest 읽기까지 모든 통합 Git subprocess는 그 Exec의 자손이다. helper 입력 artifact의 경로·SHA-256을 launch argv에 고정하고 helper도 읽은 bytes를 검증한다. 입력과 결과는 각각 256 KiB 이하이며 결과는 소유 stdout pipe로만 받는다. 종료 저장과 stream 정리 후에만 후보 또는 충돌 결정을 원자적으로 반영한다. 입장 대기는 실행을 복제하지 않고, 취소/시간 초과는 소유 그룹 종료를 확인하며, 저장 장애는 같은 결과의 기록을 재시도한다.

재시작 후 실행 중이던 통합은 Unknown으로 보존하며 자동 재실행이나 후보 승격을 하지 않는다. native 식별자가 검증된 OS의 재소유·취소·종료 확인을 공유한다. 충돌을 발견한 결정적 Run은 failed이고 partial workspace는 retained다. 충돌 결정과 연결하며 일반 실패 재시도 결정을 중복 생성하지 않는다.

명시적 해결 결정은 같은 Task를 Resolving으로 바꾸고 배정된 Integrator의 새 Run에 같은 workspace의 독점 lease를 부여한다. Integrator가 허용 역할이 아니거나 역할 연결이 없는 미션(빠른 미션)은 충돌 결정에 해결 선택지를 내지 않으며, 그 답변은 `option_invalid`로 거절한다. 원래 입력 순서·scope·base/ours/theirs를 보존한다. 모델의 Patch 보고는 해결 증거 artifact이며 바로 후보를 만들지 않는다. Continuing 자동 Run이 실제 파일의 충돌 표시·원래 writer scope·심볼릭 링크 경로를 검사하고 capture한 뒤 남은 입력을 적용한다. 실패한 이전 Run은 수정하지 않는다. 뒤쪽 입력의 새 충돌도 같은 Task/workspace에서 별도 결정·Run으로 처리하며 manifest에 채택한 resolution_run_ids를 기록한다. 검증 거절 뒤 명시적 재시도는 Integrator 실행으로 돌아간다.

충돌 후보 제외 결정은 기존 파일과 성공 기록을 보존하고 새 Lead 계획으로 이어진다. 제외한 source와 전이적 의존·후보 base 결과는 새 통합 입력에서 제거한다. 새 후보는 제외 Run ID를 포함할 수 없으며 manifest에 `exclusion_decision_ids`를 기록한다. 현재 candidate 포인터를 비워도 revision과 supersedes 연결은 마지막 통합 후보에서 이어진다. 새 작업 공간에서 다시 통합하고 새 후보에 검증·리뷰를 적용한다. 제외된 필수 Verify/Review는 같은 종류·검증 명령을 유지하는 대체 계약으로 새 후보 통합 이후 실행한다. 인수는 해당 대체 검증의 Passed 기록과 성공 Run, 대체 리뷰 결과의 정확한 candidate ID를 다시 확인한다. 다른 작업이나 과거 후보의 성공으로 대신하지 않는다.

충돌 경로는 NUL 구분 Git index에서 읽어 Unicode·따옴표·탭·개행을 보존한다. 해결 또는 이어가기 중 결과가 Unknown/Interrupted가 되면 기존 workspace를 격리한다. 종료 증거가 확인된 뒤 명시적 복구 결정은 원래 계획 artifact를 다시 검사하고 같은 Task를 Automatic으로 되돌린다. 새 workspace에서 기록된 후보를 재구성하며 기존 파일·Run을 채택하거나 수정하지 않는다. 새 충돌은 새 결정으로 해결한다. 특수 파일 및 OS별 검증의 남은 범위는 [구현 현황](IMPLEMENTATION_STATUS.md)에 구분한다.

모든 변경 적용 후 새 candidate ID/commit/tree/manifest를 생성한다. candidate는 불변이다. 후보 수정은 새 candidate로만 표현하고 old verification/finding의 candidate_id를 변경하지 않는다. 사용자의 base branch가 움직이면 기존 candidate를 검증된 최신 branch 결과로 표시하지 않는다. publish/rebase는 O20 범위이며 O1 결과는 기록된 base를 기준으로 한다.

## 5. 검증

VerificationCommand는 사용자가 설정한 program/argv/cwd/env profile/timeout/network 정책의 snapshot이다. planner는 allowlist의 ID를 선택하며 새 shell script를 검증 명령으로 등록할 수 없다. 각 검증은 별도 Exec와 로그 artifact를 가진다.

검증 전 candidate OID에서 독립 workspace를 생성하고 다른 agent writer를 차단한다. production 검증은 hook·fsmonitor를 끈 `worktree add --detach --no-checkout` 후 `read-tree --reset`으로 index만 초기화하고 raw blob을 직접 기록한다. 조건부/worktree-local clean/smudge/process filter와 줄바꿈 변환도 실행하지 않는다. 원본 입력은 파일당 64 MiB·합계 512 MiB로 제한하며 준비용 Git subprocess 전체의 시간/메모리 강제 한도는 별도 미완료다. 실제 파일은 필터 없는 blob hash·mode·링크 target·전체 파일 집합으로 후보와 대조하며, 명령 종료 후에도 다시 확인한다. ignored/untracked 파일, 누락 파일, 외부 링크와 지원하지 않는 tree entry는 증거로 채택하지 않는다. git status가 clean이라는 응답만으로 입력을 판정하지 않는다.

macOS의 새 검증은 deny-by-default Seatbelt에서 자식과 함께 실행한다. 파일 읽기와 실행에 필요한 제한된 시스템 기능은 허용하고, 쓰기는 별도 소유 출력 폴더와 /dev/null에 한정한다. 네트워크는 두 정책의 명시적 허용이 있어야 연다. env_clear 뒤 frozen PATH와 전용 HOME/temp/XDG/Cargo 출력 설정만 주입한다. 이 backend의 실제 파일·link·network·자식 경계를 시험했다. 원래 환경과 출력은 실행 증거에 연결하며 입력 무결성 표시는 실제 보장 수준이다.

- `enforced`: source snapshot에 외부 writer가 없고 실행 중 검증 입력의 쓰기도 환경이 차단한다. 출력은 별도 허용 경로다. read-only mount/동등한 권한 보장을 시험한 executor에서만 사용한다.
- `observed`: 시작 입력은 candidate로 확인했지만 명령이 source에 쓸 수 있다. 전후 hash가 같아도 enforced로 승격하지 않는다.
- `unknown`: 입력/환경 확인 실패. pass 출력만으로 통과 처리하지 않는다.

passed는 실제 exit=0, timeout 없음, 필수 로그 보존, 입력 무결성 unknown 아님일 때만 가능하다. enforced가 필요한 strict policy에서 observed 결과는 인수 요건을 만족하지 않는다. 일반 policy는 사용자가 observed 근거의 한계를 명시적으로 확인한 verification ID를 mission.accept에 포함하면 인수 가능하다. UI에 `통과 · 입력 쓰기 차단 미보장(관찰만)`을 표시하며 완전 격리 검증이라고 부르지 않는다.

브라우저 검증은 미리 설정한 검증 command/profile로 동작한다. 화면 캡처/영상은 candidate/시나리오/환경에 연결된 artifact다. 이미지 파일 생성만으로 기능 통과를 판정하지 않는다. 도구 출력은 untrusted content로 렌더한다.

## 6. 리뷰와 인수

reviewer는 새 provider conversation/context에서 candidate, 원래 requirements, 검증 결과를 받는다. builder의 최종 설명을 사실로 취급하지 않고 변경 내용을 읽는다. 같은 모델을 쓸 수 있지만 동일 implementation thread는 금지한다.

finding은 정확한 candidate와 source evidence/path/line에 연결한다. blocking/major가 open이면 인수 불가다. fixed는 새 candidate에서 review로 확인하고 새 증거를 기록한다. dismissed는 사유와 사용자 결정 또는 명시적 review resolution을 요구한다. 모델 다수결로 지적을 삭제하지 않는다.

mission.accept 검증 순서:

1. expected revision/candidate ID가 현재와 일치한다.
2. 필수 active plan task가 succeeded다. 검증된 계획으로 superseded가 된 작업과 optional 작업은 필수 완료 분모에서 제외한다. cancelled 필수 작업은 재시도 또는 대체 계획 없이 인수할 수 없다.
3. 모든 requirement의 필수 command가 현재 candidate에서 passed다.
4. strict policy의 input_integrity 조건을 충족하고 observed의 필요한 사용자 확인이 있다.
5. `require_independent_review=true`면 independent review가 완료됐다. 빠른 미션(false)은 review task 없이 이 조건을 통과한다. 어느 경우든 open blocking/major가 0이다.
6. human_check requirement ID가 사용자 확인 목록에 모두 있다.
7. live writer/open blocking decision과 아직 검토하지 않은 Unknown/Interrupted가 0이다. 기본 인수는 과거 불명 실행을 계속 차단한다. `acknowledged_reconciled_run_ids`의 명시적 확인은 정확한 종료 proof·Exec/launch artifact, 답변된 복구 결정과 전달 기록, 같은 Task의 성공한 새 Run 및 별도 Retained workspace, 기존 Quarantined workspace와 후보 출처를 다시 검증한 실행만 허용한다. 종료 증거(관측된 Exec 종료 또는 사용자 확인 `user_attested`)만 있거나 새 실행이 미완료이면 확인을 거절한다. 확인 기록에는 증거 종류(`termination_kind`)를 남긴다. 과거 실행을 성공으로 바꾸지 않으며 외부 효과가 알려졌다고 주장하지 않는다. 확인한 Run/proof/복구 결정/대체 Run/workspace ID를 인수 이벤트의 immutable changes_ref artifact에 보존한다.
8. accepted_at/completed/event를 atomic commit한다.

## 7. ContextBundle와 지식

context는 256 KiB bytes 상한과 runtime의 확인된 token context 상한을 둘 다 검사한다. token 수는 provider tokenizer가 있을 때만 정확한 값이라고 표시한다. 보수적 추정은 estimate 출처를 붙인다.

포함 우선순위는 시스템 권한/작업 계약 → 사용자 goal/requirements → 채택된 관련 결정 → 직접 dependency 산출물 → 실패 인계 → 관련 조사 → 기타 참조다. 필수 계약이 넘치면 CONTEXT_TOO_LARGE로 차단한다. 사용자 요구나 권한을 silent truncate하지 않는다. 선택적 본문은 참조로 대체하고 omitted 이유를 기록한다. 자료 본문은 지시가 아닌 untrusted evidence 영역으로 구분한다.

knowledge record의 fact/hypothesis/decision/question을 구분한다. 출처 없는 사실은 hypothesis로만 등록한다. accepted decision은 사용자 답변 또는 채택 plan과 연결한다. 관련 path/기준이 바뀌면 stale로 표시하고 자동 재주입하지 않는다. UI는 source artifact/run/기준 commit을 열 수 있어야 한다.

다른 모델로 handoff할 때 ContextBundle에 원계약, 관련 결정, 실제 diff, 실패 로그, 시도한 접근, 남은 질문을 포함한다. native session resume와 cross-provider handoff를 별도 동작으로 표시한다. 다른 mission의 본문을 자동 수집하지 않는다.

## 8. 결과 보관과 정리

작업 완료 후 workspace는 retained다. O1은 사용자 worktree/branch를 자동 삭제하지 않는다. daemon 소유 workspace 정리는 ownership marker(worker)/canonical path/활성 참조/격리 상태/dirty status/보존 정책을 모두 검사한 별도 관리 동작이다. dirty workspace는 자동 정리에서 제외한다. UI 닫기/archive는 정리 요청이 아니다.

정리는 사용자가 누를 때만 `workspace.cleanup`으로 실행한다(UI는 확정·보관 뒤 `workspace.usage`로 제안). 보존 정책:

- 대상 미션: completed/cancelled/failed(보관 여부 무관). 실행 자리를 잡은 Run(live 또는 종료 증거 없는 unknown/interrupted)이나 Exited가 아닌 Exec가 있으면 미션 전체를 정리하지 않는다(`run_unreconciled`). 활성 미션은 `mission_active`.
- 제거 조건(모두 충족): projection의 `owned_by_daemon` workspace이고, canonical 경로가 `<missions root>/<mission>/workspaces` 바로 아래의 daemon 발급 이름(`<uuid>` 또는 `integration-<uuid>`)이며, worker workspace는 sibling ownership marker(`<id>.owner.json`, 일반 파일)의 mission_id·workspace_id·repository_id가 이 미션·workspace·저장소와 일치하고(통합·검증 worktree는 marker를 만들지 않으므로 발급 이름·위치·projection·worktree 등록이 그 역할을 한다. 그 밖의 종류는 marker가 없으면 소유로 보지 않는다), 사용자 저장소 checkout 안이 아니고 저장소가 그 안에 있지도 않으며, writer lease·실행 자리를 잡은 Run이 참조하지 않고, Quarantined(불확실하거나 사용자 확인으로 정리한 실행의 격리 작업 공간)가 아니며, 미션 저장소의 `git worktree list`에 등록돼 있고, `git status`(untracked 포함)가 비어 있다. 하나라도 아니면 `kept`에 이유를 남기고 파일을 건드리지 않는다. 등록되지 않은 임의 경로는 §1 규칙대로 삭제하지 않는다. 저장소 경로를 canonicalize할 수 없거나 worktree 목록을 읽지 못하면 `repository_unavailable`, 격리 작업 공간은 `quarantined`다. Windows의 `\\?\` canonical 접두어는 Git이 보고하는 경로와 비교하기 전에 떼어 낸다. Git 2.36 미만처럼 `worktree list --porcelain -z`가 없으면 줄 단위 porcelain을 읽는다(그 형식으로 표현되지 않는 경로는 어떤 workspace와도 일치하지 않아 남는다).
- 제거 방법: 항목마다 시작 전에 3초 예산을 확인하고, 넘기면 남은 항목은 `deferred`로 두고 재요청으로 이어간다. Git 조회(worktree 목록·status)는 남은 예산 안에서만 기다리며 넘기면 `deferred`다. 제거는 `--force` 없는 `git worktree remove`라서 status 확인 뒤 생긴 변경이 있으면 Git이 거절하고 경로가 남아 `remove_failed`가 된다(변경을 지우지 않는다). 경로가 사라졌는지 확인한 뒤 sibling ownership marker(`<id>.owner.json`)와 같은 workspaces 폴더 안의 검증 출력(`<id>.verification-output`)을 지운다. `git worktree prune`은 실행하지 않는다: 저장소 전체의 등록을 대상으로 하므로 잠시 연결이 끊긴 외장 디스크 등 사용자 worktree 등록까지 지울 수 있고, `worktree remove`가 제거한 항목의 등록은 이미 지운다.
- Git ref: 확정 결과를 포함한 모든 candidate ref(`refs/iyagi/missions/<id>/candidates/*`)와 그 commit은 남긴다(사용자가 계속 가져오고 후속 작업 base로 쓸 수 있게). 미확정 미션의 후보 ref도 부분 결과의 유일한 사본일 수 있어 남긴다. 작업 입력 합성용 `refs/iyagi/missions/<id>/inputs/*`는 한 번의 `git update-ref --stdin` 트랜잭션으로, `input-indexes` 임시 폴더와 함께 항목 정리 전에(예산 안에서, 넘기면 `deferred`) 지운다. 남긴 worktree는 자기 HEAD로 입력 commit을 계속 참조한다. 사용자 branch·tag·HEAD·index는 목록 조회조차 하지 않는다.
- 기록: Workspace projection 행과 Run/Exec/artifact 증거는 이력으로 남긴다. 사용량은 디스크에 남은 경로만 센다. 크기는 1.5초·25만 항목 상한과 2분 캐시를 둔 추정치이며 상한에 걸리면 하한값이다. usage의 `cleanable`은 막힌 이유가 없고 위 제거 조건을 모두 통과하는 workspace가 하나 이상일 때만 true다(모두 남길 항목이면 false, `blocked_reason`은 null). Git 조회는 호출 전체 3초·조회당 0.75초 안에서만 기다리고, 답을 받지 못한 항목은 제거 가능으로 간주해 정리가 실제 이유를 보고한다. 이 상한으로 브리지 RPC 5초 안에 응답한다.

artifact 보존 상한 도달은 조용한 유실 대신 ARTIFACT_LIMIT로 새 작업 차단/진행 중 쓰기의 안전 종료를 요청한다. 필요한 증거가 유실되면 검증 unknown으로 기록하고 완료 판정을 보류한다.
