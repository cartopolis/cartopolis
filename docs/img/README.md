# docs/img

Images used by the docs and the top-level README.

The named files — `city.png`, `city-dusk.png`, `avatar.png`, `route.png`,
`horizon.png`, `globe.png` — are **generated**, one per entry in
[`docs/shots.toml`](../shots.toml). Regenerate them all with:

```bash
cargo shots
```

Don't edit or replace them by hand: change the camera position in
`docs/shots.toml` (the dev panel's **📋 Copy as preset** button writes the block
for you) and re-run.

`screenshot-1.png` … `screenshot-3.png` are scratch slots for one-off in-app
captures (developer panel ▸ **📷 Capture slot N**). Nothing links to them.
See [Taking screenshots](../manual.md#taking-screenshots).
