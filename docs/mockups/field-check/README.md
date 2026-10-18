---
topic: field-check
triggers: [field check, rtk, gnss_receiver, camera.rs, photo.rs, front camera, back camera, agent, shot harness, correcting map data]
updated: 2026-10-15
---

# Field check: the real view and Cartopolis's view, side by side

A design mockup, not code. **v4 is the current proposal.**

## v4: simple

The purpose is one thing: verify the virtual view against the real image.
Point the phone, press once, and get one picture, real next to Cartopolis,
with the position, accuracy, direction, camera, time and app version under it
(enough to render the same view again). It goes to the agent, who says where
the two differ. The capture screen is the camera, a corner render, a shutter
and front/rear, with one chip for the position quality.

The pose also travels as data, including the ground height the receiver
measured (converted to NAP, minus the antenna height), so the agent can check
it against AHN. That is data only, nothing on screen.

The data is a shot preset (`docs/shots.toml` format), kept in
`docs/field-checks.toml`, so the agent can put the camera back on exactly the
same view later with `cargo shots --only <name>`. That needs three keys the
harness does not have yet: `roll`, `fov` (the phone lens) and `date` (with
`time`, the sun). Measured values go in a `[shot.measured]` table that is data
only.

![v4](v4.png)

### The chat

The field check has its own button in the chat bar, only in a chat with the
agent. (Built so; since 2026-10-18 the button is in Developer tools ▸ View
instead, and the picture gains a third panel, the aerial photograph from above
with the camera marked — `docs/notes/field-check.md`.) The agent answers by calling the harness: it saves the check as a
preset, renders the same view on a build node, confirms its render matches the
phone's, then says what differs from the photo. Later it can put the camera
back on that view and show real, before and after. Each harness call shows as
one grey line in the chat.

![v4, the chat](v4-chat.png)

## Superseded: v3, what the position knows

Judged too complicated on 2026-10-15: the height check, compass log, touch
check and full quality sheet are kept here as ideas, not as the design.

The receiver reports far more than a position, and each part is a check on the
world. Its **height** (converted to NAP, minus the antenna height) checks AHN and
the ground we grade from it. **Lining up the ghost** gives a true heading beside
the compass heading, and keeping both measures the compass over a walk. A
**touch check** (receiver held against a building corner, kerb or pole, no
camera) compares a centimetre point with where the BAG and BGT put that object.
The quality rows (fix, error per direction, satellites, correction age, station
distance, antenna height) decide how far each check may be trusted, and all of
it goes into the picture's strip.

![v3](v3.png)

The picture with the full strip:

![the picture, v3](pair-v3.jpg)

Real in v3: the BAG outline of building 0014100010906600, AHN4 ground height
6.97 m NAP at `53.2183, 6.5679`, the PDOK aerial, the correction station
DELZ00NLD0 at 26 km, and the BGT having no surveyed poles on the Grote Markt.
Every receiver reading, the compass log and the agent's walk summary are
examples.

Open: a touch on something the registers do not have is a measurement, not a
check. Whether those may become world content is undecided; the rule so far is
real registers only.

## Superseded: v2, a harness for correcting the world

A tool for the agent first, and a feature maybe later. Standing somewhere with
the RTK receiver, one shutter makes **one picture**: the photo on the left,
Cartopolis drawn from the same position, heading, tilt and lens on the right,
and the pose (with its accuracy and the client build) written underneath.
It goes to the agent with a line about what's wrong. The agent traces each
difference to the data or rule that draws it, fixes that, and replies with a
before/after rendered from the same pose. The front/rear switch turns the
render round with the lens.

![v2](v2.png)

The picture itself, as the shutter would make it:

![the picture](pair.jpg)

Open: whether dragging the ghost overlay into line may correct the compass
heading before the shot, with the correction stored beside the measured heading
rather than replacing it.

## Superseded: v1, a chat feature

Drawn first as an ordinary chat feature: an entry on everyone's attach menu, and
a message carrying two pictures with a Compare view. Replaced on 2026-10-15:
the primary use is asking the agent to correct the world's data, and the output
should be one picture with both views side by side.

![v1](v1.png)

## Image credits

The real photo is "Martinitoren, Grote Markt Groningen" by JAHensens, CC BY 4.0,
from Wikimedia Commons. It stands in for a phone picture and was not taken from
the rendered pose. The renders are Cartopolis captures on deck-2 (client
410d46f4) from `53.2183, 6.5679` at 1.7 m, looking north and south. The
front-camera picture and v2's "after" are stand-ins.
