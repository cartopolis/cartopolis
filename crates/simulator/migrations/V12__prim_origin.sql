-- Which content pack, if any, installed a prim (`crate::content`).
--
-- Reinstalling a pack has to remove what it installed last time, or running the
-- command twice leaves two firebirds circling the same city. The first version
-- of that matched on the prim *names* the pack declares, and it does not work:
-- the beacon's own script renames it to whatever its label currently says
-- (`vfSetName`), so the second install found no prim called "Beacon", left the
-- old one standing and added another. Provenance is a fact about the row, not
-- something to infer from a field a script is free to rewrite.
--
-- NULL means "somebody built this", which is every prim that existed before this
-- migration and every prim a person ever makes. Deliberately *not* carried in
-- `PrimDto`: no client has any use for it, and the wire is expensive.
ALTER TABLE prims ADD COLUMN origin TEXT;

CREATE INDEX IF NOT EXISTS idx_prims_origin ON prims(origin);
