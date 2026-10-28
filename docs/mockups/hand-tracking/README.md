---
topic: hand-tracking
triggers: [held_phone, HeldPhone, high_precision, HighPrecision, PreciseHand, "High precision", hud.rs, locate_overlay, map_osd.rs, face_picker, camera_screen.rs, avatar_selfie, sense_hands, theremin]
updated: 2026-10-28
---

# Your hand, through the phone: mockup

## v2 (2026-10-28): the Mixed selfie

Asked for after v1: the Mixed selfie, where the avatar points the phone at
itself and the real half shows the person, in sync; the front camera also
gives the avatar the person's expression.

- **In a selfie the phone's turn says where it is**: on a sphere round the
  head, looking at the face. The motion sensors give which way it faces, the
  face's size in the front camera how far (eyes about 6.3 cm apart), the GPS or
  RTK receiver where you stand. So a selfie needs only the front camera: the
  photograph, the expression (`face_tracking`) and the distance from one
  stream. The button's "turning only" state becomes **selfie**.
- **Mixed selfie**: real half the front camera's photograph; Cartopolis half
  drawn from the phone's own pose, looking back at the avatar, which holds the
  phone toward the lens, faces it, and wears the person's expression.
- The Cartopolis half in the picture is a stand-in (the test pose from 1.5 m,
  the nearest the capture camera goes), and the real half a silhouette.

![v2](v2.png)

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
