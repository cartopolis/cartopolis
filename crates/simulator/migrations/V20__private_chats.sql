-- Private chats (protocol 25): the MLS delivery service's tables.
-- `docs/notes/encrypted-chat.md`, and `src/e2ee.rs` for the rules.
--
-- Nothing in here can be read without keys the server never holds. What the
-- rows *do* say — who has which devices, who is in which chat, when each entry
-- was written and by whom — is the metadata the note lists as visible to the
-- operator, and deliberately nothing more.
--
-- No FOREIGN KEY clauses: this connection runs with `foreign_keys` off (see
-- `db::delete_prim`), so a declared cascade would be a comment that lies.
-- `db::erase_account_traces` deletes an account's rows here explicitly.

-- One row per installation that has registered for private chats.
CREATE TABLE IF NOT EXISTS e2ee_devices (
    device     TEXT PRIMARY KEY,           -- UUID the client chose
    account    TEXT NOT NULL,              -- accounts.id
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    last_seen  TEXT
);
CREATE INDEX IF NOT EXISTS e2ee_devices_account ON e2ee_devices (account);

-- Each device's stock of one-use public key packages. Handing one out deletes
-- it: a package opens exactly one invitation.
CREATE TABLE IF NOT EXISTS e2ee_key_packages (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    device TEXT NOT NULL,
    bytes  BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS e2ee_key_packages_device ON e2ee_key_packages (device);

-- One row per chat. `epoch` is the MLS epoch a commit must have been made in to
-- be accepted; `head` is the last sequence number written to its log.
-- A direct chat names its two accounts in sorted order, so a second direct chat
-- between the same two people is found instead of created.
CREATE TABLE IF NOT EXISTS e2ee_groups (
    id         BLOB PRIMARY KEY,
    created_by TEXT NOT NULL,
    direct_a   TEXT,
    direct_b   TEXT,
    epoch      INTEGER NOT NULL DEFAULT 0,
    head       INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS e2ee_groups_direct
    ON e2ee_groups (direct_a, direct_b) WHERE direct_a IS NOT NULL;

-- Who a chat's entries are delivered to. The server's list, for routing only:
-- membership in the cryptographic sense is the MLS group state on the devices.
CREATE TABLE IF NOT EXISTS e2ee_members (
    group_id BLOB NOT NULL,
    account  TEXT NOT NULL,
    PRIMARY KEY (group_id, account)
);
CREATE INDEX IF NOT EXISTS e2ee_members_account ON e2ee_members (account);

-- Each chat's log, in the one order every device applies it.
CREATE TABLE IF NOT EXISTS e2ee_log (
    group_id    BLOB NOT NULL,
    seq         INTEGER NOT NULL,
    sender      TEXT NOT NULL,             -- accounts.id
    sender_name TEXT NOT NULL,
    device      TEXT NOT NULL,
    is_commit   INTEGER NOT NULL,
    sent_at_ms  INTEGER NOT NULL,
    message     BLOB NOT NULL,
    PRIMARY KEY (group_id, seq)
);

-- Invitations waiting for a device. Resent on every sync until the device
-- reports a cursor for that chat, i.e. until it has joined.
CREATE TABLE IF NOT EXISTS e2ee_invites (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    device      TEXT NOT NULL,
    group_id    BLOB NOT NULL,
    sender_name TEXT NOT NULL,
    after       INTEGER NOT NULL,
    welcome     BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS e2ee_invites_device ON e2ee_invites (device);
