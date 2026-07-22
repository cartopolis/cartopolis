-- ADR-028: chat scrollback for durable channels (Global, Rooms). The Local
-- proximity tier is ephemeral and never written here.
CREATE TABLE IF NOT EXISTS chat_messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel     TEXT NOT NULL,
    sender_id   INTEGER NOT NULL,
    sender_name TEXT NOT NULL,
    sent_at_ms  INTEGER NOT NULL,
    body        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS chat_channel_time ON chat_messages (channel, sent_at_ms);
