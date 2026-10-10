# 자원 거버넌스 개선 계획 (실행 지시서)

작성일: 2026-10-10. 이 문서는 실행 담당 모델이 **순서대로** 따라 하는 지시서다.
각 작업은 "바꿀 파일 → 바꿀 내용 → 시험 → 완료 기준" 순서로 적는다. 판단이 필요한
곳은 문서가 이미 정해 두었다. 문서에 없는 판단이 필요하면 멈추고 사람에게 묻는다.

## 0. 작업 규칙 (모든 작업 공통)

- 한 번에 한 작업만 한다. 작업 하나가 끝나면(완료 기준 통과) 그 작업만 커밋하고 다음으로 간다.
- 문서에 적힌 파일 외에는 고치지 않는다. 시험이 깨져서 다른 파일을 고쳐야 하면 멈추고 묻는다.
- 줄 번호는 2026-10-10 기준이다. 어긋나면 `grep -n`으로 그 식별자를 찾아 간다.
- Rust 검증(해당 작업마다 실행):
  ```sh
  cargo fmt --all
  cargo clippy -p <crate> --all-targets --locked -- -D warnings
  cargo test -p <crate> --locked
  ```
- TypeScript 검증:
  ```sh
  npm run typecheck
  npx vitest run <바꾼 시험 파일>
  npm run test:unit          # 작업 끝에 한 번
  ```
- 커밋 메시지는 영어 한 줄 제목 + 빈 줄 + 2~5줄 본문. 작성자는 `sh2orc <sh2orc@gmail.com>`.
  `Co-Authored-By` 같은 attribution 줄은 넣지 않는다.
- 주석은 주변 코드처럼 한국어로 쓴다. 기존 주석의 톤·밀도를 따른다.
- 새 상수는 파일 상단 기존 상수 옆에 `pub const`/`const`로 두고 한 줄 주석을 단다.

## 1. 배경 (왜 바꾸는가)

현재 동작과 문제는 다음과 같다. 작업마다 "무엇이 왜 잘못인지"는 여기서 찾는다.

| 기능 | 현재 | 문제 |
|---|---|---|
| 자원 가드 (`crates/iyagi-termd/src/guard.rs`) | 포커스 없는 워크로드가 **6코어 또는 4GiB를 20초** 넘기면 프로세스 트리 전체를 SIGSTOP. `auto_resume` 기본 꺼짐 | CPU를 많이 쓰는 건 빌드·테스트의 정상 동작이다. 얼리면 에이전트의 API 호출까지 끊긴다. CPU 경합은 "양보(우선순위 낮춤)"로 다뤄야 한다 |
| 호스트 메모리 CRITICAL 정지 (같은 파일) | 가용 <10% 또는 <1GiB면 3초마다 RSS 최대 하나씩 SIGSTOP | 메모리가 회복돼도 **아무도 자동 재개되지 않는다**(포커스·수동·10분 규칙뿐). 위기가 지나면 백그라운드 전부가 얼어 있다 |
| 압력 완화 (`crates/iyagi-termd/src/relief.rs`) | CPU 압력 WARNING(85%)에서 포커스 없는 워크로드 **전부**를 한꺼번에 background 등급으로 | 이미 유휴인 에이전트까지 내린다. 개별 워크로드의 CPU 독점에는 반응하지 않는다 |
| 보호 표시(`protected`) | relief만 본다 | guard는 무시하고 얼린다 |
| 재시작 자동 일괄 재개 (`src/features/terminal/sessionController.ts`) | 이어서 열 pane 전부를 동시에 실행 | 에이전트 6개면 Node 프로세스 6개가 동시에 뜬다. admission을 거치지 않는다 |
| 기본값 파일 (`crates/term-contracts/src/defaults.rs`) | `docs/implementation/defaults.json`을 **빌드 시점의 저장소 절대 경로**에서 런타임에 읽는다 | 설치한 PC에는 그 파일이 없어 `config.rs`의 fallback(문서와 다른 임계값: CRITICAL 5%/512MiB, WARNING 12%, 회복 18%)으로 돈다 |

원칙:
- **CPU 경합 → 우선순위로 나눠 쓴다.** 얼리지 않는다.
- **RAM 경합 → 단계적으로 얼리고, 회복되면 자동으로 되살린다.** 보고 있는 pane은 끝까지 제외.
- 사용자가 `protect`로 표시한 워크로드는 자동 양보·자동 정지 모두에서 제외한다.

## 2. 1단계 작업 (이 문서의 본문)

순서대로 한다. T1 → T2 → T3 → T4 → T5 → T6.

---

### T1. 기본값 JSON을 바이너리에 내장한다

**바꿀 파일**
- `crates/term-contracts/src/defaults.rs`
- `crates/iyagi-termd/src/config.rs`

**바꿀 내용**

1. `crates/term-contracts/src/defaults.rs`의 `spec_defaults_path`(162줄 근처)와
   `load_spec_defaults`(169줄 근처)를 다음으로 바꾼다. `spec_defaults_path`를 쓰는 곳이
   있으면(`grep -rn spec_defaults_path crates`) 남겨 두되, `load_spec_defaults`는 파일을
   읽지 않는다.

   ```rust
   /// `docs/implementation/defaults.json` 원문. 빌드에 내장해 설치한 PC에서도
   /// 저장소와 같은 값으로 돈다(예전에는 빌드 시점 경로를 런타임에 읽어
   /// 저장소 밖에서는 fallback 값이 쓰였다).
   pub const SPEC_DEFAULTS_JSON: &str = include_str!("../../../docs/implementation/defaults.json");

   pub fn load_spec_defaults() -> Option<Defaults> {
       serde_json::from_str(SPEC_DEFAULTS_JSON).ok()
   }
   ```

   `include_str!` 경로는 `crates/term-contracts/src/` 기준이다. 경로가 틀리면 컴파일
   오류가 난다 — 그러면 `ls docs/implementation/defaults.json`으로 위치를 확인해 고친다.

2. `crates/iyagi-termd/src/config.rs` 98~99줄의
   `defaults::load_spec_defaults().or_else(spec_defaults_fallback)`에서
   `.or_else(spec_defaults_fallback)`를 지운다. 그 결과 `spec_defaults_fallback`(347줄 근처)을
   아무도 쓰지 않으면 함수와 그 안의 JSON 문자열, 그리고 둘을 비교하던 시험
   (404~427줄 근처, `spec_defaults_fallback`을 부르는 `#[test]`)을 모두 지운다.
   `load_spec_defaults()`가 `None`이면 지금처럼 그 다음 처리(에러 또는 expect)를 유지한다.

**시험**
```sh
cargo test -p term-contracts --locked
cargo test -p iyagi-termd --locked config
cargo clippy -p term-contracts -p iyagi-termd --all-targets --locked -- -D warnings
```
dead code 경고가 나오면 그 함수를 지운다(`#[allow]`로 덮지 않는다).

**완료 기준**
- `grep -rn "spec_defaults_fallback" crates`가 아무것도 찾지 않는다.
- 저장소 밖(예: `/tmp`)에서 데몬 바이너리를 실행해도 같은 임계값을 쓴다. 확인 방법:
  `cargo build -p iyagi-termd` 후 `cd /tmp && <target/debug/iyagi-termd 경로> --help`가 뜨면 된다
  (값 자체는 단위 시험이 보증한다).

**커밋 제목**: `Embed defaults.json in the binary; drop the config fallback`

---

### T2. CPU 초과는 얼리지 않고 양보시킨다

가드의 "CPU 한도 초과 → SIGSTOP"을 없애고, 그 한도를 넘긴 워크로드를 압력 완화가
"양보(background 등급)"시키게 바꾼다. 호스트 CPU 압력이 NORMAL이어도 적용한다.

**바꿀 파일**
- `crates/iyagi-termd/src/guard.rs`
- `crates/iyagi-termd/src/relief.rs`
- `crates/iyagi-termd/src/telemetry_loop.rs`

#### T2-a. guard.rs: CPU 사유 자동 정지 제거

`GuardController::plan`(137줄 근처)에서:

```rust
// 바꾸기 전
let over_cpu = workload.cpu_cores.is_some_and(|c| c > cpu_limit);
let over_rss = workload.resident_bytes.is_some_and(|r| r > rss_limit);
let over = over_cpu || over_rss;
let reason = if over_rss { GuardReason::MemoryLimit } else { GuardReason::CpuLimit };
```
```rust
// 바꾼 뒤 — CPU 초과는 정지 사유가 아니다(완화가 양보로 다룬다, T2-b).
// 메모리 초과도 호스트가 NORMAL이면 얼리지 않는다: 호스트에 여유가 있는데
// 큰 워크로드 하나를 얼려 봐야 얻는 게 없고, SIGSTOP은 RSS를 줄이지도 않는다.
let over = mem_level != PressureLevel::Normal
    && workload.resident_bytes.is_some_and(|r| r > rss_limit);
let reason = GuardReason::MemoryLimit;
```

- `cpu_limit` 변수가 쓰이지 않게 되면 지운다. `LiveUsage.cpu_cores` 필드는 **남긴다**
  (telemetry_loop가 T2-c에서 같은 값으로 양보 판정을 만든다).
- `GuardReason::CpuLimit` variant는 계약(`term-contracts`)에 있으므로 지우지 않는다.
  자동 경로에서 더는 만들지 않을 뿐이다.
- 같은 파일 시험 모듈(400줄 이후)에서 `GuardReason::CpuLimit`을 기대하는 시험(775줄 근처)을
  찾아 "CPU만 초과하면 정지하지 않는다"로 바꾼다:
  ```rust
  #[test]
  fn cpu_over_limit_alone_never_suspends() {
      // cpu_cores 8.0, resident 100MiB, sustain 20s를 훨씬 넘긴 뒤에도 ops가 비어야 한다.
      // mem_level은 Normal·Warning 둘 다 돌려 본다.
  }
  ```
- 시험 하나를 더 추가한다: `rss_over_limit_suspends_only_when_host_not_normal`
  — 같은 입력(RSS 5GiB 지속)으로 `mem_level = Normal`이면 ops 없음,
  `mem_level = Warning`이면 20초 뒤 `Suspend { reason: MemoryLimit }`.
- `plan`을 부르는 시험 22곳의 인자는 바뀌지 않는다(시그니처 유지).

#### T2-b. relief.rs: "CPU 독점" 워크로드 양보

`ReliefController::plan`(150줄 근처)의 시그니처에 인자 두 개를 추가한다:

```rust
pub fn plan(
    &mut self,
    now_ms: u64,
    cpu_level: PressureLevel,
    focused: &[SessionId],
    live: &[LiveWorkload],
    capability_supported: bool,
    /// 이 틱에 가드 정책의 `cpu_cores_limit`을 넘긴 워크로드(telemetry_loop가 계산).
    cpu_over: &[WorkloadId],
    /// 그 초과가 이만큼 이어져야 "독점"으로 본다(가드 정책의 `sustain_ms`).
    cpu_sustain_ms: u64,
) -> Vec<ReliefOp>
```

`Record`(68줄 근처)에 필드를 추가한다:
```rust
/// `cpu_cores_limit` 초과가 시작된 시각. 초과가 끊기면 None.
cpu_over_since_ms: Option<u64>,
```

`plan` 본문에서, 기존 1) 포커스 복원 단계 **앞에** 다음을 한다:

```rust
// 0) CPU 독점 추적: 이 틱에 한도를 넘긴 워크로드는 시작 시각을 기억하고,
//    넘기지 않은 워크로드는 지운다. 포커스된 세션은 세지 않는다(보고 있는
//    pane은 느리게 두지 않는다).
let over_now: HashSet<&str> = cpu_over.iter().map(WorkloadId::as_str).collect();
for workload in &ordered {
    let record = self.records.entry(workload.workload_id.clone()).or_default();
    if over_now.contains(workload.workload_id.as_str())
        && !focused.contains(workload.session_id.as_str())
    {
        record.cpu_over_since_ms.get_or_insert(now_ms);
    } else {
        record.cpu_over_since_ms = None;
    }
}
let hot = |record: &Record| {
    record.cpu_over_since_ms.is_some_and(|since| now_ms.saturating_sub(since) >= cpu_sustain_ms.max(1))
};
```

주의: `ordered`는 지금 `focused` 집합을 만든 **뒤에** 만들어진다. 위 코드는 `focused`와
`ordered`가 모두 있는 위치(기존 "1)" 루프 바로 앞)에 넣는다. `or_default()`를 쓰려면
`Record: Default`여야 한다 — 이미 `#[derive(Default)]`다.

그다음 규칙을 넣는다:

- **독점 양보(모든 압력 단계에서):** 기존 2) NORMAL 분기 **앞에**, 다음 루프를 추가한다.
  ```rust
  // 0-1) CPU 독점 워크로드는 호스트 압력과 무관하게 양보시킨다. 수동·보호·
  //      미지원·이미 양보된 것은 건너뛴다(부분 양보는 다시 시도).
  if capability_supported && self.policy.auto_yield {
      for workload in &ordered {
          if workload.state != WorkloadState::Running
              || planned.contains(workload.workload_id.as_str())
              || focused.contains(workload.session_id.as_str())
          {
              continue;
          }
          let Some(record) = self.records.get(&workload.workload_id) else { continue };
          if !hot(record) || record.protected() || record.unsupported || record.manual_yield {
              continue;
          }
          let apply = match &record.relief {
              ReliefState::None => true,
              ReliefState::Yielded { partial, .. } => *partial,
          };
          if apply {
              ops.push(ReliefOp::Yield { workload_id: workload.workload_id.clone() });
          }
      }
  }
  ```
  이 루프 뒤에 `planned`를 다시 계산한다(새로 넣은 Yield가 뒤 단계에서 중복되지 않게).
  `planned`가 `HashSet<String>`이므로 `ops`에서 다시 만든다.

- **NORMAL 복원 단계(기존 2))에서 독점 중인 워크로드는 복원 후보에서 뺀다:**
  `filter_map` 안에서 `record`를 얻은 뒤 `if hot(record) { return None; }`를 넣는다.
  초과가 끝나면 `cpu_over_since_ms`가 None이 되어 다음 릴리스 간격(3초)에 보통 자동
  양보처럼 복원된다 — 별도 복원 코드는 필요 없다.

- 기존 3) WARNING/CRITICAL 전체 양보는 그대로 둔다.

시험(`relief.rs` 421줄 이후 모듈): `plan`을 부르는 36곳에 `&[]`, `20_000`을 덧붙인다.
반복을 줄이려면 시험 모듈에 helper를 둔다:
```rust
fn plan_no_hog(c: &mut ReliefController, now: u64, level: PressureLevel,
               focused: &[SessionId], live: &[LiveWorkload], supported: bool) -> Vec<ReliefOp> {
    c.plan(now, level, focused, live, supported, &[], 20_000)
}
```
그리고 새 시험 5개를 추가한다(이름 그대로 만든다):
1. `cpu_hog_is_yielded_at_normal_after_sustain` — NORMAL, 워크로드 하나를 `cpu_over`에 넣고
   t=0, t=19_999에는 ops 없음, t=20_000에 `Yield`.
2. `cpu_hog_focused_is_never_yielded` — 같은 조건에 세션이 focused면 영원히 ops 없음.
3. `cpu_hog_protected_or_manual_is_skipped` — `manual(.., Protect)` 후에는 ops 없음.
4. `cpu_hog_restores_after_over_ends` — 양보된 뒤 `cpu_over`를 비우고 NORMAL로 3초 지나면
   `Restore`가 나온다. 비우기 전에는 3초가 지나도 `Restore`가 나오지 않는다.
5. `cpu_hog_gap_resets_sustain` — t=0 over, t=10_000 over 아님, t=10_001부터 다시 over →
   t=30_000에야 `Yield`.

#### T2-c. telemetry_loop.rs: 초과 집합 계산해 넘기기

`run_relief`(100줄 근처)를 다음처럼 바꾼다. `run_guard`(136줄 근처)에서 `usage_cache`를
복제하는 코드를 그대로 가져온다.

```rust
fn run_relief(state: &Arc<DaemonState>, now: u64, cpu_level: PressureLevel) {
    let live = state.live_workloads_for_relief();
    let focused = state.focused_session_ids();
    let supported = state.scheduling_yield_supported;
    // 가드 정책의 CPU 한도를 넘긴 워크로드 — 정지 대신 양보 대상이다(T2).
    let (cpu_limit, sustain_ms) = {
        let guard = state.guard.lock().unwrap_or_else(|p| p.into_inner());
        let policy = guard.policy();
        (f64::from(policy.cpu_cores_limit.max(1)), policy.sustain_ms.get())
    };
    let usage = state.usage_cache.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let cpu_over: Vec<WorkloadId> = live
        .iter()
        .filter(|w| {
            usage.get(&w.workload_id).is_some_and(|u| {
                matches!(u.cpu_cores.quality, MetricQuality::Measured | MetricQuality::Estimated)
                    && u.cpu_cores.value.is_some_and(|c| c > cpu_limit)
            })
        })
        .map(|w| w.workload_id.clone())
        .collect();
    let ops = {
        let mut relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
        relief.plan(now, cpu_level, &focused, &live, supported, &cpu_over, sustain_ms)
    };
    // ... 이하 기존과 같음
}
```

- `GuardController`에 `pub fn policy(&self) -> &GuardPolicy`가 없으면 추가한다
  (`guard.rs` `impl GuardController` 안, `new` 아래).
- 락 순서: `guard` 락을 잡고 **풀고 나서** `relief` 락을 잡는다(위 코드가 그렇게 돼 있다).
  두 락을 동시에 쥐지 않는다 — 기존 주석의 교착 규칙이다.
- `MetricQuality` 경로는 `run_guard`가 쓰는 것과 같다(`term_contracts::metrics::MetricQuality`).

**시험**
```sh
cargo test -p iyagi-termd --locked guard
cargo test -p iyagi-termd --locked relief
cargo test -p iyagi-termd --locked            # tests/guard.rs, tests/relief.rs 통합 시험 포함
cargo clippy -p iyagi-termd --all-targets --locked -- -D warnings
```

**완료 기준**
- 위 시험 전부 통과. `guard.rs`에서 `GuardReason::CpuLimit`을 만드는 코드가 시험 밖에 없다
  (`grep -n "GuardReason::CpuLimit" crates/iyagi-termd/src/guard.rs` 결과가 시험 모듈 안뿐).
- 새 시험 7개(가드 2 + 완화 5)가 있다.

**커밋 제목**: `Resource guard: yield CPU hogs instead of freezing them`

---

### T3. 메모리가 회복되면 얼린 워크로드를 자동으로 되살린다

**바꿀 파일**: `crates/iyagi-termd/src/guard.rs`

**바꿀 내용**

1. `GuardController`(95줄 근처)에 필드를 추가한다:
   ```rust
   /// 마지막 메모리 회복 재개 시각(3초에 하나 — 한꺼번에 풀면 RSS가 한 번에
   /// 돌아와 다시 CRITICAL로 떨어진다).
   last_pressure_resume_ms: Option<u64>,
   ```
   `new`에서 `None`으로 초기화한다.

2. `plan`의 끝, 기존 "호스트 메모리 CRITICAL" 블록 **뒤에** 추가한다:
   ```rust
   // 호스트 메모리 NORMAL: 메모리 사유(호스트 압력·개별 한도)로 자동 정지된
   // 워크로드를 3초에 하나씩, 가장 나중에 얼린 것부터 되살린다. 가장 먼저
   // 얼린 것이 RSS가 가장 크므로 역순이 재악화 위험이 작다. 수동 정지는 손대지
   // 않는다. 포커스 재개는 위 루프가 이미 처리했다(planned에 있으면 건너뛴다).
   if mem_level == PressureLevel::Normal && self.policy.auto_suspend {
       let due = self
           .last_pressure_resume_ms
           .is_none_or(|last| now_ms.saturating_sub(last) >= 3_000);
       if due {
           let planned: HashSet<&str> = ops.iter().map(|op| op.workload_id().as_str()).collect();
           let target = ordered
               .iter()
               .filter(|w| !planned.contains(w.workload_id.as_str()))
               .filter_map(|w| {
                   let record = self.records.get(&w.workload_id)?;
                   if record.unsupported || now_ms < record.resume_retry_after_ms.unwrap_or(0) {
                       return None;
                   }
                   match &record.state {
                       GuardState::Suspended { since_ms, manual: false, reason, .. }
                           if matches!(reason, GuardReason::HostMemoryPressure | GuardReason::MemoryLimit) =>
                       {
                           Some((since_ms.get(), w.workload_id.clone()))
                       }
                       _ => None,
                   }
               })
               .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
           if let Some((_, id)) = target {
               self.last_pressure_resume_ms = Some(now_ms);
               if let Some(record) = self.records.get_mut(&id) {
                   record.resume_retry_after_ms = Some(now_ms + RESUME_RETRY_BACKOFF_MS);
               }
               ops.push(GuardOp::Resume { workload_id: id });
           }
       }
   }
   ```
   `since_ms`의 타입이 `U64String`이 아니라 `u64`면 `.get()`을 뺀다(기존 코드
   `since_ms.get()` 사용 여부를 230줄 근처에서 확인한다).

3. 이 블록이 `MemoryLimit` 정지를 되살려도 진동하지 않는 이유: T2-a에서 NORMAL일 때는
   `over_rss`가 거짓이라 다시 얼리지 않는다. T2가 먼저 끝나 있어야 한다.

**시험**(같은 파일 시험 모듈)
1. `memory_recovery_resumes_one_every_3s_newest_first` — 워크로드 3개를 CRITICAL에서
   t=0, 3_000, 6_000에 얼린 뒤(`HostMemoryPressure`, 기존 경로로 만든다), NORMAL로
   t=10_000: `Resume`가 **하나**, 대상은 t=6_000에 얼린 것. t=12_000: 없음. t=13_000: 다음 하나.
2. `memory_recovery_skips_manual_suspensions` — `manual(.., Suspend)`로 얼린 것은 NORMAL에서도
   `Resume`가 나오지 않는다.
3. `memory_recovery_does_nothing_at_warning` — WARNING에서는 `Resume`가 나오지 않는다.
4. `memory_recovery_respects_resume_backoff` — 재개가 실패로 기록된(`record`에 실패 outcome)
   워크로드는 10초 안에 다시 `Resume`가 나오지 않는다. 기존 시험 중 백오프를 다루는 것을
   그대로 본떠 만든다.

```sh
cargo test -p iyagi-termd --locked guard
cargo clippy -p iyagi-termd --all-targets --locked -- -D warnings
```

**완료 기준**: 시험 4개 추가·통과. 기존 "포커스하면 재개" 시험이 그대로 통과.

**커밋 제목**: `Resource guard: resume memory-suspended workloads once the host recovers`

---

### T4. guard가 `protected` 표시를 존중한다

**바꿀 파일**
- `crates/iyagi-termd/src/relief.rs` — 조회 API 추가
- `crates/iyagi-termd/src/guard.rs` — `LiveUsage`에 필드, 두 정지 규칙에서 제외
- `crates/iyagi-termd/src/telemetry_loop.rs` — 값 채우기

**바꿀 내용**

1. `relief.rs` `impl ReliefController`에 추가(142줄 `is_unsupported` 옆):
   ```rust
   /// 사용자가 보호한 워크로드인가(sticky 또는 압력 중 수동 복원). 가드가
   /// 자동 정지 대상에서 빼는 데 쓴다.
   pub fn is_protected(&self, workload_id: &WorkloadId) -> bool {
       self.records.get(workload_id).is_some_and(|record| record.protected())
   }
   ```
2. `guard.rs` `LiveUsage`에 `pub protected: bool,`을 추가하고 주석
   `/// 완화 쪽 보호 표시 — 자동 정지(개별 한도·호스트 압력) 대상에서 뺀다.`를 단다.
   `plan`에서:
   - 개별 한도 분기: `if over {` → `if over && !workload.protected {`. (`else` 분기는 그대로
     — 보호된 워크로드는 `over_since_ms`가 쌓이지 않게 `else`에서 None이 된다.)
   - CRITICAL 희생 선택: `.filter(|w| !focused.contains(..))` 뒤에 `.filter(|w| !w.protected)`.
   - T3의 회복 재개는 보호 여부와 무관(재개는 이로운 조작).
   - 시험 모듈의 `LiveUsage { .. }` 생성 22곳에 `protected: false`를 넣는다. 생성 helper가
     있으면 helper만 고친다.
3. `telemetry_loop.rs` `run_guard`에서 `LiveUsage`를 만들 때:
   ```rust
   let protected_ids: HashSet<WorkloadId> = {
       let relief = state.relief.lock().unwrap_or_else(|p| p.into_inner());
       live_ids.iter().filter(|id| relief.is_protected(id)).cloned().collect()
   };
   ```
   식으로 `relief` 락을 **먼저 잡고 풀고 나서** `guard` 락을 잡는다. `LiveUsage`에
   `protected: protected_ids.contains(&w.workload_id)`를 채운다. (`live_ids`는
   `live_workloads_for_relief()` 결과에서 id만 모은 것.)

**시험**(guard.rs 시험 모듈)
1. `protected_workload_is_never_auto_suspended` — RSS 5GiB, WARNING, 60초 지나도 ops 없음.
2. `protected_workload_is_skipped_as_pressure_victim` — CRITICAL, 보호된 것이 RSS 최대여도
   두 번째로 큰 것이 희생된다.

**완료 기준**: 시험 2개 추가·통과, `cargo test -p iyagi-termd --locked` 전체 통과.

**커밋 제목**: `Resource guard: honor the protect flag`

---

### T5. 재시작 자동 일괄 재개에 시차를 둔다

**바꿀 파일**
- `src/features/terminal/sessionController.ts` — `resumeRestoredAgents`(2302줄 근처)
- `src/features/terminal/sessionControllerAgentResume.test.ts`

**바꿀 내용**

`resumeRestoredAgents`를 "동시에 최대 3개"로 바꾼다. 하나가 끝나면(성공·실패 무관) 다음을
시작한다. 시작 전 호스트 메모리 압력이 `CRITICAL`이면 풀릴 때까지(최대 30초) 기다렸다가
시작한다. 복원 나머지 작업은 이 함수를 기다리지 않는다(지금과 같이 `void`로 부른다).

```ts
/** 한 번에 띄우는 자동 재개 수. 에이전트는 기동 때 Node 로딩·대화 기록 읽기로 잠깐
 *  CPU·RAM을 많이 쓰므로 전부 동시에 띄우지 않는다. 3개면 10코어 PC에서 체감되지 않는다. */
const RESTORED_RESUME_CONCURRENCY = 3;
/** CRITICAL 메모리 압력이 풀리길 기다리는 상한. 넘기면 그냥 띄운다(영원히 안 띄우지 않는다). */
const RESTORED_RESUME_PRESSURE_WAIT_MS = 30_000;

private async resumeRestoredAgents(targets: readonly RestoredResume[]): Promise<void> {
  const seen = new Set<string>();
  const queue = targets.filter((target) => {
    const key = `${target.resume.agent}:${target.resume.agentSessionId}`;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
  const worker = async (): Promise<void> => {
    for (;;) {
      const next = queue.shift();
      if (!next || this.disposed) return;
      await this.waitForMemoryHeadroom(RESTORED_RESUME_PRESSURE_WAIT_MS);
      if (this.disposed) return;
      try { await this.resumeRestoredAgent(next); } catch { /* 개별 실패는 pane에 남는다 */ }
    }
  };
  await Promise.all(Array.from({ length: RESTORED_RESUME_CONCURRENCY }, worker));
}

/** 호스트 메모리가 CRITICAL이 아닐 때까지 500ms 간격으로 기다린다(상한 있음). 샘플이
 *  아직 없으면(host null) 기다리지 않는다. */
private async waitForMemoryHeadroom(maxMs: number): Promise<void> {
  const start = Date.now();
  while (useWorkbenchStore.getState().host?.pressure === "CRITICAL") {
    if (this.disposed || Date.now() - start >= maxMs) return;
    await new Promise<void>((done) => setTimeout(done, 500));
  }
}
```

- 기존 함수의 `Promise.allSettled` 구현을 위 코드로 대체한다. `resumeRestoredAgent`는 그대로.
- `host` 필드 이름은 `useWorkbenchStore` 상태의 `host: HostSample | null`이다
  (`src/features/monitor/ResourceStrip.tsx`가 `s.host`로 읽는다). `pressure` 값은
  `"NORMAL" | "WARNING" | "CRITICAL"`.

**시험**(`sessionControllerAgentResume.test.ts`, "복원:" describe 안)

기존 시험 `이어서 열 pane이 여럿이면 서로를 기다리지 않고 한꺼번에 띄운다`는 pane 2개라
3개 한도 안이므로 그대로 통과해야 한다. 추가:
1. `자동 재개는 한 번에 3개까지만 띄우고, 하나가 끝나면 다음을 띄운다` — pane 5개(서로 다른
   `agent_session_id`), `workloadLaunch`를 "끝나지 않는 Promise"로 잡아 두고 `vi.waitFor`로
   launches가 3개가 될 때까지 기다린 뒤 `setTimeout 0` 한 번 더 지나도 **3개**인지 확인.
   그다음 잡아 둔 Promise 중 하나를 resolve(또는 reject)하면 4개가 되는지 확인. 잡아 두기는
   `holdLaunches`를 본떠 resolver 배열을 반환하는 helper를 만든다.
2. `메모리 압력이 CRITICAL이면 자동 재개를 미루고 풀리면 띄운다` — `vi.useFakeTimers()`,
   `useWorkbenchStore.setState({ host: {...sample, pressure: "CRITICAL"} })`(`host` 객체는
   `src/features/monitor/ResourceStrip.test.tsx`의 `sample()`을 복사해 만든다), 복원 후
   `advanceTimersByTimeAsync(2000)`에도 launches 0개 → `pressure: "NORMAL"`로 바꾸고
   `advanceTimersByTimeAsync(600)` → launches ≥ 1. 끝에 `vi.useRealTimers()`.

```sh
npm run typecheck
npx vitest run src/features/terminal/sessionControllerAgentResume.test.ts
```

**완료 기준**: 시험 2개 추가·통과, 파일 전체(70개 이상) 통과.

**커밋 제목**: `Stagger auto-resumed agent sessions after a restart`

---

### T6. 메모리 초과 판정을 더 빨리 한다 (CPU·메모리 sustain 분리)

메모리 급등은 20초를 기다리면 늦다(테스트 워커가 10초 안에 수 GB를 먹는다).
가드 정책에 메모리 전용 지속 시간을 두고 기본 5초로 한다. CPU 쪽 `sustain_ms`(20초)는
T2의 양보 판정이 계속 쓴다.

**바꿀 파일**
- `crates/term-contracts/src/snapshot.rs` — `GuardPolicy`(181줄 근처)
- `docs/implementation/defaults.json` — `resource_guard`
- `crates/iyagi-termd/src/config.rs` — 정책 로딩(`resource_guard` 읽는 곳, `grep -n resource_guard`)
- `crates/iyagi-termd/src/guard.rs` — `plan`의 sustain
- `src/generated/GuardPolicy.ts` — **직접 고치지 않는다.** ts-rs가 생성한다(아래 명령).
- `src/features/daemon/mockClient.ts` — 기본 정책 객체에 필드 추가

**바꿀 내용**
1. `GuardPolicy`(`snapshot.rs` 181줄 근처)에 필드를 추가한다(기존 `sustain_ms` 바로 아래).
   예전 데몬·설정 JSON에 이 키가 없어도 읽히도록 serde 기본값 함수를 둔다:
   ```rust
   /// 메모리 한도 초과를 "지속"으로 인정하는 시간(밀리초). CPU보다 짧다 —
   /// 메모리 급등은 20초를 기다리면 스왑이 먼저 온다.
   #[serde(default = "default_rss_sustain_ms")]
   pub rss_sustain_ms: U64String,
   ```
   ```rust
   fn default_rss_sustain_ms() -> U64String {
       U64String::new(5_000).expect("5 s in range")
   }
   ```
   `impl Default for GuardPolicy`에 `rss_sustain_ms: default_rss_sustain_ms(),`를 추가한다.
2. `defaults.json`의 `resource_guard`에 `"rss_sustain_ms": "5000"`을 추가한다(문자열 — 같은
   블록의 `sustain_ms` 표기를 따른다). `config.rs`에서 그 블록을 `GuardPolicy`로 옮기는
   코드가 있으면 새 필드를 같이 옮긴다.
3. `guard.rs` `plan`에서 메모리 초과 판정에 `rss_sustain_ms`를 쓴다:
   `let sustain = self.policy.rss_sustain_ms.get().max(1);` (T2 뒤에는 이 `sustain`이
   메모리에만 쓰인다.)
4. 생성 타입 갱신: `cargo test -p term-contracts --locked`가 ts-rs로 `src/generated/*.ts`를
   다시 쓴다. 실행 뒤 `git diff --stat src/generated`에 `GuardPolicy.ts`만 나와야 한다. 다른
   파일이 바뀌면 멈추고 묻는다. 커밋 전에 `npm run verify:contracts`가 통과해야 한다
   (생성 결과와 커밋이 같은지 검사한다).
5. `mockClient.ts`에서 `GuardPolicy` 객체를 만드는 곳에 `rss_sustain_ms: "5000"`을 넣는다.

**시험**
- guard.rs: 기존 메모리 초과 시험들이 20초를 가정하면 5초로 바꾸거나, 시험용 정책의
  `rss_sustain_ms`를 20_000으로 맞춰 둔다(둘 중 시험 변경이 적은 쪽).
- 새 시험 `rss_sustain_is_independent_from_cpu_sustain` — `rss_sustain_ms = 5000`,
  `sustain_ms = 20000`, WARNING, RSS 초과가 t=5000에 `Suspend`.
```sh
cargo test -p term-contracts -p iyagi-termd --locked
npm run typecheck && npm run test:unit
```

**완료 기준**: 위 통과, `src/generated/GuardPolicy.ts`에 `rss_sustain_ms` 존재.

**커밋 제목**: `Resource guard: separate memory sustain window (5 s) from CPU`

---

## 3. 2단계 작업 (설계만 확정 — 1단계 뒤에 사람이 순서를 정한다)

각 항목은 "무엇을, 어디에" 수준으로 적는다. 착수 전에 1단계처럼 세부 지시서를 쓴다.

### P2-1. 우선순위·에이전트 활동 상태를 희생 순서에 쓴다
- `crates/iyagi-termd/src/state.rs` 워크로드 entry에는 `priority: Priority`(258줄)와
  `agent: Option<AgentStatus>`(287줄, `session_status`: "busy"/"shell"/"waiting"/"idle")가 이미 있다.
- `relief::LiveWorkload`·`guard::LiveUsage`에 `priority: u8`, `agent_activity: Option<String>`을 추가하고
  `live_workloads_for_relief`(state.rs 538줄)에서 채운다.
- guard CRITICAL 희생 순서를 `(활동: idle/waiting 먼저 → 낮은 우선순위(2) 먼저 → RSS 큰 것)`으로 바꾼다.
- relief WARNING에서는 `priority == 2`와 CPU 독점만 양보하고, CRITICAL에서 전부 양보한다.
- 큐 에이징(`crates/term-core/src/queue.rs` 48줄)은 한 단계(2→1)까지만, 창은 5분으로.

### P2-2. 가드·완화 정책을 설정 화면에서 바꾸고 저장한다
- RPC `guard.set_policy`/`relief.set_policy`는 이미 있다(`src/features/daemon/client.ts` 371·379줄).
  UI가 없을 뿐이다.
- `src/features/settings/GeneralPanel.tsx` 옆에 `ResourcePanel.tsx`를 만들고 `cpu_cores_limit`,
  `rss_limit_bytes`(GiB 입력), `rss_sustain_ms`, `auto_suspend`, `auto_yield`를 둔다.
- 값은 `src/store/preferences.ts`에 저장하고, 데몬 연결 직후(`sessionController` 시작 경로)에
  다시 보낸다 — 데몬은 정책을 메모리에만 둔다.

### P2-3. macOS 프로세스 스캔을 틱당 한 번만 한다
- `crates/term-platform/src/group/macos_tree.rs` 48~60줄: `member_identities`가 호출마다 전체
  프로세스를 새로 긁고, `telemetry_loop.rs` 251·254줄이 워크로드마다 두 번 부른다.
- 틱 시작에 한 번 스냅샷을 만들어 모든 워크로드가 공유하게 바꾼다.

### P2-4. 커널 메모리 압력 신호를 쓴다
- `crates/term-core/src/pressure.rs` 190줄 `force_critical`은 호출처가 없다.
- macOS: `sysctl kern.memorystatus_vm_pressure_level`(1 normal, 2 warn, 4 critical)을 틱마다 읽어
  4면 `force_critical`. Linux: `/proc/pressure/memory`의 `some avg10 > 20` 또는 `full avg10 > 5`.
  Windows: `CreateMemoryResourceNotification(LowMemoryResourceNotification)`.

### P2-5. 셸 경로 에이전트 실행에 가벼운 admission을 건다
- 퀵스타트·이어서 열기·수동 재개(`sessionController.resumeAgentSession`)도 T5의
  `waitForMemoryHeadroom`을 거치게 한다. 거부는 하지 않는다(대기 상한 30초).

## 4. 3단계 (구조 변경 — 설계 메모)

- **셸 에이전트 1급 워크로드화.** `agent_watch`가 찾은 에이전트 pid를 루트로 하는 가상
  워크로드를 만들어 측정·양보·정지를 셸 트리가 아니라 에이전트 트리 단위로 한다. 지금은
  PTY 루트에서 부모-자식 추적을 2초 캐시로 해서 재부모화된 자손이 빠져나간다.
- **무거운 자식 실행 게이트.** 프로세스 이름(`cargo`, `rustc`, `tsc`, `vitest`, `jest`, `docker`,
  `xcodebuild`, `gradle`)으로 빌드·테스트 트리를 인식하고, 압력 WARNING 이상에서 호스트 전체
  동시 k개(기본 2, RAM WARNING이면 1)만 허용. 초과분은 생성 직후 SIGSTOP, 하나가 끝나면 해제.
  같은 cwd의 트리는 한 묶음(빌드 락 때문).
- **Linux/Windows 보강.** Linux 비위임 환경은 `systemd-run --user --scope -p CPUWeight=`로
  사용자 슬라이스 cgroup 검토. Windows는 pid별 `NtSuspendProcess`로 정지 경로 추가.

## 5. 참고: 현재 수치 한눈에

| 항목 | 값 | 위치 |
|---|---|---|
| 메모리 CRITICAL / WARNING / 회복 | 가용 <10% 또는 <1GiB / <20% / ≥25% 10초 | `crates/term-core/src/pressure.rs` |
| CPU WARNING / CRITICAL / 회복 | ≥85% / ≥95% / ≤70% 10초 | 같은 파일 |
| 악화 판정 | 2샘플 연속 | 같은 파일 |
| 가드 한도 | 6코어, 4GiB, 20초, auto_resume off | `crates/term-contracts/src/snapshot.rs` `GuardPolicy` |
| CRITICAL 희생 | 3초에 하나, RSS ≥512MiB 최대 | `guard.rs` 42·253줄 |
| 완화 복원 간격 | 3초에 하나 | `defaults.json` `relief_release_interval` |
| 관리 실행 동시 수 / 큐 | 2 / 64 | `defaults.json` `limits` |
| 우선순위 | 0(높음)~2(낮음), 30초마다 한 단계 상승 | `crates/term-core/src/queue.rs` |
| 텔레메트리 주기 / 신선도 | 1초 / 3초 | `crates/term-platform/src/telemetry.rs` |
