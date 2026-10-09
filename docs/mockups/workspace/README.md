---
topic: workspace
triggers: [atlas web, "crates/atlas/web", workspace, channels, "work chat", calls, webcall.js, "share my view", "building as office", palette, theme.rs, app.css]
updated: 2026-10-09
---

# The web UI as a work chat with the world behind it: mockup

A design mockup, not code. The `.html` files are the sources; open them in a
browser from this folder. Everything marked **new** does not exist today.

The idea: cartopolis.org becomes a Slack-like work chat (workspaces, channels,
threads, DMs, calls) built on the app's own encrypted chats and calls, and
every channel keeps the app's **Chat | World** switch from chat-world v5, with
World being the existing web viewer embedded in the page. On top of that, six
things only a world can give a work chat: channels bound to a place, presence
that can say where, calls that take place in a room, sharing a camera view
instead of a screen, the team's building as its office, and searching by place.

## Branding

Drawn in a palette halfway between the web UI today (`crates/atlas/web/src/css/app.css`,
slate + full brand orange) and the app today (`systems::theme`, charcoal +
ember): every value is the RGB midpoint of the two. Font, wordmark, the brand
orange's scope (logo, badges, live dot) and the real-location blue are unchanged.
The swatch strip is on sheet 1.

## v1, sheet 1: chat, and the same channel in World

![v1: chat and world](v1.png)

## v1, sheet 2: calls as a room, and share my view

![v1: calls](v1-calls.png)

## v1, sheet 3: the office is a building, a place is a search term

Also the proposed build order and the open questions.

![v1: office and places](v1-office.png)
