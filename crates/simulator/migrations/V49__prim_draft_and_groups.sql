-- The design room (protocol 51, docs/notes/design-room.md).
--
-- A draft is a prim only its owner is sent, until it is placed and this goes
-- back to 0.
ALTER TABLE prims ADD COLUMN draft INTEGER NOT NULL DEFAULT 0;
-- The groups a prim is in, outermost first, as JSON [{"id":…,"name":…}].
-- Each prim carries its whole chain, so there is no groups table to keep in
-- step: renaming or dissolving a group rewrites the prims that are in it.
ALTER TABLE prims ADD COLUMN groups TEXT NOT NULL DEFAULT '[]';
