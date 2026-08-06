-- ADR-016: local account store. One row per principal; password stored as an
-- argon2id PHC string, never plaintext. `id` is the stable principal (UUID).
CREATE TABLE IF NOT EXISTS accounts (
    id         TEXT PRIMARY KEY,
    username   TEXT NOT NULL UNIQUE,
    pw_hash    TEXT NOT NULL,
    disabled   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
