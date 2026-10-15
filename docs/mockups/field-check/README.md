---
topic: field-check
triggers: [field check, rtk, gnss_receiver, camera.rs, photo.rs, front camera, back camera, agent, shot harness, correcting map data]
updated: 2026-10-15
---

# Field check: the real view and Cartopolis's view, side by side

A design mockup, not code. **v2 is the current proposal.**

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
