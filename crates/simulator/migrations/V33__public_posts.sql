-- A community's public posts, and this server's part in the fediverse
-- (docs/notes/fediverse.md, docs/mockups/communities/v4-public-posts.html).
--
-- Posts are readable to the server on purpose: it sends them to other
-- servers. Topics are untouched and stay encrypted. No FOREIGN KEY clauses, for
-- the reason V20 gives; `fediverse::posts` cleans up after a community or an
-- account is deleted.

-- The public half of a community. `handle` is its address before @server and
-- never changes once set: a follower's server knows the community by it.
-- who_posts: 0 members, 1 moderators.
CREATE TABLE IF NOT EXISTS community_public (
    community   TEXT PRIMARY KEY,
    handle      TEXT NOT NULL UNIQUE,
    enabled     INTEGER NOT NULL,
    who_posts   INTEGER NOT NULL,
    created_ms  INTEGER NOT NULL
);

-- A handle once used is never handed out again, even after the community is
-- deleted: another server may still hold followers and posts under it.
CREATE TABLE IF NOT EXISTS retired_handles (
    handle      TEXT PRIMARY KEY,
    retired_ms  INTEGER NOT NULL
);

-- A post or a reply. `ap_id` is its fediverse id: our own URL for one written
-- here, the other server's for one that arrived. One remote post may be in two
-- communities here (it mentioned both), so the id is unique per community.
CREATE TABLE IF NOT EXISTS posts (
    id              TEXT PRIMARY KEY,   -- UUID
    community       TEXT NOT NULL,
    parent          TEXT,               -- the post a reply is under
    ap_id           TEXT NOT NULL,
    author_account  TEXT,               -- accounts.id, written here
    author_actor    TEXT,               -- fedi_actors.id, written elsewhere
    via_actor       TEXT,               -- the followed source that brought it
    text            TEXT NOT NULL,
    picture         TEXT,               -- a hash in the picture store
    lat             REAL,
    lng             REAL,
    place_name      TEXT,
    view_alt        REAL,
    view_pitch      REAL,
    view_yaw        REAL,
    url             TEXT,
    created_ms      INTEGER NOT NULL,
    edited_ms       INTEGER,
    hidden_by       TEXT                -- the moderator who hid it
);
CREATE UNIQUE INDEX IF NOT EXISTS posts_ap_id ON posts (community, ap_id);
CREATE INDEX IF NOT EXISTS posts_feed ON posts (community, parent, created_ms);
CREATE INDEX IF NOT EXISTS posts_parent ON posts (parent, created_ms);
CREATE INDEX IF NOT EXISTS posts_global_ap_id ON posts (ap_id);

-- A signing key for each actor this server speaks as: 'instance', 'u:<account>'
-- or 'c:<community>'. RSA, because it is the only key type every fediverse
-- server verifies.
CREATE TABLE IF NOT EXISTS fedi_keys (
    actor        TEXT PRIMARY KEY,
    private_pem  TEXT NOT NULL,
    public_pem   TEXT NOT NULL,
    created_ms   INTEGER NOT NULL
);

-- What we know of an account or community on another server.
CREATE TABLE IF NOT EXISTS fedi_actors (
    id              TEXT PRIMARY KEY,   -- its URL
    kind            TEXT NOT NULL,      -- Person, Group, Service, ...
    username        TEXT NOT NULL,
    host            TEXT NOT NULL,
    name            TEXT NOT NULL,
    inbox           TEXT NOT NULL,
    shared_inbox    TEXT,
    key_id          TEXT NOT NULL,
    public_pem      TEXT NOT NULL,
    url             TEXT,
    fetched_ms      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS fedi_actors_key ON fedi_actors (key_id);

-- Accounts on other servers following one of ours. `local` is 'c:<community>'
-- or 'u:<account>'.
CREATE TABLE IF NOT EXISTS fedi_followers (
    local        TEXT NOT NULL,
    actor        TEXT NOT NULL,
    followed_ms  INTEGER NOT NULL,
    PRIMARY KEY (local, actor)
);

-- An account or community elsewhere that a community here follows.
CREATE TABLE IF NOT EXISTS community_follows (
    community   TEXT NOT NULL,
    actor       TEXT NOT NULL,
    follow_id   TEXT NOT NULL,          -- our Follow's id, for the Undo
    accepted    INTEGER NOT NULL DEFAULT 0,
    created_by  TEXT NOT NULL,
    created_ms  INTEGER NOT NULL,
    PRIMARY KEY (community, actor)
);

-- Servers nobody here hears from or sends to. 'shared' rows are the imported
-- block list and are replaced each time it is read; 'admin' rows are this
-- server's own and are never touched by an import.
CREATE TABLE IF NOT EXISTS fedi_server_blocks (
    host      TEXT PRIMARY KEY,
    source    TEXT NOT NULL,
    reason    TEXT NOT NULL DEFAULT '',
    added_ms  INTEGER NOT NULL
);

-- An account elsewhere that a community's moderators blocked from it.
CREATE TABLE IF NOT EXISTS community_actor_blocks (
    community  TEXT NOT NULL,
    actor      TEXT NOT NULL,
    blocked_by TEXT NOT NULL,
    added_ms   INTEGER NOT NULL,
    PRIMARY KEY (community, actor)
);

-- Activities waiting to be delivered to another server's inbox, retried with a
-- growing delay until they go or are given up on.
CREATE TABLE IF NOT EXISTS fedi_deliveries (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    inbox       TEXT NOT NULL,
    signer      TEXT NOT NULL,          -- fedi_keys.actor
    body        TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_ms     INTEGER NOT NULL,
    created_ms  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS fedi_deliveries_due ON fedi_deliveries (next_ms);
