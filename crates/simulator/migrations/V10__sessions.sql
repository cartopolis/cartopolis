-- Session tokens, persisted so "stay signed in" survives a simulator restart.
-- Hashed: the client keeps the raw token in its user.json, but a copy of this
-- database must not be a bag of bearer credentials. `touched` is unix seconds,
-- renewed on use (the TTL slides).
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY,
    principal TEXT NOT NULL,
    username TEXT NOT NULL,
    touched INTEGER NOT NULL
);
