-- Prim scripts (Luau) and the per-region capacity budget.
--
-- Capacity is per *region*, not global: a region is the unit an operator hands
-- out, so it is the unit a budget has to attach to. The defaults are deliberately
-- small — a region that has never been configured should hit its ceiling during
-- testing rather than the first time someone scripts a loop in production.
ALTER TABLE regions ADD COLUMN max_prims INTEGER NOT NULL DEFAULT 1000;
ALTER TABLE regions ADD COLUMN max_scripts INTEGER NOT NULL DEFAULT 200;

-- Source lives in its own table rather than a column on `prims`, for one
-- replication reason: every prim column is carried in `PrimDto`, and `PrimDto` is
-- re-sent inside every `WorldSnapshot` at the tick rate. A source column would
-- put each script's full text on the wire 20 times a second, to every observer in
-- the AoI, for the benefit of the one person with the editor open.
CREATE TABLE IF NOT EXISTS prim_scripts (
    prim_id    INTEGER PRIMARY KEY,
    source     TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (prim_id) REFERENCES prims(id) ON DELETE CASCADE
);
