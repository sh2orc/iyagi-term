-- Keep observations in the Run transaction and index live reset windows,
-- including runs in archived missions. Older documents have no observation.
CREATE INDEX orch_run_rate_limit_reset
ON orch_runs(CAST(json_extract(document_json, '$.rate_limit.resets_at_unix_ms') AS INTEGER))
WHERE json_type(document_json, '$.rate_limit') = 'object';
