---
topic: camera-drag-pacing
triggers: [free_camera.rs, pointer_trail.rs, camera_pacing.rs, os_drag.swift, DragReplay, CursorMoved, "drag", "judder", "stutter", "camera movement", no_drag_replay, firefox_probe, cdp_probe]
updated: 2026-10-21
---

# A drag moves the camera as evenly as the pointer events arrive

Every camera mode moves the camera by the cursor's movement since the last
frame, from that frame's `CursorMoved` events. `CursorMoved` has no time on it,
so the camera can only be as even as the delivery: a frame the browser handed no
pointer event does not move, and the next moves twice as far. That is judder at
a steady frame rate, and no frame-rate figure shows it.

**Chrome hides it, Firefox does not.** Chrome delivers pointer moves aligned with
animation frames, one per frame: over a steady scripted drag on mac-2
(2026-10-21) 1–3% of its frames got none. Firefox does not align them: 27% of
frames got none and 7% two, measured both at the page (a `pointermove` counter
per animation frame, `firefox_probe --drag`) and in the client.

## What is measured, and how

- **`camera pacing`** (`systems::camera_pacing`), every 5 s while a button is
  held: frame intervals (p50, p95, worst), pointer events per frame (`ev0` …
  `ev3plus`), the spread of cursor pixels per millisecond (`speed_cv`, and
  `off_pace`: frames under half or over twice the median), and the same two for
  what the camera actually moved (`applied_cv`, `applied_off_pace`).
- **Scripted drags**: `firefox_probe --drag <s>` and `cdp_probe --drive-script
  drag`, a steady 3 s drag back and forth.
- **A scripted drag cannot judge pacing.** BiDi and CDP dispatch input on the
  browser's own timers (a BiDi action costs ~15 ms of dispatch, so 375 separate
  moves took 11 s), so their events are as uneven in *time* as in delivery.
  `firefox_probe --drag-os` posts operating-system mouse events at an even rate
  instead (`tools/os_drag.swift`), but **from an ssh session macOS drops them**:
  the page saw zero pointer moves. Creating a `.hidSystemState` event source
  there deadlocks inside SkyLight; a nil source posts but nothing arrives. It
  needs Accessibility permission granted at the Mac's own screen. Until then the
  fix below is verified by its unit test and judged by a person with a mouse.

## The fix: replay the trail by the browser's timestamps (web)

`platform::pointer_trail` records every pointer event's position with its
`timeStamp` (the browser's own creation time, on `performance.now()`'s clock),
expanding `getCoalescedEvents()` so a merged event loses nothing. Earth mode's
`DragReplay` (`free_camera.rs`) replays that path a short delay behind the
present and moves the camera by the path between last frame's moment and this
one's, so the camera moves exactly as evenly as the hand.

The delay is the whole trade. It is how stale the newest sample tends to be when
a frame looks, plus half a sample gap, between 8 and 50 ms: it rises at once when
samples come late and eases back slowly, and is measured only on frames that got
samples (a hand held still sends none, and that is not lateness). A first
version that sized it from the sample gap alone stalled whenever a sample came
late, and its own test caught it (spread 0.24 against a limit of 0.1). The cost:
the camera trails the pointer by that delay, and the last delay's worth of a drag
is dropped at release.

`a_steady_drag_delivered_unevenly_replays_evenly` is the specification: a
125 Hz mouse at a constant speed, each sample reaching the page 0–25 ms late,
frames at 60 Hz. The events as they arrived have a per-frame spread over 0.3; the
replay's is under 0.1 at the hand's speed.

`?no_drag_replay` (`CARTO_NO_DRAG_REPLAY`) turns it off, to compare by hand.
Natively there is no trail (winit's events carry no time) and the events are
used as they come. Only Earth mode replays; Fly and Follow still read raw
events.

## Not fixed here

Firefox's frame intervals are uneven too while a drag streams new ground: p95
28–85 ms against Chrome's ~23 ms on the same node. That is the other half of
what a person calls stutter, and it is `wasm-performance.md`'s business.
