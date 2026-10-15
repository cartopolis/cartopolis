---
topic: field-check
triggers: [field check, rtk, gnss_receiver, camera.rs, photo.rs, front camera, back camera, pictures in chats, agent, shot harness]
updated: 2026-10-15
---

# Field check: one tap, two pictures

A design mockup, not code. On a walk with the RTK receiver, one tap in a chat
takes the phone's photo and Cartopolis's render from the same pose (position,
heading, tilt, lens) and sends them as one message, so a person or the agent
can compare the world with the place. The front/rear switch turns the render
round with the lens.

## v1

![v1](v1.png)

Open: whether the ghost overlay may be dragged to correct the compass heading
before the shot, with the correction stored beside the measured heading
rather than replacing it.

Image credits: the real photo is "Martinitoren, Grote Markt Groningen" by
JAHensens, CC BY 4.0, from Wikimedia Commons. It stands in for a phone picture
and was not taken from the rendered pose. The renders are Cartopolis captures
on deck-2 from `53.2183, 6.5679` at 1.7 m, looking north and south. The
front-camera picture is a drawn stand-in.
