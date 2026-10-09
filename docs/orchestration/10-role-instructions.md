# 10. 역할 지시문과 실행 도구 지시 파일 격리

[명세 입구](../../ORCHESTRATION_SPEC.md) · 상한: [defaults.json](defaults.json) `max_role_instruction_bytes` · 구현 상태: [IMPLEMENTATION_STATUS](IMPLEMENTATION_STATUS.md) 49절 · 관련: [03 adapter](03-adapters.md), [04 작업 공간](04-workspaces.md)

wire 타입 `RoleInstruction { role, source_path, blob_oid, artifact }`와 `Mission.role_instructions`는 Mission 연결 단계에서 [contracts.ts](contracts.ts)에 추가한다.

## 1. 목적과 경계

역할별 작업 방식(리뷰 기준, 저장소 규칙, 보고 형식)을 저장소의 md 파일 한 벌로 관리하고, daemon이 미션 시작 때 고정해 세 runtime에 같은 내용으로 전달한다. md는 **지시 내용만** 담당한다. 권한·도구·모델·쓰기 경로·완료 조건·결과 schema는 binding, task contract, policy가 결정하며 md로 바꿀 수 없다.

실행 도구가 스스로 읽는 지시 파일은 CLI마다 형식과 위치가 다르다(2026-09-17 공식 문서와 설치 버전 Claude Code 2.1.274, Codex 0.154.0, OpenCode 1.18.30 기준). Claude Code는 CLAUDE.md만 자동으로 읽고 하위 에이전트는 `.claude/agents/*.md`다([memory](https://code.claude.com/docs/en/memory), [sub-agents](https://code.claude.com/docs/en/sub-agents)). Codex는 AGENTS.md만 읽고 하위 에이전트는 `developer_instructions`를 가진 TOML이며 skill은 `.agents/skills`다([AGENTS.md](https://developers.openai.com/codex/guides/agents-md), [subagents](https://developers.openai.com/codex/subagents), [skills](https://developers.openai.com/codex/skills)). OpenCode는 AGENTS.md가 없으면 CLAUDE.md를 읽고 `.claude/skills`·`.agents/skills`도 탐색한다([rules](https://opencode.ai/docs/rules/), [agents](https://opencode.ai/docs/agents/), [skills](https://opencode.ai/docs/skills/)). 따라서 CLI 고유 형식을 세 벌 두지 않는다. 자동 실행에는 daemon이 고정한 지시문만 전달하고, 실행 도구의 자체 지시 파일은 §5에 따라 차단하거나 관측한다.

## 2. 원본 파일

원본은 `Mission.base_oid` commit의 Git tree에서 정확히 `.iyagi/roles/<role>.md` 경로에 있는 항목이다. `<role>`은 `Role` wire 값(`lead`, `researcher`, `architect`, `builder`, `test_author`, `reviewer`, `specialist`, `diagnostician`, `integrator`, `documenter`)이다. working tree, index, 다른 branch는 읽지 않는다. 시작 검사가 clean 상태와 HEAD==base_oid를 이미 확인하므로 사용자가 보는 파일과 같다.

- 허용: mode 100644 또는 100755 blob, UTF-8, `max_role_instruction_bytes`(32 KiB) 이하.
- 지시문 없음: 경로가 없거나 내용이 공백뿐인 경우.
- 시작 거절: 같은 경로의 항목이 일반 파일이 아닌 경우(symlink 120000, submodule 160000), 상한 초과, UTF-8이 아닌 경우. `mission.control(start)`는 `INVALID_ARGUMENT`와 `details.reason_code=role_instruction_invalid`를 반환하고 오류 메시지에 경로와 사유를 포함한다. 조용히 건너뛰지 않으며 미션·작업 상태를 바꾸지 않는다.
- 무시: 역할 이름과 맞지 않는 파일(`README.md`, 오타, 하위 디렉터리)과 §3 대상이 아닌 역할의 파일.

파일 전체를 지시문 텍스트로 취급한다. frontmatter를 해석하지 않으므로 `tools`, `model`, `permission` 같은 필드는 효과가 없다.

## 3. 시작 시 고정

`mission.control(start)`에서 저장소 등록·clean·HEAD·binding 검사를 통과한 뒤 읽는다. 대상 역할은 `policy.allowed_roles`와 `role_bindings[].role`의 합집합이며 `Role` 선언 순서로 처리한다. 각 파일을 미션 소유 artifact(`text/markdown`)로 저장하고 `Mission.role_instructions[]`에 `{role, source_path, blob_oid, artifact}`를 기록한다. 이 기록은 첫 Lead task와 같은 트랜잭션에 저장한다.

고정 후에는 바꾸지 않는다. 재개, daemon 재시작, 재계획, 재시도, 모델 변경에서도 다시 읽지 않는다. writer 작업이 `.iyagi/roles/`를 수정해도 현재 미션의 지시문은 그대로이며 인수·commit 이후 새 미션부터 적용된다. 이 기능 이전에 시작했거나 아직 draft인 미션의 값은 빈 배열이다. 같은 요청 ID의 재생은 최초 응답을 반환하며 파일을 다시 읽지 않는다. commit 실패로 참조되지 않은 artifact는 기존 엔진 artifact와 같은 보존 정책을 따른다.

## 4. 실행 문맥 전달

Run 문맥 JSON의 `role_instructions`에는 해당 task의 role에 고정된 항목 하나만 `{role, source_path, blob_oid, text}`로 넣는다. role이 없는 Verify와 지시문이 없는 역할은 `null`이다. 다른 역할의 지시문은 넣지 않는다.

고정 prompt는 우선순위를 명시한다. 지시문은 역할 수행 방식에 대한 저장소 소유자의 지침이며, 이 prompt·task contract·결과 schema보다 낮다. `allowed_paths` 확대, 원래 완료 조건 변경, 결과 형식 변경, 도구 승인에는 사용할 수 없다. 본문은 기존 미션 artifact 읽기 경로로 소유권·길이·SHA-256을 확인한 뒤 넣는다. 확인 실패와 전체 문맥 상한(`max_context_bytes`) 초과는 다른 문맥 artifact와 같은 오류 규칙을 따른다.

세 adapter와 fake adapter는 같은 문맥 경로로 받는다. CLI별 system/developer 채널(Claude `--append-system-prompt-file`, Codex `thread/start.developerInstructions`, OpenCode 메시지 `system`)은 §6의 실행 증거 전까지 사용하지 않는다.

## 5. 실행 도구 자체 지시 파일의 격리

자동 실행은 사용자 전역 설정 파일을 수정하지 않는다. 끌 수 있는 자동 로드는 실행별 인자·환경으로 끄고, 끌 수 없는 것은 관측 대상으로 남긴다.

| runtime | 자동으로 읽는 것 | 조치 | 남은 노출 |
|---|---|---|---|
| Claude Code | CLAUDE.md, skills, 사용자 정의 agent·command, plugin, hook, MCP | `--safe-mode --setting-sources "" --strict-mcp-config`, 실행 전용 `CLAUDE_CONFIG_DIR` | 없음. 실제 실행 증거는 §6 |
| Codex | 프로젝트 `AGENTS.override.md`/`AGENTS.md`/fallback, `CODEX_HOME`의 AGENTS.md, `.agents/skills`, `.codex/agents/*.toml` | 하위 에이전트·plugin·hook·MCP 끄기(기존). app-server에 `-c project_doc_max_bytes=0`을 넘겨 프로젝트 AGENTS.md 탐색을 끈다. 0.154.0은 이 값이 0이면 탐색을 중단한다([agents_md.rs](https://github.com/openai/codex/blob/rust-v0.154.0/codex-rs/core/src/agents_md.rs)). `thread/start` 응답의 `instructionSources`에 작업 공간 안 경로가 있으면 turn 전에 `POLICY_DENIED`로 실행을 거절한다 | 구독 경로는 관리 로그인 때문에 사용자 `CODEX_HOME`과 HOME을 유지하므로 `CODEX_HOME`의 AGENTS.md와 skill 탐색은 끌 수 없다. API 키 경로는 전용 `CODEX_HOME`을 쓴다 |
| OpenCode | 프로젝트 AGENTS.md·CLAUDE.md·CONTEXT.md, `opencode.json`, `.opencode/` agent·command·skill, `~/.claude`·`~/.agents` skill | `OPENCODE_DISABLE_PROJECT_CONFIG=true`와 실행 전용 HOME/XDG(기존). 1.18.30은 이 값으로 프로젝트 지시 파일·설정·`.opencode` 디렉터리 탐색을 끈다. 추가로 `OPENCODE_DISABLE_CLAUDE_CODE=true`, `OPENCODE_DISABLE_EXTERNAL_SKILLS=true` | 작업 공간 안 `.claude/skills`·`.agents/skills` 탐색 여부는 미확인 |

새 runtime이나 버전을 추가할 때 이 표를 갱신한다. 끌 수 없는 자동 지시를 차단했다고 표시하지 않는다.

## 6. 남은 범위와 출시 전 증거

1. UI: 새 AI 작업 대화상자에서 시작 전 미리보기(repository.inspect 확장), 미션 상세에서 고정된 경로·blob·본문 표시, 인수 화면에서 `.iyagi/roles/` 변경이 다음 미션부터 적용된다는 안내.
2. Codex 구독 경로의 `instructionSources` 중 작업 공간 밖 항목과 `skills/list` 결과를 Run 증거로 저장하고 표시.
3. OpenCode 작업 공간 안 외부 skill 디렉터리 탐색을 실제 서버로 확인하고, 탐색되면 끄는 설정 또는 거절 조건을 추가.
4. CLI별 system/developer 채널 전환. Claude는 `--safe-mode`에서 `--append-system-prompt-file`이 적용되는지, Codex는 `developerInstructions`, OpenCode는 메시지 `system`이 추가인지 대체인지 실제 실행으로 확인한 뒤 capability 증거와 함께 전환한다.
5. 세 runtime의 실제 실행에서 §5 조치의 효과(지시 파일이 prompt에 섞이지 않음)를 확인한 증거.

## 7. 시험

- Git: 일반 파일 읽기, 경로 없음, 공백 파일, symlink·submodule 거절, 상한 초과, 비 UTF-8, base_oid 이후 working tree 변경의 무시.
- 시작: 대상 역할만 고정, artifact 해시와 blob ID 기록, 거절 시 미션·작업 미변경.
- 문맥: 자기 역할만 포함, Verify와 미고정 역할은 `null`, 우선순위 문장 포함, 변조된 artifact 거절.
- adapter: Codex launch 인자의 `project_doc_max_bytes=0`, `instructionSources`의 작업 공간 안 경로 거절과 밖 경로 허용, OpenCode 환경 변수.
