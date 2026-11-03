---
topic: indoor positioning
triggers: [high_precision.rs, precise_sensors.rs, FieldReadings, "crates/vio", "indoor positioning", "magnetic field", magnetometer, "Wi-Fi RTT", WifiRttManager, UWB, "Wi-Fi sensing", CSI]
updated: 2026-11-03
---

# Indoor positioning

RTK gives centimetres under open sky and nothing indoors: the UM980 in a room
on 2026-10-13 held a single fix at ±18–30 m ([rtk-receiver.md](rtk-receiver.md)).
What "precise indoors" can mean here was worked through on 2026-11-03; nothing in
this note beyond the log's field readings is built, and none of the accuracies
below were measured here — they are the published or commercial figures.

## The shape it has to take

Every source fails differently, so the answer is a combination, with the camera
tracking ([camera-tracking.md](camera-tracking.md)) as the spine:

| Source | Accuracy | Fails how |
|---|---|---|
| Camera + motion sensors | ~1 cm relative to where it locked (bench) | drifts with distance (camera tracking in general ~1 %), loses itself on blank walls and in the dark; a restart is a new scale |
| RTK fix carried in through the door | 1–2 cm, once | only outside; the anchor today is one *phone* GNSS fix, which indoors is tens of metres off |
| Magnetic field matching | 1–3 m (IndoorAtlas-class) | needs the building's field mapped once; a magnet near the phone disturbs it; one reading is ambiguous, only a stretch of walk matches |
| Wi-Fi RTT (802.11mc/az) | 1–2 m | only where access points answer it *and* their positions are known; most home routers do not |
| The building's walls | rules places out | 3DBAG and BAG give the shell only; doors are derived ([doors-and-rooms.md](doors-and-rooms.md)) |

The fast relative source carries the hand; the slow absolute ones pull the track
back onto the map now and then. Metre sources do not add up to centimetres:
absolute centimetres everywhere indoors needs UWB anchors per venue. Relative
centimetres is what a hand-sensing prim needs, and the high-precision note's
rule holds indoors too — no correction applied while a hand is playing.

**Magnetic matching fits this project better than it fits most:** the camera
tracking can map a building's field on the first visit (it knows where the phone
is to centimetres while the walk is short), so no separate survey is needed; and
a field map says nothing about people, so keeping it does not reverse "nothing
the camera sees is kept".

## What the log measures (2026-11-03)

`FieldReadings` in `high_precision.rs` writes the field a second at a time — its
strength and its up component, both independent of how the phone is turned — from
Android's calibrated magnetometer at 50 Hz (`platform::precise_sensors`, optional:
a phone without one says "magnet 0/s"). The test to run: the same indoor route
walked three times with high precision on, then the log copied. If the three
strength sequences line up, the OnePlus's field is repeatable enough to match
against; if they do not, the magnetic row above is off the table for this phone.
Not run yet.

## Rejected

- **Wi-Fi sensing** (CMU's DensePose from WiFi, MIT's RF-Pose, UCSB's Wiffract,
  802.11bf): it reconstructs a room and the people in it from channel state
  information (CSI), which Android does not give apps — the demos use Intel 5300
  or patched Atheros cards, or ESP32s, in a fixed setup. It senses what moves
  around a fixed rig, not where a moving phone is; CSI-fingerprint localisation
  reaches ~0.5–1 m and still needs CSI. And it is a technique for seeing people
  through walls. Declined 2026-11-03.
- **Wi-Fi signal-strength fingerprints**: 3–5 m, a survey per building that ages
  whenever a router moves, and Android throttles scans to four per two minutes in
  the foreground. The access-point addresses would be personal data to keep.
  Android's own network location already uses Wi-Fi this way and gives the
  building, not the room (`device-location.md`: the watch listens to every
  provider).
- **Wi-Fi RTT** is not rejected, only unchecked: whether the OnePlus 15 has it
  (`FEATURE_WIFI_RTT`) and whether any access point near the person answers it
  are runtime questions.
