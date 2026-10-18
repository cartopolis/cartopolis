-- stats.db: usage counts, kept apart from world.db (crate::stats).
--
-- Days are UTC day numbers (unix seconds / 86400) and hours unix hours, so no
-- table depends on a time zone and the web page draws them in the reader's.

-- One row per accepted handshake. Per account, so "came back" and "active in
-- 30 days" can be answered; deleted after 90 days, once folded into
-- active_daily.
CREATE TABLE sessions (
    id        TEXT PRIMARY KEY,          -- the connection's session id
    account   TEXT NOT NULL,             -- accounts.id
    client    TEXT NOT NULL,             -- android | desktop | web | chat | unknown
    build     TEXT NOT NULL DEFAULT '',  -- the app's build commit, when it says
    protocol  INTEGER NOT NULL,
    started   INTEGER NOT NULL,          -- unix seconds
    last_seen INTEGER NOT NULL,          -- refreshed each minute while open
    ended     INTEGER                    -- NULL while connected
);
CREATE INDEX sessions_started ON sessions (started);
CREATE INDEX sessions_account ON sessions (account);

-- Different accounts in the world per day and app, written when a day closes.
-- Kept for good: it is what charts older than 90 days are drawn from.
CREATE TABLE active_daily (
    day    INTEGER NOT NULL,
    client TEXT NOT NULL,
    people INTEGER NOT NULL,
    PRIMARY KEY (day, client)
);

-- Handshakes the server turned away, by why. `devices` is distinct callers by
-- the same daily hash as web visitors: before sign-in there is no account.
CREATE TABLE refused_daily (
    day      INTEGER NOT NULL,
    reason   TEXT NOT NULL,
    protocol INTEGER NOT NULL,
    client   TEXT NOT NULL,
    build    TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    devices  INTEGER NOT NULL,
    PRIMARY KEY (day, reason, protocol, client, build)
);

-- Accounts signed in, sampled once a minute: summed, counted and the most at
-- once, per hour. Kept for good (24 rows a day).
CREATE TABLE online_hourly (
    hour    INTEGER PRIMARY KEY,
    sum     INTEGER NOT NULL,
    samples INTEGER NOT NULL,
    peak    INTEGER NOT NULL
);

-- What passed through the server, by kind, per day; community and topic are ''
-- when the event has none. Kept for good.
CREATE TABLE events_daily (
    day       INTEGER NOT NULL,
    kind      TEXT NOT NULL,
    community TEXT NOT NULL DEFAULT '',
    topic     TEXT NOT NULL DEFAULT '',
    n         INTEGER NOT NULL,
    PRIMARY KEY (day, kind, community, topic)
);

-- Who did something in which community and topic on a day — what "active
-- members" and "people writing" count. Per account, so 90 days like sessions.
CREATE TABLE activity (
    day       INTEGER NOT NULL,
    account   TEXT NOT NULL,
    community TEXT NOT NULL,
    topic     TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (day, account, community, topic)
);

-- Minutes avatars spent in each square, and how many different people that
-- day. A closed day keeps only squares with at least 5 people.
CREATE TABLE squares_daily (
    day     INTEGER NOT NULL,
    sx      INTEGER NOT NULL,
    sy      INTEGER NOT NULL,
    minutes INTEGER NOT NULL,
    people  INTEGER NOT NULL,
    PRIMARY KEY (day, sx, sy)
);

-- Web visits per day, as totals only: per site, one row per (dimension, value)
-- — total, path, source, device, language, bots, a community's visitors.
CREATE TABLE web_daily (
    day      INTEGER NOT NULL,
    site     TEXT NOT NULL,
    dim      TEXT NOT NULL,
    value    TEXT NOT NULL DEFAULT '',
    visitors INTEGER NOT NULL,
    views    INTEGER NOT NULL,
    PRIMARY KEY (day, site, dim, value)
);
