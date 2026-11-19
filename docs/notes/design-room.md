---
triggers: [design_room.rs, systems::design_room, designs.rs, "/api/inventory/designs", "/api/inventory/drafts", V48__designs.sql, V49__prim_draft_and_groups.sql, DESIGN_LAYER, RenderLayers, "Design here", designroom, draft, PrimDto::draft, place_draft, only_for, SimWorld::sees]
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
egui overlay anchored to the world — those do not go through render layers at
all, so each one that matters asks `DesignRoom::spot` itself: place names, and
the map chrome (dock, compass column, touch clusters, zoom rail) all stand down
while the room is open, as they do for driver mode and the camera. Place pins
do not yet (their system is at Bevy's parameter cap). Looking up near the
horizon, a row of faint pink and yellow pixels shows along the top edge;
`CARTO_NO_CLOUDS=1 CARTO_NO_ATMOSPHERE=1` removes it, and the cloud shell is a
layer-0 mesh, so it is the atmosphere pass. Not chased further (2026-11-19).

## One white, unlit, and shadows by hand

Floor, grid and walls are one unlit white at linear 1.6 — above 1.0 because the
tonemapper takes 1.0 to a light grey. A lit floor was tried first and took the
sun's colour (peach at mid-morning against grey walls; the person called it
ugly). Unlit takes no sun shadow, and a second, neutral directional light just
for the room is ruled out by `directional-lights-share-one-cascade-config.md`, so
each part whose underside is within 0.3 m of the floor gets a feathered dark
patch as a child (`ContactShadow`). It has to be strong — 62% at the centre —
because at 1.6 the tonemapper squeezes a 28% darkening to no visible change; the
grid's greys are low (0.35–0.66 of the white) for the same reason. The patch is
placed against the room's flat floor, not the part's own ground: the two differ
by centimetres even on the Grote Markt.

## Drafts: built unseen, placed as one act (protocol 51)

What the room builds is a **draft** (`PrimDto::draft`, `V49__prim_draft_and_groups.sql`):
the edit dialog sends `CreatePrim { draft: true }` while the room is open, and a
kept design placed into the room comes in as drafts too. The bar's **Place in
the street** (`POST /api/inventory/drafts/place`, `PrimService::place_draft`)
clears the flag on all of them in one request, and only then does anybody else
hear of them.

Where a draft is kept from others, each one a place that would otherwise leak:

- **Sending.** A change goes out as `Outbound::Thing { only_for: Some(owner) }`,
  which `net::deliver` gives to the owner's connections alone, wherever they
  stand; the reach pass (`net::reach::on_tick`) skips drafts it may not show
  (`SimWorld::sees`) and never takes one *out* of its maker's reach — the room
  can be opened anywhere on the map, further than the 500 m an avatar's reach
  covers.
- **Refusals.** `prims::hidden` answers "not found" for somebody else's draft,
  admins included: a refusal is followed by the true prim
  (`refuse_and_correct`), which would have carried the draft to them.
- **Touch, hands, carrying, physics, sensors.** A draft is not touched by
  others, senses no hands, cannot be carried, is neither a body nor a collider,
  and its script sees nobody (`script_views`).

Not kept from others yet: what a script **on** a draft says, sounds or prints
(`say`, tones, sounds, the console go out by radius or to everybody). A script
is rare on a draft today; the v4 sheet wants scripts in the room, and that is
when these need the owner filter too.

On the client a draft carries `design_room::Draft` and is drawn on the room's
layer alone (`layer_prims`), so its maker sees it in the room and not in the
street.

## The AI builds in the room (MCP, and so the Midas device link)

The client's agent tools gain `design_room` (`open` at a spot, `close`,
`place`), `group` and `ungroup` (`crates/mcp_server/src/server.rs`; the Midas
link lists the same server's tools, so a session on the Midas page reaches them
on the person's phone with no list of its own to edit). While the room is open,
`build` makes drafts and takes the room's spot as its origin. In the room, a bar
at the top says what the agent is doing, read from the activity log the
desktop's editor overlay draws (`McpBridge::log`), with **Pause**: paused, the
building tools (build, edit, delete, script, group, ungroup, design_room)
answer that the person paused it, so it waits and the person steers in the
room or in the session.

Checked end to end on 2026-11-19 against a scratch simulator and a headless
client on mac-1, through `bun tools/mcp.ts`: open, an eight-part stall built as
drafts, grouped, ungrouped, placed. That run found that group ids must stay
below 2^53: a JavaScript client rounded a 63-bit id and its `ungroup` missed.

## Editing by hand

- **One way out**: "Back to the map" top left, and Escape / Android Back
  (`dismiss::Rung::DesignRoom`, after the share sheet and before driver mode,
  so a press first closes the editor a part is open in). The bar has no close
  button of its own any more.
- **Axes**: the grid's two lines through the spot are Blender's red X (east)
  and blue Z (south here), fading with the grid.
- **Snapping**: in the room a handle drag moves in 10 cm steps, turns in 15°
  and sizes in tenths (`gizmo::SNAP_M`, `SNAP_DEG`); the street keeps the free
  drag. G moves (Blender), in the room only: in the street G puts down what the
  avatar carries.
- **Duplicate**: Shift-D, or the bar's Duplicate — a draft copy a metre east.
  The bar shows the selected part's name and size (width × depth × height).

## Not built yet, from v3

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
