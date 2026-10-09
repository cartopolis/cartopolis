-- Workspaces and their channels (protocol 38, docs/notes/web-chat.md).
--
-- A channel is an encrypted chat (`e2ee_groups`); this is only what the
-- server must know in the clear to list it to people not yet in it: its name,
-- its topic, whether it is open to every member of the workspace. Who is in a
-- channel is `e2ee_members`, as for any other chat.
CREATE TABLE IF NOT EXISTS workspaces (
    id          TEXT PRIMARY KEY,          -- UUID
    name        TEXT NOT NULL,
    created_by  TEXT NOT NULL,             -- accounts.id
    created_ms  INTEGER NOT NULL
);

-- role: 0 owner, 1 admin, 2 member.
CREATE TABLE IF NOT EXISTS workspace_members (
    workspace   TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    account     TEXT NOT NULL,             -- accounts.id
    role        INTEGER NOT NULL,
    joined_ms   INTEGER NOT NULL,
    PRIMARY KEY (workspace, account)
);
CREATE INDEX IF NOT EXISTS workspace_members_account ON workspace_members (account);

CREATE TABLE IF NOT EXISTS channels (
    group_id    BLOB PRIMARY KEY,          -- e2ee_groups.id
    workspace   TEXT NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    topic       TEXT NOT NULL DEFAULT '',
    private     INTEGER NOT NULL,
    created_by  TEXT NOT NULL,
    created_ms  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS channels_name ON channels (workspace, name);

-- Somebody waiting to be added to a public channel by a member's device. The
-- row goes when a commit whose invitation names them is accepted
-- (`e2ee::append`), so a request outlives the device that was handed it.
CREATE TABLE IF NOT EXISTS channel_joins (
    group_id     BLOB NOT NULL,
    account      TEXT NOT NULL,
    requested_ms INTEGER NOT NULL,
    PRIMARY KEY (group_id, account)
);
