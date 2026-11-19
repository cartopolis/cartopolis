-- Designs (docs/mockups/design-room/README.md): a group of prims a person
-- built in the design room and saved to their inventory, kept as the same
-- `{version, prims}` document `/api/content/export` writes, plus the spot it
-- was designed for, so a copy can be placed anywhere by moving that spot.
CREATE TABLE designs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    name TEXT NOT NULL,
    origin_lat REAL NOT NULL,
    origin_lng REAL NOT NULL,
    parts INTEGER NOT NULL,
    document TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX designs_account ON designs(account_id, id);
