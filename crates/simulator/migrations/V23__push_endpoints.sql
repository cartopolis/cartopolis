-- Where to wake a device whose app is closed (protocol 29, docs/notes/calls.md):
-- one UnifiedPush endpoint per device, owned by the account that registered it.
-- Keyed by the endpoint itself, which is unique per device and per app, so a
-- device that signs in as somebody else moves its endpoint rather than being
-- rung for two people.
CREATE TABLE IF NOT EXISTS push_endpoints (
    endpoint   TEXT PRIMARY KEY,
    account    TEXT NOT NULL,              -- accounts.id
    updated_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS push_endpoints_account ON push_endpoints (account);
