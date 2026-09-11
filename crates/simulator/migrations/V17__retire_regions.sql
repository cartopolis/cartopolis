-- Regions are gone. A prim is anchored to the Earth, and nothing else.
--
-- A region was a 256 m square of ground with a raster tile on it, from before
-- this client streamed a map: the client drew the square, prims were placed on
-- it, and it was the unit an area-of-interest filter, a capacity budget and a
-- script channel were scoped to. Every one of those has been overtaken —
-- placement has been an absolute `GeoAnchor` since ADR-029, the ground is the
-- streamed OSM/3DBAG map, the budget is one server's and now lives in
-- `cartopolis.toml`, and the AoI filter was aimed by a single global observer
-- field shared by every connection, so what it actually did was blank every
-- prim for everybody once one client walked 500 m from a synthetic grid origin.
--
-- `prims` is rebuilt rather than `ALTER TABLE ... DROP COLUMN`-ed: `region_id`
-- is named in both an index and a table-level FOREIGN KEY clause, and SQLite
-- refuses to drop a column either of those mentions.
--
-- `legacy_alter_table` is on for the rename **on purpose**. Without it SQLite
-- rewrites every other table's references to the renamed one, which would point
-- `prim_scripts`' foreign key at the temporary table this migration then drops.
-- With it, that clause keeps naming `prims` — which is recreated below, so it is
-- correct again by the time the migration ends. (Foreign keys are not enforced
-- on this connection either way, `db::delete_prim` documents why, but leaving a
-- dangling reference in the stored schema is how the next person reading it is
-- misled.)
PRAGMA legacy_alter_table = ON;
ALTER TABLE prims RENAME TO prims_with_region;
PRAGMA legacy_alter_table = OFF;

CREATE TABLE prims (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT NOT NULL DEFAULT 'Prim',
    shape            TEXT NOT NULL DEFAULT 'box',
    position_x       REAL NOT NULL DEFAULT 0,
    position_y       REAL NOT NULL DEFAULT 0,
    position_z       REAL NOT NULL DEFAULT 0,
    rotation_x       REAL NOT NULL DEFAULT 0,
    rotation_y       REAL NOT NULL DEFAULT 0,
    rotation_z       REAL NOT NULL DEFAULT 0,
    scale_x          REAL NOT NULL DEFAULT 1,
    scale_y          REAL NOT NULL DEFAULT 1,
    scale_z          REAL NOT NULL DEFAULT 1,
    color_r          REAL NOT NULL DEFAULT 0.5,
    color_g          REAL NOT NULL DEFAULT 0.5,
    color_b          REAL NOT NULL DEFAULT 0.5,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
    texture_id       TEXT DEFAULT NULL,
    path_cut_begin   REAL NOT NULL DEFAULT 0.0,
    path_cut_end     REAL NOT NULL DEFAULT 1.0,
    hollow           REAL NOT NULL DEFAULT 0.0,
    twist_begin      REAL NOT NULL DEFAULT 0.0,
    twist_end        REAL NOT NULL DEFAULT 0.0,
    taper_x          REAL NOT NULL DEFAULT 0.0,
    taper_y          REAL NOT NULL DEFAULT 0.0,
    top_shear_x      REAL NOT NULL DEFAULT 0.0,
    top_shear_y      REAL NOT NULL DEFAULT 0.0,
    slice_begin      REAL NOT NULL DEFAULT 0.0,
    slice_end        REAL NOT NULL DEFAULT 1.0,
    alpha            REAL    NOT NULL DEFAULT 1.0,
    glow             REAL    NOT NULL DEFAULT 0.0,
    full_bright      INTEGER NOT NULL DEFAULT 0,
    repeat_u         REAL    NOT NULL DEFAULT 1.0,
    repeat_v         REAL    NOT NULL DEFAULT 1.0,
    flip_u           INTEGER NOT NULL DEFAULT 0,
    flip_v           INTEGER NOT NULL DEFAULT 0,
    texture_rotation REAL    NOT NULL DEFAULT 0.0,
    offset_u         REAL    NOT NULL DEFAULT 0.0,
    offset_v         REAL    NOT NULL DEFAULT 0.0,
    latitude         REAL NOT NULL DEFAULT 0,
    longitude        REAL NOT NULL DEFAULT 0,
    particles        TEXT,
    origin           TEXT,
    owner            TEXT,
    model            TEXT
);

INSERT INTO prims (
    id, name, shape,
    position_x, position_y, position_z,
    rotation_x, rotation_y, rotation_z,
    scale_x, scale_y, scale_z,
    color_r, color_g, color_b,
    created_at, updated_at, texture_id,
    path_cut_begin, path_cut_end, hollow,
    twist_begin, twist_end, taper_x, taper_y,
    top_shear_x, top_shear_y, slice_begin, slice_end,
    alpha, glow, full_bright, repeat_u, repeat_v, flip_u, flip_v,
    texture_rotation, offset_u, offset_v,
    latitude, longitude, particles, origin, owner, model
)
SELECT
    id, name, shape,
    position_x, position_y, position_z,
    rotation_x, rotation_y, rotation_z,
    scale_x, scale_y, scale_z,
    color_r, color_g, color_b,
    created_at, updated_at, texture_id,
    path_cut_begin, path_cut_end, hollow,
    twist_begin, twist_end, taper_x, taper_y,
    top_shear_x, top_shear_y, slice_begin, slice_end,
    alpha, glow, full_bright, repeat_u, repeat_v, flip_u, flip_v,
    texture_rotation, offset_u, offset_v,
    latitude, longitude, particles, origin, owner, model
FROM prims_with_region;

-- Takes `idx_prims_region` with it, along with the three indexes below that were
-- renamed onto the temporary table with it.
DROP TABLE prims_with_region;

CREATE INDEX IF NOT EXISTS idx_prims_origin ON prims (origin);
CREATE INDEX IF NOT EXISTS idx_prims_owner  ON prims (owner);
CREATE INDEX IF NOT EXISTS idx_prims_model  ON prims (model);

DROP INDEX IF EXISTS idx_regions_tile;
DROP TABLE IF EXISTS regions;
