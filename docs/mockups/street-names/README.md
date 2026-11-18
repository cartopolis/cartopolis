---
topic: street-names
triggers: [street_labels.rs, systems::street_labels, "street names", GroundLayer::StreetLabel, "first person", "street level"]
updated: 2026-11-18
---

# Street names at street level

## v1 — proposed 2026-11-18

![v1](v1.png)

Asked for 2026-11-18: walking as the avatar, the street names cannot be read.
The name lies flat in the road (`map-labels.md`), and from 1.7 m the road is
seen almost edge-on.

Three ways drawn over a capture from the person's phone in Nieuwstad: **A** a
dark-blue street-name plate on the corner building of each junction, in the
world; **B** the road name tilting upright along the kerb as the camera comes
down; **C** a chip under the search bar naming the street the avatar is in.
Proposed: A + C, with the road name fading out at eye height. **Chosen
2026-11-18: B**, built in `systems::street_labels` (`map-labels.md`).
