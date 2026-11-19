-- Drafts from the design room (protocol 51, docs/notes/design-room.md): a
-- prim only its owner is sent, until it is placed and this goes back to 0.
ALTER TABLE prims ADD COLUMN draft INTEGER NOT NULL DEFAULT 0;
