-- What a lamppost looks like on this server.
--
-- The street objects the client draws — bins, benches, lampposts, parked cars,
-- moored boats — come from a national or a municipal register, so nobody placed
-- them and nobody can select one and swap it. Their forms are procedural boxes
-- built into the client (`cartopolis_geo::furniture::form`), which is why a whole
-- city of them costs one mesh per cell and no downloads.
--
-- This table is the one statement an operator can make about them: a form, and a
-- model out of the shared library to draw instead of it. World-wide rather than
-- per person, because it is a statement about *this world* — two people standing
-- on the same street seeing different lampposts is not a preference, it is a
-- disagreement about what is there.
--
-- One row per form, so a `kind` primary key rather than an id: "what does a bench
-- look like here?" has exactly one answer, and setting it again replaces it.
CREATE TABLE IF NOT EXISTS placeholder_models (
    -- A slug from `cartopolis_core::placeholders::PLACEHOLDER_KINDS`. Checked
    -- against that list by the route, not by a constraint here: the list is Rust
    -- and a CHECK would be a second copy of it that no test can reach.
    kind        TEXT PRIMARY KEY,
    -- The model. A reference into `assets`, and ON DELETE is deliberately absent:
    -- `admin_forget_asset` refuses to remove a file anything still points at, and
    -- a placeholder is one of the things that points at it.
    asset_hash  TEXT NOT NULL REFERENCES assets (hash),
    -- Metres per model unit. A model exported in centimetres is the ordinary case
    -- and is not something an operator should have to re-export to fix.
    scale       REAL NOT NULL DEFAULT 1.0,
    -- Degrees, added to the angle the client already gives the object — a bench
    -- squares up to the pavement, and a model whose front faces +x rather than +z
    -- would otherwise sit sideways on every street in the city.
    yaw_offset  REAL NOT NULL DEFAULT 0.0,
    -- Who set it, for the same reason `library_entries.shared_by` is kept: an
    -- operator action on a shared world is worth being able to ask about.
    set_by      TEXT NOT NULL,
    set_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
