---
topic: mvt-capacity-guard
triggers: [guard_tile_body, MAX_TILE_BYTES, geometry_capacity, mvt-reader, "refusing the tile", "tile decode failed", "with_capacity", low-zoom, z10, tile bomb, vector_tiles/tile.rs, lowzoom_10_526_334.mvt, lowzoom_10_534_336.mvt]
updated: 2026-08-11
---

# You cannot tell a tile bomb from a coastline by counting reservations

A guard that walked the protobuf, computed what `mvt-reader` would
`Vec::with_capacity`, and refused a body asking for too much shipped on
2026-08-11 and was reverted the same day. It refused real tiles. This note is
here so nobody rebuilds it from the same reasoning.

## The attack is real

`mvt-reader` 2.4.0's `parse_geometry` starts a fresh
`Vec::with_capacity(geometry_data.len())` on every `MoveTo` and every
`ClosePath` — the length of the *whole* feature's command stream, not of the
part still to come. `[9, 0, 0]` is three bytes and produces one such vector, so
a body of `n` bytes of it repeated costs roughly `8n²/3`. Measured here with
`VmPeak` around a full `get_features` on every layer:

| body | walker's estimate | actual virtual cost |
|------|-------------------|---------------------|
| bomb, 12 KB | 48 M slots | **+320 MiB** |
| bomb, 30 KB | 300 M | **+1.9 GiB** |
| bomb, 60 KB | 1 200 M | **+6.9 GiB** |

Quadratic, and `panic = "abort"` with no `catch_unwind` makes a failed
allocation a SIGABRT with a network-controlled trigger.

## The estimate does not predict the cost

| body | estimate | worst single feature | slots/byte | actual virtual cost |
|------|----------|---------------------|-----------|---------------------|
| z10 10/526/334, 69 KB | 57 M | 55 M | 832 | none measurable |
| z10 10/527/336, 79 KB | 92 M | 74 M | 1 169 | none measurable |
| z10 10/534/336, 157 KB | **545 M** | **421 M** | **3 463** | **none measurable** |
| z14 Groningen, 653 KB | 12 M | 8 M | 18 | none measurable |
| bomb, 12 KB | 48 M | 48 M | 3 992 | +320 MiB |

A real z10 tile asks for **more** than a 30 KB bomb and costs nothing.
Generalised low-zoom geometry is one enormous multi-part coastline whose parts
do not stay live at once; the bomb's do.

Three metrics were tried and all three overlap:

- **total slots** — 545 M real against 48 M for the tamest bomb
- **worst single feature** — 421 M real against 48 M
- **slots per body byte** — 3 463 real against 3 992

There is no threshold between them, because the walker models an allocation
pattern `mvt-reader` does not have.

## What is there now

Only `MAX_TILE_BYTES` (8 MiB), which bounds the copy and the prost decode and
nothing else. The tree is where it was before the guard: exposed to a tile
server that serves a deliberate bomb. Worth stating plainly rather than
papering over — and note the realistic threat is small, since the source is
first-party over HTTPS and anyone who controls it can serve garbage anyway.

**The fix, if it is ever worth doing, is to bound the allocation where it
happens** — vendor or patch `mvt-reader` so the capacity is
`min(len, remaining)` — not to guess from outside.

## How this was found, which is the other lesson

Not by a test. `cargo test` was green: the guard's own tests used a z14 city
fixture, and z14 is the one zoom where the ratio is nowhere near the threshold
(18 against 3 463 at z10). It surfaced in a CI scene-smoke log as
`map tile decode failed … refusing the tile`, repeated for every z10 tile on
screen, which reads as a tile-server problem rather than as our own guard.

Two live z10 tiles are now fixtures (`crates/geo/tests/fixtures/lowzoom_*.mvt`)
and `low_zoom_tiles_decode` asserts they parse. **A fixture at one zoom is not
a fixture for the format.**
