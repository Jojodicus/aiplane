-- Large binary parts of the activity log's model exchanges (a base64 image
-- in a request), stored once per chain by content hash. An event's detail
-- holds `activity-blob:sha256:<hash>` in the part's place; the hash is
-- inside the event's own signed hash, and a blob that no longer matches it
-- is not served. The retention sweep deletes a chain's blobs with the chain.
CREATE TABLE activity_blobs (
    chain_key  TEXT NOT NULL,
    hash       TEXT NOT NULL,
    data       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (chain_key, hash)
) STRICT, WITHOUT ROWID;
