-- Contacts and message requests (protocol 27). `docs/notes/contacts.md`, and
-- `src/contacts.rs` for the rules.
--
-- No FOREIGN KEY clauses, for the reason V20 gives: this connection runs with
-- `foreign_keys` off. `db::erase_account_traces` deletes an account's rows here
-- explicitly.

-- A contact is mutual, and stored once per direction so "my contacts" is one
-- indexed read. Both rows are written and deleted together.
CREATE TABLE IF NOT EXISTS contacts (
    account    TEXT NOT NULL,              -- accounts.id
    contact    TEXT NOT NULL,              -- accounts.id
    since_ms   INTEGER NOT NULL,
    met_at_ms  INTEGER,
    met_lat    REAL,
    met_lng    REAL,
    PRIMARY KEY (account, contact)
);

-- Every request ever sent, kept after it is answered: the per-day limit counts
-- what was sent, not what is still waiting, and a declined request is what
-- stops the same person knocking again straight away.
--
-- status: 0 waiting, 1 accepted, 2 declined, 3 withdrawn.
CREATE TABLE IF NOT EXISTS contact_requests (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    from_acc    TEXT NOT NULL,
    to_acc      TEXT NOT NULL,
    sent_at_ms  INTEGER NOT NULL,
    status      INTEGER NOT NULL DEFAULT 0,
    answered_ms INTEGER,
    met_at_ms   INTEGER,
    met_lat     REAL,
    met_lng     REAL
);
CREATE INDEX IF NOT EXISTS contact_requests_to ON contact_requests (to_acc, status);
CREATE INDEX IF NOT EXISTS contact_requests_from ON contact_requests (from_acc, sent_at_ms);

-- Who may reach this account, and its contact link. `request_policy` is
-- `RequestPolicy::to_db`: 1 = people you have been near, the default.
ALTER TABLE accounts ADD COLUMN request_policy INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN show_online INTEGER NOT NULL DEFAULT 1;
ALTER TABLE accounts ADD COLUMN notify_requests INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN separate_new_accounts INTEGER NOT NULL DEFAULT 1;
-- Made on first use rather than here: SQLite has no random text default that
-- is worth trusting as a capability.
ALTER TABLE accounts ADD COLUMN link_code TEXT;
