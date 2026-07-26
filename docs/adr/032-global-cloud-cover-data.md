# ADR-032: Global Cloud Cover from GFS (Real Weather Driving Procedural Clouds)

---
**Metadata:**
- **ID**: ADR-032
- **Status**: Accepted
- **Date**: 2026-07-22
- **Tags**: [rendering, globe, weather, live-data, dependency, clouds]
- **Decision Type**: technology_choice
- **Aligns with**: N/A — extends ADR-025 (Planetary Globe and Space Tier) and the live met.no weather work; realism/liveness of the globe view.
- **Roadmap tier**: P2
- **Doc resolution**: R1
- **Related**: [ADR-025, ADR-005, ADR-024, ADR-026, ADR-027]
- **Supersedes**: []
- **Superseded By**: []
---

## Context

**Problem**: The globe/space-tier cloud layer (`shaders/globe_clouds.wgsl`) is
procedurally generated — latitude banding + domain-warped noise — and its overall
cloudiness was initially wired to the *ground* weather slider (`dev.cloud_cover`,
the met.no reading at the single map anchor). One town's 89% cover overcast the
whole planet, which is wrong: the globe should reflect *global* weather. That
coupling has been removed (the layer now holds a fixed `GLOBE_CLOUD_COVER`
baseline), leaving the globe realistic-looking but not tied to *today's* actual
weather. We want the cloud *positions* on the globe to match reality without
turning the layer into a pasted satellite photo.

**Requirements**:
- Global coverage field (the whole planet), updated on a real cadence.
- Drives the *existing procedural* shader — real weather sets **where** clouds
  are; noise still synthesizes **shape, detail and motion** (no image projection).
- Open data, no per-seat API key, ideally redistributable; Rust-only tooling.
- Cheap and churn-safe: must respect the iGPU device-loss constraint
  (no bursty large uploads/despawns — see CLAUDE.md).
- Graceful degradation to the current procedural bands when data is absent/stale.

## Decision

Drive the globe cloud layer's coverage from **NOAA GFS Total Cloud Cover (TCDC)**,
fetched directly from NOMADS, decoded in pure Rust, and uploaded as one small
equirectangular data texture the shader samples as its coverage field.

**Approach**:
- **Source**: NOAA GFS `TCDC` (entire-atmosphere total cloud cover), via the
  NOMADS GRIB filter (`filter_gfs_0p25.pl`), server-side subset to just TCDC at a
  coarse resolution (0.5°–1.0°). ~hundreds of KB per fetch, not the full field.
- **Cadence**: one background fetch per GFS cycle (~every 6 h), same task shape as
  the existing met.no weather fetch. Between cycles the field is held; the
  procedural warp supplies fine motion.
- **Decode**: `gribberish` (pure-Rust GRIB2) → an `f32` lat/lng grid.
- **Upload**: one `≤256×128` R16F equirectangular texture, refreshed on fetch —
  a single small upload every ~6 h (churn-safe).
- **Shader**: add a `prime_ref` uniform (the lng=0 equatorial direction in the
  local anchor frame) alongside the existing `north_axis`; a `geo_uv()` helper
  maps the fragment's sphere direction → geographic (lng,lat) → `[0,1]²`; sample
  the TCDC texture there as `cover`, replacing the constant `GLOBE_CLOUD_COVER`.
  The hand-authored `zonal_wind()`/latitude-band functions remain as the fallback
  when the texture is absent (real TCDC already contains the ITCZ/storm tracks).
- **Motion**: unchanged from the current shader — a bulk uniform spin plus a
  bounded, latitude-keyed warp flow. Real large-scale motion arrives via the 6-h
  data refresh; the procedural layer only adds bounded fine motion, so it can
  never accumulate shear (see the globe_clouds.wgsl header).
- **Flagged**: behind an env/config gate; off ⇒ current procedural bands.

## Rationale

**Primary Reasoning**:
1. TCDC is a *scalar coverage field*, not imagery — it modulates the procedural
   clouds instead of being displayed, so the clouds stay synthetic (shape, swirl,
   motion) while their *placement* becomes real. Coarse resolution is sufficient
   and desirable (the noise fills sub-grid detail).
2. GFS is **US-government public domain** — no key, no attribution, freely
   redistributable — the lowest-friction option and a natural fit for the
   project's self-hosted, Rust-only posture.
3. One small periodic fetch + one small texture upload is well within the
   device-loss/churn budget; it adds no per-frame streaming.
4. Reuses the established live-data pattern (met.no weather, OVapi transit).

**Alternatives Considered**:

| Alternative | Pros | Cons | Rejected Because |
|------------|------|------|------------------|
| Open-Meteo API (JSON `cloud_cover`) | Clean JSON, no GRIB decode | Free tier non-commercial + rate-limited; CC-BY; solo-maintainer bus factor | Same underlying GFS/ICON data with licensing/rate strings; direct GFS avoids them (self-host Open-Meteo remains a fallback) |
| NASA GIBS satellite imagery | Literally today's clouds | High-res RGB *picture*; daily snapshot, polar-orbit seams; hard to animate | Violates "no picture / keep it procedural"; static |
| ECMWF / DWD ICON open data | Best model quality | GRIB2 too; ECMWF CC-BY, DWD attribution | Good future swap; GFS chosen first for public-domain + simplest access |
| Keep fixed `GLOBE_CLOUD_COVER` | Zero deps | Not real weather | Doesn't meet the "match reality" goal; kept as the fallback path |

## Consequences

**Positive**:
- Globe clouds track real synoptic weather (today's lows, ITCZ, clear highs)
  while remaining fully procedural in appearance and motion.
- No key/attribution burden; redistributable; self-host-compatible ethos.
- Small, infrequent I/O; no new per-frame cost; graceful procedural fallback.

**Negative**:
- **New dependency** (`gribberish`) + GRIB2/cycle-timing logic (which run
  published, forecast-hour selection, occasional missing fields): *Mitigation* —
  isolate in one module behind the fetch task; fall back to procedural bands on
  any decode/fetch failure.
- **Frame conversion correctness** (geographic lng/lat in the local anchor frame
  must land clouds on the right continents): *Mitigation* — derive `prime_ref`
  from the same `TangentFrame` as `north_axis_local()`; verify with `--shot`.
- **Coarse/flattened field** (total cloud only, ~6-h latency): *Monitoring* —
  acceptable for an orbital view; revisit resolution/model if it reads stale.
- **Upstream availability** (NOMADS outages): *Mitigation* — cache last good
  texture; fall back to procedural.

## Related

- [ADR-025](./025-planetary-globe-and-space-tier.md): The globe/space tier this
  cloud layer renders in.
- [ADR-005](./005-sky-lighting-system.md): Sky/lighting the clouds are shaded by.
- [ADR-024](./024-transit-live-boards.md), [ADR-026](./026-live-transit-vehicles.md):
  Prior live-data fetch/ingest patterns this reuses.
- [ADR-027](./027-3dbag-buildings.md): Precedent for self-hosted/open data ingest.
