-- An account an AI agent speaks for (protocol 43, docs/mockups/voice-agent/):
-- set by an operator over HTTP, shown to everybody as a badge beside the
-- name, so people know before they call or answer that what they say to it
-- is processed by a model provider.
ALTER TABLE accounts ADD COLUMN agent INTEGER NOT NULL DEFAULT 0;
