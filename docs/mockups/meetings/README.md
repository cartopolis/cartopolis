---
topic: meetings
triggers: [calls, "video call", meeting, "screen share", "screen sharing", lobby, "raise hand", captions, "breakout rooms", call_screen.rs, systems::call_screen, call.ts, webcall.js, crates/calls]
updated: 2026-10-17
---

# Calls: the tools a professional meeting needs: mockup

A design proposal, not code. It adds the usual conferencing tools to the
encrypted calls in `docs/notes/calls.md`: planning a meeting, a lobby, hosts,
screen sharing, raised hands, reactions, captions, shared notes, polls, breakout
rooms and a record afterwards.

It builds on `../workspace/` v1, sheet 2 (calls held in a chat's place). The
user asked on 2026-10-17 to keep that tie to the world, so a meeting has the
chat's place and **Room** is one of its four layouts, but to add **no new world
interaction** for now: the new tools are the ordinary conferencing ones.
Hearing by distance and Share my view stay in that sheet.

Every tool says where it runs, because the server forwards sealed frames and
must never be able to open one. Lobby, host mute, lock and remove are enforced
by the server with what it already sees. Hands, reactions, notes, polls and the
record are sealed chat messages. Captions, noise suppression, background blur
and recording run on a person's own device. Dial-in by telephone is not offered,
because a gateway would have to decrypt the call.

## v2: the golden standard

The same tools, redrawn to the standard of the best conferencing apps and
following the principles at the top of the sheet. The stage gets the space and
the controls float on glass. Every interruption arrives as a notice with its
action in it. State is never hidden. The meeting catches the common mistakes:
talking while muted, the last host leaving, sharing a whole screen. Section 2
draws nine of these moments on their own. The people are drawn by `faces.js`,
not taken from photographs.

![v2](v2.png)

## v1

![v1](v1.png)

Open, as drawn in section 9 of v1:

- Breakout rooms: one key with the server keeping rooms apart, or one encryption group per room?
- Guests without an account, by link and always through the lobby?
- Recording: on a device only, off by default, allowed per meeting?
- Captions: a speech model downloaded on first use; can a phone keep up?
- In a community, are its moderators always hosts?
