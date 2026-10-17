---
topic: location
triggers: [location.rs, platform::location, platform/android.rs, locate.rs, driver.rs, geo_nav.rs, LocationWatch, LocateTracking, GpsFix, stale_accuracy_m, FIRST_FIX_MAX_AGE, STALE_DRIFT_MS, start_location, run_location, GPS_TELEPORT_M, fix_is_far, place_avatar, FixGlide, SHORT_JUMP_M, "Waiting for GPS", "device fix", "GPS fix", geolocation, LocationManager, "fused provider"]
updated: 2026-10-17
---

# Where the device is: one shared watch, and a far fix is a teleport

`platform::location::LocationWatch` is the one seam for "where does the *machine*
think it is": `navigator.geolocation` on the web, `LocationManager` over JNI on
Android, and nothing on a desktop — `available()` is false there unless an external
receiver is switched on ([rtk-receiver.md](rtk-receiver.md)), whose fresh fix is
asked first on every platform. A desktop with no GNSS answers *unknown*, never a
guess, and driver mode then narrates the avatar instead, which is also how it is
testable without a dongle. The watch is a subscription: dropping it ends it, which
turns the browser's location indicator off and lets a phone's radio go cold.

## One subscription, refcounted

Two resources own a receiver: `locate::LocateTracking` while the ⌖ button is lit,
and `driver::DriverState` while driving. Driver mode makes locate stand down — two
systems writing one position from two subscriptions resolves to whichever ran last
— so pressing Start used to drop a receiver that had been warm for minutes and open
a new one: re-check the permission, re-register with `LocationManager`, wait out
another cold GNSS acquisition. That was most of "Waiting for GPS…", and it was
self-inflicted.

`LocationWatch::start` now joins the running provider when there is one. `SHARED`
holds only a `Weak`, so it is a rendezvous point and never an owner: the radio still
goes down when the last handle drops. The handover works because each consumer
starts its watch *before* the one it displaces stands down — one frame of overlap.

## Android: every provider, and an old fix shown as a poor one

**The watch registers on every enabled provider**, not on `fused` alone
(`PROVIDERS` = fused, gps, network, intersected with what the platform lists as
enabled). The fused provider decides for itself when to spin GNSS up, and asked for
a 1 Hz stream it will answer from the network for the first half minute; naming
`gps` starts the acquisition at the moment the user pressed Start. `fused` here is
the platform's `FUSED_PROVIDER` (public only from API 31, hence the intersection),
not Play Services' client — that arrives as an AAR, i.e. Gradle. Each registration
takes its own request code, so two overlapping watches cannot cancel each other's
`PendingIntent`.

**A cached first fix is published with its accuracy widened by its age**
(`stale_accuracy_m`: + age × `STALE_DRIFT_MS`, 13 m/s, an urban drive) rather than
refused. `FIRST_FIX_MAX_AGE` was one minute, and a phone holding a serviceable
two-minute-old fused fix sat on "Waiting for GPS…" for most of a minute after Start
— the loudest difference from the reference apps. It is ten minutes now, and that
is safe only because staleness is no longer hidden: an old position shown as a
*poor* one beats an arrow that has not appeared, and every consumer — the GPS chip,
the reroute's uncertainty guard — already reads `accuracy_m`. Publishing an old fix
at its original accuracy is the one thing not allowed; it is confident about where
you were. What a device has and has not confirmed is [android-build.md](android-build.md).

## A far fix is a teleport, not a position write

`driver::place_avatar` writes flat-frame metres measured from the **map anchor**,
and nothing in that path moves the anchor. The everyday case — plan a route where
you were browsing, tap Drive, the phone reports where you actually are — dropped
the avatar and the chase camera up to hundreds of kilometres from the origin of a
frame the tiles, buildings, terrain and route line were all still built around.
Every streamer rebuilt at once, on a phone GPU: the mass-churn device loss
([upload-bursts.md](upload-bursts.md)), and then a picture frozen for good.

So past `GPS_TELEPORT_M` (2 km) the fix goes through `Teleport`; under it, the cheap
write, glided over the following frames (`FixGlide`). The gap is measured from the
**avatar**, not from the previous fix, because what must not be left behind is the
world the avatar stands in. 2 km is past anything a vehicle covers between two 1 Hz
fixes (7,200 km/h) and well inside the flat frame's honest range; it is a genuine
distance, not a duration in disguise ([flight-at-speed.md](flight-at-speed.md)).
`locate` uses the same `fix_is_far`, and both teleports are silent.

**The rule generalises: a *move* is a write, a *jump* is a teleport.** `Teleport`
(`geo_nav::apply_teleport`) is the one operation that means "the world is somewhere
else now". It decides about the frame — within `SHORT_JUMP_M` (3 km) of the anchor
it moves the camera and avatar inside it, past that it re-anchors and retires the
old tiles through the bounded despawn queue — and it brings the camera along.
Anything that can relocate the avatar or the camera by an arbitrary distance goes
through it rather than writing a transform.

Online, even the near write is not enough on its own: the sim owns the avatar's
translation and every snapshot puts it back, so `place_avatar` also sends the
wire's `Teleport` — once per *new* fix (`fix_is_new`), because
`LocationWatch::latest` is a level, not an event. Sent every frame it would be sixty
a second on a wire that refuses intents closer than 50 ms apart; one per report is
what lets the sim leave teleports unrated.
