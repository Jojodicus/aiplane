-- visitor_sessions.session_id had no index: the per-visitor and per-IP rate
-- windows join chat_turns to it on session_id, and deleting a chat session
-- cascades into it by session_id, so both scanned the table.

CREATE INDEX visitor_sessions_session ON visitor_sessions(session_id);
