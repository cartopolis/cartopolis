---
topic: avatar-standing-height
triggers: [GROUND_HEIGHT, AVATAR_GROUND_Y, GROUND_Y, standing height, avatar floats, origin at feet, core/src/motion.rs, systems/nav/geo_nav.rs, driver::placement, FOLLOW_EYE_HEIGHT, FIRST_PERSON_EYE_HEIGHT]
updated: 2026-10-18
---

# An avatar stands on two numbers for the ground

The avatar bodies stand **origin-at-feet**: the MakeHuman pack grounds every clip
on its lowest ankle (`makehuman-avatars.md`), as the Quaternius characters before
it were authored. The fox before those had its origin at its centre, and its
offsets survived it: 0.45 (ground + half a fox) in three places and `+1.0` on the
sim's spawn floated the humanoids. The teleport one was the hard one to see,
because it re-plants the avatar on every geo-nav and at startup.

What the code holds today is **two** values, not one:

- **0.0** — `motion::GROUND_Y`. The sim spawns at `Vec3::ZERO` and clamps to it
  (`SimWorld::step`), the online prediction falls to it, and since a fix
  `driver::placement` writes it for a GPS step.
- **0.05** — `avatar::GROUND_HEIGHT`, the old region tile's top surface. The
  offline gravity clamp stands on it; `collision`'s snap-out backstop reuses the
  `pub` constant rather than a copy; `geo_nav::AVATAR_GROUND_Y` and
  `shot_harness`'s `GROUND_Y` are literal copies of it.

The disagreement has already cost one bug: GPS placement wrote 0.05 while the
sim and the prediction stood at zero, so every fix put the avatar 5 cm up and the
prediction reported a landing — a 6 m/s touchdown several times a second at
120 Hz, thud and camera dip included, for as long as the phone moved
(`driver::placement`'s doc). `geo_nav::place_avatar` still sends 0.05 to the sim
with an online teleport (`NetMessage::Teleport`), so each online jump arrives
5 cm up and lands once.

The follow camera pivots and aims at `FOLLOW_EYE_HEIGHT` (1.5 m, the chest of a
1.8 m figure) above the feet; first person uses `FIRST_PERSON_EYE_HEIGHT` (1.62 m,
the eyes), and the third-person aim rises a further `FRAMING_RISE` of the boom
(`avatar-game-feel.md`). The designer lowers the aim while its phone sheet is up
(`CameraState::aim_height`, `makehuman-avatars.md`).
