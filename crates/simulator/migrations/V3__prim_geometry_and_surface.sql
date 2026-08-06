-- ADR-017 / protocol v8: persist prim geometry (path-cut / hollow / warp) and
-- texture surface params (transparency, glow, full-bright, repeats, rotation, offset).
-- The geometry columns close the prior server gap where load_world hardcoded defaults.

-- Geometry (path cut, hollow, twist, taper, top-shear, slice).
ALTER TABLE prims ADD COLUMN path_cut_begin REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN path_cut_end   REAL NOT NULL DEFAULT 1.0;
ALTER TABLE prims ADD COLUMN hollow         REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN twist_begin    REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN twist_end      REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN taper_x        REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN taper_y        REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN top_shear_x    REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN top_shear_y    REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN slice_begin    REAL NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN slice_end      REAL NOT NULL DEFAULT 1.0;

-- Texture surface params.
ALTER TABLE prims ADD COLUMN alpha            REAL    NOT NULL DEFAULT 1.0;
ALTER TABLE prims ADD COLUMN glow             REAL    NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN full_bright      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE prims ADD COLUMN repeat_u         REAL    NOT NULL DEFAULT 1.0;
ALTER TABLE prims ADD COLUMN repeat_v         REAL    NOT NULL DEFAULT 1.0;
ALTER TABLE prims ADD COLUMN flip_u           INTEGER NOT NULL DEFAULT 0;
ALTER TABLE prims ADD COLUMN flip_v           INTEGER NOT NULL DEFAULT 0;
ALTER TABLE prims ADD COLUMN texture_rotation REAL    NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN offset_u         REAL    NOT NULL DEFAULT 0.0;
ALTER TABLE prims ADD COLUMN offset_v         REAL    NOT NULL DEFAULT 0.0;
