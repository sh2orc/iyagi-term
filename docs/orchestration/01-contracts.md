# 01. 타입, RPC, 저장소

[명세 입구](../../ORCHESTRATION_SPEC.md) · 필드 정답: [contracts.ts](contracts.ts).

## 1. ID와 모델 의미

Mission = 사용자 목표, Task = 검증 가능한 하위 계약, Run = 그 계약의 한 실행 시도다. 기존 R1 Task/Attempt와 별개이며 DB 접두사는 `orch_`다. 재시도는 동일 Task에 새 Run/attempt 번호를 만든다. 종료된 Run은 다시 running으로 변경하지 않는다. 모델 교체도 새 Run이다.

RoleBinding은 template 참조이고 Run.binding_snapshot은 실제 사용 설정의 불변 복사본이다. binding 수정은 실행 중 Run에 적용하지 않는다. `credential_ref`만 저장하며 비밀값을 복사하지 않는다. provider_session_id는 표시용 UUID 검사를 하지 않고 길이 512 bytes/control 문자 없음으로 검사한다. argv에 넣을 때는 해당 adapter의 별도 입력 검사를 통과해야 한다.

모든 ID는 UUID v4, wire sequence/revision/bytes는 10진 문자열, DB는 signed INTEGER다. JS에서는 BigInt로 비교하고 Number 변환하지 않는다. mission revision과 event seq는 생성 시 1, mission transaction당 함께 1 증가한다. timestamp는 UTC이고 timeout은 monotonic으로 계산한다. 재시작 중 경과 시간은 active_time에 추측해 더하지 않는다.

`null`은 실제 미관측 또는 해당 없음이다. 모델 토큰, 비용, 완료 예상 시간의 null을 0으로 표시하지 않는다. USD는 1달러=1,000,000 micros 정수로 계산한다. 구독 quota 퍼센트를 dollar 또는 token으로 변환하지 않는다.

## 2. 트랜잭션과 CAS

모든 mission mutation은 다음 순서다.

1. 인증된 연결 scope, frame/필드 길이·문자·enum 검증.
2. request_id의 기존 fingerprint 확인. 동일 payload는 저장된 **최초 응답**을 반환; 다른 payload는 REQUEST_CONFLICT. 재전송 때 최신 revision 검사를 다시 하지 않는다.
3. mission 존재와 expected_revision 확인. 불일치면 REVISION_CONFLICT와 current_revision. UI 자동 재적용 금지. 사용자 mutation(아래 housekeeping 규칙)은 `semantic_revision <= expected_revision <= revision`이면 일치로 본다.
4. 권한·상태·참조·계획 검증. 검증 실패는 side effect 없음.
5. 동일 writer의 `BEGIN IMMEDIATE`: projection, event, outbox, request 응답, revision을 함께 커밋.
6. 커밋 후 event hint broadcast. 외부 I/O는 transaction 밖에서 수행.

**housekeeping 커밋과 semantic_revision.** 모든 커밋은 revision/event seq를 1 올린다. 그중 시간 checkpoint(`engine.time_checkpoint`, 1초)와 실행 활동 시각(`engine.activity`, Run당 초당 1회)은 active_time_ms·Run.active_time_ms·Run.last_activity_at·updated_at만 바꾸는 housekeeping이다. `Mission.semantic_revision`은 housekeeping이 아닌 마지막 커밋의 revision이며 daemon만 기록한다. 필드가 없는 기존 문서는 revision과 같다고 본다. 사용자 mutation(`mission.control`·`message`·`task.control`·`decision.answer`·`accept`·`plan.apply`·`policy.update`·`finding.resolve`·`run.attest_exited`)은 expected_revision이 `[semantic_revision, revision]` 안이면 통과하고, daemon이 단일 커밋 lock 안에서 저장된 revision으로 rebase해 저장소 CAS는 정확히 일치로 유지한다. rebase는 housekeeping 필드를 되돌리지 않는다(최댓값 유지). 그 사이 의미 있는 커밋이 있으면(semantic_revision > expected) 또는 expected > revision이면 기존처럼 REVISION_CONFLICT다. 엔진/actor 자신의 커밋 CAS는 완화하지 않는다. `mission.accept`의 revision 검사도 같은 규칙이다. fingerprint에는 클라이언트가 보낸 expected_revision이 그대로 들어가므로 재전송 규칙은 바뀌지 않는다. UI는 snapshot의 revision을 그대로 보내면 되고 semantic_revision을 보낼 필요는 없다.

fingerprint는 method+정규화 payload의 SHA-256. object key 재귀 정렬, array 순서 유지, null 포함, request_id만 제외한다. expected_revision은 fingerprint에 포함된다. DB insert 후 network timeout이면 동일 request_id로 조회/재전송한다. 실패한 validation은 request row를 남기지 않는다. 요청 상태 조회의 not_found만으로 새로운 request_id를 만들어 재실행하지 않는다.

UI의 메시지 저장 복구 원장은 daemon request 원장과 구분한다. `messageJournal.ts`는 미션·수신 작업·대체 원본 ID별 하나의 exact `mission.message` payload를 IndexedDB readwrite transaction으로 예약한다. version·scope·필드·정수·artifact 참조를 검증하며 알 수 없는 필드를 제거해 보내지 않고 오류로 보존한다. 원장에는 본문과 비밀값을 복사하지 않는다. 저장 완료 전에 message RPC를 보내지 않으며, 복구는 원래 request_id와 revision을 유지해 daemon의 fingerprint 검증을 거친다. UI 재시작만으로 message mutation을 실행하지 않는다. 복구 본문의 cursor/총 bytes/SHA-256/UTF-8 검증 실패는 내용을 표시하지 않지만 정확한 원래 요청의 결과 확인을 막지 않는다.

state 변경 helper마다 event를 만들지 말고 상위 transaction당 한 event에 바뀐 entity ID를 모은다. encode 후 16 KiB 이하면 changes 배열과 changes_ref=null을 저장한다. 초과하면 변경 목록 artifact를 먼저 fsync하고 changes=null/changes_ref를 저장한다. 두 필드 중 정확히 하나만 non-null이다. artifact 채택과 event 기록은 같은 transaction이다. CAS 실패로 생긴 미참조 파일은 GC 대상으로 남는다. UI는 두 경우 모두 snapshot을 갱신하므로 변경 목록 본문을 반드시 내려받을 필요는 없다.

## 3. RPC와 전송

메서드 전체 params/result는 contracts.ts의 Rpc interface가 정답이다. envelope는 기존 `{v:1,id,method,params}`를 사용한다. hello에 `mission_protocol:1` capability를 추가한다. UI/daemon 중 한쪽이 없으면 작업 생성 UI를 잠그고 일반 터미널은 유지한다. 동작을 지원하는 척하는 빈 result를 반환하지 않는다.

기존 프레임은 prefix까지 65,536 bytes다. JSON escape 후 크기로 검사한다. 큰 prompt/plan/log/diff를 RPC에 inline으로 넣지 않고 artifact 참조를 보낸다. title 최대 256 UTF-8 bytes, message artifact 최대 256 KiB. `Requirement.text` 최대 2 KiB, requirements 최대 32개; 최종 frame 검사로 더 작은 한도가 적용될 수 있다. 크기 초과는 전송 전에 설명하고 자동 절단하지 않는다.

snapshot의 Entity 하나는 encoded JSON 48 KiB 이하로 제한한다. 여러 작은 필드의 합도 검사한다. 본문은 artifact로 분리하며 한 entity가 frame보다 커서 pagination으로도 읽을 수 없는 상태를 저장하지 않는다. projection 전체가 snapshot 예산에 근접하면 새 task/message 접수를 차단하고 보존된 실행 이력을 정리할 수 있는 관리 안내를 낸다. 실행 완료·취소·복구 기록용으로 마지막2MiB를 예약하여 일반 접수에는6MiB까지만 사용한다. 상한 예측이 불가능한 생성 결과는 artifact 본문으로 저장하고 projection에는 작은 ref만 추가한다.

artifact 업로드:

1. begin에서 예상 bytes/hash/media_type 검사, 임시 파일 생성; mission_id=null은 생성 전 staging 전용.
2. write는 4 KiB raw chunk를 base64로 전송. expected offset만 append. 이전 offset의 동일 bytes 재전송은 동일 next_offset, 다른 bytes는 REQUEST_CONFLICT. 건너뛴 offset은 INVALID_ARGUMENT.
3. commit에서 길이·SHA-256 확인 후 fsync+rename, artifact DB 등록. 재호출은 같은 ref 반환.
4. 생성 전 artifact는 생성 연결의 client_id에 귀속; mission.create transaction이 해당 mission에 채택한다. 다른 client나 mission으로 참조 이동 금지.
5. 연결 종료만으로 upload를 삭제하지 않고 1시간 후 미완성 업로드만 정리한다. DB 등록 전 orphan 완성 파일은 다음 GC에서 참조 확인 후 정리한다.

읽기는 caller의 mission 접근 범위와 실제 소유 경로를 확인한다. path를 RPC 인자로 받지 않는다. max_bytes는 4 KiB 이하. artifact bytes를 HTML로 직접 렌더하지 않는다.

binding.probe는 저장된 실행 파일에 로컬 `--version`만 호출한다. 인증·모델 목록·유료 추론은 조회하지 않는다. 결과는 `{binding, models: [], installation}`이며 installation은 `verified|not_found|failed|timed_out|output_limit|unrecognized_version`이다. 최대 3초, stdout/stderr 각각 4 KiB 제한을 적용하고 소유 자식을 정리한다. 성공은 설치 버전 관측만 의미하며 자동 실행 호환성은 별도 시험 증거를 요구한다.

probe는 관측한 runtime_version·checked_at·capabilities를 시작 시 binding revision에 대한 CAS로 저장하고 증가한 revision을 반환한다. 조회 중 설정이 바뀌면 REVISION_CONFLICT이며 최신 설정을 덮어쓰지 않는다. 실패도 checked_at을 갱신하고 과거 버전과 capability를 지우며, 사용자가 정한 enabled는 유지한다. 매 probe는 새로운 로컬 관측이며 서버 내부 request ID로 저장한다. 응답을 잃으면 binding.list로 저장된 상태를 다시 읽을 수 있다.

runtime.detect(params `{}`)는 첫 설정용 읽기 전용 조회로 아무것도 저장하지 않는다. codex·claude·opencode 순서로 실행 파일 절대 경로(데몬 PATH 다음 Homebrew·npm·bun·volta·cargo 등 사용자 설치 디렉터리), binding.probe와 같은 상한의 로컬 `--version` 결과, 로그인 흔적(codex는 기본 file 저장소일 때 `auth.json` 존재, `cli_auth_credentials_store`가 keyring/auto면 unknown, claude `.claude.json`의 `oauthAccount` 키 존재, opencode unknown), 설정된 기본 모델, 증거 provider·고정 모델(이 OS·아키텍처·감지 버전에 실제 증거가 있을 때만)을 병렬로 조회해 반환하며 로그인·설정 파일의 내용이나 비밀값은 반환·기록하지 않는다. verified_roles는 감지 버전·구독 로그인·제안 provider·(proven ?? configured) 모델로 만든 미저장 binding에 현재 registry를 적용해 시작 시 역할 capability 검사를 통과하는 lead/builder/reviewer/integrator다. 모델을 알 수 없으면 시작이 빈 모델을 거절하므로 빈 목록이며, 실제 실행 권한은 여전히 binding.save 후 binding.probe 관측을 요구한다.

같은 행의 `grade`는 [11 §3.2](11-local-evidence.md#32-compatibilitygrade)·[11 §7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers)의 `CompatibilityGrade` 판정을 그대로 따른다: 출시 증거(정확 버전 또는 `version_line`)만으로 동의 없이 네 역할이 통과하면 `verified`, 출시 증거는 없지만 `local_probe`/`observed_runs`로 통과하면 `verified_locally`(O22 신설), 같은 라인의 출시 증거가 있으나 이 버전의 로컬 프로토콜 진단이 없거나 실패했으면 `same_line_unverified`, 그 밖은 `unverified`(버전 미확인 포함), 실행 파일이 없으면 `not_installed`다. `experimental_roles`는 같은 미저장 binding에 동의를 넣고 registry를 적용했을 때 시작 gate를 통과하는 역할이다(값의 뜻은 바뀌지 않는다). 모델이 없거나 어댑터가 그 인증 경로를 실행하지 못하면(예: 저장된 연결 참조가 없는 OpenCode 구독 행) 빈 목록이다.

binding.save는 revision=0으로 생성, 기존은 CAS다. 실제 CLI의 runtime_version·checked_at·capabilities 입력은 증거로 받지 않는다. probe가 만든 비공개 관측을 binding과 같은 JSON document/CAS 트랜잭션에 저장한다. 관측에는 형식 버전, 연결 ID와 runtime/program/provider/model/effort/auth_route/credential_ref/endpoint_ref의 해시, OS, 관측 버전과 시각을 기록한다. 새 연결이나 대상이 바뀐 연결은 unknown이며, 이름·비용 등 대상 밖 편집은 일치하는 서버 관측을 유지한다. 관측을 RPC에 복사해 넣어도 채택하지 않는다. fake runtime은 명시적으로 주입된 시험 어댑터 전용이며 production factory는 거절한다.

**로컬 증거(`Binding.local_evidence`).** daemon이 이 PC에서 측정한 무추론 자가 진단·성공 실행 관측을 담는 선택 필드다(`local_evidence?: LocalEvidence | null`). 클라이언트가 `binding.save`에 보낸 값은 항상 버리며, daemon만 `binding.probe`(자가 진단, save-source `binding.probe`)와 성공 Run 관측(save-source `binding.run_evidence` = `term_contracts::mission::rpc::methods::BINDING_RUN_EVIDENCE`)에서 갱신한다. `LocalEvidence`/`LocalProbeReport`/`LocalRunEvidence`의 필드와 저장·보존·초기화 규칙은 [11 §3.3](11-local-evidence.md#33-localevidence-신설-missiontypesrs-tsexport-deny_unknown_fields-없음)·[§3.5](11-local-evidence.md#35-save-source-상수)·[§7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers)이 정답이며 여기서 다시 정의하지 않는다. Binding `Support.reason_code`가 이 증거를 가리키는 전체 slug 목록(`null`·`version_line`·`local_probe`·`observed_runs`·`experimental_opt_in`·`local_probe_failed`·`adapter_not_implemented`·`no_compatibility_evidence`와 그 밖의 출시 fixture slug)은 [11 §3.1](11-local-evidence.md#31-supportreason_code-값)이 정답이다.

**실험적 연결 동의(`Binding.experimental_version`).** 사용자가 이 연결에서 미검증(구현됨) 기능을 실험적으로 쓰는 데 동의했다는 연결당 플래그다. binding.save가 문자열 그대로 저장한다(null 또는 앞뒤 공백·제어 문자 없는 1..=128 bytes, 아니면 INVALID_ARGUMENT). 증거가 아니며 관측 대상 해시에도 들어가지 않으므로 동의를 바꿔도 설치 관측은 유지된다. 클라이언트가 보낸 capabilities는 여전히 무시한다. 값은 동의 시점의 관측 버전(표시용)이며, [11 §3.4](11-local-evidence.md#34-bindingexperimental_version-의미-변경)에 따라 **관측 버전과 같아야 한다는 조건은 없앴다** — CLI가 업데이트돼도 동의는 그대로 유효하고 `experimental_version_mismatch`는 더 이상 발생하지 않는다(과거 기록 표시용 오류 문자열 매핑만 남을 수 있다). registry(`capabilities_for_binding`)가 이 동의를 capability로 투영하는 정확한 순서는 [11 §4](11-local-evidence.md#4-capability-투영-규칙-capability_evidencecapabilities_for_binding)가 정답이다: 기존 증거가 supported로 증명한 기능은 그대로 두고, 자가 진단이 실패로 증명한 기능(`local_probe_failed`)은 동의로도 열리지 않으며, 나머지 미증명 기능만 어댑터가 그 provider/인증 경로를 실행할 수 있을 때 구현 기능은 `{supported:true, reason_code:"experimental_opt_in"}`, 구현하지 않은 기능은 `{supported:false, reason_code:"adapter_not_implemented"}`다([03 §6](03-adapters.md#6-capability登録と権限) 표). UI는 필수 기능 중 하나라도 `experimental_opt_in`이면 실험적 연결로 표시하고, Run.binding_snapshot의 같은 표시로 실험적 연결이 만든 결과를 구분한다. 시작·배정·재시도에서 필수 기능이 부족하면 `capability_<name>`으로 거절한다(`experimental_version_mismatch`는 제거됐다). 준비된 Run은 binding_snapshot의 동의만으로 실행하지 않는다: 실행 준비(prepare)와 실행 직전 검사에서 현재 저장된 binding 문서에 동의가 남아 있을 때만(AND) 적용하고, 그 사이 철회(null)나 binding 삭제가 있으면 적용하지 않는다(`consent_withdrawn`, [11 §7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers)) — 버전이 달라졌다는 이유로는 거절하지 않는다. 동의는 한 실행 대상에 대한 진술이다: binding.save가 저장된 문서와 비교해 `runtime`·`program`·`provider_id`·`auth_route`·`credential_ref`·`endpoint_ref` 중 하나를 바꾸면서 저장된 동의와 같은 값을 그대로 보내면(옮겨 온 동의) daemon은 null로 저장하고 응답도 null이다. 같은 저장에서 저장된 값과 다른 버전을 보내면 새 대상에 대한 새 동의로 보고 그대로 저장한다(같은 버전을 새 대상에 다시 동의하려면 대상 변경 저장 뒤 한 번 더 저장한다). 필드가 null이면 저장 문서와 응답에서 생략하며(`experimental_version?: string | null`), 생략과 null은 같은 뜻이다. 그래서 이 필드가 생기기 전 binding.save 요청을 같은 request_id로 재전송해도 지문이 같다.

binding.list/save/probe 응답과 새 Run 예약은 관측의 형식·대상·OS를 확인하고 현재 서버 registry에서 capability를 다시 계산한다. 레거시 binding의 public checked_at/capability만으로는 지원을 인정하지 않는다. 실행 준비에서는 immutable Run의 버전·확인 시각과도 일치하는 관측만 사용하며 이전 Run 기록을 덮어쓰지 않는다. DB 원문에 보존된 예전 capability는 실행 권한의 근거가 아니다. 시작·계획 적용·정책 변경·재시도·모델 재배정과 실행 예약/준비에서 역할 또는 실제 TaskKind의 필수 capability를 검사한다. 부족하면 CAPABILITY_UNSUPPORTED 또는 capability_* 대기로 남긴다. adapter.start 직전 worker는 같은 상한의 로컬 버전 조회를 다시 수행하며 조회 실패나 버전 불일치는 미전송 실패로 저장한다. 동일 버전을 보고하는 다른 파일과 조회/시작 사이의 교체를 막는 executable identity 고정은 남아 있다. binding mutation의 request cache는 mission_id 없이 같은 중복 규칙을 사용한다.

template.save와 verification.save도 신규 expected_revision=0/기존 CAS를 사용한다. 실제 반환 revision은1부터 시작한다. 내부 document의 revision은 expected_revision과 같아야 하며 서버가+1한다. binding 비활성은 enabled=false다. template/command의 기존 실행 snapshot을 삭제하지 않는다. verification command 목록의 repository_id는 daemon이 canonical Git common directory에서 배정한 값이다. 최초 mission.create가 repository registry를 생성하고 UI는 반환 mission snapshot에서 ID를 얻는다. 최초 작업에서 command가 없으면 draft를 만든 뒤 command 등록과 policy.update로 allowlist를 설정하고 start한다.

repository.inspect의 `verification_supported`는 이 daemon OS에 격리 검증 실행기(현재 macOS `sandbox-exec`)가 있을 때만 true다. false면 UI는 생성 단계에서 검증 명령 선택을 막는다. 선택하더라도 검증 Run은 CAPABILITY_UNSUPPORTED(`verification_unsupported_os`)로 실패한다.

**커밋하지 않은 변경을 base로(`mission.create.include_uncommitted`, `Mission.base_snapshot`).** true면 daemon이 사용자 작업 트리를 private snapshot commit으로 기록해 `base_oid`로 쓰고 `Mission.base_snapshot`(`head_oid`, `entry_count`)을 남긴다. `expected_base_oid`는 그대로 사용자 HEAD여야 하며 다르면 INVALID_STATE `base_changed`다. `follow_up_of`와 함께 오면 INVALID_ARGUMENT `follow_up_snapshot`이다. 작업 트리가 깨끗하면 snapshot을 만들지 않고 base는 HEAD, `base_snapshot`은 없다 — 그 미션은 일반 미션과 같다. commit은 `refs/iyagi/missions/<id>/inputs/base`로만 도달하게 하고 사용자 index·checkout·branch·HEAD는 쓰지 않는다(staging은 저장소 index 복사본에서만 한다). ignored 파일은 제외, untracked 파일은 포함한다. 저자·시각을 고정해 같은 tree·같은 HEAD면 같은 commit OID가 나오므로 재기록·요청 재생이 객체를 늘리지 않는다. unmerged path가 있으면 DIRTY_WORKTREE `unmerged_paths`, 항목 수·파일 크기·합계 한도를 넘으면 INVALID_ARGUMENT `snapshot_too_large`다. 요청 필드는 None이면 직렬화하지 않아 기존 create 요청 fingerprint를 유지한다(TS `include_uncommitted?: boolean | null`). start는 clean 검사 대신 기록한 `head_oid`가 현재 HEAD인지 확인하고 작업 트리를 다시 기록한다 — HEAD가 움직였으면 INVALID_STATE `base_changed`이고, 그 사이 사용자가 전부 커밋했으면 base는 HEAD로 돌아가고 `base_snapshot`은 사라진다.

**후속 작업(`mission.create.follow_up_of`, `Mission.follow_up_of`).** 설정하면 대상은 같은 저장소 ID의 completed·accepted_at 있음·현재 candidate가 있는 미션이어야 한다. 아니면(없는 ID 포함) INVALID_ARGUMENT `follow_up_not_accepted`. `expected_base_oid`는 그 확정 candidate의 commit OID여야 하며, commit이 daemon의 private candidate ref(`refs/iyagi/missions/<id>/candidates/<candidate>`)로 도달 가능하고 tree가 기록과 같아야 한다. 아니면 INVALID_ARGUMENT `follow_up_base_mismatch`. 새 미션의 base_oid가 그 commit이 되며 모든 작업 공간이 거기서 만들어진다. 요청 필드는 None이면 직렬화하지 않아 기존 create 요청 fingerprint를 유지한다(TS `follow_up_of?: Id | null`). start는 후속 작업이면 "HEAD == base"와 사용자 checkout clean 검사를 하지 않고, 같은 조건을 다시 확인해 실패 시 INVALID_STATE `follow_up_not_accepted`/`follow_up_base_mismatch`를 보낸다. clean 검사를 뺀 근거: 일반 미션의 `dirty_worktree`는 "base = 사용자가 보고 있는 HEAD" 의미를 지키기 위한 것이다(커밋하지 않은 변경이 base에서 조용히 빠지는 혼동 방지). daemon의 worker·통합·검증 worktree는 모두 `git worktree add --detach <daemon 경로> <base commit>`과 private index로 만들며 사용자 checkout의 파일·index·branch를 읽거나 쓰지 않는다. 후속 작업의 base는 사용자 HEAD가 아니라 확정 결과 commit으로 명시되므로 사용자 checkout 상태가 base를 바꾸지 않는다. UI는 후속 작업에서 dirty checkout을 막지 않아도 되며, 결과를 사용자 checkout에 가져올 때의 충돌은 별도 동선이다.

**멈춘 실행 사용자 확인(`mission.run.attest_exited`).** params는 Mutation + `run_id` + `attestation`(사용자가 확인한 문장 키, `[a-z0-9_]{1,64}`, 예: `process_absent_confirmed`), 결과는 MutationResult다. request_id 재생·semantic_revision 완화는 다른 사용자 mutation과 같다. 대상은 종료 증거 없이 실행 자리를 잡은 `unknown`/`interrupted` Run뿐이다. Exec가 없거나, 있으면 이미 Exited가 아니고 현재 daemon 인스턴스가 소유하지 않으며 native recovery가 재소유할 수 있는 신원(identity·started_at·group_reference와, 종류가 맞는 group_identity: cgroup v2+`cgroup` 또는 macOS guardian+`observed_tree`)이 없어야 한다(현재 daemon이 감독 중이거나 recovery로 감시할 수 있는 프로세스·stopping Run은 daemon이 종료를 확인한다). 그 밖에는 INVALID_STATE `attestation_not_applicable`. 효과는 관측된 종료 조정과 같은 해제다: 증거 artifact를 `reconciliation_ref`로 연결하고 `Run.reconciliation_kind = "user_attested"`, 연결 Exec는 `exited`(ended_at=확인 시각, exit_code=null)로 닫아 복원된 자원 예약을 해제, writer workspace는 Quarantined로 lease 해제, 작업은 `blocked:outcome_unknown_ended`. Run 상태·결과·시간은 바꾸지 않는다. 증거 문서는 `{version:1, kind:"user_attested", mission_id, run_id, fencing_token, attested_at, request_id, attestation, exec_before, exec_after}`이다. 이후 기존 복구 결정(`retry_reconciled_task`/`stop_reconciled_mission`)과 인수의 `acknowledged_reconciled_run_ids` 확인을 그대로 거치며, 인수 이벤트의 확인 기록에 `termination_kind`가 남는다. 제공자 결과와 외부 영향은 계속 미확인이다.

`Run.reconciliation_kind`(`"exec_exited" | "user_attested"`, 선택 필드)는 `reconciliation_ref`가 무엇을 증명하는지 구분한다. 감독기의 영속 Exited를 확인한 자동 조정은 `exec_exited`를 기록하며, 필드가 없는 기존 증거도 관측된 종료다. 이 값과 증거 문서 종류가 다르면 종료 증거로 인정하지 않는다.

**작업 공간 정리(`workspace.usage`, `workspace.cleanup`).** 자동 삭제는 없다. usage(`{mission_id: Id | null}`)는 daemon 소유 worktree 중 디스크에 남은 것의 수와 크기를 미션별로 돌려준다(null이면 작업 공간이 남은 모든 미션). 크기는 시간·항목 상한과 짧은 캐시를 둔 추정치이며 상한에 걸리면 하한값이다. `cleanable`은 정리 조건을 만족하고 남은 작업 공간 중 제거 조건(04 §8)을 모두 통과하는 것이 하나 이상 있을 때만 true이고(모두 남길 항목이면 false이며 `blocked_reason`은 null), 막힌 이유는 `blocked_reason`(`mission_active`: completed/cancelled/failed가 아님, `run_unreconciled`: 실행 자리를 잡은 Run 또는 Exited가 아닌 Exec가 있음)이다. cleanup(`{request_id, mission_id}`)은 같은 조건이 아니면 INVALID_STATE와 그 slug를 reason_code로 보낸다. 결과 `{removed, freed_bytes, kept[{path, reason}]}`의 kept reason은 `dirty`(git status에 변경), `run_active`(writer lease 또는 실행 자리를 잡은 Run), `not_daemon_owned`(미션의 daemon workspaces 폴더 밖, 사용자 checkout 안, 또는 worker workspace의 ownership marker 불일치), `quarantined`(불확실하거나 사용자 확인으로 정리한 실행의 격리 작업 공간, 조사용으로 보존), `unregistered_worktree`(저장소에 등록된 worktree가 아님), `repository_unavailable`(저장소 경로를 canonicalize할 수 없거나 worktree 목록을 읽지 못함), `status_unavailable`, `remove_failed`(`--force` 없는 제거를 Git이 거절, 예: 확인 뒤 생긴 변경), `deferred`(3초 예산 초과, 다시 요청하면 이어서 정리. 입력 ref 뒤 `input-indexes` 폴더가 예산을 넘기면 그 폴더 경로로도 온다)다. 모르는 reason은 UI가 일반 문구로 표시한다. 같은 request_id 재요청은 daemon 수명 안에서 최초 결과를 돌려주고, 재시작 후 재요청도 수렴한다. 보존 정책은 [04 §8](04-workspaces.md#8-결과-보관과-정리)이다.

mission.policy.update는 사용자 control 연결만 가능하며 running/paused/draft 상태에 허용한다. active Run snapshot에는 영향을 주지 않고 이후 dispatch만 새 정책을 쓴다. 축소로 기존 run이 위반하게 되면 INVALID_STATE와 pause/cancel 안내를 반환한다. `require_independent_review`를 true에서 false로 바꾸면 같은 커밋에서 아직 시작하지 않은 Review task(Run이 하나도 없고 planned/ready/blocked)를 계획 retire와 같은 `superseded`로 바꿔 확정 조건(필수 작업 succeeded/superseded)을 막지 않게 한다. 실행 중이거나 끝난 Review task와 그 Run은 그대로 둔다. mission.finding.resolve는 현재 candidate의 finding만 dismissed로 변경하며 reason_ref가 필수다. fixed 판정은 reviewer 결과를 처리하는 내부 engine만 생성한다.

## 4. Snapshot와 이벤트

변경 이벤트는 entity 본문 대신 invalidation ID를 전한다. notification 이름은 `mission.changed`, payload `{mission_id, latest_seq}`이며 100ms 단위 병합 가능하다. notification은 hint이므로 유실되어도 정확성이 유지되어야 한다.

1. mission.snapshot(null,null)은 read transaction에서 모든 현재 entity를 읽어 **불변 snapshot**을 만든다. artifact 본문 제외. 반환 at_seq가 같은 transaction 기준점이다.
2. 서버는 snapshot을 connection별 최대 2개, 각 8 MiB, 60초로 보관한다. read lock은 materialize 후 즉시 해제한다. page는 최대 50개이며 encode_frame 안에 들어오도록 줄인다.
3. 후속 page에는 snapshot_id와 opaque cursor를 둘 다 보낸다. cursor는 snapshot/page 위치에 binding; 다른 mission에서 사용하면 INVALID_ARGUMENT.
4. UI는 모든 page를 받은 뒤 한 번에 교체한다. loading 중 기존 화면을 유지하되 동기화 중 표시. snapshot 전체 수신 전 서로 다른 revision의 entity를 혼합하지 않는다.
5. mission.events(after_seq)는 commit된 순서만 반환한다. 중복 seq는 무시하고 gap 또는 변경 이벤트를 받으면 새 snapshot을 요청한다. 새 snapshot 이후 이전 seq 이벤트는 버린다.
6. snapshot 도중 변경이 오면 dirty flag를 켠다. 현재 snapshot commit 후 watermark를 비교하고 필요한 경우 다시 갱신한다. 동시에 snapshot 요청은 mission당 하나만 둔다.
7. 이벤트가 계속 나와도 snapshot은 불변이므로 page 수신을 매번 처음부터 취소하지 않는다. pause/cancel/decision는 snapshot refresh와 별개 제어 요청으로 보낸다.
8. 재접속은 snapshot부터 시작한다. snapshot expiry는 재생성, event retention gap은 CURSOR_EXPIRED와 snapshot 재요청이다.

모델 text delta는 mission revision을 매 token 변경하지 않는다. 별도 bounded activity spool에 기록하고 50ms batch로 선택한 실행에 전달한다. `mission.activity`는 완료된 immutable chunk ref를 반환한다. offset은 논리 activity byte offset, complete는 run 종료와 최종 chunk 저장이 모두 확인됐을 때 true. UI는 200개 활동을 넘으면 이전 부분을 pagination한다.

## 5. 저장소

[0003_orchestration.sql](../../crates/term-storage/migrations/0003_orchestration.sql)은 O1 migration 0003의 입력이다(R1 0002 agent-sessions 등록 이후 다음 번호로 확정). 본 문서 링크와 verifier는 이 실제 migration 파일을 가리킨다(중복 정답 없음). 기존 0001/0002를 수정하지 않는다.

현재 migration runner는 schema 적용 COMMIT 후 version row를 기록한다. O03은 **새 migration을 추가하기 전에** schema와 version insert를 동일 transaction으로 수정해야 한다. 기존 DB에서 version row가 없는 테이블이 이미 있는 경우 자동 DROP/IF NOT EXISTS로 덮지 않고 MIGRATION_FAILED와 복구 지침을 낸다. rollback fixture와 0001→0002→O1 upgrade 시험을 추가한다.

projection document JSON은 contracts.ts Entity의 value다. SQL의 상태/ID와 JSON의 상태/ID는 writer가 같은 transaction에 동일 값으로 생성한다. SQL CHECK는 enum/참조/unique 일부만 검사한다. JSON 본문 스키마와 cross-mission 참조는 Rust가 검증한다. UI가 JSON 열을 직접 쓰지 않는다.

Run의 binding_snapshot과 context_ref, Candidate, Verification은 생성 후 불변이다(단 Run의 lifecycle/usage/provider IDs/result는 해당 이벤트에서 갱신). request cache, event, outbox가 없으면 자동 작업 시작을 허용하지 않는다.

## 6. 권한·보존

O1은 기존 사용자 전용 UDS/named pipe 인증을 사용한다. secret store credential 값은 adapter 자식에게만 전달하며 RPC/error/event/context/log에 포함하지 않는다. 프로젝트 환경 파일을 통째로 context에 넣지 않는다. 같은 OS 사용자에 대한 완전한 sandbox를 주장하지 않는다.

목표·메시지·결과는 사용자가 O1 작업을 시작할 때 로컬 저장된다는 안내를 표시한다. 기본 terminal 기록 정책은 바꾸지 않는다. 활성 mission content는 자동 삭제하지 않는다. 완료 후 기본 30일 보존, pin 가능. 정리 시 메타데이터와 hash는 남기고 body 상태를 expired로 기록한다. 참조가 있는 body를 조용히 삭제하지 않는다. UI는 CONTENT_EXPIRED로 구분한다.

archive는 목록 분류다. 삭제나 cancel이 아니다. 활성 작업 archive는 INVALID_STATE. 숨기기는 UI 로컬 동작으로 언제나 가능하다.

## 7. 오류 표시와 재시도

오류 code는 contracts.ts의 ErrorCode, 사용자가 읽는 문구는 i18n 키다. upstream 응답 body/토큰/전체 argv를 details로 노출하지 않는다. request correlation ID는 제공한다.

- REVISION_CONFLICT: 최신 상태 동기화 후 사용자가 변경 내용을 다시 확인. 같은 클릭을 자동 새 revision으로 재시도 금지.
- AUTH_REQUIRED/MODEL_UNAVAILABLE: binding 수정 화면 연결; 자동 대체 금지.
- PROVIDER_RATE_LIMITED: reset/retry_after가 확인될 때 표시. 실행 접수 여부부터 검사.
- OUTCOME_UNKNOWN: recovery decision 생성; 정상 실패와 구별.
- STORAGE_UNAVAILABLE: 새 실행·변경 접수 중지, 진행 중 adapter 안전 중단 시도, 오류를 메모리에만 성공 처리하지 않음.
- STALE_DECISION/STALE_CANDIDATE: 현재 질문/후보로 이동; 이전 답을 새 대상에 적용 금지.

### reason_code

`MissionErrorDetails.reason_code`는 UI가 번역·행동 버튼을 붙이는 안정 slug다. code보다 구체적이며 message 문구는 계약이 아니다. 아래 표가 daemon mission 오류가 보내는 reason_code의 전체 목록이다. 기존 이름은 바꾸지 않으며, 표에 없는 값을 받으면 code 기준 일반 문구로 표시한다. `capability_<name>`만 접두사 규칙이다. Binding `Support.reason_code`(capability 근거)는 오류 details가 아니므로 이 표에 포함하지 않는다 — 전체 값과 뜻은 [11 §3.1](11-local-evidence.md#31-supportreason_code-값)이 정답이다. `experimental_version_mismatch`는 [11 §3.4](11-local-evidence.md#34-bindingexperimental_version-의미-변경)·[§7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers)에 따라 더 이상 발생하지 않아 아래 표에서 제거했다(과거 기록에는 남을 수 있다). workspace.cleanup 결과의 `kept[].reason`도 오류가 아니며 §3에 목록이 있다(`quarantined` 추가: 격리 작업 공간 기본 보존).

| reason_code | code | 발생 경로 | UI 행동 |
|---|---|---|---|
| `git_unavailable` | INVALID_STATE | repository.inspect, mission.control start | Git 설치 안내. daemon이 `git`을 실행하지 못함(저장소 문제 아님) |
| `not_a_repository` | INVALID_ARGUMENT | repository.inspect, start | 다른 폴더 선택 또는 `git init` 안내 |
| `no_commits` | INVALID_ARGUMENT | repository.inspect, start | 첫 커밋 생성 안내(HEAD 없음) |
| `dirty_worktree` | DIRTY_WORKTREE | start | 변경 커밋/stash 또는 `include_uncommitted` 안내. 후속 작업(`follow_up_of`)과 `base_snapshot` 미션 start는 검사하지 않는다 |
| `unmerged_paths` | DIRTY_WORKTREE | mission.create, start | merge/rebase를 끝내거나 중단한 뒤 다시 시도. 충돌 표시를 base로 기록하지 않는다 |
| `snapshot_too_large` | INVALID_ARGUMENT | mission.create, start | 커밋하지 않은 집합이 한도 초과 — 커밋·ignore하거나 HEAD에서 시작 |
| `follow_up_snapshot` | INVALID_ARGUMENT | mission.create | 후속 작업에는 `include_uncommitted`를 보내지 않는다 |
| `path_not_accessible` | INVALID_ARGUMENT | mission.create | 경로 확인(Git이 아닌 접근 가능한 폴더는 draft 생성 허용) |
| `repository_changed` | INVALID_STATE | start | 현재 저장소로 새 작업 생성 |
| `base_changed` | INVALID_STATE | mission.create, start | 현재 HEAD로 새 작업 생성. 후속 작업은 HEAD 대신 확정 commit 도달 가능성을, `base_snapshot` 미션은 기록한 HEAD 유지를 검사 |
| `bindings_missing` | INVALID_ARGUMENT | start | 모델 연결 허용 목록 설정 |
| `lead_missing` | INVALID_ARGUMENT(start), MODEL_UNAVAILABLE(`replan`) | start, decision.answer | Lead 역할 연결 |
| `lead_not_allowed` | INVALID_ARGUMENT | start | Lead 연결을 허용 목록에 추가 |
| `builder_missing` | INVALID_ARGUMENT | start | Builder 역할 연결(모든 미션에 필수) |
| `reviewer_missing` | INVALID_ARGUMENT | start | `require_independent_review=true`인데 Reviewer 연결 없음. Reviewer 지정 또는 리뷰 생략 |
| `follow_up_not_accepted` | INVALID_ARGUMENT(create), INVALID_STATE(start) | mission.create, start | 같은 저장소의 확정 완료 미션을 선택 |
| `follow_up_base_mismatch` | INVALID_ARGUMENT(create), INVALID_STATE(start) | mission.create, start | base를 이전 미션의 확정 결과 commit으로 다시 지정. commit에 도달할 수 없으면 일반 새 작업 생성 |
| `model_unavailable` | MODEL_UNAVAILABLE | start | 역할별 활성 연결·모델 지정 |
| `capability_<name>` | CAPABILITY_UNSUPPORTED | start, policy.update, plan 적용, task.control retry/reassign, 결정 재시도 | 설치 재확인 또는 호환 연결 선택. `<name>`은 부족한 capability: `structured_result`·`events`·`cancel`·`scoped_write`(쓰기 작업)·`read_only`(그 외). 같은 값이 작업 `blocked_code`에도 쓰인다 |
| `experimental_consent_withdrawn` | CAPABILITY_UNSUPPORTED | 실행 준비·시작 직전 검사에서 현재 binding 문서가 실험적 사용 동의를 철회(null)했거나 binding이 삭제돼 `experimental_opt_in` 기능을 적용할 수 없을 때([11 §7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers) `consent_withdrawn`) | 설정에서 다시 동의하거나 다른 연결 선택 |
| `illegal_transition` | INVALID_STATE | mission.control | 현재 상태 재동기화 |
| `plan_format_rejected` | RESULT_INVALID, PLAN_CYCLE | mission.plan.apply, decision.answer(`apply`), Lead 계획 결과 | 계획 형식/그래프 오류. Lead 형식 수리 또는 수정 요청 |
| `revision_mismatch` | REVISION_CONFLICT | mission.accept | 최신 상태 동기화 후 다시 확인 |
| `candidate_mismatch` | STALE_CANDIDATE | mission.accept | 현재 후보로 이동 |
| `not_awaiting_acceptance` | INVALID_STATE | mission.accept | 실행 중(running) 작업만 인수 가능 |
| `required_task_not_succeeded` | INVALID_STATE | mission.accept | 필수 작업 완료/대체 필요 |
| `verification_missing` | INVALID_STATE | mission.accept | 필수 검증이 현재 후보에 없음 |
| `verification_not_passed_on_candidate` | INVALID_STATE | mission.accept | 필수 검증 실패/미통과 |
| `integrity_policy_unmet` | POLICY_DENIED | mission.accept | 강제(enforced) 검증 필요 |
| `observed_not_acknowledged` | POLICY_DENIED | mission.accept | observed 검증 확인 체크 |
| `review_incomplete` | INVALID_STATE | mission.accept | 독립 리뷰 대기 |
| `open_finding` | INVALID_STATE | mission.accept | blocking/major 지적 해결 또는 근거 해제 |
| `human_check_missing` | INVALID_STATE | mission.accept | 사람 확인 요구사항 체크 |
| `live_run` | INVALID_STATE | mission.accept | 실행 종료 대기 |
| `unknown_run` | INVALID_STATE | mission.accept | 불명 실행을 `acknowledged_reconciled_run_ids`로 확인 |
| `open_blocking_decision` | INVALID_STATE | mission.accept | 열린 차단 결정 답변 |
| `policy_update_required` | INVALID_ARGUMENT | decision.answer(`adjust_limits`) | 답변 대신 policy.update 편집기 열기 |
| `option_required` | INVALID_ARGUMENT | decision.answer(provider 차단 결정) | 선택지 선택 |
| `option_invalid` | INVALID_ARGUMENT | decision.answer(provider 차단 결정, 통합 충돌의 `resolve_and_reintegrate`를 결정이 내지 않았거나 Integrator 역할 연결 없이 선택. 정책이 Integrator 역할 자체를 허용하지 않으면 reason 없는 POLICY_DENIED) | 현재 결정의 선택지 다시 선택 |
| `attestation_not_applicable` | INVALID_STATE | mission.run.attest_exited | 종료 증거 없는 unknown/interrupted 실행만 확인 가능. 감독 중이거나 recovery가 재소유할 수 있는 native 신원이 있거나 이미 증거가 있으면 대기·재동기화 |
| `mission_active` | INVALID_STATE | workspace.cleanup(usage의 `blocked_reason`) | 미션 종료(완료·취소·실패) 후 정리 |
| `run_unreconciled` | INVALID_STATE | workspace.cleanup(usage의 `blocked_reason`) | 종료 증거 없는 실행을 확인(`mission.run.attest_exited`)하거나 조정 대기 |
| `answer_not_text` | INVALID_ARGUMENT | decision.answer | 선택지와 함께 보내거나 plan 결정에 쓰는 답변은 UTF-8 텍스트 |
| `answer_too_large` | INVALID_ARGUMENT | decision.answer | 위 답변은 64 KiB 이하 |
| `model_not_changed` | INVALID_STATE | decision.answer(`change_model`) | 먼저 task.control reassign으로 다른 연결 지정 |
| `lead_plan_in_progress` | INVALID_STATE | decision.answer(`replan`) | 진행 중(planned/ready/running/awaiting_input/awaiting_review)인 Lead Plan 작업이 있음. 다른 이유로 blocked인 Plan 작업은 세지 않음. 계획 대기 또는 Lead에게 메시지 |
| `plan_limit` | PLAN_LIMIT | decision.answer(`replan`) | 작업/계획 한도 도달, 후속 작업 사용 |
| `mission_not_active` | INVALID_STATE | decision.answer(provider 차단 결정) | running/pausing/paused에서만 처리 |
| `verification_unsupported_os` | CAPABILITY_UNSUPPORTED | 검증 실행(verify Run 실패로 기록) | 이 OS에서는 검증 명령 미지원. 생성 단계에서 `verification_supported=false`로 미리 막는다 |
| `integration_empty` | INVALID_STATE | 엔진 통합(실행 실패 기록) | 통합할 변경 없음 |
| `integration_conflict` | INVALID_STATE | 엔진 통합(실행 실패 기록) | 충돌 결정으로 이어짐 |
| `excluded_source` | INVALID_STATE | 엔진 통합(실행 실패 기록) | 제외된 작업이 통합 입력에 남음 |
| `verification_task_mismatch` | INVALID_STATE | 엔진 검증 준비(실행 실패 기록) | 내부 불일치, 재동기화 |
| `verification_already_finished` | INVALID_STATE | 엔진 검증 준비 | 내부 불일치, 재동기화 |
| `verification_command_mismatch` | INVALID_STATE | 엔진 검증 준비 | 내부 불일치, 재동기화 |
| `verify_run_missing` | INVALID_STATE | 엔진 검증 결과 기록 | 내부 불일치, 재동기화 |
| `verify_fence_changed` | INVALID_STATE | 엔진 검증 결과 기록 | 오래된 검증 결과 무시 |
| `verify_task_missing` | INVALID_STATE | 엔진 검증 결과 기록 | 내부 불일치, 재동기화 |
| `findings_require_review_run` | INVALID_STATE | 엔진 리뷰 결과 기록 | 내부 불일치, 재동기화 |
| `deterministic_cancelled_before_launch` | INVALID_STATE | 엔진 검증/통합 명령 시작 전 취소 | 취소로 처리(사용자 표시 불필요) |

R1 터미널 RPC(`RpcError.details` JSON)에서는 `claude_transcript_missing`만 쓴다. mission 오류 표와 별개다.

## 8. 결정 선택지와 답변 기록

DecisionOption.id는 daemon이 정한 안정 slug다. UI는 id로 번역하고 `label`(영어)은 알 수 없는 id의 폴백으로만 쓴다. product 결정의 선택지는 provider가 제시한 값이라 고정 id가 아니다.

| 결정 | kind | option id | 답변 효과 |
|---|---|---|---|
| 계획 제안(자동 적용 꺼짐) | plan | `apply` | 제안 적용 |
| | | `revise` | 새 Lead plan task. answer_ref 텍스트는 objective(`plan_revision_request`)의 `user_request`로 전달 |
| provider 승인 요청 | approval | `accept`, `decline` | 같은 provider 요청에 그대로 전달 |
| 작업 실패 | recovery | `retry_failed_task`(시도 남을 때만), `stop_failed_mission` | 재시도(새 Run) / 실패로 종료 |
| 결과 불명 실행 확인 | recovery | `retry_reconciled_task`(시도 남을 때만), `stop_reconciled_mission` | 새 시도 / 중단 |
| 재시작 후 미전송·불명 Run | recovery | `resume_unsent`(미전송 보류일 때만), `stop_mission` | 보류 Run 시작 / 중단 |
| provider `blocked` 결과 | recovery | `retry_with_instruction`(시도 남을 때만) | 작업 Ready. answer_ref 텍스트는 그 작업 다음 Run 문맥에 새 입력(queued 메시지)으로 전달 |
| | | `change_model`(시도 남을 때만) | 먼저 `mission.task.control` reassign으로 다른 연결을 지정한 뒤 답변. 연결이 그대로면 `model_not_changed`. 임의 대체 없음 |
| | | `replan`(Plan 작업이 아닐 때) | Lead plan task 생성. objective(`provider_blocked_replan`)에 차단 코드·보고서(최대 32 KiB)·사용자 지시 포함. 차단 작업은 계획이 retire/대체할 때까지 blocked. 단계: planning/implementing은 유지(실패 수리와 같음), integrating이면 planning으로 바꾸고 `candidate_id`를 비움(후보 제외와 같음, 다음 통합이 새 후보를 만듦), 후보가 만들어진 뒤 단계(validating/reviewing 등)면 후보를 유지한 채 planning(검증·리뷰 수리와 같음) |
| | | `stop_mission` | 작업 중단 |
| 실행 한도(automatic_start_limit·active_time_limit) | budget | `stop_mission` | 작업 중단 |
| | | `adjust_limits` | UI 동선 전용. 답변하면 `policy_update_required`. policy.update로 한도를 올리면 결정이 obsolete되고 작업이 풀린다 |
| 리뷰 수정 한도 소진 | budget | `stop_review_repair` | 작업 중단 |
| | | `adjust_limits` | 위와 같음(max_repair_cycles). 근거를 남긴 finding.resolve로도 해소 |
| 비용 보류 | budget | `stop_cost_mission` | 작업 중단. 비용 한도는 policy.update |
| 메시지 재계획 불가(작업·계획 한도, Lead 없음) | budget | `stop_mission` | 작업 중단 후 후속 작업 사용 |
| 통합 충돌 | conflict | `resolve_and_reintegrate`(Integrator가 허용 역할이고 역할 연결이 있을 때만), `exclude_candidate`, `stop_mission` | 해결 후 재통합 / 후보 제외 후 Lead 재계획 / 중단. resolve 판정 순서: 정책 allowed_roles에 Integrator가 없으면 POLICY_DENIED(역할 허용 안내), 허용됐지만 결정이 이 선택지를 내지 않았거나 Integrator 연결이 없으면 `option_invalid` |

provider `blocked` 결정은 blocking=false(독립 작업 계속), requesting_run_id=차단 결과를 낸 Run, affected_task_ids=[차단 작업]이다. question_ref는 `application/json`:

```json
{"kind":"provider_blocked","version":1,"task_id":"…","task_title":"…","run_id":"…","code":"…","report_ref":{"id":"…","sha256":"…","bytes":"…","media_type":"text/plain"},"attempt_count":1,"max_attempts_per_task":3,"message":"English fallback"}
```

report_ref는 결과가 만료·손상됐으면 null이다. 한 Run에는 현재 결정이 하나만 열리며, 시도 한도·plan revision·후보가 바뀌면 obsolete 후 다시 만든다. `replan` 답변 뒤 Lead 계획이 끝났는데도 작업이 여전히 같은 Run으로 차단돼 있으면 결정을 다시 연다.

**답변 기록 메시지.** decision.answer는 role=system Message를 같은 트랜잭션에 만든다. 대상은 affected_task_ids가 하나면 그 작업이다. 본문 형식:

- answer_ref만 있고 option_id가 없으면 answer_ref 본문(text) 그대로.
- option_id가 있으면 `application/json` 문서. answer_ref가 있으면 UTF-8 64 KiB 이하 본문을 `answer_text`로 함께 싣는다.

```json
{"kind":"decision_answer","version":1,"decision_id":"…","decision_kind":"recovery","option_id":"retry_with_instruction","option_label":"Retry with an instruction","answer_ref":null,"answer_text":null}
```

- `exclude_candidate`는 기존 제외 근거 문서(검증 대상)를 본문으로 쓴다.

기존 `{"decision_id","option_id"}` 문서는 위 형식의 부분집합이다. recovery/conflict/failure/cost 답변은 delivered(기록)이고, `retry_with_instruction`·`change_model`에 answer_ref가 있으면 queued + route intent로 다음 Run 문맥에 새 입력으로 들어간 뒤 provider 접수 시 delivered가 된다. product 답변은 대상 작업의 다음 Run 문맥에 queued로 포함된다. plan `revise`의 사용자 텍스트는 새 plan task objective로 전달되며 거절된 제안(64 KiB 이하일 때)도 함께 싣는다.
