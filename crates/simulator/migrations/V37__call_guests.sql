-- Guests in a call, by link (docs/notes/calls.md, "Guests").
--
-- A link is a code the server knows and a secret it never sees (the part of
-- the URL after `#`). The code names the chat whose call it lets a guest
-- into, and stops working at `expires_at` (unix seconds).
CREATE TABLE call_links (
    code TEXT PRIMARY KEY,
    group_id BLOB NOT NULL,
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX call_links_expiry ON call_links (expires_at);

-- A guest is an account made by the link, for one meeting: it may only join
-- the call of `guest_of` (a chat's group id), always through the waiting room,
-- and it is deleted after `guest_expires_at`. NULL for everybody else.
ALTER TABLE accounts ADD COLUMN guest_of BLOB;
ALTER TABLE accounts ADD COLUMN guest_expires_at INTEGER;
