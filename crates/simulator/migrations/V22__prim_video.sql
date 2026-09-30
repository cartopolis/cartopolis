-- Video on a prim (protocol 28, docs/notes/video-on-a-prim.md).
--
-- `prims.video` is the playing clip as JSON (`PrimVideo`: asset hash, start
-- time, loop flag), NULL for none. Written only by SetPrimVideo, never by an
-- ordinary prim edit, which rewrites every other column.
ALTER TABLE prims ADD COLUMN video TEXT;

-- The video store: one row per clip, keyed by the hash of its bytes, which
-- live in `{asset_dir}/videos/{hash}.webm`. What was measured at upload, so a
-- client can size its player without opening the file, and who sent it.
-- No FOREIGN KEY, as V20 and V21 have none: this connection runs with
-- `foreign_keys` off. A deleted account's clips stay, as its models do —
-- other people's prims may be playing them.
CREATE TABLE videos (
    hash        TEXT PRIMARY KEY,
    byte_size   INTEGER NOT NULL,
    width       INTEGER NOT NULL,
    height      INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    has_audio   INTEGER NOT NULL,
    uploaded_by TEXT NOT NULL,          -- accounts.id
    created_at  INTEGER NOT NULL
);
