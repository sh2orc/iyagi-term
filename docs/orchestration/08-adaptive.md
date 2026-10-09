# 08. 적응형 팀과 프로젝트 학습

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. O1과 확장 경계

O1에서 반드시 구현: 역할 template, 계획 재분해, 전문 자문, 실패 대응, 구조화된 맥락 인계, 공유 결정 기록, 독립 리뷰, 질문 통합, 부분 재실행. 자동 모델 점수 학습과 경쟁 해법의 자동 채택은 O19다. 이 구분은 품질 목표 축소가 아니라 측정되지 않은 최적화를 기본 동작에 넣지 않기 위한 경계다.

## 2. 팀 template

template은 role binding과 policy의 이름 있는 버전이다. mission 생성 시 복사하며 template 변경이 실행 중 mission에 전파되지 않는다. 기본 template 예시는 balanced/strict-review/fast-feedback이고 특정 모델명은 포함하지 않는다. 최초 설정에서 사용자가 실제 binding을 연결한다.

balanced 기본값: max_parallel=4, attempt=3, repair=3, independent_review=true, enforced_verification=false, unknown_cost=allow_with_notice, network=false, automatic_plan_apply=true, recovery_unsent=true. dollar 상한은 null이며 4시간/64 starts/45분 run 상한은 유지한다. strict-review는 enforced_verification=true. fast-feedback은 parallel=2이며 검증 생략을 뜻하지 않는다.

사용자가 mission policy를 바꾸면 새 revision을 만들고 이후 dispatch에 적용한다. 허용범위 축소로 기존 실행과 충돌하면 먼저 pause/cancel 범위를 표시한다. 변경으로 실행 중 native 권한이 즉시 축소됐다고 표시하지 않는다. 비용 상한 확대/다른 auth route 추가는 사용자 UI mutation만 가능하다.

## 3. Agent 위임 도구

agent-facing 도구는 UI의 전체 RPC 권한을 갖지 않는다. daemon이 run별 임시 capability token을 발급하고 run ID/mission ID/역할/허용 operation/expiry에 binding한다. token은 provider prompt 대신 runtime의 로컬 tool 연결 환경에만 전달한다. 만료/terminal run의 token은 거절한다.

필수 internal 도구:

| 도구 | 입력 | 결과/제약 |
|---|---|---|
| `task.request_consult` | specialty, question_text, source_artifact_ids | request ID; allowlisted specialist role만, task당3회/depth3 |
| `task.request_replan` | reason_text, affected_task_ids | Lead에 제안; 직접 DAG 수정 권한 없음 |
| `task.report_knowledge` | kind,text,sources,paths | proposed knowledge; source 없는 fact 거절 |
| `task.read_artifact` | artifact_id,offset,max_bytes | 같은 mission과 context allowlist만, 4KiB chunk |
| `task.request_input` | question,options,affected_task_ids | product decision; tool approval과 별개 |

request_consult는 agent가 하위 프로세스를 직접 spawn하는 기능이 아니다. 엔진이 현재 task를 checkpoint하고 Run 종료를 확인한 뒤 consult task를 생성한다. 원 task는 blocked:awaiting_consult로 유지하고 자문 완료 후 새 Run에 결과를 인계한다. active scheduler slot을 상호 대기하며 고갈시키지 않는다. 이 요청도 attempt/start/time budget에 포함한다.

내부 도구 응답 본문은 untrusted input이다. 모델이 tool 이름을 텍스트로 출력했다고 실제 도구 요청으로 실행하지 않는다. structured tool 호출 또는 검증된 ProviderResult만 채택한다.

## 4. 결정 충돌과 검토

상반된 전문가 의견은 양쪽 source/finding을 남긴다. Lead는 논쟁을 하나의 질문과 필요한 evidence로 정리한다. 이미 allowlist에 있는 benchmark/test로 해결할 수 있으면 verify task를 제안한다. 새 명령 실행이나 제품 방향 선택이 필요하면 사용자 decision을 만든다.

review findings는 severity와 근거를 기준으로 다룬다. 모델 수에 따른 다수결, 긴 설명, 자기 confidence만으로 승자를 정하지 않는다. 사실 검증과 제품 선택을 구분한다.

## 5. O19 자동 routing 알고리즘

처음에는 세 가지 사용자가 선택하는 전략만 제공한다: 품질 우선/빠른 피드백/예산 우선. 모델 registry에 일반적인 성능 서열을 하드코딩하지 않는다.

후보 필터 순서: role capability → auth/provider 허용범위 → context 크기 → known availability → hard policy → 관측 quota. 하나도 없으면 MODEL_UNAVAILABLE 또는 관련 오류와 decision. 사용 가능하지만 품질 기록이 없으면 unknown으로 남긴다.

평가 단위는 repository/task kind/role/runtime version/model ID/effort 조합이다. 측정 항목은 독립 인수 통과율, blocking defect, 사용자 수정 시간, 전체 완료 시간, 알려진 사용량과 미관측률이다. 성능 score 하나로 모든 축을 숨기지 않는다.

도입 순서:

1. 수동 template 실행의 outcome을 수집한다.
2. 기록으로 추천을 만들되 shadow mode에서 실제 배정은 변경하지 않는다.
3. 동일 task strata의 holdout에서 frozen baseline과 비교한다.
4. 사용자가 추천을 채택하면 routing config version을 생성한다.
5. 새 config에서 회귀가 관측되면 이전 config로 되돌릴 수 있다. 작업 중인 run binding은 바꾸지 않는다.

O19의 실제 scoring formula와 최소 sample/통계 기준은 평가 데이터 분포를 확인한 ADR로 고정한다. 그 전에는 `학습된 최적 배정`을 구현했다고 표시하지 않는다.

## 6. 선택적 복수 해법

사용자가 비교 전략을 선택한 task만 동일 input snapshot에서 독립 workspace에 복제한다. 두 해법 모두 budget에 포함하고 결과를 서로 보지 않게 한다. 동일 frozen requirement/검증 suite/독립 reviewer로 평가한다. 자동 승자 채택은 사전 정책이 있을 때만 가능하다.

candidate 간 비교에는 정확성·변경 범위·검증·사용량·시간을 함께 보여준다. 테스트가 없는 후보를 빠르다는 이유만으로 승자로 선정하지 않는다. 실패/중단한 대안도 평가 기록에 남긴다. 비교 run을 일반 성공률 표본에 중복 집계하지 않는다.

## 7. 다음 기능에 공통인 불변식

원격 실행, 외부 서비스, 새로운 runtime, 프로젝트 memory를 추가해도 task 계약과 candidate/evidence 연결은 유지한다. unknown outcome, 명시적 auth route, 실행과 view 분리, 제한된 자동 재시도는 plugin이 우회할 수 없다. 외부 feature의 실패가 기본 terminal 입력·취소를 막으면 출시 불가다.
