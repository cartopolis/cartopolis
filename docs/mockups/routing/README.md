---
topic: routing
triggers: [navigation.rs, driver.rs, "route planner", "travel mode", "walk me there", "meet up", chats.rs, private_chat.rs]
updated: 2026-10-06
---

# Routing: walk in the world, ride and drive with a GPS, meet through a chat

A design mockup, not code. Every frame is a real capture of the phone build at
412×915 (`--shots` with a scratch manifest, Groningen, 2026-10-06, main at
27a5e67) with the proposal drawn over it in HTML; `shots/` holds the captures.
Anything outlined in dashed orange is new.

## v1

![v1](v1.png)

Decided with the user on 2026-10-06, before drawing:

- **Walk** is the in-world journey: the avatar, the map and the follow camera
  stay; guidance is drawn on the ground; *Walk me there* and *Skip ahead*; the
  route ends at the BAG entrance point.
- **Bike** stays its own mode, because of the Netherlands' bike-only roads.
- **Car** is real-world navigation only and needs a position fix. There is no
  drivable car in the world. A desktop location source may come later.
- **Navigation belongs in the friends chat**, to make meeting easier: *Route
  here* on points, live locations and avatars, a *meet-up* message, and each
  person's route and arrival time on the chat's map. All of it inside the
  chat's end-to-end encrypted messages.
