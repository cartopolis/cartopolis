-- Who built a prim.
--
-- Until now nothing recorded it: `insert_prim` is reached from the wire (a player
-- building in front of them), from the admin authoring API and from a content
-- pack, and none of the three wrote down whose it was. That was liveable while
-- the only question asked of a prim was "draw it", and stops being liveable the
-- moment somebody may export their own work — "their own" needs a definition
-- that is a fact about the row rather than an inference from who happens to be
-- standing near it.
--
-- The account uuid as TEXT, matching `house_claims.principal` and `accounts.id`,
-- which is how every other per-account row in this schema is keyed.
--
-- NULL means nobody in particular, and that is the honest value for three real
-- cases: every prim that existed before this migration, everything a content
-- pack installs (the server built it, no account did), and anything created by an
-- operator acting as the server rather than as themselves. Export is scoped to a
-- caller's own rows, so NULL is exported by no one — which is the correct
-- refusal rather than an oversight.
--
-- Deliberately **not** carried in `PrimDto`: no client has a use for it, the wire
-- is expensive, and putting an account id on an object every observer in the area
-- receives twenty times a second is a privacy decision nobody asked for.
ALTER TABLE prims ADD COLUMN owner TEXT;

CREATE INDEX IF NOT EXISTS idx_prims_owner ON prims(owner);
