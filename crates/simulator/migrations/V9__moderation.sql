-- Blocks and reports: the durable half of the user-safety surface every store
-- with user-generated content requires (App Store guideline 1.2 asks for a
-- filter, a report path, a block, and published contact details; the filter is
-- `cartopolis_core::moderation` and the contact line is in the client's About
-- panel).

-- Who a person refuses to hear. Keyed by the *blocker's principal*, because a
-- block has to survive the session that made it, and by the blocked account's
-- **username**, because that is the identity the wire carries and the one a
-- person recognises. `avatar_id` would have been the natural join here and is
-- wrong: it is minted per connection and reused, so a block stored against one
-- would silently start muting a stranger.
--
-- No foreign key on `blocked_username` on purpose. A block must be storable
-- against a name that is not (or is no longer) an account — deleting an account
-- must not quietly unblock the person who reappears under it, and refusing the
-- insert would put a "that person does not exist" error in front of someone who
-- is trying to get away from them.
CREATE TABLE IF NOT EXISTS blocks (
    blocker_id       TEXT NOT NULL,
    blocked_username TEXT NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (blocker_id, blocked_username)
);

-- Reports awaiting an operator. `message` is the reported line as the reporter
-- saw it: proximity chat is never written to `chat_messages`, so for the tier
-- where most abuse happens this row is the only copy that exists.
--
-- The reporter is kept (a pattern of reports from one account is itself a
-- signal, in both directions) but is nullable, so deleting an account can
-- anonymise its reports rather than destroy the safety record.
CREATE TABLE IF NOT EXISTS reports (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    reporter_id       TEXT,
    reporter_username TEXT,
    subject_username  TEXT NOT NULL,
    reason            TEXT NOT NULL,
    note              TEXT NOT NULL DEFAULT '',
    message           TEXT,
    created_at        TEXT NOT NULL DEFAULT (datetime('now')),
    -- 0 = open. An operator resolves through the admin API; "timely responses to
    -- concerns" is a process, but it needs somewhere to record that one happened.
    resolved          INTEGER NOT NULL DEFAULT 0,
    resolved_at       TEXT,
    resolution        TEXT
);

CREATE INDEX IF NOT EXISTS reports_open ON reports (resolved, created_at DESC);
CREATE INDEX IF NOT EXISTS reports_subject ON reports (subject_username);
