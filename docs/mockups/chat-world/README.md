---
topic: chat-world
triggers: [chat, private_chat.rs, e2ee, contacts, "message request", points, places, notices, ActivityPub]
updated: 2026-09-29
---

# Chat with a world attached: mockup

A design mockup, not code. The `.html` files are the sources; open them in a
browser from this folder. **v5 is the current proposal**: the essence, with
everything else stripped, plus two addenda. v1–v4 are kept below as the history
of the decisions.

## v5: the essence

![v5](v5.png)

### Addendum: who can read it (end-to-end encryption)

Private chats are encrypted; everything else is public and labelled so. Its
third reach, "public post", is now done through places (next addendum) rather
than by following people.

![v5 addendum: private, public in Cartopolis, public post](v5-reach.png)

Decisions on keys and backup: [keys-and-trust.md](keys-and-trust.md).

### Addendum: follow a place (ActivityPub)

Places are what you follow: every area is a public community with a fediverse
address, reachable from Cartopolis and from Mastodon.

![v5 addendum: follow a place](v5-places.png)

### Addendum: notices from the municipality

A notice is a point about specific register objects (BAG, BGT), with dates, a
status and an official author, posted to the places those objects are in.

![v5 addendum: notices from the municipality](v5-notices.png)

### Addendum: contacts and message requests

A stranger's first message is a request, held apart from your chats until you
accept; accepting makes you contacts. You add people mostly in the world, or by
link, QR code or exact username.

![v5 addendum: contacts and message requests](v5-contacts.png)

## Superseded: following people and identity proofs

Drawn and then taken out of scope on 2026-09-26: fediverse and email proofs,
and following people rather than places.

![superseded: fediverse proofs and following people](v5-fediverse.png)

## v4: people and the world, one gesture

Anything you can tap in the world can go into a chat, and anything in a chat
opens back into the world at the camera view it was sent from.

![v4, screens 1–4](v4-screens-1-4.png)

![v4, screens 5–8](v4-screens-5-8.png)

![v4, the loop and what is new against today](v4-loop-and-new.png)

## v3: ordinary car routing kept

![v3, screens 1–4](v3-screens-1-4.png)

![v3, screens 5–8: routing and driving](v3-screens-5-8.png)

![v3, signed out, legend and who-sees-what](v3-signed-out-and-legend.png)

## v2: real location separated from the avatar

![v2, screens 1–4](v2-screens-1-4.png)

![v2, screens 5–7, legend and who-sees-what](v2-screens-5-7.png)

## v1: first design

![v1, phone screens](v1-phones.png)

![v1, wide screen and what exists vs what is new](v1-desktop.png)
