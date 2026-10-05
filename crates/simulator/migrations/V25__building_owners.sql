-- Owning a building: one building, one owner; one owner, several buildings.
--
-- Replaces `house_claims` (V12), which keyed on the *address* and allowed one
-- per account. What a person now holds is the building — the BAG `pand`, which
-- is the key the client draws by, atlas files a building design under, and the
-- floor-plan editor edits — because what they can do with it (design its
-- inside, and later its outside) is a thing done to a building, not to a front
-- door. Two consequences, both deliberate: a block of flats has one owner, and
-- an account may hold several (bounded by `max_buildings_per_account`, a
-- setting, not a constraint here).
--
-- The address the claim was made through is kept, for the listing a person and
-- an operator read: a BAG number means nothing to anybody.
CREATE TABLE IF NOT EXISTS building_owners (
    pand_id             TEXT PRIMARY KEY,
    principal           TEXT NOT NULL,
    nummeraanduiding_id TEXT NOT NULL,
    address             TEXT NOT NULL,
    postcode            TEXT,
    latitude            REAL NOT NULL,
    longitude           REAL NOT NULL,
    claimed_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS building_owners_principal ON building_owners (principal);

-- Every existing claim becomes the building it was in. Where two people held
-- flats in one building the earlier claim keeps it; on the day this ran the
-- hosted server held exactly one claim, so that rule decided nothing.
INSERT OR IGNORE INTO building_owners
    (pand_id, principal, nummeraanduiding_id, address, postcode, latitude, longitude, claimed_at)
SELECT pand_id, principal, nummeraanduiding_id, address, postcode, latitude, longitude, claimed_at
FROM house_claims
ORDER BY claimed_at, nummeraanduiding_id;

DROP TABLE house_claims;
