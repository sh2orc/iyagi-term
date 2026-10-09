-- Pending process ownership survives mission completion and archival.
CREATE INDEX orch_execs_pending ON orch_execs(id) WHERE state != 'exited';
