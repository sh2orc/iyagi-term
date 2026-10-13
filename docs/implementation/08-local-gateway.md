# 08 — 로컬 관측 게이트웨이 (Local Observability Gateway)

> 상태: MVP (2026-10). 라우팅된 Claude Code pane의 모델 요청을 데몬이 한 번
> 거쳐 가게 하여 **세션별 토큰 장부**와 **요청 단위 킬 스위치**를 제공한다.
> 프록시는 패스스루다 — 헤더 재작성(인증)과 사용량 파싱 외에는 아무것도
> 바꾸지 않는다.

## 1. 동기

"CLI는 그대로, IYAGI는 리소스 거버넌스"라는 제품 경계에서 **비용·토큰도
리소스**다. 기존 가드는 프로세스(CPU·메모리)에만 붙어 있었고, 모델 사용량은
각 CLI의 구독 게이지로만 어렴풋이 보였다. 데몬이 이미 스폰 시점에
`ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN`을 주입하고 있으므로(`07` 이전의
`claude_provider` 라우팅), 목적지를 루프백 게이트웨이로 바꾸는 것만으로
관측 지점이 생긴다 — CLI 내부를 건드리지 않고.

## 2. 구성

* **서버** (`iyagi-termd/src/gateway.rs`): 데몬 기동부터 항상
  `127.0.0.1:<임시 포트>`에 뜬다. 바인드 실패는 기능 침묵(라우팅 불가)이지
  기동 실패가 아니다. 한 연결 = 한 요청(`Connection: close`).
* **계약** (`term-contracts/src/gateway.rs`): `gateway.status` /
  `gateway.set_policy`의 파라미터·결과 타입. ts-rs 미러가
  `src/generated/Gateway*.ts`로 생성된다.
* **RPC** (`dispatch.rs`): `GATEWAY_STATUS`, `GATEWAY_SET_POLICY`. 구 데몬은
  이 메서드가 없다 — UI는 실패를 "지원 안 함" 안내로 흡수한다.
* **UI** (`IntegrationPanel.tsx` → GatewayPanel): 토글, 사용량 요약(2초 폴),
  킬 스위치 버튼. 데몬 정책은 데몬 수명이라, 정책 전체(`enabled`·`blocked`·
  `pool_strategy`)를 `preferences`(`gatewayObservability`·`gatewayBlocked`·
  `gatewayPoolStrategy`)에 저장하고 `SessionController`가 **붙을 때마다**
  (시작 시 작업 공간 복원 전, 데몬 재연결 시) 다시 적용한다 — 설정 화면을
  열지 않아도 재시작 뒤 첫 pane부터 게이트웨이를 거친다.

## 3. 라우팅 결정 규칙

`enabled`(관측 라우팅 on/off)은 **"새 라우팅을 감쌀까"만** 결정한다. 서버는
항상 수신하고, 이미 감싸인 세션의 토큰은 계속 유효하다 — 도중에 꺼진다고
감기를 풀면 토큰이 어긋나 401이 된다.

| 시점 | 코드 | 하는 일 |
|---|---|---|
| 셸 spawn 직전 | `orchestrator::launch_shell` → `Gateway::rewire` | base URL → `http://127.0.0.1:<현재 포트>`, 토큰 → 1회용 게이트웨이 토큰, redactor 재생성. `register_workload`가 redactor를 복사해 가기 **전에** 한다. |
| 관리 실행 시작 | `start_managed_admitted` → `Gateway::start_route` | 시작 시점에 토큰을 다시 결정: 게이트웨이면 1회용 토큰, 아니면 실제 키 재판독(`zai_key_missing` 규칙 유지). |
| gate 프레임 조립 | `Gateway::frame_env(env, via_gateway)` | 토큰 발급 판정을 그대로 받아 base URL을 확정한다 — 데몬 재시작으로 포트가 바뀌어도 묵은 URL을 물지 않고, 토큰·URL이 한 판정으로 묶여 어긋나지 않는다. |
| 워크로드 종료 | `DaemonState::note_workload_terminal` → `forget_workload` | 장부 항목 제거(토큰 무효화). 비정상 잔존은 상한(256) + LRU 퇴출이 보험. |

## 4. 프록시 동작

* **범용 패스스루**: `/healthz`(인증 없음)와 인증·차단 검사를 제외하면 모든
  메서드·경로를 업스트림(Z.ai Anthropic 호환 엔드포인트)에 그대로 넘긴다.
* **인증**: 자식은 `Authorization: Bearer iyagi-gw-…`(또는 `x-api-key`)를
  보낸다. 등록된 토큰이 아니면 401(Anthropic 오류 JSON 모양). 192비트 난수.
* **킬 스위치**: `blocked`면 등록된 토큰이어도 403 `permission_error`.
* **업스트림 헤더**: 허용 목록(`content-type`, `accept`,
  `anthropic-version`, `anthropic-beta`, `user-agent`, `x-app`)만 실어 보내고
  인증은 **실제 키를 요청 시점에 `term-secrets`에서 다시 읽어** 두 헤더
  (`authorization`/`x-api-key`)로 다시 쓴다. 회전·삭제가 즉시 반영된다.
* **본문**: 요청은 Content-Length 필수(청크 요청은 411, Claude Code는 고정
  길이 JSON만 보낸다), 상한 64 MiB. 응답은 SSE면 청크 인코딩으로 스트리밍,
  아니면 버퍼(상한 8 MiB) 후 content-length로 응답.
* **사용량 파싱**(`/v1/messages`만): SSE는 `data:` 줄 단위로
  `message_start`(input·cache)와 `message_delta`(output 누적값의 최종)을,
  비스트리밍은 응답 JSON의 `usage`를 읽는다. 파싱은 스캔일 뿐 — 본문을
  저장하지 않는다(저널·DB 무관).

## 5. 보안 불변

1. 루프백 전용 바인드. 게이트웨이 토큰 없는 요청은 업스트림에 절대 안 간다.
2. 게이트웨이 토큰·실제 키는 로그·상태 응답에 절대 나오지 않는다. 상태는
   워크로드/세션 id·사용량·모델명만 싣는다.
3. 실제 키의 수명: 요청 시점 판독 → 업스트림 헤더 직행. 자식 환경·게이트웨이
   토큰 맵 어디에도 남지 않는다(셸 모드에서 자식이 물고 있는 것은 1회용
   게이트웨이 토큰뿐이다).
4. PTY 출력에서 1회용 토큰은 기존 redactor 경로로 지워진다(rewire가
   redactor를 해당 토큰으로 다시 만든다).

## 6. 명시적 비목표 (MVP)

* **심층 프로토콜 번역** — thinking envelope·compaction·모델 치환(라우팅
  재작성)은 하지 않는다. 업스트림·다운스트림 프로토콜이 같아서 가능한
  패스스루 관측이 이 설계의 전부다.
* **Codex/OpenCode 경유** — 현재는 Z.ai 라우팅 Claude pane만. 다른
  CLI·프로바이더는 업스트림만 바꾸면 되는 구조적 확장(후속).
* **비용(USD) 환산·예산 차단** — 토큰 장부만 계량. 가격표·예산 게이트는
  미션쪽 `Usage` 체계와 합칠 때 설계한다.
* **영속 장부** — 데몬 수명으로 한정(재시작하면 0부터). SQLite 이관은
  예산·일별 리포트가 필요할 때.
* **claude-exec / 미션 런타임 경유** — `iyagi-termd claude-exec`와
  `agent_runtime::claude`는 아직 직접 라우팅이다(후속).

## 7. 시험

`crates/iyagi-termd/src/gateway.rs`의 `#[cfg(test)]`:

* 발급/재발급/무효화, 정책 토글 독립성, `frame_env` 판정 고정.
* SSE 스캔의 청크 경계 분할·누적 output 최종값.
* **전 구간 목업**: 로컬 HTTP 목업 업스트림 ↔ 게이트웨이 ↔ 클라이언트로
  (1) 실제 키만 업스트릍에 도달하고 게이트웨이 토큰은 새지 않으며 (2) SSE가
  청크로 그대로 흐르고 (3) 장부에 사용량이 남는지.
* 인증 실패 → 401 + 장부 무변화, 킬 스위치 → 403.
