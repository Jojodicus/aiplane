-- #111: `agent_audit` becomes the agent activity log. Every event carries its
-- correlation ids as columns and a place in a hash chain: one chain per
-- conversation (`conversation:<root session id>`, sub-agent runs included)
-- and one per agent for everything outside a conversation
-- (`agent:<principal id>`). `hash` is HMAC-SHA256 over the event's canonical
-- JSON, which includes `prev_hash`, under the log key `key_id` names. Rows written before this migration keep
-- NULL chain columns: they were never chained, and verify reports them as
-- unchained instead of inventing a history for them.
ALTER TABLE agent_audit ADD COLUMN chain_key TEXT;
ALTER TABLE agent_audit ADD COLUMN seq INTEGER;
ALTER TABLE agent_audit ADD COLUMN prev_hash TEXT;
ALTER TABLE agent_audit ADD COLUMN hash TEXT;
ALTER TABLE agent_audit ADD COLUMN agent_id TEXT;
ALTER TABLE agent_audit ADD COLUMN version INTEGER;
ALTER TABLE agent_audit ADD COLUMN conversation_id TEXT;
ALTER TABLE agent_audit ADD COLUMN session_id TEXT;
ALTER TABLE agent_audit ADD COLUMN turn_id TEXT;
ALTER TABLE agent_audit ADD COLUMN round INTEGER;
ALTER TABLE agent_audit ADD COLUMN call_id TEXT;
ALTER TABLE agent_audit ADD COLUMN visitor_id TEXT;
ALTER TABLE agent_audit ADD COLUMN caller_id TEXT;
ALTER TABLE agent_audit ADD COLUMN duration_ms INTEGER;
ALTER TABLE agent_audit ADD COLUMN key_id TEXT;

UPDATE agent_audit
   SET agent_id = COALESCE(json_extract(chain, '$.frames[0].principal_id'), principal_id),
       conversation_id = json_extract(chain, '$.root_session'),
       session_id = json_extract(detail, '$.session_id'),
       turn_id = json_extract(detail, '$.turn_id');

-- The new columns sit after `detail`, which for a model exchange spills onto
-- overflow pages; reading a column behind it from the table means walking
-- that chain. So every query that does not need `detail` is answered from an
-- index instead.
--
-- The chain's order, and the guard against two writers forking it.
CREATE UNIQUE INDEX idx_agent_audit_chain ON agent_audit (chain_key, seq);
-- The chain's head (seq and hash) for every append, without the row.
CREATE INDEX idx_agent_audit_chain_head ON agent_audit (chain_key, seq, hash);
-- The activity API pages through one agent's events, or one conversation's,
-- by rowid: an index on the one column is ordered by (column, rowid).
CREATE INDEX idx_agent_audit_agent ON agent_audit (agent_id);
CREATE INDEX idx_agent_audit_conversation ON agent_audit (conversation_id);
-- The retention sweep: each conversation chain of an agent and its newest
-- event.
CREATE INDEX idx_agent_audit_sweep
    ON agent_audit (agent_id, chain_key, conversation_id, created_at);
