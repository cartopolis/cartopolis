-- Access tokens: credentials an account makes for something that is not a
-- person at a sign-in card — a bot on a build machine, a script — and can see
-- and revoke (docs/notes/access-tokens.md). Hashed like `sessions`: the raw
-- token is shown once, at creation, and never stored. A table of their own
-- rather than a column on `sessions`, because the two age differently: a
-- session slides and is evicted at a ceiling, an access token lasts until its
-- `expires_at` (NULL = never) or until somebody revokes it.
CREATE TABLE access_tokens (
    id TEXT PRIMARY KEY,
    token_hash TEXT NOT NULL UNIQUE,
    principal TEXT NOT NULL,
    username TEXT NOT NULL,
    label TEXT NOT NULL,
    -- 1 = carries the account's elevated permissions (running the server,
    -- editing other people's things); 0 = acts as a player would.
    elevated INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_used INTEGER,
    expires_at INTEGER
);
CREATE INDEX access_tokens_principal ON access_tokens (principal);
