# 03. Runtime/provider adapter 계약

[명세 입구](../../ORCHESTRATION_SPEC.md)

## 1. 공통 포트

daemon 내부 interface이며 wire DTO는 contracts.ts를 사용한다. adapter는 DB/UI를 직접 수정하지 않고 아래 정규화 이벤트만 반환한다.

```text
probe(binding) -> installation version + evidence-scoped capabilities
start(RunStart { run_id, fencing_token, binding_snapshot, context_ref,
                 workspace, permission_policy, result_schema }) -> Started
send_message(provider_session_id, message_id, body_ref) -> DeliveryReceipt
answer(provider_request_id, decision_id, answer) -> DeliveryReceipt
interrupt(provider_session_id, provider_turn_id) -> CancelReceipt
inspect(provider_session_id, provider_turn_id) -> Running|Finished|Absent|Unknown
close(exec_id) -> confirmed ownership cleanup
```

이벤트는 Started(provider IDs), Activity(chunk ref), ApprovalRequested(exact provider request), Usage(observed), RateLimited(observed/reset UTC milliseconds), Result(AgentResult), Failed(normalized code), InvalidResult(완료 답변의 형식 검증 실패), FailedBeforeSubmission(작업 요청 미전송 증거), Disconnected다. 모든 이벤트에 Run ID와 fence를 포함한다. 종료 이벤트 이후 delta로 상태를 다시 바꾸지 않으며, process cleanup이 확인되기 전에는 Run 종료를 보류한다.

InvalidResult는 Codex의 correlated turn/completed, Claude의 성공 result envelope와 exit 0, OpenCode의 소유 session/parent/message/model이 일치하는 영속 완료 답변에서만 schema 실패로 만든다. daemon의 검증된 수리 가능 Plan 오류도 같은 이벤트로 정규화한다. 초기화 JSON 오류·프로토콜 상한 초과·완료 envelope 누락/중복·소유권 불일치는 일반 실패로 유지한다. 거절한 답변은 메인 IPC에 싣지 않고 미션 소유 artifact로 저장하며, Lead Plan에만 제한된 형식 수리를 허용한다. 보통 실패 코드만으로 형식 수리를 추론하지 않는다.

FailedBeforeSubmission은 CLI의 자유 텍스트나 오류 코드만으로 만들지 않는다. Codex는 `turn/start` 쓰기를 시도하기 전에 발생한 transport 실패만 해당하며, 부분 쓰기 이후에는 Disconnected로 남긴다. Claude의 인증 stream은 user 프레임을 아직 꺼내지 않은 초기화 실패를 구분한다. OpenCode는 작업 session/prompt 이전 server 준비 단계의 확인된 transient IO 실패를 사용하며, factory 오류 반환 전에 소유 process cleanup을 확인한다. AUTH/MODEL/POLICY/형식 오류는 이 증거로 자동 재시도하지 않는다. actor는 종료 확인 후 이 증거를 Run에 저장하고, 스케줄러가 별도의 새 시도를 판단한다.

RateLimited는 비종료 이벤트다. provider가 명시한 제한과 유효한 미래 reset만 정규화하고 raw header/body는 포함하지 않는다. Claude는 `rate_limit_event.rate_limit_info.status=rejected`와 `resetsAt`을 사용한다. `allowed`/`allowed_warning` 및 overage의 거절만으로 현재 연결을 차단하지 않는다. [공식 SDK 제한 이벤트](https://code.claude.com/docs/en/agent-sdk/python)

Codex는 저장된 공식 `AccountRateLimitsUpdatedNotification` schema의 명시적 `rateLimitReachedType=rate_limit_reached`와 소진된 창 또는 `spendControlReached`와 individual limit reset을 사용한다. OpenCode는 저장된 OpenAPI의 `APIError`/429 및 `responseHeaders`에서 대소문자 구분 없이 하나의 `Retry-After`만 읽고 초 또는 HTTP-date를 해석한다. 중복·누락·잘못된 시각은 미확인으로 남는다. 희소한 정상 이벤트로 기존 제한을 지우지 않고, 전송 당시 계산한 deadline을 사용해 DB 재시도로 대기가 연장되지 않게 한다.

Startedは接続成功ではなくセッション/turnが受理された根拠を持つ。CancelReceiptも要求受付と終了確認を区別する。初期実装の`bool`返却は禁止。

## 2. 入力・出力制限

raw protocol line/frame最大1 MiB。改行が来ない入力も上限で切断し RESULT_INVALID。stderrはbounded診断spoolへ送る。巨大ツール出力はartifact chunkへ流し、main IPCに全文を載せない。秘密値のredactionは診断保存前に行う。

構造化 AgentResultの参照IDはdaemonが提供したIDに解決できることを確認する。モデルが架空のartifact IDを返したらRESULT_INVALID。モデルが作る本文はadapterがartifactに登録してrefを置換する。そのためモデル用output schemaはwire AgentResultをそのまま使わず、`report_text`, `question_text`, `findings[].evidence_text`, `tasks[].objective_text`を許す専用ProviderResult DTOとし、O02で全variantを定義して相互変換試験を行う。生のfilesystem pathをArtifactRef扱いしない。

작업 종류가 없는 기존 호출은 최상위 객체의 필수 `result` 안에 여섯 ProviderResult의 `anyOf`를 둔다. 종류가 지정된 일반 작업은 쓰기 작업의 patch 또는 읽기 작업의 report와 question/blocked만 허용한다. native Verify에는 모델의 성공 보고를 허용하지 않는다. Plan/Review는 실제 Codex의 복잡한 객체 union 선택 실패를 확인해 `format: iyagi-result-v2`와 평탄한 nullable `result` 필드를 사용한다. 서버가 전달한 TaskKind에 따라 성공 kind 또는 question/blocked를 허용하며, 스키마를 작업 문맥에도 포함한다. kind별 활성 필드는 원래 타입을 유지하고 비활성 필드는 null이어야 한다. 모든 필드를 필수로 요구하며 활성 빈 배열은 []다. 디코더는 비활성 데이터·누락·unknown 필드를 거절한 뒤 canonical ProviderResult로 재검증한다. 모든 객체는 additionalProperties=false이며 kind는 문자열 enum이다. Codex·Claude·OpenCode는 같은 작업별 스키마와 파서를 사용하고 과거 직접 DTO/일반 envelope도 계속 엄격하게 읽는다. 독립 JSON Schema validator와 canonical 파서의 긍정/부정 사례를 검사한다. [공식 Structured Outputs의 최상위 객체 제약](https://developers.openai.com/api/docs/guides/structured-outputs#root-objects-must-not-be-anyof-and-must-be-an-object)

モデルにprovider認証値やOSのsecret store参照を送らない。model_idが実際のresponse metadataで異なる場合、requested/observedを両方記録する。metadataがなければ observed=null。UI表示だけの短縮名をAPI IDとして使わない。

## 3. Codex

確認日2026-09-13。公式資料は `app-server`のstdio JSONL、初期化、thread/turn、model/list、approval、outputSchemaを記載している。WebSocket等にはexperimentalの注意がある。O1はローカルstdioを対象とし、インストール版のschemaとホスト試験で保証する。[公式App Server資料](https://learn.chatgpt.com/docs/app-server)

実装手順:

1. `Command(program)`で`app-server`を起動。stdin/stdout pipe、stderrは診断。現在usageコードの`--stdio`を無条件流用せず、検証バージョンのhelp/schemaに一致するargv fixtureを保存する。
2. JSON-RPC `initialize`にclientInfoと必要最小capability、その成功後`initialized`。内部Iyagi envelopeを外部protocolに流用しない。
3. `account/read`と`model/list`で選択bindingを検証。auth flowはCodex公式機構に任せる。IyagiがOAuth tokenを独自に再実装しない。
4. `thread/start`でmodel/cwd/approval/sandboxを明示する。再開は所有を記録したthreadだけ`thread/resume`。
5. `turn/start`にinput/result outputSchemaを指定。返されたthread/turn IDを保存。
6. item/agentMessage/delta等をactivityへ、tool approval requestをDecisionへ正規化。hidden reasoningの取得や画面化を試みない。
7. `turn/completed`のstatusと構造化結果を組み合わせる。partial textの末尾を完了と推定しない。
8. 実行中入力は対応版の`turn/steer`、中断は`turn/interrupt`。通信復旧時はthread/read等の公式状態取得で確認する。

thread/start等の正確なparamsは対象CLIの`app-server generate-json-schema`の成果物をO08 fixtureに固定する。未確認フィールドを推測で追加しない。probeはそれ自体で実際のモデル利用権限や課金成功を保証しない。

app-server実行をPTYへ変換してprotocol JSONを擬似ターミナルに表示しない。native terminal attachは同一runへの接続が検証できた版だけ有効。それ以外はactivity viewerを表示する。

관리 구독의 모델 요청은 `https://chatgpt.com/backend-api/codex`, API 키는 `https://api.openai.com/v1`로 분리한다. 계정 메타데이터의 ChatGPT 주소는 별도 `https://chatgpt.com/backend-api/`다. API 키용 모델 주소를 구독에도 강제하면 account/read는 성공해도 실제 추론은 401로 실패하는 것을 실제 CLI에서 확인했다. 시작 시 반환된 실제 설정을 인증 방식과 대조한다.

자동 작업은 앱/플러그인/훅·다중 에이전트·브라우저/컴퓨터 도구·shell snapshot·로그인 shell·notify를 사용하지 않는다. managed login의 MCP 테이블은 빈 객체를 덮어써도 기존 항목이 병합되므로, 새 thread의 config에서 관측한 각 연결을 비활성화하고 thread별 MCP 상태/도구 목록까지 확인한 뒤 프롬프트를 보낸다. 사용자 설정 파일은 수정하지 않는다. 네트워크가 금지된 작업은 web search도 끈다. 쓰기 작업은 기존의 절대 workspace 경로가 없으면 app-server를 생성하지 않는다. 승인 정책은 그 실행이 받은 쓰기 권한을 따른다 — 읽기 전용 실행은 `never`(샌드박스가 읽기 전용이고 네트워크도 꺼져 있어 승인으로 더 막을 것이 없다), 쓰기 실행은 `on-request`(쓰기는 이미 그 실행의 worktree로 묶여 있고, 그 밖을 원할 때만 제공자가 묻는다). 경계는 승인 프롬프트가 아니라 샌드박스가 정한다 — `untrusted`를 얹으면 `pwd`·`git status` 한 줄까지 사용자 결정으로 올라와 계획 단계에서 실행이 멈춘다. 도착한 승인 요청은 그대로 사용자 결정으로 전달한다.

## 4. Claude Code

公式の非対話実行は`-p`、JSON/stream-json、構造化結果、会話再開を提供する。bare modeと通常モードには認証・設定読込の違いがある。[公式programmatic usage](https://code.claude.com/docs/en/headless)

O1 baselineはCLI print adapter。起動argvの基本形は`-p --output-format stream-json --verbose --include-partial-messages`。model/result schema/permission settingsは検証した版の追加argv fixtureで指定する。promptはstdinまたはprivate入力ファイルで渡し、シェル文字列結合しない。

1. program/version確認、bindingごとのauth route・設定scope検証。
2. run固有の設定/環境で起動。通常のAnthropic bindingとZ.ai bindingを同時実行して干渉しないことをfixtureで確認。
3. system/session metadataとresultからprovider session IDを取得。文字列画面解析で推定しない。
4. stream eventsを正規化し、final resultのerror/successとprocess exitを両方確認。
5. resumeは所有sessionだけ。active turnへの対話的steer/approval_replyはprint adapterで確認できなければfalse。入力は次回runにqueueする。
6. 許可外tool要求をprint経路で対話承認できない場合、実行を止めてcapability不足を返す。自動Enterや全権限flag追加で回避しない。
7. 検証済みSIGINT/終了経路で中断する。SIGTERM時の未完了turnを正常結果として扱わない。

`--bare`を全bindingへ自動追加しない。subscription認証を期待する設定と異なる動作になるため、auth routeごとに明示的に検証する。SDK経路を追加する場合は同じnormalized portを実装し、別のcapability証拠を持たせる。

대화형 pane 라우팅(`LaunchRequest.claude_provider`, 구현 01 §2)은 이 adapter의 미션 run과 별개다. 설정 → Z.ai Coding Plan의 스위치는 Iyagi이 시작·재개하는 Claude Code **터미널**에만 적용되고 미션 binding에는 영향을 주지 않는다. 다른 점 다섯 가지. (1) 키 저장소는 미션 binding의 모양에 따라 갈린다 — pane과 **참조 없는** 미션 Z.ai binding(빠른 설정의 `Claude Code · Z.ai Coding Plan` 카드가 만드는 것)은 같은 앱 전용 암호화 파일(`<data_dir>/secrets/zai.enc`, 구현 01 §7)을 실행 시점에 읽고, 두 참조를 가진 미션 binding만 `iyagi-termd connection add`의 OS 키체인 참조를 쓴다. 키가 없으면 실행이 `zai_key_missing`으로 거절된다. (2) pane에는 초기화 응답 검증이 없다 — 위 1·2단계의 auth route·설정 scope 확인을 하지 않고, 사용자가 화면에서 직접 보는 대화형 세션이므로 Claude Code가 출력하는 인증 출처 경고를 그대로 보여 준다. (3) pane은 사용자의 `~/.claude` 전체(hooks·statusLine·plugins·MCP·세션·resume)를 그대로 Z.ai 아래에서 실행한다. 미션 run의 별도 HOME/config 격리는 없다. (4) 모델 지정 방식이 다르다 — pane은 `--model` argv를 붙이지 않고 alias env(`ANTHROPIC_DEFAULT_OPUS_MODEL`/`SONNET_MODEL`/`HAIKU_MODEL`)로 슬롯만 바꿔 사용자의 `/model` 선택과 settings.json이 그대로 살아 있게 하고, 미션은 검증한 argv fixture에 정확한 model ID를 명시한다. (5) 튜닝 값은 공유한다 — 요청 타임아웃(`API_TIMEOUT_MS`)은 경로의 성질이라 Z.ai binding이면 언제나 넣고, 자동 압축 창(`CLAUDE_CODE_AUTO_COMPACT_WINDOW`)은 모델 id가 `[1m]`으로 1M 컨텍스트를 선언할 때만 넣는다(선언하지 않은 모델에 1M을 박으면 압축이 너무 늦다). 값은 `claude_provider`의 상수 하나에서 pane·`claude-exec`·미션이 함께 읽어 세 경로가 갈라지지 않게 한다. [Z.ai Claude Code 연결](https://docs.z.ai/devpack/tool/claude), [환경 변수](https://code.claude.com/docs/en/env-vars), [LLM gateway 연결](https://code.claude.com/docs/en/llm-gateway-connect)

## 5. OpenCode + Z.ai

OpenCode公式server APIはsession、message、abort、provider metadata、SSEを公開している。Z.aiはOpenCodeでの接続を案内する。[OpenCode server](https://opencode.ai/docs/server/)、[Z.ai integration](https://docs.z.ai/devpack/tool/opencode)

1. run単位のdaemon所有server processをlocalhost限定で起動。認証付きアクセスを設定し、使用するport/credentialsをログに出さない。実際の起動flagsと認証方式は対象版の公式仕様/試験で固定。
2. health確認後session作成、providerID/modelIDを明示してmessage送信。OpenCode既定モデルへの暗黙fallback禁止。
3. SSEからsession/turn correlationとactivityを正規化する。SSE切断は結果不明であり再POSTの理由ではない。
4. session statusと保存messageを照合して完了確認。構造化結果の取得方法は対象版のOpenAPIからfixture化し、未対応版はautomated=false。
5. `/session/:id/abort`で中断要求し、状態と所有server子プロセスの終了を確認。
6. Z.ai endpoint/model/auth routeをbindingで保持。subscription coding endpointと一般API endpointを別設定として扱う。

OAuthやAPI key設定をユーザーのglobal configに自動書込みしない。run別config overrideがその版で保証されない場合、別config rootを検証するかbindingをunsupportedにする。GLM-5.3の文字列を他モデルのaliasへ黙って変更しない。

## 6. Capability登録と権限

capability証拠は`runtime, version, OS, provider, auth_route, tested_at, fixture_digest, results`で保存する。新しいCLI版では既存証拠を無条件に引き継がずunknownに戻す。UIは手動terminal互換と自動task実行互換を別表示する。

현재 probe 경로는 runtime·OS·정확한 버전에 제공자·인증 경로·저장된 인증 참조 유무를 함께 대조한다. Codex `openai`/Claude `anthropic`의 기존 구독 로그인 증거를 API 키나 별도 토큰 연결로 옮기지 않는다. OpenCode 구독 증거는 `zai-coding-plan`과 저장된 두 참조가 있는 연결에만 적용한다. prerelease/build 접미사를 보존하며 미등록 버전·실패한 조회는 unknown으로 되돌린다. `--version` 성공은 현재 계정·모델 권한이나 인증 성공의 증거가 아니다. 실제 CLI의 클라이언트 입력 capability는 증거가 아니다. 서버 소유 설치 관측과 현재 registry로 목록·저장 응답·새 Run snapshot을 만들고, 실행 준비에서도 같은 관측인지 확인한다. 오래된 DB의 관측 출처 없는 값이나 다른 OS의 기록은 unknown으로 처리한다.

출시 증거 자체(위 문단, sha256로 고정한 라이브 fixture와 macOS aarch64 Codex 0.154.0 기록)는 이 정확한 버전 대조로만 확정되며 바뀌지 않는다. 같은 OS·같은 라인의 더 높은 버전에는 [11 §5](11-local-evidence.md#5-버전-라인-규칙)의 라인 규칙을 만족하고 **이 버전의 로컬 프로토콜 자가 진단(`protocol_ok`)이 통과할 때만** 그 출시 증거를 적용한다(`version_line`, [11 §4](11-local-evidence.md#4-capability-투영-규칙-capability_evidencecapabilities_for_binding)). 진단이 없거나 실패하면 적용하지 않고 `SameLineUnverified`로 남는다.

필수 capability 검사는 역할 배정과 실제 TaskKind 모두에 적용한다. Implement/TestAuthor/Integrate/Document는 scoped_write, 다른 모델 작업은 read_only를 요구한다. 결정적 검증 명령과 모델을 사용하지 않는 Git 통합에는 적용하지 않는다. 시작 worker는 adapter.start 전에 필수 기능과 서버 관측을 다시 확인하고, 상한이 있는 로컬 버전 조회로 Run snapshot과 정확한 버전 일치를 검사한다. 실패는 CAPABILITY_UNSUPPORTED와 미전송 증거로 저장하며 자동 재시도하지 않는다. 저장된 설치 관측이나 과거 Run의 버전을 자동 교체하지 않는다. 설치 관측 format 2는 canonical entrypoint 경로·길이·SHA-256과 관측이 저장된 binding revision을 보존한다. 버전 조회 전후에 지문과 파일 metadata의 안정성을 확인하고 시작 worker에서도 저장된 지문을 먼저 대조한 뒤 버전을 다시 조회한다. 같은 버전의 다른 파일 및 symlink 대상 변경은 거절한다. 이전 지문 없는 관측은 다시 설치 확인해야 한다. 같은 시각에 재검사해도 이전 Run의 binding revision보다 새 관측을 적용하지 않는다. 이 단계는 entrypoint 파일을 검사하며 interpreter·wrapper가 실행하는 별도 바이너리·로드된 이미지 고정과 마지막 검사 이후 OS exec 사이의 원자적 교체 방지는 아직 남아 있다. macOS aarch64의 Codex 0.154.0/openai/구독/gpt-5.6-luna에 한해 production adapter의 실제 읽기·파일 변경 승인·즉시 취소와 command/exec의 12개 sandbox 경계를 확인해 scoped_write를 포함한 필수 기능을 등록했다. resume/steer/usage/native_terminal_attach는 이 조합에서도 미검증이다. [증거와 재현 명령](CODEX_MACOS_01540.md).

自動run必須: structured_result, events, cancel、およびroleに必要なread_only/scoped_write。resume/steer/native_terminal_attachは任意。Unsupportedならボタンを理由付きで無効化する。

read_only/scoped_writeはprompt上のお願いではない。native tool permissionとsandbox、または外部隔離executorによる実効制限を確認する。Bashが任意ホスト書込可能なままscoped_write=trueにしない。対応しない組合せは一般terminalにのみ残す。O1の完全対応OS/役割表は実際の試験後に確定する。

실행 도구가 스스로 읽는 지시 파일(CLAUDE.md·AGENTS.md·skill·사용자 정의 agent)의 차단과 관측, 저장소 역할 지시문의 전달은 [10 역할 지시문](10-role-instructions.md)을 따른다.

### 실험적 연결(사용자 동의)

증거가 없는 CLI 버전은 연결별 명시 동의(`Binding.experimental_version`)가 있을 때만 자동 실행한다. 동의는 증거가 아니며 다음 규칙으로만 capability에 반영된다([01 §3](01-contracts.md#3-rpc와-전송), [11 §3.4](11-local-evidence.md#34-bindingexperimental_version-의미-변경)).

1. 증거가 supported로 증명한 기능은 그대로 둔다(실험 표시 없음). 증거가 없거나 미증명인 기능에만 동의를 적용한다. 일부 기능만 증명된 버전(예: Linux Codex 0.154.0의 `scoped_write`)도 동의하면 나머지 구현 기능을 실험적으로 쓸 수 있다.
2. 동의는 연결당 한 번이다. 값은 동의 시점 관측 버전(표시용)일 뿐 이후 CLI가 업데이트돼도 그대로 유효하며 재동의를 요구하지 않는다(`experimental_version_mismatch`는 더 이상 발생하지 않는다). 버전이 바뀌면 재동의 대신 로컬 자가 진단을 다시 돌린다(설치 확인 = `지금 확인`).
3. 어댑터가 그 provider/인증 경로를 실제로 실행할 수 있어야 한다. 아래 경로 밖(local/custom 인증, 참조가 맞지 않는 조합)은 동의해도 `no_compatibility_evidence`로 남는다.
4. 어댑터 코드가 구현한 기능만 `experimental_opt_in`으로 supported가 되고, 구현하지 않은 기능은 `adapter_not_implemented`로 계속 막는다. 역할 필수 기능(structured_result·events·cancel과 read_only 또는 scoped_write)이 모두 구현된 경우에만 그 역할이 시작 gate를 통과한다. 모델은 binding의 모델 그대로이며 대체하지 않는다.
5. 로컬 자가 진단이 실행되어 어떤 기능을 실패로 증명했으면(`local_probe_failed`) 동의로도 그 기능은 열리지 않는다. 동의는 "증거 없음"만 대체하고 "증거가 실패를 증명함"은 대체하지 않는다.

### 로컬 자가 진단과 실행 관측

daemon은 모델 호출·인증 토큰 없이 이 PC에서 무추론 자가 진단을 돌려(`binding.probe` 시, 그리고 `runtime.detect`의 저비용 버전인 `run_cheap`) 프로토콜 handshake·OS sandbox 경계·모델 목록 존재를 증명한다(`local_probe`, [11 §6](11-local-evidence.md#6-로컬-자가-진단-agent_runtimelocal_proberrs-신설)). 여기에 이 PC·이 모델의 성공 실행 횟수가 임계치를 넘으면 추가로 승격한다(`observed_runs`, [11 §4](11-local-evidence.md#4-capability-투영-규칙-capability_evidencecapabilities_for_binding)). 두 층 모두 출시 증거 없이 4역할이 동의 없이 통과할 수 있으며 그 결과가 `CompatibilityGrade::VerifiedLocally`다([11 §3.2](11-local-evidence.md#32-compatibilitygrade), [11 §7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers)).

어댑터가 구현한 기능(production mission 경로 기준, 2026-09-17 코드 확인):

| 기능 | Codex app-server | Claude Code print | OpenCode server | 판단 근거 |
|---|---|---|---|---|
| structured_result | 구현 | 구현 | 구현 | Codex `turn/start.outputSchema`, Claude 인증 경로 `--json-schema`, OpenCode prompt `format: json_schema` |
| events | 구현 | 구현 | 구현 | app-server notification, `stream-json`, SSE |
| cancel | 구현 | 구현 | 구현 | `turn/interrupt`, 감독 Exec 중단 단계, `/session/:id/abort`+소유 server 종료 |
| resume | 미구현 | 미구현 | 미구현 | mission 경로에서 `thread/resume` 호출 없음, Claude 인증 실행은 resume 거절, OpenCode는 새 session만 |
| steer | 구현 | 미구현 | 미구현 | `turn/steer`(증거 capability가 있어야 사용), print/server는 SteerUnsupported |
| approval_reply | 구현 | 미구현 | 구현 | approval 응답 전송, Claude는 prompt 거부 모드라 응답 채널 없음, `/permission/:id/reply` |
| read_only | 구현 | 구현 | 구현 | sandbox `readOnly`, `--permission-mode plan`+Read/Glob/Grep, permission `*` deny+read/glob/grep |
| scoped_write | 구현 | 구현 | 구현 | sandbox `workspaceWrite`(writableRoots=workspace), `acceptEdits`+Edit/Write만 허용(셸 없음·prompt 거부), `edit` 허용+`external_directory` deny. 어느 경우든 daemon capture가 허용 경로 밖 변경을 다시 거절한다 |
| model_listing | 구현 | 미구현 | 미구현 | `model/list` 검증, Claude 없음, OpenCode `provider_models`는 mission 경로에서 쓰지 않음. 설정 화면의 후보 목록은 이 capability와 무관한 별도 경로다 — 아래 "모델 고르기 힌트" 참고 |
| usage | 구현 | 구현 | 구현 | token usage notification, result envelope usage, assistant message tokens/cost |
| native_terminal_attach | 미구현 | 미구현 | 미구현 | 모두 파이프/HTTP 프로세스 |

따라서 동의 시 세 어댑터 모두 lead·builder·reviewer·integrator 필수 기능을 갖춘다. 차이는 선택 기능(steer·approval_reply·model_listing)과 실행 가능한 인증 경로다.

| runtime | 실행 가능한 provider/인증 경로 |
|---|---|
| Codex | `openai` + subscription(참조 없음, 관리 로그인) 또는 api_key(credential_ref·endpoint_ref 모두) |
| Claude Code | `anthropic` + subscription(참조 없음, 관리 로그인), `zai-coding-plan` + subscription(참조 없음, 앱 전용 키 저장소), 또는 `anthropic`/`zai-coding-plan` + api_key·subscription(두 참조 모두) |
| OpenCode | 저장된 연결(credential_ref·endpoint_ref 모두) + api_key·subscription, provider 비어 있지 않음 |

runtime.detect의 `grade`(`verified`/`verified_locally`/`same_line_unverified`/`unverified`/`not_installed`, [11 §7](11-local-evidence.md#7-저장투영게이트-missionservicers-missionbinding_evidencers))/`experimental_roles`는 이 규칙을 감지 행의 미저장 구독 binding에 적용한 결과다. `verified_locally`는 출시 증거 없이 이 PC의 로컬 자가 진단·실행 관측만으로 네 역할이 동의 없이 통과할 때다. OpenCode 감지 행은 연결 참조가 없으므로 `experimental_roles`가 비어 있고, 연결 저장 후 probe로 판단한다.

### 모델 고르기 힌트 (mission 실행 경로 밖)

위 기능 표는 **mission 실행 경로**를 적은 것이다. 설정 화면의 모델 칸을 채우는 후보 목록은 그와 별개의 읽기 전용 경로이며, 어떤 경우에도 capability evidence가 아니다 — 호환 등급이나 역할 판정에 영향을 주지 않고, 실패하면 빈 목록이 될 뿐 `runtime.detect`나 `binding.probe`를 실패시키지 않는다.

| runtime | 힌트 출처 | 비고 |
|---|---|---|
| Codex | `codex app-server`에 `initialize` → `model/list`(`nextCursor` 페이지네이션) | `hidden` 항목 제외, `supportedReasoningEfforts`를 effort로 옮긴다 |
| OpenCode | `opencode models`가 찍는 `provider/model` 줄 | 서버를 띄우지 않는다. `provider_models`(`/config/providers`)는 인증된 run 소유 서버가 필요해 고르기 힌트에는 과하다. `runtime.detect`에서는 조회하지 않는다 — 감지 행만으로는 연결을 만들 수 없어 쓸 수 없는 힌트다. `binding.probe`에서 provider와 함께 제공된다 |
| Claude Code | 목록 API 없음 → `--model`이 받는 별칭(`opus`·`sonnet`·`haiku`) + 로컬 transcript에서 관측된 모델 id | 버전이 박힌 id를 하드코딩하지 않는다(바로 낡는다). 연결의 provider로 거른다(anthropic이면 `claude-*`와 별칭만), 최신 관측순 정렬 |

`capability_evidence`의 `CODEX_MACOS_MODEL` 같은 상수는 sha256으로 고정된 라이브 증거 픽스처와 묶여 있으므로 "최신 모델"로 갱신하는 대상이 아니다. 고를 수 있는 모델을 늘리는 일은 전부 이 힌트 경로에서 한다.

## 7. Fake adapter

開発とCIの既定はfake。ネットワーク/認証不要で、以下のscriptをfake clockで再生する。

- start→activity→valid result→cleanup
- invalid result / missing final / duplicate final
- approval→answer→finish / obsolete approval
- accept後disconnect / start送信前failure / timeout
- observed model mismatch / unknown usage
- cancel accepted but process alive / stale fencing callback
- read-only違反 / artifact上限 / stdout改行なし1MiB超過

fakeを通常のbinding一覧に出すのはdevelopment buildだけ。fake成功を本物のprovider成功と混ぜて使用量・品質評価へ記録しない。
