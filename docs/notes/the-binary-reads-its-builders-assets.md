---
topic: asset-root
triggers: [crates/cartopolis/src/lib.rs, asset_root, ASSET_ROOT, CARTO_ASSET_ROOT, BEVY_ASSET_ROOT, "shader A/B"]
updated: 2026-10-31
---

# The client binary reads the assets of the checkout that built it

`lib.rs::asset_root` bakes the building checkout's `assets/` path in at compile
time. `BEVY_ASSET_ROOT` and the working directory are ignored; only
`CARTO_ASSET_ROOT` overrides it (desktop only). So a binary run from anywhere
other than the checkout that built it reads *that* checkout's assets, and an
edit to a `.wesl` changes nothing: two shader A/Bs came back byte-identical
(2026-09-09) for exactly this reason.

On a midas node the binary and the synced assets are one checkout, so a plain
`cargo run` there is fine. The trap is a binary copied elsewhere or kept from an
earlier build. To see the baked path:

```bash
strings -a <binary> | grep -o "/[^ ]*crates/cartopolis/../../assets"
```

To prove a shader file is the one being read, append a line of garbage to it and
look for its path with a line number in the log: shader errors show only at
runtime, so a silent run means you are editing a file nothing loads.
