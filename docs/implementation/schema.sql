-- iyagi metadata schema 0001. Output bytes and telemetry are not stored here.
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA busy_timeout = 5000;

CREATE TABLE schema_migrations (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL
) STRICT;

CREATE TABLE tasks (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  created_at TEXT NOT NULL
) STRICT;

CREATE TABLE attempts (
  id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES tasks(id),
  ordinal INTEGER NOT NULL CHECK (ordinal >= 1),
  created_at TEXT NOT NULL,
  UNIQUE (task_id, ordinal)
) STRICT;

CREATE TABLE workloads (
  id TEXT PRIMARY KEY,
  attempt_id TEXT UNIQUE REFERENCES attempts(id),
  mode TEXT NOT NULL CHECK (mode IN ('shell', 'managed')),
  state TEXT NOT NULL CHECK (state IN (
    'QUEUED', 'STARTING', 'RUNNING', 'STOPPING', 'DRAINING',
    'SUCCEEDED', 'FAILED', 'CANCELLED', 'INTERRUPTED'
  )),
  priority INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 2),
  reservation_bytes INTEGER NOT NULL CHECK (reservation_bytes > 0),
  cpu_slots INTEGER NOT NULL CHECK (cpu_slots > 0),
  enforcement TEXT NOT NULL CHECK (enforcement IN ('observe', 'prefer', 'require')),
  memory_max_bytes INTEGER CHECK (memory_max_bytes > 0),
  cpu_max_cores REAL CHECK (cpu_max_cores > 0),
  pids_max INTEGER CHECK (pids_max > 0),
  effective_policy_json TEXT NOT NULL,
  queue_reason TEXT,
  cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
  root_exited INTEGER NOT NULL DEFAULT 0 CHECK (root_exited IN (0, 1)),
  exit_code INTEGER,
  last_error_code TEXT,
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT,
  CHECK ((mode = 'managed' AND attempt_id IS NOT NULL) OR
         (mode = 'shell' AND attempt_id IS NULL))
) STRICT;

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  workload_id TEXT NOT NULL UNIQUE REFERENCES workloads(id),
  initial_cols INTEGER NOT NULL CHECK (initial_cols BETWEEN 2 AND 1000),
  initial_rows INTEGER NOT NULL CHECK (initial_rows BETWEEN 2 AND 1000),
  journal_relative_path TEXT NOT NULL UNIQUE,
  journal_limit_bytes INTEGER NOT NULL CHECK (journal_limit_bytes > 0),
  journal_bytes INTEGER NOT NULL DEFAULT 0 CHECK (journal_bytes >= 0),
  last_seq INTEGER NOT NULL DEFAULT 0 CHECK (last_seq >= 0),
  replay_status TEXT NOT NULL DEFAULT 'complete'
    CHECK (replay_status IN ('complete', 'tail_truncated', 'corrupt', 'deleted')),
  pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  created_at TEXT NOT NULL
) STRICT;

CREATE TABLE process_ownership (
  workload_id TEXT PRIMARY KEY REFERENCES workloads(id),
  pid INTEGER NOT NULL CHECK (pid > 0),
  start_token TEXT NOT NULL,
  boot_id TEXT NOT NULL,
  group_kind TEXT NOT NULL CHECK (group_kind IN ('cgroup', 'job', 'observed_tree')),
  group_reference TEXT,
  coverage TEXT NOT NULL CHECK (coverage IN ('group', 'observed_tree', 'partial')),
  recorded_at TEXT NOT NULL
) STRICT;

CREATE TABLE requests (
  id TEXT PRIMARY KEY,
  method TEXT NOT NULL CHECK (method IN ('workload.launch', 'workload.cancel', 'daemon.shutdown')),
  fingerprint TEXT NOT NULL CHECK (length(fingerprint) = 64),
  workload_id TEXT REFERENCES workloads(id),
  outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'completed', 'failed', 'unknown')),
  created_at TEXT NOT NULL
) STRICT;

CREATE TABLE lifecycle_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  workload_id TEXT NOT NULL REFERENCES workloads(id),
  from_state TEXT,
  to_state TEXT NOT NULL,
  reason_code TEXT,
  created_at TEXT NOT NULL
) STRICT;

CREATE TABLE layouts (
  workspace_id TEXT PRIMARY KEY,
  schema_version INTEGER NOT NULL CHECK (schema_version = 1),
  tree_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE INDEX workloads_queue ON workloads(state, priority, created_at);
CREATE INDEX lifecycle_workload ON lifecycle_events(workload_id, id);
CREATE INDEX requests_workload ON requests(workload_id);

INSERT INTO schema_migrations(version, applied_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
