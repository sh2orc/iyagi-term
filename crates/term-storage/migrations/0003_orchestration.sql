CREATE TABLE orch_missions (
  id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  event_seq INTEGER NOT NULL CHECK (event_seq = revision),
  state TEXT NOT NULL CHECK (state IN ('draft','running','pausing','paused','stopping','completed','failed','cancelled')),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  archived_at TEXT
) STRICT;

CREATE TABLE orch_tasks (
  id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL REFERENCES orch_missions(id),
  state TEXT NOT NULL CHECK (state IN ('planned','ready','running','awaiting_input','awaiting_review','blocked','succeeded','failed','cancelled','superseded')),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  UNIQUE (mission_id,id),
  UNIQUE (mission_id,ordinal)
) STRICT;

CREATE TABLE orch_dependencies (
  mission_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  dependency_id TEXT NOT NULL,
  PRIMARY KEY (mission_id,task_id,dependency_id),
  FOREIGN KEY (mission_id,task_id) REFERENCES orch_tasks(mission_id,id),
  FOREIGN KEY (mission_id,dependency_id) REFERENCES orch_tasks(mission_id,id),
  CHECK (task_id <> dependency_id)
) STRICT;

CREATE TABLE orch_runs (
  id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  attempt INTEGER NOT NULL CHECK (attempt >= 1),
  state TEXT NOT NULL CHECK (state IN ('prepared','starting','running','awaiting_input','stopping','succeeded','failed','cancelled','interrupted','unknown')),
  fencing_token INTEGER NOT NULL CHECK (fencing_token >= 1),
  dispatch_state TEXT NOT NULL CHECK (dispatch_state IN ('unsent','may_have_sent','acknowledged')),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  UNIQUE (mission_id,id),
  UNIQUE (task_id,attempt),
  FOREIGN KEY (mission_id,task_id) REFERENCES orch_tasks(mission_id,id)
) STRICT;

CREATE UNIQUE INDEX orch_one_live_run_per_task ON orch_runs(task_id)
WHERE state IN ('prepared','starting','running','awaiting_input','stopping');

CREATE TABLE orch_execs (
  id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL,
  run_id TEXT NOT NULL UNIQUE,
  state TEXT NOT NULL CHECK (state IN ('prepared','spawned','stopping','exited','unknown')),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  FOREIGN KEY (mission_id,run_id) REFERENCES orch_runs(mission_id,id)
) STRICT;

CREATE TABLE orch_entities (
  mission_id TEXT NOT NULL REFERENCES orch_missions(id),
  kind TEXT NOT NULL CHECK (kind IN ('message','decision','workspace','candidate','verification','finding','knowledge')),
  id TEXT NOT NULL,
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  PRIMARY KEY (mission_id,kind,id)
) STRICT;

CREATE TABLE orch_events (
  mission_id TEXT NOT NULL REFERENCES orch_missions(id),
  seq INTEGER NOT NULL CHECK (seq >= 1),
  revision INTEGER NOT NULL CHECK (revision = seq),
  transaction_id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  created_at TEXT NOT NULL,
  PRIMARY KEY (mission_id,seq)
) STRICT;

CREATE TABLE orch_requests (
  id TEXT PRIMARY KEY,
  mission_id TEXT REFERENCES orch_missions(id),
  method TEXT NOT NULL,
  fingerprint TEXT NOT NULL CHECK (length(fingerprint) = 64),
  response_json TEXT NOT NULL CHECK (json_valid(response_json)),
  created_at TEXT NOT NULL
) STRICT;

CREATE TABLE orch_outbox (
  id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL REFERENCES orch_missions(id),
  run_id TEXT,
  operation TEXT NOT NULL CHECK (operation IN ('start','message','cancel','answer','verify','workspace_prepare','workspace_capture')),
  dedupe_key TEXT NOT NULL UNIQUE,
  fencing_token INTEGER NOT NULL CHECK (fencing_token >= 1),
  state TEXT NOT NULL CHECK (state IN ('prepared','sending','acknowledged','failed','unknown')),
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY (mission_id,run_id) REFERENCES orch_runs(mission_id,id)
) STRICT;

CREATE TABLE orch_workspace_leases (
  workspace_id TEXT PRIMARY KEY,
  mission_id TEXT NOT NULL REFERENCES orch_missions(id),
  owner_run_id TEXT,
  fencing_token INTEGER NOT NULL CHECK (fencing_token >= 1),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  FOREIGN KEY (mission_id,owner_run_id) REFERENCES orch_runs(mission_id,id)
) STRICT;

CREATE TABLE orch_artifacts (
  id TEXT PRIMARY KEY,
  mission_id TEXT REFERENCES orch_missions(id),
  staging_client_id TEXT,
  sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
  bytes INTEGER NOT NULL CHECK (bytes >= 0 AND bytes <= 67108864),
  media_type TEXT NOT NULL,
  relative_path TEXT NOT NULL UNIQUE,
  content_state TEXT NOT NULL CHECK (content_state IN ('available','expired','corrupt')),
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0,1)),
  created_at TEXT NOT NULL,
  CHECK ((mission_id IS NOT NULL AND staging_client_id IS NULL) OR
         (mission_id IS NULL AND staging_client_id IS NOT NULL))
) STRICT;

CREATE TABLE orch_uploads (
  id TEXT PRIMARY KEY,
  client_id TEXT NOT NULL,
  mission_id TEXT REFERENCES orch_missions(id),
  expected_bytes INTEGER NOT NULL CHECK (expected_bytes >= 0 AND expected_bytes <= 67108864),
  next_offset INTEGER NOT NULL DEFAULT 0 CHECK (next_offset >= 0 AND next_offset <= expected_bytes),
  expected_sha256 TEXT NOT NULL CHECK (length(expected_sha256) = 64),
  media_type TEXT NOT NULL,
  temp_relative_path TEXT NOT NULL UNIQUE,
  committed_artifact_id TEXT REFERENCES orch_artifacts(id),
  expires_at TEXT NOT NULL
) STRICT;

CREATE TABLE orch_bindings (
  id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  document_json TEXT NOT NULL CHECK (json_valid(document_json))
) STRICT;

CREATE TABLE orch_config (
  kind TEXT NOT NULL CHECK (kind IN ('template','verification','repository')),
  id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  document_json TEXT NOT NULL CHECK (json_valid(document_json)),
  PRIMARY KEY (kind,id)
) STRICT;

CREATE INDEX orch_missions_recent ON orch_missions(updated_at DESC,id);
CREATE INDEX orch_tasks_schedule ON orch_tasks(mission_id,state,ordinal);
CREATE INDEX orch_runs_by_mission ON orch_runs(mission_id,state);
CREATE INDEX orch_outbox_pending ON orch_outbox(state,created_at,id);
CREATE INDEX orch_artifacts_mission ON orch_artifacts(mission_id,content_state);
