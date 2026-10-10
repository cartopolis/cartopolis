---
topic: tile-texture-compression
triggers: [texcomp.rs, tile_loader.rs, tile_texels, map_stream.rs, globe.rs, quality.rs, compressed_tile_px, tile_compression, CARTO_TILE_COMPRESSION, BC3, ETC2, ASTC, ctt, "texture compression", "GPU memory", "tile_px"]
updated: 2026-10-09
---

# Streamed map tiles are block-compressed on the worker

Every flat-map and globe tile is mipmapped and then compressed to an 8-bit-per-texel
block format on the worker that rasterised it (`systems::tile_loader::tile_texels`,
`cartopolis_geo::texcomp`): BC3 where the device offers `TEXTURE_COMPRESSION_BC`,
ETC2 RGBA where it offers only `TEXTURE_COMPRESSION_ETC2`, RGBA8 where it offers
neither. `CARTO_TILE_COMPRESSION=0` turns it off; `=etc2` / `=bc3` force a format the
device decodes.

## What it is for, measured

**Not mainly memory.** Measured 2026-10-09 with `cargo perf` at the handset's
configuration (`CARTO_QUALITY=balanced CARTO_HANDSET=1`, Bevy 0.20, Deck RADV and
container lavapipe agree on asset bytes), textures are **9 %** of what is resident
over a city: 87 MB of 958 at the `city` scenario, 136 of 1,495 at `street`. The
rest is geometry. Compressing the same 256² tiles would save ~65 MB.

**What it buys is the 512² basemap back on a phone.** `HANDSET_TILE_PX` caps a
handset at 256² because 512² RGBA8 cost ~424 MB more over Groningen (2026-08-19,
`quality::handset_guarded`). A 512² chain at 8 bits per texel is 350 kB — what a
256² RGBA8 chain is — so `Budget::compressed_tile_px` lifts the cap to 512 whenever
compression is on. Same scenarios, lavapipe, BC3, 2026-10-09:

| scenario | textures, off (256² RGBA8) | textures, on (512² BC3) |
|---|---|---|
| city | 87 MB | 65 MB |
| street | 136 MB | 113 MB |
| orbit (globe, 256² either way) | 69 MB | 32 MB |
| low-orbit | 38 MB | 12 MB |

## Why the formats are BC3 and ETC2, and the encoder is ours

- **8-bit alpha is not optional.** The flat tiles carry the water mask in alpha and
  the globe tiles the ocean mask. BC1 / ETC1 / ETC2 RGB have none or one bit.
- **Not ASTC.** Every ASTC encoder is C++ (`astcenc`), and so is every BC7 encoder
  worth using — `ctt`, which Bevy 0.20's `CompressedImageSaver` wraps, binds
  astcenc, bc7enc_rdo, ISPC, etcpak and Compressonator. C in the build is what the
  wasm lane cannot take (the reason Opus lost, `docs/notes/voice-chat.md`), and the
  saver is an asset processor anyway: it never sees a texture rasterised at runtime.
- **Not on the GPU.** A compute-shader encoder would keep the CPU free but still
  uploads every tile uncompressed once, and is a WGSL encoder to own per format.
  The CPU encoders are pure Rust, run in the web build's workers, and are tested
  against independent decoders.
- **ETC2 needs all four of its colour modes for a map.** ETC1's individual and
  differential modes can only shift brightness inside a sub-block, so a 1-px road
  across coloured landuse came back at 20.9 dB (BC3: 42.9). Planar fixes smooth
  chroma gradients and T/H fix two-colour blocks; with them the same blocks are
  36.4 dB. Checked with crates.io `texture2ddecoder`, which shares no code with ours.

## Cost

Single thread, x86-64-v3, a 512² chain (`texcomp`'s ignored timing test): BC3
~6 ms map-like / 14 ms all-aerial-grain, ETC2 ~14 ms / 22 ms. That is on top of the
48–68 ms a tile takes to rasterise (`tile-raster-cost.md`). **Not measured on a
phone.** If an Oryon core is much slower, `texcomp`'s module note lists what to cut
first (`ETC_TH_THRESHOLD`, the planar ±1 refinement, the ETC1 search).

**Whether the Adreno 840 offers BC is not known** — if it does, a phone gets BC3 and
the ETC2 encoder is unused there. The capability log line
(`device capabilities measured … tile_blocks=…`) says which on the device.

## The Deck freezes, and it is not this

Captures at 512² froze the Deck's capture slot on 2026-10-10 (4 of 8 runs at 512²,
0 of 7 at 256², on unmodified `main` too). It is **GTT exhaustion, not a GPU hang**:
the Deck has 1 GB of VRAM and an 8 GB GTT, a frozen run sat at 0 % GPU busy with GTT
at 8.67 GB and a pool thread in the kernel's `ttm_global_swapout`, and a run killed
in that state cannot exit (the zombie keeps its buffers until the swap-out ends — a
reboot was the only way out). Read GTT from
`/sys/class/drm/card*/device/mem_info_gtt_used`; don't `kill -9` a capture that is
already in `ttm_global_swapout`.

What fills it, measured the same day with GTT before and at peak around the three
viewpoints over Groningen:

- the always-on `viberbot.service` client — 3.3 GB fresh after boot, 4.1 GB a quarter
  of an hour later, 8.3 GB after six hours: it grows;
- a desktop-`balanced` capture adds ~3.6–4.4 GB at the 150 m viewpoint, twice what
  `upload_meter` counts as resident asset bytes (~1.8 GB). BC3 lowers it by
  0.2–0.4 GB; it does not fix it.

The handset configuration adds **3.30 GB with 256² RGBA8 and 3.31 GB with 512² BC3**,
so the lift to 512² costs a phone nothing in graphics memory.
