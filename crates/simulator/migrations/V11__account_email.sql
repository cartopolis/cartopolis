-- Self-service registration (email-verified). Two things the account store did
-- not have: an address to write to, and a record of whether anyone answered.
--
-- The gate is deliberately expressed as a *pair* of columns rather than a
-- `verified` flag with a default, because the default is the whole problem: every
-- account that already exists was created by an operator (`--create-admin`, or
-- the admin API) and has no email address at all. Defaulting them to unverified
-- locks out the running server; defaulting them to verified makes the flag mean
-- "verified or ancient". So the rule is stated over both columns and reads the
-- same for a row of either vintage:
--
--     pending  ==  email IS NOT NULL AND email_verified_at IS NULL
--
-- An operator-made account is never pending because it has no email; a
-- registered one stops being pending the moment its address answers. Nothing
-- about an existing database changes.
ALTER TABLE accounts ADD COLUMN email TEXT;
ALTER TABLE accounts ADD COLUMN email_verified_at TEXT;

-- One account per address. Partial, so the many operator-made rows with a NULL
-- email do not collide with each other, and over `lower(email)` because nobody
-- believes Alice@ and alice@ are two people — SQLite's own NOCASE collation is
-- ASCII-only, and `lower()` here is the same folding the API applies before it
-- writes, so the index and the code agree by construction.
CREATE UNIQUE INDEX IF NOT EXISTS accounts_email_unique
    ON accounts (lower(email)) WHERE email IS NOT NULL;

-- Outstanding verification links. Hashed for the same reason session tokens are
-- (`V10__sessions.sql`): the raw token is a bearer credential that activates an
-- account, and a copy of this database must not be a bag of them.
--
-- `expires_at` and `created_at` are unix seconds, matching the sessions table.
-- A row is deleted on use, on re-send and on account deletion, so the table is
-- the set of links currently outstanding rather than a log.
CREATE TABLE IF NOT EXISTS email_verifications (
    token_hash TEXT PRIMARY KEY,
    principal  TEXT NOT NULL,
    email      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS email_verifications_principal
    ON email_verifications (principal);
