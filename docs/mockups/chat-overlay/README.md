---
topic: chat-overlay
triggers: [chat_overlay.rs, chat.rs, voice.rs, "speech bubble", transcript, composer, "push to talk", "voice badge", chat_messages, CHAT_REPLAY, "chat retention", "Here", "Everyone"]
updated: 2026-10-17
---

# Chat over the map: volatile, quieter, with voice: mockup

A design proposal, not code. It follows `docs/notes/chat-without-a-panel.md`,
which describes the overlay as it is.

Public chat (Here and Everyone) stops being an archive. A line is over the map
for 30 seconds and in Chats for a day. After that it is deleted, on the server
as well as the device, and sign-in replays only that day. The overlay becomes
one glass column with the composer under it, in the bottom-left corner. Private
messages leave the public column and arrive as a single notice. Voice moves
from ☰ ▸ Chat into the chat: a mic disc in the dock, a mic in the empty
composer, and a badge over whoever is talking.

`today.png` is the screenshot the proposal answers (2026-10-17). It shows the
composer open with yesterday's lines in it.

## v1

![v1](v1.png)

Open, as drawn in section 6 of v1:

- 30 s at rest and five minutes (this session only) while writing, or 30 s throughout?
- Does a private notice show the first line, or only who it is from?
- Do private chats leave the composer's scope?
- Is the mic disc always in the dock, or only once voice is switched on?
