-- One-shot scheduled PTY input owned by the daemon. Status strings mirror
-- term-contracts ScheduledCommandStatus exactly (enums.rs keeps them tied).
CREATE TABLE scheduled_commands (
  id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  command TEXT NOT NULL CHECK (length(command) BETWEEN 1 AND 4000),
  fire_at_ms INTEGER NOT NULL CHECK (fire_at_ms >= 0),
  status TEXT NOT NULL CHECK (status IN ('pending','firing','fired','failed','skipped','cancelled')),
  attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0),
  fired_at_ms INTEGER CHECK (fired_at_ms IS NULL OR fired_at_ms >= 0),
  reason_code TEXT
) STRICT;

-- The due-claim scan reads (status, fire_at_ms); the list view reads the
-- same rows newest-first.
CREATE INDEX scheduled_commands_due ON scheduled_commands(status, fire_at_ms);
