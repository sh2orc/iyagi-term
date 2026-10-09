# iyagi 멀티 서비스 오케스트레이션 구현 명세

작성일: 2026-09-13 · 계약 버전: O1 · 상태: **구현을 위한 설계 기준, 기능 구현·서비스 호환성 검증 전**.

## 1. 목표와 적용 범위

사용자가 개발 목표를 맡기면 여러 AI 코딩 서비스가 조사·설계·구현·검토·통합을 수행하고, Iyagi이 작업 상태와 검증 근거를 관리한다. 사용자는 작업당 탭 하나에서 Lead와 대화하고, 참여 에이전트와 실행 화면·변경 결과를 확인한다.

이 명세는 아이디어 목록이 아니라 구현 계약이다. `MUST`는 필수, `MUST NOT`은 금지다. 문서의 기본 수치와 성능 수치는 측정 결과가 아니라 출시 전 검증할 목표다. 지원 표시는 설치된 실행 도구 버전·인증 방식·OS의 실제 시험 결과로 결정한다. GPT-5.6-sol과 GLM-5.3은 사용자가 제시한 역할 배정 예시이며 사용 가능성을 하드코딩하지 않는다.

구현 요청 전까지 이번 산출물은 문서·참조 계약·명세 검증기만 추가한다. 실행 도구 설치, 유료 모델 호출, 기존 설정 변경, DB migration 적용, 실제 작업 생성은 수행하지 않는다.

## 2. 문서 읽기 순서와 정답의 위치

| 순서 | 문서 | 이 문서가 결정하는 것 |
|---|---|---|
| 1 | [제품·아키텍처](docs/orchestration/00-product.md) | 제품 경계, 기존 코드 재사용, 책임과 단계 |
| 2 | [타입·RPC·저장소](docs/orchestration/01-contracts.md) | 데이터 의미, 트랜잭션, 프레임, 동기화 |
| 3 | [실행 엔진](docs/orchestration/02-engine.md) | 계획, 상태 전이, scheduling, 중단·복구 |
| 4 | [서비스 adapter](docs/orchestration/03-adapters.md) | Codex·Claude Code·OpenCode 연동과 capabilities |
| 5 | [작업 공간·맥락·검증](docs/orchestration/04-workspaces.md) | Git 격리, 산출물, 인계, 완료 증거 |
| 6 | [UI 상세 계약](docs/orchestration/05-ui.md) | 레이아웃, 모든 주요 행동, 빈 화면과 오류 |
| 7 | [시험·출시 판정](docs/orchestration/06-verification.md) | 결정적 시험, GUI 시나리오, 품질 비교 |
| 8 | [구현 티켓](docs/orchestration/07-tickets.md) | 순서, 변경 경로, 구현 절차, 완료 조건 |
| 9 | [적응형 팀·학습](docs/orchestration/08-adaptive.md) | 조건부 전문가, 전략 선택, 평가 기반 개선 |
| 10 | [구현 보충과 예제](docs/orchestration/09-implementation-recipes.md) | 최초 설정, Exec 자원 연결, 메시지/결과 조건표 |
| 11 | [역할 지시문과 지시 파일 격리](docs/orchestration/10-role-instructions.md) | 저장소 역할 md의 고정·전달, 실행 도구 자체 지시 파일의 차단과 관측 |
| 12 | [로컬 호환성 증거와 신뢰 등급](docs/orchestration/11-local-evidence.md) | 증거 층, 자가 진단, 실행 관측 승격, 연결당 동의 |

기계 판독 계약:

- [contracts.ts](docs/orchestration/contracts.ts): wire 필드와 enum, RPC params/result의 정답. 제품 코드에 직접 import하지 않고 Rust/ts-rs 타입으로 옮긴다.
- [contracts.examples.ts](docs/orchestration/contracts.examples.ts): 실제 API를 호출하지 않는 타입 검증용 작업·팀·병렬 계획 예제.
- [defaults.json](docs/orchestration/defaults.json): 모든 O1 초기 상한과 timeout의 정답.
- [states.json](docs/orchestration/states.json): 허용 상태 전이의 정답. 조건은 02 문서에 있다.
- [0003_orchestration.sql](crates/term-storage/migrations/0003_orchestration.sql): SQLite **migration 0003 입력**(R1 0002 agent-sessions 다음 번호). 실제 migration 등록은 O03에서 한다.
- [cases.json](docs/orchestration/cases.json): 양성·음성 상태/계획 시험 입력.
- [verify_spec.py](docs/orchestration/verify_spec.py): 이 문서 묶음의 링크·기본값·migration 0002 DDL·사례 검사.

동일 필드를 산문에서 재정의하지 않는다. 변경 시 정답 파일과 해당 사례를 함께 수정한다. 문서 간 실제 모순이 발견되면 임의 선택 대신 이 명세에 정정 근거를 남긴다.

## 3. 기존 명세와의 우선순위

기존 구현 명세(R1)는 일반 터미널과 R1 managed workload의 계약으로 유지한다. O1 기능에 한해 아래 변경을 적용한다. 이전 구현의 완료 여부를 이 문서가 다시 판정하지 않는다.

| 이전 경계 | O1의 명시적 변경 | 그대로 유지하는 경계 |
|---|---|---|
| 플랫폼 모델/provider 선택 없음 | O1 role binding에서 runtime/provider/model/auth 경로를 선택 | 일반 터미널의 argv·사용자 전역 CLI 설정은 수정하지 않음 |
| 대화·프롬프트 저장 없음 | O1 목표·메시지·인계 묶음을 접근 제한된 artifact 저장소에 저장 | 기존 agent_sessions에는 대화나 비밀값을 추가하지 않음 |
| Task:Attempt:Workload:PTY 1:1 | O1 Mission/Task/Run을 새 namespace로 생성; 프로세스는 별도 Exec handle | 기존 tasks/attempts/workloads/sessions 의미는 변경하지 않음 |
| daemon 재시작 후 자동 재실행 없음 | O1은 영속 상태 조정 후 검증된 미전송 실행만 정책에 따라 시작 | 결과 불명인 외부 실행은 자동 중복 실행하지 않음 |
| 탭은 terminal split root | terminal/mission/agent-view 구분; mission 탭에는 PTY가 없어도 됨 | 일반 터미널의 닫기·IME·단축키 동작은 회귀 방지 |
| DAG·고급 연결 후속 범위 | O1의 제품 기능으로 구현 티켓 지정 | OS별 자원 강제 제한의 실제 능력을 과장하지 않음 |

`O1`은 기존 R1/R2/R3 번호와 별개다. O1을 추가하기 위해 과거 migration 0001/0002 또는 그 입력 파일을 수정해서는 안 된다.

## 4. 완료 기준

O01–O18 전체의 필수 조건과 세 OS capability별 게이트가 통과해야 O1 제품 구현 완료다. UI mock 통과, 한 번의 실제 모델 성공, fake adapter 시험만으로 제품 완료를 선언하지 않는다. O19–O22는 같은 목표에 속한 확장 티켓이며, 미완료 기능은 지원·자동 최적화 문구에 포함하지 않는다.

최종 경험은 **목표 입력 → 실행 팀 확인 → 병렬 실행 관찰 → 필요한 결정 → 통합 결과와 검증 확인 → 인수**까지 이어져야 한다. 인수는 로컬 결과 확인이며 push·PR·merge·배포와는 별도 외부 동작이다.

## 5. 지금 실행할 수 있는 검사

```sh
python3 docs/orchestration/verify_spec.py
python3 docs/implementation/verify_spec.py
git diff --check
```

이 검사는 문서 자산의 정합성만 확인한다. 제품 구현이나 실제 provider 접속을 검증하지 않는다. 런타임 시험 명령과 출시 증거는 06/07 문서에 정의한다.
