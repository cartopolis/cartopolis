---
triggers: [design_room.rs, systems::design_room, designs.rs, "/api/inventory/designs", V48__designs.sql, DESIGN_LAYER, RenderLayers, "Design here", designroom]
updated: 2026-11-19
---

# The design room, as built

The mockups (`docs/mockups/design-room/`, v3) draw a whole tool. What exists
since 2026-11-19 is its smallest useful part: **Design here** on the map's ring
opens a white room around the spot, the ordinary editor builds in it, **Keep**
stores the objects within 10 m of the spot as one design in the account's
inventory, and **Place here** stands a copy of a kept design at the room's spot.

## The world is taken away by render layer

Hiding the street by `Visibility` would make the room a second writer of every
streamed entity's visibility — buildings, tiles, trees, furniture, traffic,
crows each have exactly one writer, and two writers race
(`globe::update_globe_visibility`'s comment lists the ones that did). So the
room hides nothing: the camera is put on `DESIGN_LAYER` (7) alone, and the
room's floor, grid and walls, every prim (with its children) and the
directional lights are put on it.

The lights are not optional. bevy 0.20 draws a directional light only into a
view whose `RenderLayers` it intersects (`bevy_pbr` `render/light.rs`), so a
sun left on layer 0 leaves the room unlit; and it renders shadow casters only
from its own layers, which is why the sun goes on layer 7 *alone* while the room
is open — on both, the hidden facades would still throw their shadows across
the white floor. Prims keep both layers after the room closes, which layer 0
alone still draws.

The gizmo config is put on both layers once at startup, or the editing handles
vanish in the room. Point lights (street lamps) are filtered by layer too, so
the room has none of them, deliberately.

What still shows: the sky (bevy's atmosphere is a camera effect, not an entity —
the room's walls, a 40 m unlit sphere, are what hide it from inside), and every
egui overlay anchored to the world (names, pins) — those do not go through
render layers at all.

## Not built yet, from v3

- Parts are live prims: others see them appear as they are built, and leaving
  the room leaves them standing. v3's "place it back as one act" needs drafts
  held apart from the world (server-side, per the mockup), which is the next
  piece.
- No `group` field on the wire: a placed copy is separate objects again.
- No Eye level / From the street views, no avatar for scale, no "designing"
  presence, no Share or Move to the server library (`library_publish`).
- The kept designs are listed only in the room's bar, not in Library.

## The server side

`simulator::designs`, under `/api/inventory/designs` because `cartopolis.org`
proxies `/api/inventory/*` already (`model-assets-and-inventory.md`). A design
is the `/api/content/export` document of the caller's *own* prims among the
ids sent, so a client cannot keep somebody else's work, plus the spot. Placing
moves each part by its offset from that spot **in metres**, so a stall keeps
its shape at another latitude, and creates through `authoring::create` — the
import path, with the same permission, shape and budget checks. At most 64
parts.

## How to look at it

`--ui designroom` opens the room where the shot's camera looks and stands
`design_room::DEMO_STALL` in it (negative ids, offline only).
