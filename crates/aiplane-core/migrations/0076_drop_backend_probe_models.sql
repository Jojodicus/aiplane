-- Whether a backend discovers its models is now decided by whether any are
-- configured for it, so the per-backend switch has nothing left to say.
ALTER TABLE backends DROP COLUMN probe_models;
