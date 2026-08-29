-- Claiming a house: one account, one address, first come first served.
--
-- The key is the BAG **nummeraanduiding** — the address, not the building. In a
-- city centre one `pand` is a whole block of flats, so keying on the building
-- would hand the first caller everybody else's front door as well. The pand is
-- carried alongside because it is the join to the geometry the client draws
-- (3DBAG's `identificatie`, `systems::building_facts`), and it is deliberately
-- *not* unique: several claims share a building, which is what a block of flats
-- is.
--
-- `principal` is UNIQUE, which is the "one house per account" rule expressed
-- where it cannot be raced. Checking it in the handler would leave two
-- simultaneous claims from one account both passing the check and both writing.
CREATE TABLE IF NOT EXISTS house_claims (
    nummeraanduiding_id TEXT PRIMARY KEY,
    principal           TEXT NOT NULL UNIQUE,
    pand_id             TEXT NOT NULL,
    -- The address as the register renders it ("Grote Markt 1, 9712HN
    -- Groningen"), stored rather than re-fetched: this row has to be legible in
    -- an admin list without a round trip to a service that may be down, and the
    -- register's rendering is the one the person recognises.
    address             TEXT NOT NULL,
    postcode            TEXT,
    latitude            REAL NOT NULL,
    longitude           REAL NOT NULL,
    claimed_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS house_claims_pand ON house_claims (pand_id);
