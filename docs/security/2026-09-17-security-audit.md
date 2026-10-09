# iyagi 보안 취약점 점검 — 2026-09-17~18

다중 에이전트 보안 감사 2라운드, 수정 2라운드, 통합 검증, 회귀 수정(3차) 결과 기록.

- 1차 감사: 핵심 코드베이스의 공격 표면 8개.
- 2차 감사: 1차 크리틱이 지적한 미커버 영역(릴리스·서플라이체인, CI, SSH 원격 실행 설계, 프로세스 그룹 감시 백엔드, PTY 저널·저장소 무결성).
- 통합 검증: 클러스터별 리뷰로는 보이지 않는 연결부를 확인했다. 그 결과 보안 수정이 만든 기능 회귀를 찾아 3차에서 고쳤다.

## 방법론

- **감사**: 공격 표면별 리뷰어를 병렬로 돌렸다. 위협 모델은 네 가지다: (A1) 악성 리포지토리 콘텐츠·에이전트 출력, (A2) 변조된 에이전트 CLI 바이너리, (A3) 같은 머신의 다른 로컬 사용자·프로세스, (A4) 악성 터미널 이스케이프 시퀀스. 2차에는 (A5) 서플라이체인·CI와 (A6) 원격 호스트를 추가했다.
- **반박 검증**: 발견 건마다 독립 검증자 1~3명이 실제 코드를 대조했다(exploitability / code-accuracy / mitigation-hunt). 공격 경로에 실제로 도달할 수 있는 것만 남겼고, critical/high는 3표 다수결로 판정했다.
- **수정**: 파일 소유권이 겹치지 않는 클러스터로 나눠 병렬 수정했다. 이어 스냅샷 diff 기반 읽기 전용 리뷰를 거치고, 문제가 나오면 보수했다.
- **통합 검증**(1·2차 수정 후): 클러스터 간 연결부, 두 라운드에서 모두 고친 파일, 1차 수정이 다른 세션 편집에 덮이지 않았는지, 문서·CI 정확성을 점검했다. 중대 이슈 12건이 나왔고 그중 10건이 보안 수정이 만든 기능 회귀였다.
- **3차 회귀 수정**: 클러스터 7개로 고친 뒤, 이번에는 여러 클러스터를 묶은 공동 리뷰로 연결부와 CI 게이트(rustfmt 너비, clippy `-D warnings`, `src/generated` 변화)를 읽어서 확인했다. 차단·중대 이슈는 보수하고 재확인했다.

## 1차 감사 — 확정 13건 / 이견 1건 / 기각 7건

| # | 심각도* | 위치 | 요약 | 처리 |
|---|---|---|---|---|
| 1 | high | `workspace/git.rs:191` | `git worktree add` 때 리포지토리가 심어 둔 `post-checkout` 훅·`core.fsmonitor`가 사용자 권한으로 실행됨(임의 코드 실행). 방어는 verification 경로에만 있었음 | ✅ 수정+회귀 테스트 (3차에서 Windows 보완) |
| 2 | medium | `mission/verification_isolation.rs:17` | 검증 샌드박스가 `(allow file-read*)`로 읽기 무제한 — 에이전트가 작성한 코드가 `~/.ssh` 등 임의 사용자 파일을 읽을 수 있음 | ✅ 수정 (3차에서 회귀 수정) |
| 3 | medium | `ipc.rs:107` | Windows 네임드파이프 이름이 결정적 → 다른 로컬 사용자가 먼저 잡으면 데몬이 싱글턴 락을 쥔 좀비로 남음 | ✅ 수정(재시도 후 셧다운) |
| 4/5 | medium | `bridge/paste_image.rs:113-114` | 공유 /tmp 아래 붙여넣기 이미지 디렉터리 — 소유권 미점검, chmod 실패 무시, 파일이 0644로 생성됨 | ✅ 수정(사용자별 디렉터리, 0600 원자적 생성) |
| 6 | low | `ipc.rs:215` | 연결별 송신 큐 무제한 — 스펙 상한 128 미적용, 읽지 않는 피어가 있으면 메모리가 무한 증가 | ✅ 수정 (3차에서 데이터 연결 회귀 수정) |
| 7 | low | `agent_runtime/codex/mod.rs:1281` | 악성 app-server 출력으로 승인 요청이 무한 적립됨 | ✅ 수정 (3차에서 상한 회귀 수정) |
| 8 | low | `mission/execution.rs:823` | 파일 변경 승인이 내용에 묶이지 않음. 수락 UI에 diff가 없음 | ⏸ 보류(UX 설계 필요) |
| 9 | low | `orchestrator.rs:272` | 셸 실행이 세션 액터 기동 실패를 무시 — FAILED 워크로드에 RUNNING 회신, 고아 프로세스 | ✅ 수정 (3차에서 `mark_running` 실패 경로 추가) |
| 10 | low | `state.rs:51` | 브로드캐스트 증폭을 포함한 연결별 무제한 큐 | ✅ 수정(#6과 같은 근원) |
| 11 | low | `paths.rs:115` | 예측 가능한 /tmp 폴백 소켓 디렉터리 — 소유권 검사 전에 chmod가 실패해 기동이 영구 차단됨 | ✅ 수정 (3차에서 하드링크 미지원 파일시스템 대응) |
| 12 | low | `bridge/hooks_json.rs:415` | `~/.claude/settings.json`(토큰이 들어갈 수 있음) 재작성 임시 파일이 0644, 첫 생성도 0644로 굳음 | ✅ 수정(create_new + 0600) |
| 13 | info | `paths.rs:216` | 제어 토큰 파일 권한이 원자적으로 적용되지 않음(잠깐 0644) | ✅ 수정 |

\* 반박 검증 뒤 보정한 심각도. 기각 7건에는 Windows 드롭 인용 인젝션, OSC 52 클립보드 등이 있다(근거나 도달 가능성이 부족함). 스케줄러 `.expect` 건은 이견으로 남았다.

## 2차 감사 — 확정 20건 / 기각 2건

탐지 23건 → 중복 제거 22건 → 확정 20건(high 2 / medium 7 / low 10 / info 1). 기각 2건은 원격 스냅샷 경로 검증 부재, 사이드카 무결성이다.

### 코드·문서 수정

| # | 심각도 | 위치 | 요약 |
|---|---|---|---|
| 1 | medium | 어댑터 3종 이벤트 채널(`codex/mod.rs:688` 등) | 무제한 채널 — 출력이 많은 CLI가 데몬 힙을 고갈시킴(자식 cgroup 정책 밖) |
| 2 | medium | `mission/activity.rs:48` | 델타마다 ~1 MiB 테일 전체 재작성 + fsync, 엔티티 그래프 전체 로드 |
| 3 | medium | `mission/activity.rs:127` | 읽기 폴링마다 새 불변 아티팩트 발행, GC 없음 |
| 4 | medium | `group/macos_tree.rs:169` | 읽을 수 없는 프로세스 정체성 하나가 그룹 스캔 전체를 중단시킴 |
| 5 | low | `term-pty/journal.rs:783` | 세그먼트 삭제에 실패해도 예산을 해제해 2 GiB 상한이 무너짐 |
| 6 | low | `lib.rs:177` | 재시작 시 디스크에 있는 저널 바이트를 예산에 반영하지 않음 |
| 7 | low | `group/linux_cgroup.rs:1382` | 일괄 검증 뒤 raw PID로 시그널 — PID 재사용 시 다른 프로세스를 죽일 수 있음 |
| 8 | low | `telemetry_loop.rs:279` | 셸 PID를 정체성 없이 감시 — 재활용된 PID의 무관한 프로세스를 우선순위 강등 |
| 9 | low | `group/macos_guardian.rs:501` | Stop/Retire/Members/Sample이 같은 UID의 누구에게나 응답 — 워크로드 간 DoS |
| 10 | low | `exec/mod.rs:589` | 정지 drain 무한 루프 — blocking 스레드와 예약을 영구 점유 |
| 11 | low | `orchestrator.rs:60` | executor 필드를 무시하고 로컬에서 실행(fail-open) |
| 12 | low | `search_scan.rs:30` | CRC 전체 스캔이 바이트 예산을 우회하고 IPC 루프를 동기로 막음 |
| 13 | low/info | `scripts/docker/ubuntu-24.04/Dockerfile:12` | rustup `curl \| sh` 버전 미고정 → 1.28.2 고정 + SHA-256 검증(해시는 공식 `rustup-init.sha256`과 대조해 일치 확인) |
| 14 | low | `docs/implementation/05-remote.md` | SSH 전송 설계 — 전송 하드닝 요건 추가, host key 방침은 OPEN으로 명시 |
| 15 | docs | `README.md` / `README_KR.md` | `xattr -d com.apple.quarantine` 안내 제거(우클릭 → 열기 / 그래도 열기로 대체), 신뢰 수준을 정직하게 기술 |

### CI 정책(사용자 결정: 셸 러너는 보호 ref 전용)

- **HIGH — 셸 러너의 무신뢰 코드 실행**, **MEDIUM — cargo 캐시 오염**: `.gitlab-ci.yml`에서 셸 러너·호스트 접근 잡(windows-check, windows-pty-integration, macos-check, bundle-windows, bundle-macos, linux-cgroup)을 보호 ref 전용으로 게이팅했다. MR 파이프라인에서는 실행하지 않는다. 캐시 키는 `$CI_JOB_NAME-$CI_COMMIT_REF_SLUG-protected-$CI_COMMIT_REF_PROTECTED`다. frontend 잡에는 `tags: [linux]`를 붙였다.
- **주의: YAML 게이트는 보안 경계가 아니다.** GitLab은 브랜치 커밋의 `.gitlab-ci.yml`로 파이프라인을 만든다. 그래서 브랜치를 push할 수 있는 사람은 규칙을 지우거나 셸 러너 태그로 잡을 추가할 수 있다. YAML 게이트는 잡이 대기열에 쌓이지 않게 할 뿐이다. **실제 통제는 GitLab 서버 설정이며 다음이 필수다:**
  1. `windows`·`macos`·`cgroup-delegated` 러너를 "Protected"로 설정하고, 프로젝트 러너면 "Lock to current projects"를 켜고, "Run untagged jobs"를 끈다. MR 파이프라인이 보호 변수·러너를 쓰게 하는 프로젝트 옵션도 끈다.
  2. Settings > CI/CD > General pipelines에서 "Use separate caches for protected branches"를 켠다.
  3. `main`과 릴리스 태그를 보호 ref로 지정한다(아니면 게이팅된 잡이 실행되지 않는다).
  4. `linux` 태그 러너와 태그 없는 잡을 받는 러너는 모두 docker executor여야 한다.
- **커버리지 트레이드오프**: macOS·Windows·cgroup 플랫폼 회귀 테스트는 이제 병합 후 보호 ref에서만 돈다. 플랫폼별 보안 변경은 병합 전에 로컬에서 돌리거나, 유지관리자가 보호 ref 파이프라인을 실행해야 한다. 릴리스는 녹색 보호 파이프라인에서만 태그한다.

### 인프라 결정 필요(로드맵만 기록)

- **HIGH — 릴리스 무결성**: DMG와 SHA-256이 같은 저장소 채널에 있고, ad-hoc 서명만 있으며 공증(notarization)이 없다. Developer ID와 공증 파이프라인, 보호 ref CI 빌드, 독립 채널을 통한 해시·서명 게시가 필요하다. `releases/README.md`에 미구현 로드맵으로 기록했다.
- **MEDIUM — vendored webgl 애드온**: 재빌드 후 diff로 검증하는 CI 잡이 없다.

## 통합 검증과 3차 회귀 수정

통합 검증 결과는 차단 1건, 중대 12건, 경미 25건, 사소 16건이었다. 차단 1건(`claude_provider_routing` 필드가 추가됐는데 TS 리터럴에는 빠짐)과 중대 1건(라우팅 기능을 구현보다 먼저 광고)은 병행 세션의 Z.ai 라우팅 작업에서 나왔다. 해당 세션이 둘 다 해결했다고 확인했다. Dockerfile 해시 건은 공식 값과 일치해 문제가 아니었다. 나머지 중대 10건은 이번 보안 수정이 만든 회귀였고 3차에서 고쳤다.

| 회귀(원 수정) | 증상 | 3차 수정 |
|---|---|---|
| IPC 128개 큐 상한(1차 #6) | 데이터 연결에도 적용됨. 작은 출력 레코드가 몰리면 연결이 끊기고 재연결·재생이 반복됨 | 넘치면 끊는 동작은 제어 연결에만 적용. 데이터 연결은 `reserve_frame`(try_reserve)으로 백오프하고, 흐름 제어는 바이트 예산이 맡음. 소켓 쓰기를 close 알림과 경쟁시켜 멈춘 피어도 정리됨. `mission.changed`는 미션별로 최신 것만 전달 |
| 샌드박스 읽기 차단(1차 #2) | `<data>` 상위 디렉터리의 메타데이터 조회까지 막혀 Node·git의 realpath가 EPERM으로 실패 | 조상 디렉터리마다 `(allow file-read-metadata (literal …))`만 허용(형제 미션·토큰은 계속 차단). 자격증명 deny 목록은 prepare 때 기록한 것을 재사용해, HOME이 바뀌거나 업그레이드해도 무결성 비교가 깨지지 않음 |
| codex 승인 질문 4 KiB 상한(1차 #7) | 실제 파일 변경 승인 대부분이 자동 거절됨 | 질문 상한 128 KiB, 누적 바이트에 질문 크기 포함. wire id 4 KiB와 256건 상한은 유지 |
| 어댑터 채널 bound(2차 #1) | 소비 속도를 가정한 메모리 한도, 기존 통합 테스트 실패 | 창별 256 KiB 바이트 할당량 추가. 액터는 자기 run의 Activity만 소모(run_id·토큰 둘 다 확인), 예산 밖으로 비움. 실패 경로에서 대기 이벤트가 사라지던 문제 복원. flush 실패 시 종료 이벤트를 최대 8회 미룸. 테스트는 연속 Activity를 이어 붙여 검증 |
| 저널 예산 시드(2차 #6) | 기존 설치가 이미 상한을 넘었으면 새 세션이 전부 JOURNAL_LIMIT로 실패 | 90% 초과 시 고정·활성이 아닌 가장 오래된 저널부터 80%까지 비우는 공간 압력 경로 추가(고아 파일 우선). 해제량은 실제로 지운 디스크 바이트. 해소 불가 상태에서는 5분마다 재확인. rotate는 예약 먼저, 검색 예산은 실제 읽은 바이트로 계산 |
| gated 실행 정리 drain 기한(2차 #10) | 기한 초과 시 구성원이 살아 있는데 Exited 기록과 예약 해제 | Stopping 기록 후 소유 레코드와 함께 그룹을 고정하고 `StopStranded` 반환. 비었음이 확인되면 reconcile이 Exited 커밋, retire, 예약 해제를 수행. 정지 경로에 finalized 확인 추가. 고정 정리 중 네이티브 락을 잡은 채 플랫폼 호출하지 않음 |
| macOS 정체성 실패 무시(2차 #4) | 살아 있는데 읽을 수 없는 구성원이 있어도 그룹이 비었다고 판정 | 읽을 수 없는 구성원을 unverifiable로 세어 비어 있지 않은 것으로 판단(시그널은 보내지 않음) |

그 밖에 고친 경미 이슈는 다음과 같다.

- `launch_shell`에서 `mark_running` 실패 시 워크로드 실패 처리, 셸 kill·reap, 정체성 정리.
- 싱글턴 락이 하드링크를 지원하지 않는 파일시스템에서 create_new 방식으로 폴백.
- 비공개 임시 파일을 create_new로 생성.
- 원격 host id 규칙을 contracts 전반에 통일(UUID 강제 대신 안전 문자 1~128자).
- Windows에서 `core.hooksPath`를 데몬 실행 파일로 지정(`\dev\null\<hook>`을 누가 만들어도 무력). `workspace/verification.rs`의 중복 `-c core.hooksPath=/dev/null` 인자를 지워 이 설정이 실제로 적용되게 함.
- IPC 바인드 실패 시 셧다운이 유실되지 않도록 구독 순서 조정.
- rustfmt·clippy 차단 9건.
- 05-remote.md의 identity 경로 `%` 규칙 모순 정리(identity 경로는 `%` 거절).

## 남은 과제

1. **승인과 내용의 바인딩**(1차 #8): 수락 UI에 파일별 실제 diff를 보여 주고, 승인 시점 diff와 캡처 시점 blob을 대조해야 한다. UX 설계 결정이 필요하다.
2. **GitLab 서버 설정 적용**(운영 작업): 위 필수 설정 1~4. 적용 전까지 CI 셸 러너 위험은 해소되지 않는다.
3. **릴리스 무결성 인프라**: Developer ID와 공증, 보호 ref 빌드, 독립 채널 해시, webgl 애드온 재빌드 diff 잡.
4. 데이터 연결에 슬로우 리더 판정이 없다. 제어 연결은 살아 있는데 데이터 피어가 읽지 않으면, 제어가 끊기거나 소켓 오류가 날 때까지 데이터 연결이 남는다(메모리는 bounded).
5. 어댑터 이벤트 메모리는 초당 바이트 입장 제한만 있고, 대기 바이트의 절대 상한은 없다(`agent_runtime/mod.rs`의 EventStream 공유 카운터 필요). run마다 20 Hz flusher 스레드가 하나씩 뜬다. 액터는 `live.starting` 동안 이벤트를 비우지 않는다.
6. 표시용 아티팩트에 durable GC가 없다(term-storage API 필요). 같은 창의 재발행은 dedup된다.
7. `session.search`가 세션마다 저널 디렉터리를 read_dir한다(한 번 나열로 묶고 대상 세션 수 상한 필요).
8. 고정·활성 저널만으로 90%를 넘으면 새 저널을 열 수 없다. 실행 실패 대신 저하(replay 저널 없이 시작 등) 방안 검토.
9. `workload.processes`, 훅 귀속, `agent_watch`가 여전히 bare PID로 셸·에이전트를 매칭한다(OS 동작은 없음).
10. 스케줄러 `.expect` 스레드 생성 실패(이견 건), Windows 파이프 이름 난수화, `mark_running`을 액터 기동 뒤로 옮기는 순서 조정, 관리 경로 헬퍼 reap 일관화.
11. 사소: `StopStranded.force_errors`는 플랫폼 백엔드가 전달 실패를 로그만 남겨 0일 수 있음(문서 보정 필요). `dispatch.rs`의 executor 거절이 ordered validate보다 먼저 실행됨.
12. 3차 감사 후보(2차 크리틱): Tauri 셸의 데몬 바이너리 해석·실행 체인, 릴리스 데몬의 테스트 탈출구, gate_listener 헬퍼 프로토콜, Z.ai 자격증명 저장·아웃바운드 흐름, 워크스페이스 내 바이너리·공유 /tmp 하네스.

## 검증 상태와 병행 작업

- 사용자 규칙(`no-auto-verification`)에 따라 **cargo build/test/clippy/fmt, tsc, npm test를 한 번도 실행하지 않았다.** 컴파일 가능성과 CI 게이트는 독립 리뷰 에이전트가 코드를 읽어 확인했을 뿐이다. 병합 전에 최소한 다음을 실행해야 한다: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `npm run typecheck`, `npm run test:unit`, 그리고 macOS·Windows·cgroup 플랫폼 테스트.
- 작업 중 같은 작업 트리에서 다른 Claude 세션들(Z.ai 라우팅, 미션 UX 등)이 `state.rs`, `sessions.rs`, `orchestrator.rs`, `lib.rs`, `codex/mod.rs`, term-contracts 등을 동시에 편집했다. 수정 에이전트는 부분 치환만 사용했고, 편집 범위는 세션 간 메시지로 조율했다. 병합 전 diff 검토 때 두 작업이 섞여 있음을 감안해야 한다.
- 규모: 감사 85기, 수정·리뷰 36기, 통합 검증 7기, 3차 20기. 약 150기, 1,000만 토큰 이상.
