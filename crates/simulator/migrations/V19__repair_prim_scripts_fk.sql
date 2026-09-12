-- Point `prim_scripts` back at a table that exists.
--
-- `V17__retire_regions.sql` rebuilds `prims` by renaming it aside, creating the
-- new one and copying across. It sets `PRAGMA legacy_alter_table = ON` for that
-- rename **on purpose**, and its own comment says why: without legacy mode
-- SQLite rewrites every other table's references to the renamed table, which
-- would point `prim_scripts`' foreign key at the temporary table the migration
-- then drops.
--
-- The pragma does not take effect. refinery runs each migration inside a
-- transaction (`refinery_core`'s rusqlite driver: `self.transaction()` then
-- `execute_batch`), and `legacy_alter_table` set there does not change how the
-- `ALTER TABLE` in the same batch behaves. So the rename ran in modern mode, and
-- every database migrated through V17 by the server — which is all of them,
-- there being no other path — carries:
--
--     FOREIGN KEY (prim_id) REFERENCES "prims_with_region"(id) ON DELETE CASCADE
--
-- naming a table dropped eleven statements later. The symptom is not a foreign
-- key failure (this connection never turns them on, see `db::delete_prim`) but a
-- **schema parse** failure: anything that makes SQLite re-resolve that clause
-- answers `no such table: main.prims_with_region`, which is what four prim
-- deletion tests have been failing with, and what a fresh `data/world.db` would
-- hit on the first script a prim is given.
--
-- Rebuilt rather than renamed back. `ALTER TABLE prim_scripts RENAME` is the
-- operation that has to re-parse the very clause that is broken, and it is not
-- worth finding out on somebody's world whether it can; a create-copy-drop
-- touches the bad clause only in a `SELECT`, which does not resolve it.
--
-- Unconditional, because "is this database one of the broken ones?" cannot be
-- asked in SQL that also runs on the ones that are fine — and rebuilding a table
-- of scripts is cheap on every world that exists.
CREATE TABLE prim_scripts_repaired (
    prim_id    INTEGER PRIMARY KEY,
    source     TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (prim_id) REFERENCES prims (id) ON DELETE CASCADE
);

INSERT INTO prim_scripts_repaired (prim_id, source, updated_at)
SELECT prim_id, source, updated_at FROM prim_scripts;

DROP TABLE prim_scripts;

ALTER TABLE prim_scripts_repaired RENAME TO prim_scripts;
