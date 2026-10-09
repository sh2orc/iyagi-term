# 11. 로컬 호환성 증거와 신뢰 등급

작성일: 2026-09-19 · 계약 버전: O1 · 상태: **구현 계약(O22)**. [03 §6](03-adapters.md#6-capability登録と権限)의 "정확한 버전 일치 증거만 인정" 규칙을 대체한다. 이 문서와 03 §6가 다르면 이 문서가 정답이다.

## 1. 문제

지금까지 capability 증거는 작성자 머신에서 녹화한 fixture를 `(runtime, OS, 정확한 CLI 버전, provider, auth route, 때로는 모델)`로 하드코딩한 registry 하나였다. 그 결과:

- 동의 없이 미션을 돌릴 수 있는 조합이 전 OS 통틀어 "Codex + openai 구독 + 핀된 버전 하나"뿐이다. Codex는 패치가 주 2회 나오므로 그마저 며칠 안에 무효가 된다.
- Claude Code의 Win/Linux 증거는 `cancel/read_only/scoped_write`를 증명하지 않고, OpenCode 증거는 `structured_result/events/cancel`이 pending이라 두 런타임은 어떤 버전에서도 동의 없이 역할 게이트를 못 통과한다.
- 성격이 다른 세 사실 — ① 프로토콜 호환(CLI 버전 속성, OS 무관, 추론 없이 handshake로 확인 가능) ② 격리 실효성(OS 속성, 단 daemon capture가 최종 방어선) ③ 모델·provider의 구조화 출력 적합성(런타임 RESULT_INVALID + plan repair가 이미 처리) — 이 한 테이블에 묶여 가장 엄격한 키가 전체를 지배한다.

"실험적 연결" 배지는 거의 모든 조합에 붙어 정보량이 0이고, CLI 업데이트마다 버전별 재동의를 요구해 마찰만 남긴다.

## 2. 원칙

1. **검증의 위치를 옮긴다.** 작성자 머신의 빌드 시점 상수 → 사용자 머신의 설치 확인 시점 측정 + 실행에서 학습. 엄격함(daemon capture, 구조화 결과 검증, exec identity 고정, 모델 자동 대체 금지)은 그대로 둔다.
2. **증거 층을 분리한다.** 출시 증거(shipped) / 같은 버전 라인 증거(version_line) / 로컬 자가 진단(local_probe) / 성공 실행 관측(observed_runs) / 사용자 동의(experimental_opt_in). 각 층은 `Support.reason_code`로 구분되며 역할 게이트는 여전히 `supported`만 본다.
3. **동의는 연결당 한 번이다.** 버전이 바뀌면 재동의가 아니라 자가 진단을 다시 돌린다(설치 확인). 자가 진단이 실패한 capability는 동의로도 열리지 않는다.
4. **로컬 증거는 daemon만 쓴다.** 클라이언트가 `binding.save`에 보낸 `local_evidence`는 무시하고 저장된 것을 이어 간다.

## 3. Wire 계약 (term-contracts)

### 3.1 `Support.reason_code` 값

| reason_code | supported | 뜻 |
|---|---|---|
| `null` | true | 출시 증거(정확한 버전의 라이브 fixture)가 증명 |
| `version_line` | true | 같은 OS·같은 major의 출시 증거 버전 이상이고 known break가 없으며, **이 버전의 로컬 프로토콜 진단이 통과**해 출시 증거를 이 버전에 적용 |
| `local_probe` | true | 이 PC에서 daemon의 무추론 자가 진단이 증명 |
| `observed_runs` | true | 이 PC·이 버전·이 모델의 성공 실행 관측이 임계치를 넘어 증명 |
| `experimental_opt_in` | true | 사용자 동의로 어댑터 구현 기능을 미검증 상태로 사용 |
| `local_probe_failed` | false | 자가 진단이 실행됐고 이 기능이 실패 — 동의로도 열리지 않음 |
| `adapter_not_implemented` | false | 어댑터 코드가 없음 |
| `no_compatibility_evidence` | false | 어떤 증거도 없음(동의도 없음) |
| 그 밖의 기존 slug | false | 출시 증거 fixture가 남긴 미증명 사유(예: `print_mode_no_approval_reply`) |

### 3.2 `CompatibilityGrade`

```rust
#[serde(rename_all = "snake_case")]
pub enum CompatibilityGrade {
    Verified,           // 출시 증거(정확 또는 version_line)로 설정 4역할이 동의 없이 통과
    VerifiedLocally,    // 출시 증거는 없지만 local_probe/observed_runs로 4역할이 동의 없이 통과 (신설)
    SameLineUnverified, // 같은 라인의 출시 증거가 있으나 이 버전의 프로토콜 진단이 아직 없음(설치 확인으로 확정)
    Unverified,
    NotInstalled,
}
```

TS: `"verified" | "verified_locally" | "same_line_unverified" | "unverified" | "not_installed"`.

### 3.3 `LocalEvidence` (신설, `mission/types.rs`, `#[ts(export)]`, `deny_unknown_fields` 없음)

```rust
/// Daemon-measured compatibility evidence for one binding on this machine.
/// Written only by the daemon (`binding.probe`, successful Runs); a value a
/// client sends in `binding.save` is discarded. Applies only while `os` and
/// `version` equal the current installation observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalEvidence {
    pub os: String,
    pub version: String,
    /// Model the run counters were collected for. Probe results are model-independent.
    pub model_id: String,
    #[serde(default)]
    pub probed_at: Option<Timestamp>,
    /// What the no-inference self-check proved; `None` when it has not run for this version.
    #[serde(default)]
    pub probe: Option<LocalProbeReport>,
    #[serde(default)]
    pub runs: LocalRunEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalProbeReport {
    /// The CLI completed the adapter's protocol handshake / advertises every flag the adapter passes.
    pub protocol_ok: bool,
    /// OS-level write/network boundary cases (Codex `command/exec`); `None` when not attempted here.
    #[serde(default)]
    pub sandbox_cases_passed: Option<u32>,
    #[serde(default)]
    pub sandbox_cases_total: Option<u32>,
    /// The binding's model appeared in the runtime's own listing; `None` when the runtime has none.
    #[serde(default)]
    pub model_listed: Option<bool>,
    /// Short fixed slugs of failed checks (never raw CLI output).
    #[serde(default)]
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalRunEvidence {
    #[serde(default)] pub succeeded_read_only: u32,
    #[serde(default)] pub succeeded_write: u32,
    #[serde(default)] pub cancelled: u32,
    #[serde(default)] pub invalid_result: u32,
    #[serde(default)] pub last_at: Option<Timestamp>,
}
```

`Binding`에 추가:

```rust
    // Daemon-owned local evidence (11 §3.3). Omitted when None so older
    // binding.save fingerprints still replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub local_evidence: Option<LocalEvidence>,
```

TS: `local_evidence?: LocalEvidence | null`.

### 3.4 `Binding.experimental_version` 의미 변경

"이 연결의 미검증(구현됨) 기능을 실험적으로 쓰는 데 동의했다"는 연결당 플래그. 값은 동의 시점의 관측 버전(표시용). **관측 버전과 같아야 한다는 조건을 없앤다.** `experimental_version_mismatch` reason_code는 더 이상 발생하지 않는다(에러 문자열 매핑은 옛 기록 표시용으로 남겨도 된다). 동의 철회(null)는 그대로 유효하다. 준비된 Run의 snapshot에 동의가 있어도 현재 binding 문서가 철회했거나 binding이 삭제됐으면 `CAPABILITY_UNSUPPORTED` + reason_code `experimental_consent_withdrawn`으로 거절한다(§7 `consent_withdrawn`).

### 3.5 save-source 상수

`term_contracts::mission::rpc::methods`에 `pub const BINDING_RUN_EVIDENCE: &str = "binding.run_evidence";` — RPC가 아니라 Run 관측이 binding 문서를 갱신할 때 `save_binding`의 `method`/fingerprint 태그다.

## 4. Capability 투영 규칙 (`capability_evidence::capabilities_for_binding`)

입력: `binding`, `os`, `version`(관측 버전). 출력: `RuntimeCapabilities`. 기능별로 아래 순서에서 **처음 supported인 층**을 택한다.

1. `shipped = evidence_for_binding(binding, os, version)` — 오늘과 같다(정확 버전, provider/route/모델 대조). reason `null`.
2. `line` — `shipped`가 아무 기능도 주장하지 않을 때만. binding의 provider·auth route·저장 참조가 출시 증거의 경로와 같고(모델 핀은 보지 않는다 — 다른 모델은 동의가 아니라 진단·실행 관측으로 확인한다), `version`이 §5의 라인 규칙으로 이 OS의 출시 증거 버전 `ev`의 라인 위에 있고 **`local.probe.protocol_ok == true`** 이면 `capabilities_for(runtime, os, ev)`의 supported 기능을 reason `version_line`으로 적용. 프로토콜 진단이 없으면 적용하지 않는다(→ grade `SameLineUnverified`). 다른 경로(API 키·커스텀 provider)는 라인 증거를 상속하지 않는다.
3. `local = binding.local_evidence` 중 `os == os && version == version`인 것. 없으면 3·4 생략.
   - `probe` → §6의 런타임별 매핑으로 supported, reason `local_probe`.
   - `probe.failures`가 가리키는 기능 → supported=false, reason `local_probe_failed`. 이 기능은 5(동의)로도 열리지 않는다.
4. `runs` — `local.model_id == binding.model_id`일 때만. `total = succeeded_read_only + succeeded_write`. `invalid_result > total`이면 승격 없음. 그 밖에:
   - `total >= 3` → structured_result, events, usage
   - `total >= 3 && succeeded_read_only >= 1` → read_only
   - `total >= 3 && succeeded_write >= 1` → scoped_write
   - `cancelled >= 1` → cancel
   reason `observed_runs`.
5. `experimental` — `experimental_applies(binding)`(§3.4: 동의 존재 && `adapter_supports_route` && runtime != Fake)이면 `adapter_capabilities(runtime)`의 구현 기능을 reason `experimental_opt_in`으로. 단 3에서 `local_probe_failed`인 기능은 제외.
6. 아무 층도 없으면: `local_probe_failed`(3) > `adapter_not_implemented`(어댑터 미구현; 동의가 적용되는 연결(5의 조건)에서만 드러나고, 동의 전에는 아래 두 값으로 남는다 — O22 이전과 같다) > 출시 fixture의 미증명 slug(shipped가 그 기능에 남긴 것) > `no_compatibility_evidence`.

`claims_any`는 그대로(어떤 층이든 supported면 true). `evidence_for_binding`(shipped 전용 투영)은 grade 판정용으로 유지한다.

## 5. 버전 라인 규칙

`parse_semver(v) -> Option<(u64,u64,u64)>`: `-`/`+` 이후를 버리고 `major.minor.patch`(patch 없으면 0). 라인 위 판정 `on_line(version, ev)`: `major == ev.major && (major,minor,patch) >= ev` 이고 `KNOWN_BREAKS`에 `(runtime, since)`가 있어 `ev < since <= version`인 항목이 없을 때. `pub const KNOWN_BREAKS: &[(RuntimeKind, &str)] = &[];` 로 시작한다. 기존 `major_minor`는 제거한다(0.x semver에서 minor가 사실상 major여서 Codex 0.154→0.155가 다른 라인으로 판정되던 결함).

과거 버전(`< ev`)으로는 확장하지 않는다.

## 6. 로컬 자가 진단 (`agent_runtime/local_probe.rs`, 신설)

```rust
pub const BUDGET: Duration = Duration::from_secs(12);
pub fn run(binding: &Binding, program: &str, version: &str, env: &DetectionEnv, budget: Duration) -> LocalProbeReport;
/// runtime.detect용 저비용 부분(Codex handshake+model/list, Claude --help). OpenCode/Fake는 None.
pub fn run_cheap(runtime: RuntimeKind, program: &str, model_id: &str, env: &DetectionEnv, budget: Duration) -> Option<LocalProbeReport>;
/// §6 매핑: 보고서 → 증거 전용 RuntimeCapabilities(supported=true는 reason None; 실패는 reason "local_probe_failed"; 나머지 no_compatibility_evidence).
pub fn capabilities(runtime: RuntimeKind, report: &LocalProbeReport) -> RuntimeCapabilities;
```

모델 호출·인증 토큰 사용·사용자 설정 변경 없음. 실패 slug는 고정 문자열이며 CLI 원문을 담지 않는다. 각 단계는 자체 deadline을 가지며 `budget`을 넘기지 않는다(응답이 없는 세션은 별도 스레드에 두고 호출 측이 deadline을 강제한 뒤 자식을 종료한다). 자식 프로세스는 항상 reap한다.

구현 메모: Codex 프로브는 `LivePeer`가 env를 받지 않아 daemon 환경(`CODEX_HOME`·`HOME`·`TMPDIR`)을 그대로 상속한다 — thread/turn을 시작하지 않으므로 설정을 쓰지 않으며, 모델 힌트 경로(`codex/models.rs`)와 같은 조건이다. OpenCode 프로브는 `server::spawn`이 `RunStart`·`ExecSupervisor`·tokio handle을 요구해 재사용하지 못하고, 격리 XDG/HOME/TMP와 생성한 Basic-auth 비밀번호(로그·slug에 남기지 않음)로 `opencode serve --port 0 --hostname 127.0.0.1`을 직접 띄운 뒤 어댑터의 `HttpTransport`로 읽는다(Windows에서는 프로세스 그룹 종료 대신 `kill`만 수행 — 후속 과제). sandbox의 결론 불가(transport 오류·예산 소진·`command/exec` 부재)는 `sandbox: None`이며 실패로 기록하지 않는다 — `local_probe_failed`는 동의로도 되돌릴 수 없는 영구 판정이기 때문이다. 네트워크 사례의 거절 판정은 로케일에 따라 바뀌는 libc 문구 대신 Python 예외 클래스명(`PermissionError`)도 함께 본다.

| runtime | 진단 | `protocol_ok` | sandbox | `model_listed` | supported로 매핑(local_probe) |
|---|---|---|---|---|---|
| Codex | `codex app-server`를 임시 cwd에서 `LivePeer`로 띄워 `initialize`+`initialized`, `model/list`(codex/models.rs 재사용) | initialize 응답 수신 | POSIX: `scripts/codex_sandbox_probe.py`의 12사례를 Rust로 이식해 `command/exec`로 실행(writer는 `/bin/sh -c 'printf mutated > "$1"' sh <path>`, 네트워크 사례는 `python3`가 PATH에 있을 때만 2건 추가; 없으면 total=10). Windows: None | `model/list`(hidden 제외)에 `binding.model_id` 존재 | protocol_ok → structured_result, events, cancel, usage, approval_reply, steer, model_listing(model/list 성공 시). sandbox 전부 통과 → read_only, scoped_write. sandbox 일부 실패 → read_only/scoped_write `local_probe_failed`(`failures: ["sandbox:<case>"]`) |
| Claude | `claude --help`를 `installation::capture_stdout`로 수집, 플래그 존재 확인: `--output-format`, `--json-schema`, `--permission-mode`, `--allowedTools`, `--disallowedTools`, `--verbose`, `--setting-sources`, `--strict-mcp-config`, `--model` | 전부 존재 | None | None | protocol_ok → structured_result, events, cancel(daemon 소유 감독 종료 단계), usage, read_only, scoped_write(권한 모드+도구 allowlist 표면; 최종 방어선은 daemon capture). 누락 → `failures: ["flag_missing:<flag>"]`, 그 플래그에 의존하는 기능은 `local_probe_failed`(`--json-schema`→structured_result, `--output-format`→events·usage, `--permission-mode`/`--allowedTools`/`--disallowedTools`→read_only·scoped_write) |
| OpenCode | `credential_ref`+`endpoint_ref` 필요(없으면 `failures: ["connection_required"]`, protocol_ok=false). tests/mission_opencode.rs의 metadata smoke처럼 격리 XDG env로 소유 서버 `server::spawn` → `GET /global/health`(version 일치) → `GET /doc`에 어댑터가 쓰는 경로(`/session/{id}/prompt`, `/session/{id}/abort`, `/permission/{id}/reply` — 정확한 경로는 opencode/http.rs·runtime.rs에서 확인) 존재 → `GET /config/providers`에 provider·model | health+경로 | None | providers에 존재 | protocol_ok → structured_result, events, cancel, approval_reply, usage, read_only, scoped_write. providers 성공 → model_listing |
| Fake | 없음 | — | — | — | — |

`run_cheap`은 Codex(handshake+model/list, sandbox 생략)와 Claude만 수행한다. `runtime.detect`는 이미 각 행을 병렬로 처리하므로 행당 지연이 전체 지연이다 — Codex ≈0.1s, Claude `--help` ≈ 수백 ms.

## 7. 저장·투영·게이트 (`mission/service.rs`, `mission/binding_evidence.rs`)

- **`binding.probe`**: 설치 관측이 Verified면 `local_probe::run`을 실행하고 `binding.local_evidence`를 갱신한다. 이전 `local_evidence`가 같은 `os`·`version`·`model_id`면 `runs`를 보존하고 `probe`/`probed_at`만 교체, 같은 `os`·`version`이지만 모델이 다르면 `runs`를 0으로, 버전이 다르면 새 `LocalEvidence`. 그 뒤 registry로 capabilities를 투영해 저장한다. `LocalEvidence`는 `Binding`의 필드이므로 `probe_document`가 만드는 문서에 자동으로 포함된다.
- **`binding.save`**: 클라이언트가 보낸 `local_evidence`는 버린다. 이전 문서의 `local_evidence`는 `runtime, program, provider_id, auth_route, credential_ref, endpoint_ref`가 모두 같을 때 이어 간다(모델만 다르면 `probe` 유지·`runs` 초기화). 그 밖에는 None.
- **`observed_binding` / `project_observation`**: 변경 없음 — registry가 `binding.local_evidence`를 읽으므로 저장 문서에서 복원한 binding으로 capabilities를 다시 계산하면 된다. `local_evidence.version != runtime_version`이면 registry가 무시한다.
- **`experimental_version_mismatch` 제거**: `require_binding_capability`, `validate_runtime_before_start`, `launch_capabilities`의 mismatch 분기를 삭제한다. `consent_withdrawn`(현재 문서가 동의를 철회했는가)은 유지한다. 버전 변경 시 시작 전 검사는 이미 "CLI file or version changed after installation check; check installation again"으로 막으므로 사용자는 설치 확인(=자가 진단)만 다시 누른다.
- **`runtime.detect`**: 행마다 `local_probe::run_cheap` 결과를 후보 binding의 `local_evidence`(`runs` 기본값, `model_id` = 행의 model_id)로 넣고 registry로 투영한다. grade:
  1. 설치 안 됨 → `NotInstalled`
  2. `evidence_for_binding`(shipped 전용)이 한 기능이라도 주장하거나 4역할 통과의 근거가 shipped/`version_line` → `Verified`
  3. 4역할이 동의 없이 통과(local_probe/observed_runs 덕분) → `VerifiedLocally`
  4. 라인 위 출시 증거가 있으나 프로토콜 진단이 없거나 실패 → `SameLineUnverified`
  5. 그 밖 → `Unverified`
  `experimental_roles`는 동의를 넣고 다시 투영한 결과(오늘과 같다). `compatibility_grade` 시그니처는 위 입력을 받도록 바꾼다.
- **Run 관측 기록 (`mission/run_evidence.rs`, 신설)**: `apply_adapter_event`가 terminal 이벤트를 **커밋한 뒤**(트랜잭션 밖, actor.rs의 `Ok(_)` 분기 이후) best-effort로 호출한다. 실패해도 Run 상태를 바꾸지 않고 로그만 남긴다.
  - `RunState::Succeeded`(execution.rs 928) → `term_core::mission::capability::writes_workspace(task.kind)`면 `succeeded_write += 1` 아니면 `succeeded_read_only += 1`
  - Stopping/Cancelled 상태에서 어댑터의 `Result`/`Failed`가 도착해 `RunState::Cancelled`로 확정된 경우 → `cancelled += 1`
  - `AdapterEvent::InvalidResult` → `invalid_result += 1`
  - 대상 binding: `run.binding_snapshot`의 `id`; 버전은 snapshot의 `runtime_version`, 모델은 snapshot의 `model_id`, OS는 현재. 현재 문서의 `local_evidence`가 같은 `os`·`version`이면 카운터를 갱신(모델이 다르면 `runs`를 새 모델로 초기화 후 1부터), 다르면 `probe: None`인 새 `LocalEvidence`. `save_mission_binding(Id::generate(), methods::BINDING_RUN_EVIDENCE, fingerprint, revision, document, now)` CAS. 충돌 시 한 번 재읽기 후 재시도, 그래도 실패면 버린다. `last_at` 갱신.
  - `Fake` runtime과 `binding_snapshot: None`은 기록하지 않는다.
- **DI**: `MissionService`에 `local_prober: Box<dyn Fn(&Binding, &str, &str, &DetectionEnv, Duration) -> LocalProbeReport + Send + Sync>`와 `cheap_prober`를 추가하고 기본값은 `local_probe::run`/`run_cheap`. 기존 `with_binding_evidence` 테스트 경로는 프로브를 "실행 안 함"(probe None)으로 두어 기존 fixture 시험의 기대가 유지되게 한다. 시험은 고정 보고서를 주입한다.

## 8. UI 계약 (05 문서 보충)

- 연결(binding)의 **신뢰 칩** 하나: `출시 검증됨`(필수 기능이 모두 `null`/`version_line`) / `이 PC에서 확인됨`(`local_probe`/`observed_runs`가 있고 `experimental_opt_in` 없음) / `실험적`(필수 기능 중 하나라도 `experimental_opt_in`) / `미확인`. 판정은 **필수 기능**(structured_result·events·cancel·read_only 또는 scoped_write)만 본다 — steer/resume 같은 선택 기능이 실험적이라는 이유로 배지를 붙이지 않는다.
- 칩 아래 한 줄로 근거: `프로토콜 확인 · sandbox 12/12 · 성공 실행 N회 · {probed_at}`; `model_listed === false`면 `선택한 모델이 이 CLI 목록에 없습니다` 경고(막지 않음).
- `설치 확인` 버튼이 자가 진단을 포함한다. 문구는 `지금 확인`.
- 동의는 연결당 한 번. `CLI가 업데이트되어 다시 동의` 흐름(`experimentalConsentStale`, `missions.settings.experimentalReconsent`)은 제거한다. 버전 변경은 시작 전 검사가 "설치 확인 다시"로 안내한다.
- `runtime.detect` 등급 문구: `verified` `자동 실행 검증됨` / `verified_locally` `이 PC에서 확인됨` / `same_line_unverified` `같은 계열 버전은 검증됨 · 지금 확인으로 확정` / `unverified` `자동 실행 미검증` / `not_installed`.
- 결과·할 일·상세의 `실험적 연결` 배지는 위 필수 기능 기준 `isExperimentalBinding`을 쓴다. 문구에서 "CLI를 업데이트하면 다시 동의해야 합니다"를 뺀다.
- 모델 핀(`proven_model_id`)과 다른 모델을 골랐다는 이유로 동의를 요구하지 않는다(모델 적합성은 런타임 RESULT_INVALID·plan repair가 담당). `proven_model_id`는 후보 정렬 힌트로만 남긴다.

## 9. 시험 (작성만, 실행은 사용자가 요청할 때)

- `capability_evidence`: 라인 규칙(0.154.0 증거 → 0.155.0 on_line, 1.0.0은 아님, KNOWN_BREAKS 경계), 층 우선순위, `local_probe_failed`가 동의를 이기는지, runs 임계치, 모델 불일치 시 runs 무시, `version_line`이 protocol_ok 없이는 적용되지 않는지.
- `local_probe`: Claude `--help` 파서(플래그 누락 → failures/매핑), Codex 보고서→capabilities 매핑, sandbox 부분 실패 매핑. 프로세스를 띄우는 시험은 `#[ignore]` + env로 명시한 실행 파일.
- `mission_runtime_detect.rs`: 주입 보고서로 `verified_locally`/`same_line_unverified` 등급, `experimental_roles` 유지.
- `mission_binding_probe.rs`: probe가 `local_evidence`를 저장·보존·초기화하는 규칙, `binding.save`가 클라이언트 `local_evidence`를 버리는지.
- `binding_evidence` 단위 시험: mismatch 관련 시험 삭제/치환.
- Run 관측: Succeeded/Cancelled/InvalidResult 카운터, CAS 충돌 재시도, Fake 미기록.
- 프론트 dom 시험: 등급 문구, 신뢰 칩, 재동의 UI 부재, 필수 기능 기준 배지.

## 10. 변경 경로

| 영역 | 파일 |
|---|---|
| 계약 | `crates/term-contracts/src/mission/{types.rs,rpc.rs}`, `src/generated/{Binding,CompatibilityGrade,LocalEvidence,LocalProbeReport,LocalRunEvidence}.ts`(ts-rs 출력 형식을 손으로 미러), `docs/orchestration/contracts.ts` |
| 증거·진단 | `crates/iyagi-termd/src/agent_runtime/{capability_evidence.rs,local_probe.rs,mod.rs,detection.rs}` |
| 서비스 | `crates/iyagi-termd/src/mission/{service.rs,binding_evidence.rs,run_evidence.rs,execution.rs,actor.rs,mod.rs}` |
| 프론트 | `src/features/missions/{bindingSupport.ts,QuickSetup.tsx,MissionSettings.tsx,MissionCreate.tsx,RunDetail.tsx,ResultReview.tsx,TeamList.tsx,configuration.ts}`, `src/i18n/sections/missions.ts`, `src/features/daemon/mockClient.ts`, 관련 `*.test.ts(x)` |
| 문서 | 이 문서, `ORCHESTRATION_SPEC.md` 표, `01-contracts.md`, `03-adapters.md §6`, `05-ui.md`, `IMPLEMENTATION_STATUS.md`, 사용자 가이드의 실험적 연결 설명 |

`npm run verify:contracts`(cargo test로 `src/generated` 재생성·diff)와 `cargo check`는 사용자가 요청할 때 실행한다.
