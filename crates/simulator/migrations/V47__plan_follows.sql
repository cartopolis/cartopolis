-- Following a place for new plans (docs/notes/plans.md): the spots an account
-- follows, which every new notice in the municipality's feed is checked
-- against, and the notices already checked, so a restart does not wake
-- anybody for a notice twice.
CREATE TABLE plan_follows (
    account TEXT NOT NULL,
    lat REAL NOT NULL,
    lng REAL NOT NULL
);
CREATE INDEX plan_follows_account ON plan_follows(account);

CREATE TABLE plans_seen (
    id TEXT PRIMARY KEY
);
