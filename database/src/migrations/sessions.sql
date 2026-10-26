CREATE TABLE sessions(
    id BLOB PRIMARY KEY,
    user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    ip_address TEXT,
    user_agent TEXT,
    started INTEGER NOT NULL DEFAULT (unixepoch('now')),
    expires INTEGER NOT NULL DEFAULT (unixepoch('now', '2 hours')),
    CHECK (expires > started)
) STRICT;

CREATE VIEW valid_sessions AS SELECT * FROM sessions WHERE expires > unixepoch('now');
