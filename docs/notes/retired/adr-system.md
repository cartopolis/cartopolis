---
topic: retired/adr-system
triggers: ["ADR", "architecture decision record", "decision log", "docs/adr"]
updated: 2026-07-26
---

# Retired: numbered ADRs

38 numbered ADRs plus an abstract layer (vision, principles, roadmap tiers,
capability map, R0–R3 resolution grading), 2025-03 → 2026-07. Replaced by
topic notes plus commit bodies.

## What it got right, and must not be lost

The 22 written alongside real work carried things nothing else holds:

- **Measurements** — f32 ULP at planet scale (~1 m jitter at 10⁷ m, 2.4×10⁻⁷ m
  under floating origin); road-graph connectivity 58% → 92% after noding.
- **Rejected dependencies, with reasons** — `bevy_egui` / `bevy_transform_gizmo`
  lose to version lag against our Bevy bumps; `prost` is overkill for six
  protobuf field reads.
- **External-world constraints** — a browser cannot set `User-Agent`, hold a
  secret, or fetch `http://` from an HTTPS page; OSM discourages proxying its
  tile CDN; 3DBAG is CC BY 4.0.

That content moved into notes. The form did not.

## Why retired

- **16 of 38 were written in two days** (2025-03-18 and 2025-03-21), before the
  code existed. Perfect template compliance, near-zero content — a template
  hands you a shape to fill when you have nothing to say.
- **Numbering sorts by write date**, the worst retrieval axis. Answering "how do
  coordinates work" meant reading 006 (superseded) → 019 → 020 (carrying two
  self-correcting banners) → 021 (retired) → 025 → 029: six files and three
  retractions for one current answer.
- **The hand-maintained aggregate layer rotted.** `roadmap.md` cited no ADR past
  016 while 22 more existed; the index and the files disagreed on status.
- **Numbering makes deletion impossible** (the sequence would gap) and addition
  free (the next number is always available).

## Why it existed at all

The ADRs were compensating for a gap in commit hygiene. Of 113 commits, 71 have
no body, and 32 of those touch 10+ files — the big feature landings explained
nothing. The noding finding survived *only* because someone wrote ADR-037; it
landed in `c21d9ee feat: routing, screenshots, vector tiles`, which has an empty
body, and appears in no commit message anywhere. The `commit` skill now requires
a body for `feat`/`fix`/`perf` and for anything touching 5+ files, which is the
real fix.

## Do not re-propose

Numbered decision records with mandatory sections. Keep the writing habit — it
is genuinely good here — and drop the form.
