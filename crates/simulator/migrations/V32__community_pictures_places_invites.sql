-- Community pictures, places and invite links (protocol 40,
-- docs/notes/communities.md).
--
-- A picture is a hash in the picture store (`/api/pictures`), a small JPEG
-- that is not sealed: everybody the community is listed to may see it. A place
-- is what the directory's "Near you" sorts by.
ALTER TABLE communities ADD COLUMN picture TEXT;
ALTER TABLE communities ADD COLUMN place_lat REAL;
ALTER TABLE communities ADD COLUMN place_lng REAL;
ALTER TABLE communities ADD COLUMN place_name TEXT;

-- An invite link: anyone holding the code may join until it expires, whatever
-- the community's way in. Somebody removed with a block still may not.
CREATE TABLE IF NOT EXISTS community_invites (
    code        TEXT PRIMARY KEY,
    community   TEXT NOT NULL,
    created_by  TEXT NOT NULL,
    created_ms  INTEGER NOT NULL,
    expires_ms  INTEGER NOT NULL,
    uses        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS community_invites_community ON community_invites (community);
