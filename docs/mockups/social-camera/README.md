---
topic: social-camera
triggers: [camera, polaroid, world_polaroid.rs, field check, field_check.rs, selfie, EXIF, community_posts.rs, place_pins.rs, action_ring.rs, chat_overlay.rs, render_chat_world, dock.rs, locate.rs, FOLLOW_EYE_HEIGHT, AVATAR_CAM_DEFAULT]
updated: 2026-10-18
---

# Walking, the camera, and posts: mockup

A design proposal, not code. **v3 is the current proposal** (2026-10-18).

## v3: from where you stand, one button size, the full pose

Decided after v2: walking with the GPS is seen in **first person** from the
phone (about 1.5 m, the phone aiming the view), not over the shoulder; every
button is round and the same size (the dock camera is orange, not bigger); a
**public post keeps the full pose** in its picture like any other. Wherever the camera
is offered, a picture can be uploaded from the phone instead; it keeps its own
EXIF, and a GPS position in it becomes the post's place.

![v3](v3.png)

## v2: the map's own chat, round buttons

After review the same day: a chat's World view draws no chat of its own. It
uses the chat the plain map already has, the fading transcript bottom-left and
the dock's chat button with its composer pill, pointed at that chat, under a
one-row pill (back, who, Chat | World). The controls stay. A community on the
map has no chat button either. Every button is round: the shutter's Post, Send,
Save and Retake, the composer's Camera, From phone and Post.

![v2](v2.png)

## v1

Asked for after a walk with the RTK receiver: the camera was too high, taking a
picture or posting took too many steps, and a chat's World view covered half the
screen and hid the controls.

- **Walking with the GPS**: while locate is lit, Follow uses a walking framing
  (lens 2.0 m up, 3 m behind, 0.4 m right, 4° down) and starts from it every
  time, instead of the chase camera's 4.5 m up, 5.2 m behind and the last saved
  tilt. The GPS height never reaches the camera, so the numbers are the cause.
- **The camera** takes the middle of the dock; the view-mode button moves to the
  right column. Three modes: **In world** (a polaroid of the view), **Mixed** (the
  phone camera and Cartopolis from the same pose, side by side, no polaroid),
  **Real** (the phone camera in the app). Each flips to a **selfie**. It turns with
  the phone, which the real camera does not do today.
- **After the shutter**: Post (first), Send in a chat, Save to phone.
- **Every picture carries its full pose in EXIF** (position, height, direction,
  tilt, roll, lens, orientation, time), so **the field check is removed**; chat
  pictures keep their EXIF through re-encoding.
- **Post here** joins the ground's long-press ring, opening the composer over the
  map with that spot as the place.
- **A community in the World**: picking one shows only its posts, with a chip to
  clear the filter and a small chat button; nothing of its chat is on the map
  until tapped.
- **A chat's World view** keeps the dock, joystick and map buttons, with a
  one-row header and the last lines fading under it, instead of the sheet.

Superseded by v2 for the chat World view and the button shapes; the rest
stands.

![v1](v1.png)

Open: how much of the pose a *public* post's picture keeps (proposed: position
and direction only), and the walking framing's numbers, to be tuned on the phone.

## Image credits

The real photo is "Martinitoren, Grote Markt Groningen" by JAHensens, CC BY 4.0,
from Wikimedia Commons. The world pictures are Cartopolis captures reused from
the field-check and chat-world mockups; the two walking framings are crops of
one capture standing in for the two camera heights, not renders from them.
