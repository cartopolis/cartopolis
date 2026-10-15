---
topic: field-check
triggers: [field check, rtk, gnss_receiver, camera.rs, photo.rs, front camera, back camera, agent, shot harness, correcting map data]
updated: 2026-10-15
---

# Field check: the real view and Cartopolis's view, side by side

A design mockup, not code. **v3 is the current proposal**: v2 plus what the
position itself can check.

## v3: what the position knows

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

## v2: a harness for correcting the world

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
