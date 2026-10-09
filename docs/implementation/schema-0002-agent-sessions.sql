CREATE TABLE agent_sessions (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL,
  pty_session_id TEXT,
  agent TEXT NOT NULL,
  agent_session_id TEXT NOT NULL,
  cwd TEXT NOT NULL,
  title TEXT,
  program TEXT,
  source TEXT NOT NULL CHECK (source IN ('registry','lock_file','hook','launch')),
  first_seen_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  ended_at TEXT,
  end_reason TEXT,
  UNIQUE (workload_id, agent, agent_session_id)
) STRICT;

CREATE INDEX agent_sessions_recent ON agent_sessions(last_seen_at DESC);
CREATE INDEX agent_sessions_workload ON agent_sessions(workload_id);

-- iyagi 메타데이터 스키마 0002 — AI 코딩 에이전트 세션 식별·복구
-- (spec `02-runner.md` §8, `01-contracts.md` §7).
--
-- 이 파일은 migration 0002의 입력이며 `CREATE TABLE`로 시작한다:
-- migration 러너는 첫 `CREATE TABLE` 앞을 PRAGMA 머리말로 보고 autocommit
-- 에서 실행하므로 0002에는 머리말이 없다. 버전 기록(`schema_migrations`)도
-- 러너가 `INSERT OR IGNORE`로 직접 남기므로 여기서 넣지 않는다.
--
-- 저장하는 것은 **식별 정보뿐**이다(01 §7): 에이전트 자체 세션 id, 실행
-- 디렉터리, 표시용 제목, 관찰된 실행 파일 경로. 대화 내용·프롬프트 원문·
-- argv·env는 어떤 열에도 들어가지 않는다.
--
-- `workloads`로의 FOREIGN KEY는 **의도적으로 없다**. 워크로드 행이 정리된
-- 뒤에도 "이 폴더에서 이어서 열 수 있는 대화" 목록은 남아야 한다 —
-- 기록의 수명이 워크로드 장부의 수명보다 길다.
--
-- UNIQUE (workload_id, agent, agent_session_id): 같은 워크로드가 같은
-- 세션을 다시 관찰하면 새 행이 아니라 `last_seen_at` 갱신이다. 같은 세션을
-- 다른 워크로드(재시작 뒤 새 pane)에서 관찰하면 별도 행이 되고, 목록은
-- (agent, agent_session_id)별 최신 행 하나만 돌려준다.
