-- What public posts need to work with Mastodon and Lemmy rather than only with
-- another Cartopolis (docs/notes/fediverse.md). No FOREIGN KEY clauses, for the
-- reason V20 gives; `fediverse::posts` cleans up after itself.

-- What software another server runs, read from its NodeInfo. Mastodon and Lemmy
-- read a community's boost in different shapes, and each understands only its
-- own, so a boost goes to each server in the one it reads. `software` is NULL
-- when the server did not say; it is asked again a day later.
CREATE TABLE IF NOT EXISTS fedi_servers (
    host        TEXT PRIMARY KEY,
    software    TEXT,
    checked_ms  INTEGER NOT NULL
);

-- The accounts elsewhere a post written here names (`@anna@mastodon.social`):
-- tagged in the post as Mastodon tags them, so they are told, and kept so the
-- post reads the same each time another server fetches it.
CREATE TABLE IF NOT EXISTS post_mentions (
    post   TEXT NOT NULL,   -- posts.id
    actor  TEXT NOT NULL,   -- their actor URL
    name   TEXT NOT NULL,   -- @user@server, as written
    href   TEXT NOT NULL,   -- their profile page
    PRIMARY KEY (post, actor)
);

-- Likes (Mastodon's favourites, Lemmy's upvotes) and Lemmy's downvotes on a
-- post here, from accounts elsewhere. Keyed by the post's fediverse id, which
-- is the same in every community it is in. `activity` is the Like's own id,
-- which is all some servers name when they take one back.
CREATE TABLE IF NOT EXISTS post_votes (
    object      TEXT NOT NULL,   -- posts.ap_id
    actor       TEXT NOT NULL,
    activity    TEXT NOT NULL,
    value       INTEGER NOT NULL, -- 1 like, -1 dislike
    created_ms  INTEGER NOT NULL,
    PRIMARY KEY (object, actor)
);
CREATE INDEX IF NOT EXISTS post_votes_activity ON post_votes (activity);

-- A report about a post, sent by another server (`Flag`) for the community's
-- moderators. Open until a moderator hides the post or dismisses the report.
CREATE TABLE IF NOT EXISTS post_reports (
    id           TEXT PRIMARY KEY,  -- UUID
    community    TEXT NOT NULL,
    post         TEXT NOT NULL,     -- posts.id
    reporter     TEXT NOT NULL,     -- the actor that sent it, often a server's own
    reason       TEXT NOT NULL,
    created_ms   INTEGER NOT NULL,
    resolved_by  TEXT,
    resolved     TEXT,              -- 'hidden' or 'dismissed'
    resolved_ms  INTEGER
);
CREATE INDEX IF NOT EXISTS post_reports_open ON post_reports (community, resolved_ms);

-- An account deleted here that other servers know: what to tell them, kept
-- until the delivery worker has queued it (`db::erase_account_traces` runs
-- without the server's public address, so it cannot). `inboxes` and `posts` are
-- JSON arrays: the inboxes to tell, and the posts' fediverse ids. The account's
-- key stays in `fedi_keys`: the deletion must be signed by it, and its actor
-- answers 410 Gone for as long as the key is there.
CREATE TABLE IF NOT EXISTS fedi_gone_accounts (
    account   TEXT PRIMARY KEY,
    inboxes   TEXT NOT NULL,
    posts     TEXT NOT NULL,
    gone_ms   INTEGER NOT NULL
);
