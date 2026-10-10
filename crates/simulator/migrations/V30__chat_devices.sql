-- Which devices hold which private chat (protocol 39,
-- `docs/notes/encrypted-chat.md`, "Every device of every member").
--
-- `e2ee_members` routes by account; this is the per-device half, so the server
-- can tell that somebody's phone is missing from a chat their browser is in and
-- ask a device that is in it to add the phone. Rows come from what the server
-- already sees: the device that registered the chat, the devices an invitation
-- names, a device that writes to the chat, and every cursor a device syncs
-- with (which is how chats from before this table get their rows).
CREATE TABLE IF NOT EXISTS e2ee_group_devices (
    group_id BLOB NOT NULL,
    device   TEXT NOT NULL,
    PRIMARY KEY (group_id, device)
);
CREATE INDEX IF NOT EXISTS e2ee_group_devices_device ON e2ee_group_devices (device);

-- When a device in a chat was last asked to add a missing one. Asking spends
-- one of the missing device's key packages, so it is not repeated while the
-- first ask may still be on its way.
CREATE TABLE IF NOT EXISTS e2ee_device_asks (
    group_id BLOB NOT NULL,
    device   TEXT NOT NULL,
    asked_ms INTEGER NOT NULL,
    PRIMARY KEY (group_id, device)
);
