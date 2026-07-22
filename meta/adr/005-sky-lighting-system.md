# ADR-005: Sky and Lighting System

---
**Metadata:**
- **ID**: ADR-005
- **Status**: Accepted
- **Date**: 2025-03-18
- **Tags**: [rendering, lighting, sky, atmosphere]
- **Decision Type**: technology_choice
- **Related**: [ADR-001, ADR-003]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The scene needs a sky and proper lighting. A solid void background and basic directional light do not provide sufficient visual quality or a sense of place.

**Requirements**:
- Procedural sky visible from the camera
- Directional light (sun) aligned with sky appearance
- Physically plausible illuminance, adapting across a day/night cycle
- Reads correctly from ground level to orbit (planetary scale)

## Decision

Use **Bevy 0.19's built-in `bevy_pbr` atmosphere** (planetary-scale physical scattering) on the camera, with a `DirectionalLight` sun whose colour/illuminance and exposure follow a day/night cycle.

**Approach**:
- Camera carries `AtmosphereSettings` + `AtmosphereEnvironmentMapLight` + a physical `Exposure`; the atmosphere lives on a **planet-centred entity** ~6,360 km below local ground (surface at `y=0`), moved each frame to Earth's true centre in render coords so it works at any altitude.
- Sun is a `DirectionalLight` at `RAW_SUNLIGHT`; `apply_day_night_cycle` adapts exposure, ambient, sun colour/extinction, fog, stars, and moon by time of day.
- Sun disk is Bevy's procedural `SunDisk` (drawn by the sky pass — no geometry, works raymarched from orbit); glare is `Bloom` (`bevy_post_process` feature).
- Space tier switches `AtmosphereMode` (LookupTexture ↔ Raymarched) and its own lighting model (see ADR-025).

**Migration note**: originally `bevy_atmosphere` (Nishita); that crate was **dropped in the Bevy 0.19 upgrade** in favour of the engine's built-in atmosphere.

## Rationale

**Primary Reasoning**:
1. **Built-in, no external crate** — `bevy_atmosphere` had no Bevy 0.19 release when we upgraded; the engine's own atmosphere is on 0.19 and is planetary-scale (essential for the globe/space tier).
2. **Physical scattering** feeds the sun light directly, so sky and surface lighting stay consistent across the day/night cycle.
3. **Works from orbit** — procedural sun disk + raymarched mode render with no ground geometry.

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| `bevy_atmosphere` (original) | Was working on 0.16 | No Bevy 0.19 release at upgrade time; not planetary-scale | Dropped in the 0.19 upgrade |
| Skybox / gradient sky | Simple, cheap | Static; no day/night or altitude response | Fails the planetary + cycle requirements |
| Custom scattering shader | Full control | High effort | Built-in atmosphere is sufficient |

## Consequences

**Positive**:
- One physical model spans ground to orbit; sky and lighting stay consistent through the day/night cycle.
- No external atmosphere dependency to keep in sync with Bevy.

**Negative**:
- Atmosphere and sun light are coupled — tuning one moves the other (see CLAUDE.md “Known issues” for the extinction/exposure interactions).
- Planet-centred entity placement must track the floating origin each frame (ADR-019).

## Related

- [ADR-001](./001-bevy-game-engine.md): Bevy provides rendering and lighting
- [ADR-019](./019-planet-scale-floating-origin.md): Floating origin the planet-centred atmosphere entity tracks
- [ADR-025](./025-planetary-globe-and-space-tier.md): Space-tier atmosphere modes and lighting
- Systems: `main.rs` (`setup_sky`, `sun_light`, `apply_day_night_cycle`), `free_camera.rs` (`setup_camera`)
