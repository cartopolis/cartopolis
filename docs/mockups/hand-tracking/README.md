---
topic: hand-tracking
triggers: [held_phone, HeldPhone, high_precision, HighPrecision, PreciseHand, "High precision", hud.rs, locate_overlay, map_osd.rs, face_picker, camera_screen.rs, avatar_selfie, sense_hands, theremin]
updated: 2026-10-28
---

# Your hand, through the phone: mockup

## v1 (2026-10-28): a hand button, not a locate option

Asked for after the avatar learned to hold the tracked phone (2026-10-27,
`systems::held_phone`): the "High precision (camera)" checkbox in the locate
button's long-press menu is really about the hand — the phone you hold becomes
the phone in your avatar's hand — so it should be designed as that.

- **A hand button** above the face button in the touch column (Follow). Tap on
  or off; a word under it says what it is doing (hold still, move it a little,
  following, turning only, lost). Long press: left or right hand, start again,
  the mirror.
- **A round mirror** shows the avatar from the front while it is on, reusing
  the avatar camera that calls' "Show your avatar instead" has.
- **Locate keeps only position**: the checkbox leaves its menu.
- **First tap**: one card saying what it uses (back camera and motion sensors,
  nothing kept) and who feels it (hand-sensing prims; later, people nearby),
  then the camera permission.
- **Walking up to a hand-sensing prim** offers it in the prompt row ("Play
  Theremin with your hand"); nothing is reported to the prim before that tap.
- **The camera's In world selfie** turns the hand on while it is up. The front
  camera is the face's (`face_tracking`), so there the hand turns with the
  phone but stays at arm's length ("turning only") unless the phone can run
  both cameras, which is to be measured per phone. Mixed holds the back camera
  for its photograph, so the same applies there.
- Further uses listed: waving in a call, showing others (needs the arm on the
  wire), placing prims by hand.

Open: whether the hand stays on across launches (proposed: no); whether the
mirror is on by default; the body behind the hand (step 2) is not drawn.

![v1](v1.png)

The avatar pictures are Cartopolis captures of `held_phone`'s fixed test poses
(`CARTO_HELD_PHONE`), taken on mac-2 on 2026-10-27; the theremin in section 3
is drawn in.
