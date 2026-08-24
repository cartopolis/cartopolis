---
topic: allocation-budgets
triggers: [perf.rs, perf_budgets.rs, "budget test", "allocation", "benchmark", "perf test", "regression test", global_allocator, "how do we measure", tile-raster-cost, retained_bytes, stopwatch]
updated: 2026-08-24
---

# Performance is a test here, and it is a count rather than a clock

`crates/geo/src/perf.rs` (the instrument) and `perf_budgets.rs` (the budgets)
assert what one tile of the pipeline is allowed to cost. They are ordinary
non-ignored tests: they run in CI and in the agent container, both of which have
no GPU and cannot render a frame.

## Why not a timer

Every previous attempt was a timer and every one was reverted —
`docs/notes/tile-raster-cost.md` ends with "The benchmarks were reverted, not
committed — nothing in the tree measures this." That is not a discipline failure,
it is structural: CI runs up to three jobs on a shared 8-core VM at two cargo
jobs apiece, so a wall-clock threshold flakes, and a threshold loose enough not to flake is
loose enough to catch nothing.

Allocation counts have neither problem. **Measured bit-identical over three
consecutive runs** (2026-08-12) — they are a property of the input and the code,
not of the machine, so the same number appears on a Fairphone 4 and a 22-core
workstation.

## What it catches, and what it is blind to

Every regression this tree has recorded was allocation-shaped: a `Vec` per
triangle, a `String` cloned per tag key *and* value per feature, an untyped
`serde_json::Value` walk whose **drop** was the single largest profile entry at
14.2% (`docs/notes/wasm-performance.md`).

It is blind to scan conversion. `render_mvt` is dominated by tiny-skia walking
spans inside buffers it already owns, which barely allocates — a rasteriser twice
as slow per pixel would not move these numbers. `perf::paths` covers that blind
spot by counting the geometry *handed to* the rasteriser, which is what its cost
scales with.

## The headroom is ~2%, and 10% was tested and rejected

Not a preference — measured. With a 10% ceiling, a deliberately injected
regression (one surviving `Vec` allocation per filled path) cost 8,688
allocations, **7.3%, and passed**. A budget loose enough never to complain never
catches anything. At ~2% the same probe fails with the count and the ceiling in
the message.

The probe also has to survive optimisation to be a fair test: the first version
was an unused `Vec::with_capacity` and LLVM deleted it, so the budget was right to
stay green. `std::hint::black_box` plus a `push` is what makes it real.

## Retained bytes joined the instrument (2026-08-24)

`Cost` gained `retained_bytes` — live heap when the measured region ends, i.e.
what its return value *keeps*, as against the traffic it churned. Same
machine-independence as the counts (verified bit-identical across runs), so it
budgets: a parsed Groningen z14 retains **3,299 KiB**, decoding every layer a
further **12,500 KiB** — the per-entry weight of the client's `parse_shared`
memo, and the number `tile-raster-cost.md` spent months calling unmeasured.
`a_parsed_and_decoded_tile_retains_a_bounded_footprint` pins both.

The clock half lives beside this file too now, as a hatch rather than a gate:
`perf::stopwatch` plus `print_tile_pipeline_time` (run it `--release`). Nothing
about the doctrine above changed — wall-clock still cannot be a CI threshold;
what changed is that the *instrument* is committed instead of being rebuilt and
reverted per measuring session.

## Numbers as of 2026-08-12

| Stage | allocs | traffic | peak |
|---|---|---|---|
| `parse`, Groningen z14 (495 KiB) | 54,364 | 6.5 MiB | 4.0 MiB |
| decode all layers, same tile | 119,785 | 24 MiB | 12.5 MiB |
| re-read a decoded layer | **0** | 0 | 0 |
| `render_mvt_sized` 256², whole | 118,905 | 46 MiB | 0.6 MiB |
| `render_mvt_sized` 512², whole | 118,906 | 47 MiB | 1.4 MiB |
| `render_mvt_sized` 256², 1/64 sub | 2,364 | 3 MiB | 0.4 MiB |
| `parse`, z10 lowzoom (68 KiB) | 2,360 | 0.9 MiB | 0.5 MiB |
| **decode all layers, z10 lowzoom** | 11,338 | **226 MiB** | 3.2 MiB |

Three of those rows are findings in their own right.

**The 256² and 512² renders agree to within one allocation.** This cost is per
*feature*, not per pixel — path construction, not scan conversion. So the phone
tier's `tile_px = 256` buys a quarter of the scan conversion and **none** of this.
That is the measurement behind "per-feature overhead is a larger share of
`render_mvt` on a phone than on a desktop", and it means per-feature work is where
the phone's remaining tile cost lives.

**A 68 KiB tile moves 226 MiB of allocation traffic** decoding its layers — ~3,300×
its own size, and 9× the traffic of the 495 KiB tile that has 25× more features.
That inversion is the signature of `mvt-reader`'s quadratic `Vec::with_capacity`,
which `docs/notes/mvt-capacity-guard.md` documents as an open abort risk and which
nothing had ever measured. It is pinned as a **ceiling on a known defect**, not an
endorsement; when the vendoring that note calls for lands, drop the ceiling hard,
or the fix can silently regress.

**Re-reading a decoded layer is exactly 0.** `TileData`'s per-layer `OnceCell`
works, so the twice-listed `buildings` and `streets` entries in `PAINT_ORDER` cost
one decode. The *paths* are still built twice — that is a separate, open cost.

## Changing a budget

Same contract as `PROTOCOL_HISTORY`: **restate the number in a reviewed commit**
and say in the body what bought it. A budget test going red is a claim that the
cost changed, which is sometimes exactly what the commit meant. What must not
happen is the number drifting up unremarked, which is what "no benchmark at all"
has meant here for months.

```bash
cargo test -p cartopolis_geo                                    # the gates
cargo test -p cartopolis_geo print_tile_pipeline_cost -- --ignored --nocapture
```

## What this does not replace

Frame time on the device. These budgets cover the CPU side of the tile pipeline
only; fill rate, bandwidth and draw counts need the phone, and the counts that
*are* checkable on a workstation today go through `--dump-state`:

```bash
CARTO_QUALITY=phone cargo run --release -p cartopolis -- \
  --shot /tmp/p.png --dump-state /tmp/p.json --size 1080x2340 \
  --at 53.2194,6.5665,420 --look=-22,35 --settle 2
```
