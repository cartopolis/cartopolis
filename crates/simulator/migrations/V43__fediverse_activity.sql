-- The id of the Follow that made each follower, so an Undo that names the
-- follow only by its id undoes that follow and nothing else. Followers from
-- before this have none, and an Undo by bare id leaves them be.
ALTER TABLE fedi_followers ADD COLUMN follow_id TEXT;
CREATE INDEX IF NOT EXISTS fedi_followers_follow_id ON fedi_followers (follow_id);

-- The post or reply a reply answers, by its fediverse id. `parent` stays the
-- thread's first post (the feed is flat); this is who the reply is to, which is
-- what another server threads it under and whom it tells.
ALTER TABLE posts ADD COLUMN in_reply_to TEXT;

-- A server deliveries keep failing to reach. While `down_until_ms` is ahead,
-- what is queued for it waits without being tried, so one dead server neither
-- uses up its own attempts nor holds up deliveries to everybody else.
CREATE TABLE IF NOT EXISTS fedi_server_health (
    host           TEXT PRIMARY KEY,
    failures       INTEGER NOT NULL DEFAULT 0,
    down_until_ms  INTEGER NOT NULL DEFAULT 0
);

-- What tells somebody here that something happened to them in public: a
-- reply to what they wrote, a mention, a like, a boost, a new follower. One
-- row per thing, so a server that sends the same like twice tells once.
CREATE TABLE IF NOT EXISTS post_notices (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    account     TEXT NOT NULL,          -- accounts.id told
    kind        TEXT NOT NULL,          -- reply | mention | like | boost | follow
    post        TEXT,                   -- posts.id it is about, if any
    actor       TEXT NOT NULL,          -- fedi_actors.id, or accounts.id when local
    local       INTEGER NOT NULL DEFAULT 0,
    created_ms  INTEGER NOT NULL,
    read        INTEGER NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX IF NOT EXISTS post_notices_once
    ON post_notices (account, kind, COALESCE(post, ''), actor);
CREATE INDEX IF NOT EXISTS post_notices_by_account ON post_notices (account, created_ms);

-- Boosts of a post here, from here or elsewhere: who passed it on.
CREATE TABLE IF NOT EXISTS post_boosts (
    object      TEXT NOT NULL,          -- the post's fediverse id
    actor       TEXT NOT NULL,          -- who boosted, by fediverse id
    activity    TEXT NOT NULL,          -- the Announce's id, for an Undo
    created_ms  INTEGER NOT NULL,
    PRIMARY KEY (object, actor)
);
CREATE INDEX IF NOT EXISTS post_boosts_activity ON post_boosts (activity);

-- A content warning (`summary`), whether the pictures are sensitive, what the
-- picture shows in words (its alt text), and the pictures after the first,
-- as JSON `[{"hash": …, "alt": …}]`.
ALTER TABLE posts ADD COLUMN warning TEXT;
ALTER TABLE posts ADD COLUMN sensitive INTEGER NOT NULL DEFAULT 0;
ALTER TABLE posts ADD COLUMN picture_alt TEXT;
ALTER TABLE posts ADD COLUMN more_pictures TEXT;
