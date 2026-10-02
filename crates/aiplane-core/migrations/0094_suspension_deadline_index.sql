-- The expiry sweep asks for the suspensions whose deadline has passed every
-- 30 seconds; this lets it read only those rows instead of the whole table.
-- expires_at is an RFC 3339 UTC string, so the index orders it by second.
-- (Number 0094: 0092 and 0093 are claimed by concurrent branches.)

CREATE INDEX chat_turn_suspensions_expires_at ON chat_turn_suspensions(expires_at);
