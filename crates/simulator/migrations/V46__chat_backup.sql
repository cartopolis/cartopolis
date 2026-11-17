-- A person's chat backup (docs/notes/encrypted-chat.md, "Recovering chats with
-- a recovery key"): the hash, in the picture store, of their chats sealed
-- under a key only their recovery key gives. The server cannot open it; this
-- is only where a new device of theirs finds it. NULL for nobody set one up.
ALTER TABLE accounts ADD COLUMN chat_backup TEXT;
