-- ADR-017 Phase 2: Add texture support to prims
-- Adds optional texture_id column to store keys from standard texture library

ALTER TABLE prims ADD COLUMN texture_id TEXT DEFAULT NULL;
