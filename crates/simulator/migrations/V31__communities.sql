-- Communities and their topics replace workspaces and channels (protocol 39,
-- docs/notes/communities.md).
--
-- A topic is an encrypted chat (`e2ee_groups`); these tables are what the
-- server must know in the clear to list a community, route to its topics and
-- enforce its moderation: names, the description, who is in it with which
-- role, who is asking to join, and what was reported. Who is in a topic is
-- `e2ee_members`, as for any other chat. No FOREIGN KEY clauses, for the reason
-- V20 gives; `communities::delete` and `communities::erase_account` clean up.

-- join_rule: 0 open, 1 asks first, 2 invite only.
CREATE TABLE IF NOT EXISTS communities (
    id                   TEXT PRIMARY KEY,   -- UUID
    name                 TEXT NOT NULL,
    about                TEXT NOT NULL DEFAULT '',
    join_rule            INTEGER NOT NULL,
    listed               INTEGER NOT NULL,
    members_start_topics INTEGER NOT NULL,
    created_by           TEXT NOT NULL,      -- accounts.id
    created_ms           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS communities_listed ON communities (listed);

-- role: 0 owner, 1 moderator, 2 member.
CREATE TABLE IF NOT EXISTS community_members (
    community  TEXT NOT NULL,
    account    TEXT NOT NULL,
    role       INTEGER NOT NULL,
    joined_ms  INTEGER NOT NULL,
    PRIMARY KEY (community, account)
);
CREATE INDEX IF NOT EXISTS community_members_account ON community_members (account);

CREATE TABLE IF NOT EXISTS topics (
    group_id    BLOB PRIMARY KEY,            -- e2ee_groups.id
    community   TEXT NOT NULL,
    name        TEXT NOT NULL,
    position    INTEGER NOT NULL,
    archived    INTEGER NOT NULL DEFAULT 0,
    created_by  TEXT NOT NULL,
    created_ms  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS topics_name ON topics (community, name);

-- Asking to join a community that asks first.
CREATE TABLE IF NOT EXISTS community_requests (
    community    TEXT NOT NULL,
    account      TEXT NOT NULL,
    message      TEXT NOT NULL,
    requested_ms INTEGER NOT NULL,
    PRIMARY KEY (community, account)
);

-- Somebody who may not ask again: removed with a block (until_ms NULL, for
-- good), or turned down (a week).
CREATE TABLE IF NOT EXISTS community_blocks (
    community  TEXT NOT NULL,
    account    TEXT NOT NULL,
    until_ms   INTEGER,
    PRIMARY KEY (community, account)
);

-- A member a moderator took out of one topic: not added back to it when
-- everybody else in the community is.
CREATE TABLE IF NOT EXISTS topic_exclusions (
    group_id  BLOB NOT NULL,
    account   TEXT NOT NULL,
    PRIMARY KEY (group_id, account)
);

-- Work only a device in the topic can do, because the server holds no key:
-- add a member (a join) or take one out (a removal). A join goes when a commit
-- whose invitation names them is accepted (`e2ee::append`); a removal when the
-- device that made it says its commit was (`TopicRemoved`). `asked_ms` is when
-- a device was last handed it, so it is not handed again at once.
CREATE TABLE IF NOT EXISTS topic_joins (
    group_id  BLOB NOT NULL,
    account   TEXT NOT NULL,
    asked_ms  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, account)
);
CREATE TABLE IF NOT EXISTS topic_removals (
    group_id  BLOB NOT NULL,
    account   TEXT NOT NULL,
    username  TEXT NOT NULL,                 -- as the MLS credential names them, before @server
    asked_ms  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, account)
);

-- Reports made to a community's moderators. `line` is the reporter's own copy
-- of what was said: the server cannot read the topic.
CREATE TABLE IF NOT EXISTS community_reports (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    community   TEXT NOT NULL,
    reporter    TEXT NOT NULL,
    reported    TEXT NOT NULL,
    group_id    BLOB NOT NULL,
    seq         INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    note        TEXT NOT NULL,
    line        TEXT NOT NULL,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS community_reports_community ON community_reports (community);

-- A moderator's deletion: the entry stays in the log so sequence numbers
-- stay gapless, and is replayed as "removed by a moderator" instead.
ALTER TABLE e2ee_log ADD COLUMN redacted_by TEXT;

-- Workspaces become communities. Invite-only and unlisted, which is what a
-- workspace was; an admin becomes a moderator. Their channels had no rows on
-- the one server running them, and a channel's private flag has no community
-- equivalent, so channels are not carried over.
INSERT INTO communities (id, name, about, join_rule, listed, members_start_topics, created_by, created_ms)
    SELECT id, name, '', 2, 0, 1, created_by, created_ms FROM workspaces;
INSERT INTO community_members (community, account, role, joined_ms)
    SELECT workspace, account, role, joined_ms FROM workspace_members;

DROP TABLE IF EXISTS channel_joins;
DROP TABLE IF EXISTS channels;
DROP TABLE IF EXISTS workspace_members;
DROP TABLE IF EXISTS workspaces;
