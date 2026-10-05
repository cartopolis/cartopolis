-- Physical prims (protocol 34, docs/notes/physical-prims.md).
--
-- `prims.physical` is 1 when the simulator's rigid-body step moves the prim.
-- Written only by SetPrimPhysical, a script's `set_physical` (at the next
-- flush) and authoring — never by an ordinary prim edit, which rewrites every
-- other column and must not switch a ball off because a tool forgot the flag.
ALTER TABLE prims ADD COLUMN physical INTEGER NOT NULL DEFAULT 0;
