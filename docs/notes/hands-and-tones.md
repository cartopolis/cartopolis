---
topic: hands near a prim, and tones
triggers: [sense_hands, on_hand, set_tone, stop_tone, HandPose, PrimTone, ToneSpec, SoundingTone, SensesHands, hand_range, prim_hand, prim_tone, "tone.rs", theremin, "high precision", PreciseHand, CARTO_HAND_FROM_CAMERA, held_phone, HeldPhone, HumanArms, CARTO_HELD_PHONE, "reach.rs", phone_grip, PhoneGrip, hand_button, hand_card, HandPrefs, HandPrimNear, phone_hold, SELFIE_REACH_M]
updated: 2026-10-27
---

# Hands near a prim, and tones

Two generic script features (protocol 36), added so a theremin could be built
**as a script** rather than as an engine feature:

- `sense_hands(range_m)` / `on_hand(avatar_id, name, x, y, z, precise)` — the
  script asks for hands within `range_m` (at most 5 m) of its prim, and every
  client in range reports its user's hand: metres from the prim's centre in the
  prim's own frame (turned with it, not scaled), up to 20 Hz per prim.
- `set_tone{pitch, gain?, voice?, glide?, vibrato?, range?}` / `stop_tone()` —
  a synthesised tone (sine, organ, saw, square) every client in range plays.

The engine maps nothing to pitch or volume and decides nothing about who
plays. `crates/simulator/content/theremin.lua` (the `theremin` content pack)
takes the first hand to arrive as its player, ignores everybody else's until
the player's stops arriving for a second, turns the hand's distance to a rod
and to a loop into `set_tone` calls, and shows who is playing in its name.

## Decisions, and what lost

**Walk up and play, not take hold (2026-10-07).** A version where you pressed
E to *hold* the prim (`set_holdable`, `on_hold`, a "holding" tag) was built
and dropped: the engine was deciding who plays, which a script can do in ten
lines, and a real theremin is played by bringing a hand near it.

**An `instrument{}` call that declared antennas and mappings was built before
that and rejected.** It was faster — the player's client evaluated the mapping
and synthesised the tone with no network in the loop — but it put an
instrument into the engine. The price of the generic version is latency: the
hand goes to the simulator, the script runs on its next tick (20 Hz), and the
tone comes back — on the order of 100–150 ms from hand to ear, against ~20 ms
for an instrument that feels immediate. `glide` hides the steps between
updates. **Not measured on a device yet.** If it is too slow, the way back is
to run the same script handler on the player's client, not to re-introduce a
mapping in the engine.

**High precision is the consent.** A hand report is a precise position report,
and nobody agrees to send one by walking past a prim, so a client reports
hands only while its user has the high-precision tracking mode on (camera
tracking against the 3DBAG building models, switched by the hand button in the touch controls since 2026-10-28 —
where the phone's pose *is* the view, so the camera position is the hand).
Until that mode exists `prim_hand::PreciseHand::active` stays false and
nothing is reported, except under `CARTO_HAND_FROM_CAMERA=1`, a developer
switch that lets a desktop camera stand in and reports it `precise = false`.
The 5 m ceiling on `sense_hands` is the same argument: past arm's reach and a
few steps a hand is a location.

**The server checks the range; the script decides the rest.** A hand outside
the prim's `hand_range` (plus 25 cm for a client a tick behind) is dropped, and
the per-connection bucket allows 100 reports a second.

**A tone is its own message (`PrimTone`), not a `PrimUpsert`.** A script
following a hand sets it 20 times a second; a whole prim per change would make
every client rebuild the prim's mesh. On the client the tone and the hand
range are components of their own (`SoundingTone`, `SensesHands`) for the same
reason — writing `Prim` triggers the rebuild. `PrimDto::tone` still carries it
for a client that arrives mid-tone. The tick loop sends one `PrimTone` per
prim per tick, the last one set.

**Hands coalesce in the script queue, per person.** A hand is a level: a
newer one replaces the same person's still waiting, so a slow handler plays
where the hand is now, not a backlog — and one busy hand cannot hide another.

**Smoothing is the synthesiser's, per sample.** The main thread writes target
pitch and gain into atomics; the audio thread glides toward them
(log-frequency at the script's `glide`, at least 4 ms; 15 ms on gain).
Stepping a sink volume per frame zippers a pure tone.

Hand range and tone are memory-only, like `sound_loop`: a restart runs
`on_start` again.

## The avatar holds the phone (2026-10-27)

While the high-precision mode holds a pose, the local avatar holds a phone and
its arm follows the real hand (`systems::held_phone`). The clip still plays the
body; after the animation and before propagation the holding arm is solved
(`cartopolis_human::reach`, a closed-form two-bone reach toward the wrist,
elbow down and out), the hand is turned to the phone's own turn
(`PreciseHand::hand_rotation`, the tracker's orientation turned by the anchor's
yaw into east-north-up), and the fingers close round it (`hands::phone_grip`,
the palm side read off which way the middle finger curls). A 7.5 × 16 cm box is
drawn in the hand.

- **Relative, not on the map.** The avatar stands on the GNSS fix and the
  tracked hand on its own anchor, metres apart, so the hand is put at a rest
  point in front of the chest at the lock and moved from there by the
  tracker's motion, in the body's frame. Carried past 95 % of the arm's length
  it drags the rest point along. So the hand drawn and the hand a script
  senses agree in motion, not in absolute place; putting the body behind the
  hand is the next step.
- **This client only.** Nothing new goes on the wire: others see the clip's arm.
- **Right hand, fixed.** `HeldPhone::side` has no setting yet.
- `CARTO_HELD_PHONE=look|selfie` holds one in a fixed pose for a capture
  (`--shot-mode avatar`). Captured on mac-2 on 2026-10-27; not run on a phone.

## The hand button, and the selfie hold (2026-10-28)

Built from `docs/mockups/hand-tracking/` v2. The mode is the **hand button**
above the face button (`map_osd::hand_button`): tap on or off, a word beside
it from `HighPrecision::hand_word`, a long press for the hand (`UserData::hand`)
and "Start again". The first tap shows a card (`map_osd::hand_card`); "Use my
hand" is the consent and is remembered (`HandPrefs::agreed`). A prim that
senses hands offers it in the interaction prompt (`prim_hand::HandPrimNear`).

**Only one camera can be open**, so while the camera screen has one (Mixed,
Real, or the In world selfie, whose front camera reads the face) the tracker
stands aside, still on (`CameraScreen::phone_hold`), and the phone is placed
by how it is turned: screen toward the eyes, `SELFIE_REACH_M` (0.5 m) away in
a selfie. In a selfie the model turns to the camera, and the Mixed selfie's
world camera sits at that same point, so the avatar holds the phone toward
the lens. Nothing is reported to prims from a selfie hold.

Not done: the distance from the face's size in the front camera, and the
face in the Mixed selfie — both need `face_tracking` to read pictures from the
camera screen's camera rather than opening its own (asked of the session that
owns it, 2026-10-28); the round mirror; the chip naming what follows you.
None of it has run on a phone.
