-- ADR-029: geo-anchored prim storage.
-- A prim's horizontal placement is an absolute geographic coordinate
-- (anchor-independent), so it survives an OSM re-anchor. Every prim is
-- geo-anchored; start from a clean slate.
DELETE FROM prims;
ALTER TABLE prims ADD COLUMN latitude REAL NOT NULL DEFAULT 0;
ALTER TABLE prims ADD COLUMN longitude REAL NOT NULL DEFAULT 0;
