-- Model assets: a stored glTF binary a prim can wear instead of its procedural
-- shape, the inventory that holds one, and the shared library that publishes it.
--
-- # Why this is four tables and not one
--
-- A file is named by a hash of its own bytes, so uploading the same bench twice
-- stores one file. That deduplication is the whole reason the other three tables
-- exist: the moment two people can be behind one file, "who uploaded it", "is it
-- shared" and "what is it called" stop being properties of the file.
--
--   * `assets`           — the bytes. Immutable, no owner, no name.
--   * `asset_claims`     — one row per person who uploaded it, and what licence
--                          they said it was under.
--   * `inventory_items`  — a person's named reference to a file. Renameable,
--                          movable, disposable, and none of that touches the file.
--   * `library_entries`  — the catalogue: one row per file, what cartopy curates
--                          and what the in-world picker lists.
--
-- Folding any two together breaks a case that will happen on the first day: if
-- Alice and Bob both upload the same bench and Bob then deletes his, Alice's must
-- not go blank, and the library entry must not lose its category.

-- The bytes. `hash` is what `assets::content_hash` produced and is the whole of
-- the address: `/api/assets/{hash}.glb` names a file that can never change, so it
-- is cacheable forever by the client, by the browser and by any proxy between.
--
-- The measured facts are stored rather than recomputed because they are what a
-- refusal was decided on. Re-reading a 12 MB file to answer "how many triangles
-- is that?" for a listing of two hundred models is the kind of cost that only
-- shows up once the library is full.
CREATE TABLE IF NOT EXISTS assets (
    hash            TEXT PRIMARY KEY,
    byte_size       INTEGER NOT NULL,
    triangles       INTEGER NOT NULL,
    nodes           INTEGER NOT NULL,
    textures        INTEGER NOT NULL,
    largest_texture INTEGER NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Who put a file here, and what they said it was.
--
-- The licence is recorded at upload rather than only at sharing, because the
-- honest moment to ask "where did this come from?" is when the person still has
-- the answer in front of them. It is a *claim*: the server cannot verify it and
-- does not pretend to. What it can do is carry it — to the curator in cartopy, to
-- the picker beside the model, and into the attribution a CC-BY model needs.
--
-- No foreign key to `accounts`, matching `blocks.blocker_id` and `prims.owner`:
-- an account may be deleted and the file it uploaded stays, referenced by prims
-- standing in the street.
CREATE TABLE IF NOT EXISTS asset_claims (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    asset_hash  TEXT NOT NULL REFERENCES assets (hash),
    account_id  TEXT NOT NULL,
    -- `own`, `cc0`, `cc-by`, or `other`. `other` may be uploaded and used and may
    -- never be shared — see `library_entries`.
    licence     TEXT NOT NULL,
    attribution TEXT,
    source_note TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (asset_hash, account_id)
);

-- A person's named reference to a file: the inventory.
--
-- `folder` is a display path (`Furniture/Benches`), not a tree of rows. A folder
-- here is a label on an item and nothing else owns it, so renaming one is an
-- UPDATE over a prefix and an empty folder simply stops existing — which is the
-- behaviour a person expects from something that never had to be created.
CREATE TABLE IF NOT EXISTS inventory_items (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    asset_hash TEXT NOT NULL REFERENCES assets (hash),
    name       TEXT NOT NULL,
    folder     TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS inventory_items_account
    ON inventory_items (account_id, folder, name);

-- The shared library: one row per file, created when somebody shares it.
--
-- `visibility` is `shared`, `curated` or `hidden`, and the three are a sequence
-- rather than a hierarchy. A newly shared model is visible at once — a library
-- that waits for review never fills — and curation adds the category, the tidy
-- name and the operator's word that it belongs. `hidden` is the moderation
-- answer, and it is enforced at the *download* route as well as in the listing:
-- hidden from the catalogue while still being served to every prim already
-- wearing it is the half-measure that looks like it works.
CREATE TABLE IF NOT EXISTS library_entries (
    asset_hash  TEXT PRIMARY KEY REFERENCES assets (hash),
    name        TEXT NOT NULL,
    category    TEXT NOT NULL DEFAULT 'other',
    -- Free tags, comma-separated, for the picker's search box. A join table buys
    -- nothing here: nothing aggregates by tag and the whole catalogue fits in one
    -- listing.
    tags        TEXT NOT NULL DEFAULT '',
    licence     TEXT NOT NULL,
    attribution TEXT,
    -- Who shared it, which is not necessarily who uploaded it first.
    shared_by   TEXT,
    visibility  TEXT NOT NULL DEFAULT 'shared',
    curated_at  TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS library_entries_browse
    ON library_entries (visibility, category, name);

-- The prim's side of it: the asset hash a prim wears, or NULL for the procedural
-- shape every prim before this migration has.
--
-- The hash and not an inventory item id. An item is a person's named reference
-- and can be renamed, moved or thrown away; the prim in the street must not go
-- blank when it is.
ALTER TABLE prims ADD COLUMN model TEXT;

CREATE INDEX IF NOT EXISTS idx_prims_model ON prims (model);

-- Reports are about people (`reports.subject_username`), and a model is the first
-- thing that can be reported which is not one. The subject stays as it is — the
-- uploader is still a person an operator may want to act on — and the hash rides
-- alongside, so acting on one report reaches every prim wearing that file rather
-- than the single one somebody happened to be standing in front of.
ALTER TABLE reports ADD COLUMN subject_asset TEXT;
