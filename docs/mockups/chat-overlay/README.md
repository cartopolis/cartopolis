---
topic: chat-overlay
triggers: [chat_overlay.rs, call_screen.rs, "chat sheet", "call strip", chat.rs, voice.rs, dock.rs, "speech bubble", transcript, composer, "push to talk", "voice badge", chat_messages, CHAT_REPLAY, "chat retention", "Here", "Everyone"]
updated: 2026-11-14
---

# Chat over the map: volatile, quieter, with voice: mockup

A design proposal, not code. It follows `docs/notes/chat-without-a-panel.md`,
which describes the overlay as it is.

Public chat (Here and Everyone) stops being an archive. A line is over the map
for 30 seconds and in Community's Here and Everyone threads for a day. After
that it is deleted, on the server as well as the device, and sign-in replays
only that day. The overlay becomes one glass column with the composer under it,
in the bottom-left corner. Private messages leave the public column and arrive
as a single notice. Voice moves from ☰ ▸ Chat into the chat: a mic disc among
the action buttons, a mic in the empty composer, and a badge over whoever is
talking.

`today.png` is the screenshot the proposal answers (2026-10-17). It shows the
composer open with yesterday's lines in it.

## v3: one chat at a time, and a call that joins it

![v3](v3.png)

Drawn 2026-11-14, from `today-v3.png`: the person found the chat on a phone
"rommelig en onoverzichtelijk" and asked for a better design with calls
in it as an option. On the phone in that screenshot five short messages
fill the screen, every run is headed "name · Everyone", the column lies over
the compass, the light and a person pin, and the field says "Say something
nearby…" under a conversation that is all Everyone.

v3 replaces the merged transcript and the composer pill with one sheet: a
row of chat tabs (Here, Everyone, the private chats with something new), the
open chat's lines at the messenger's size, the field at its foot. At rest it
is one line over the World | Community switch. Talking nearby (proximity
voice, public) and an encrypted call in a private chat are a button in the
tab row, and once on they are a strip in the sheet, with video tiles over
the top of the map; the full call screen is one tap away. Nothing on the
wire changes.

Decided 2026-11-14, on the questions in section 3:

- Tabs: Here and Everyone always, plus private chats with something new in the last hour.
- Everyone stays on the map as a tab, with no call button.
- A call rings full screen as today; once answered it folds to the tiles and the strip.
- Talk nearby is an open mic with a mute button.

## v2: nothing on the right-hand edge

![v2](v2.png)

Decided 2026-10-17, on v1: **the action buttons stay centred along the bottom**,
as they are in the app. v1 had moved them to the bottom-right corner to make
room for the chat column, with the voice popover above them and the private
notice in the top-right corner. Every panel opens on the right, so all three
would have been behind the first panel opened. In v2 the notice hangs under the
Community | World switch at the top centre, and the chat column sits above the
row of buttons rather than beside them. Section 1b also proposes centring the
buttons and the switch on the map area rather than the window, which today they
are not: with a panel open on a narrow window, the right end of the row already
slides under it.

Still open, as drawn in section 6:

- 30 s at rest and five minutes (this session only) while writing, or 30 s throughout?
- Does a private notice show the first line, or only who it is from?
- Do private chats leave the composer's scope?
- Is the mic disc always among the buttons, or only once voice is switched on?

## v1

![v1](v1.png)
