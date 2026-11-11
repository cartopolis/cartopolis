---
topic: bridges
triggers: [bridges.rs, systems::bridges, place_card.rs, place_pins.rs, "Show in world", "Open in Community", "Open chat", "Back to", "Share sheet", share_at, show_in_world, post_here, "Send this view", "Post this view", building_card.rs]
updated: 2026-11-11
---

# World and Community touch through three bridges, and no others

The person approved v2's navigation (`docs/mockups/redesign`, 2026-11-08):
two spaces, World (the map) and Community (people), with one switch between
them — and **three named bridges**, each saying which way it goes. Built
2026-11-11 in `systems::bridges`. Before it, the community reached into the
map as loose buttons ("Say hi", "Post here", "Send this view to a chat") and
markers that only flew the camera, and nothing said how the two hang
together. A fourth way across is the thing to refuse.

## ① Markers → Community

A Community thing that has a place is a marker on the map; a tap opens the
**place card** (not a new card) with where it comes from on top and the way
into Community as its main verb (`place_card::CardPlace::from`,
`bridges::Origin`):

| marker | line on top | main verb | Route here |
|---|---|---|---|
| a person sharing their location (`chat.locations`) | "sanne · sharing location" | Open chat | follows them (`route_to_person`) |
| a community's post with a place (`posts.map_posts`) | "Garage 44 · post" | Open in Community (its page, the post focused) | to the spot |
| a chat's point, in that chat's World view | "<chat> · place" | Open chat | to the spot |
| a contact's avatar (Friends layer, a chat's World view) | — the person card (`contacts`) | its Message | — |

A marker tap no longer flies the camera: the marker is on screen already, and
the card is the answer to "what is that". The card drops Save and Share for a
Community thing — it is somebody else's, not a place of the map's.

**Not markers yet:** calendar plans with a place. Nothing draws them on the
map today, and the brief was to bridge what exists, not to invent data; a
plan's place is reachable by its Show in world.

## ② Show in world

Everything in Community with a place has one button, **Show in world**: a
shared location in a chat (it was "Show"), a meet-up ("Open in world"), a point
card in a thread (its tap), a plan in the calendar, a post's place (it was the
place's name as a chip). It sets `ChatsView::show_in_world`;
`bridges::drive_bridges` remembers the exact Community screen (the open chat,
the You page and contacts page, the community page, the calendar page and its
row — `bridges::Return`), switches to World, flies there through
`Teleport::point` (never a transform write), opens the place's card — the
place's, not the plan's (N7) — and puts **‹ Back to <it>** where the search bar
was (`bridges::back_chip`, drawn by the dock). The chip, or Back with nothing
else up (`dismiss::Rung::BackToCommunity`), restores that screen.

A point in a chat used to open the chat's own World view (its numbered pins
and header) on a tap; that view is still the thread's Chat | World switch, but
a single place now goes through the bridge like everything else.

**When the chip goes** (`Back::left`): when the person searches (the Search
panel opens), another place's card opens, they go to Community by the switch,
or the view leaves the place — the camera more than 1.5 km, or three times its
height, from it, counted only once the flight has arrived there. A look round
the square keeps it; flying to the next town does not.

## ③ Share

In World a place's card, a building's card, the ring and the Search panel each
have one **Share**, and all of them open one sheet (`bridges::render_share_sheet`):
"Share <name> — Sends the place to a chat or community", the four most recent
private chats, the communities you can post in ("Community · as a post"), and
Copy link. Signed out it says so and keeps Copy link.

What it folded in, and where those went:

- **Search's "Send this view to a chat" and "Post this view to a community"**
  are one row, "Share this view…", which shares the place *with the view*
  (`SharePlace::view`). Set Home here and Copy link stay as they were.
- **The ring's "Post here"** is gone; the ring's Share opened the OS share
  sheet with a link and now opens this one (`ContextMenuState::share_at`).
- A chat row sends the place as a point through `ChatState::private_points` —
  the same sealing queue "Send this view" used, so a private chat's E2EE path
  is unchanged (`encrypted-chat.md`) — and opens that chat, so the person sees
  it arrive. A community row opens that community's composer over the map with
  the place on the post, where Post here used to.

The Share button on the place card wears the brand's orange dot (N2): the one
social verb on a World card, and the dot says it leaves the map.
