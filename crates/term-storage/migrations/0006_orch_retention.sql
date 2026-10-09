-- Retention support for orchestration tables (bounded-database fix).
--
-- Housekeeping transitions (engine.time_checkpoint / engine.activity) used to
-- be indistinguishable from meaningful events in orch_events (both store
-- event_type='changed'; only orch_requests.method knows the method). This
-- migration adds the discriminator, backfills it for legacy rows via the
-- per-transition request twin (same mission_id + same commit timestamp), and
-- adds the indexes the retention prune scans.
--
-- The backfill can mislabel only a meaningful event that committed in the
-- exact same millisecond as a housekeeping commit of the same mission; the
-- daemon serializes commits per mission, so this is legacy-data-only noise.
ALTER TABLE orch_events ADD COLUMN housekeeping INTEGER NOT NULL DEFAULT 0;
CREATE INDEX orch_requests_scan ON orch_requests(mission_id, method, created_at);
UPDATE orch_events SET housekeeping = 1
WHERE EXISTS (
    SELECT 1 FROM orch_requests r
    WHERE r.method IN ('engine.time_checkpoint','engine.activity')
      AND r.mission_id = orch_events.mission_id
      AND r.created_at = orch_events.created_at
);
CREATE INDEX orch_events_prune ON orch_events(housekeeping, mission_id, seq);
CREATE INDEX orch_requests_age ON orch_requests(created_at);
