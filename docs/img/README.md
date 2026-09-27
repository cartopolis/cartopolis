# docs/img

Images used by the docs and the top-level README.

The named files — `city.png`, `canals.png`, `tunnel.png`, `station.png`, `grote-markt.png`,
`forum.png`, `building.png`, `skyline.png`, `city-far.png`, `eemshaven.png`,
`eemshaven-sea.png`, `meet.png`, `globe.png` — are **generated**, one per entry in
[`docs/shots.toml`](../shots.toml). Regenerate them all with:

```bash
cargo shots
```

Don't edit or replace them by hand: change the camera position in
`docs/shots.toml` (the dev panel's View tab has a **📋 Copy as preset** button
that writes the block for you) and re-run.

`extras/` holds the manifest's `extras` group — the dusk view, the route planner, the interface
over the city (`chrome.png`, the densest settings screen, for checking a palette
change) and the atmosphere from 120 km. They are not in the README; regenerate
them with `cargo shots --only extras`.

`screenshot-1.png` … `screenshot-3.png` are scratch slots for one-off in-app
captures (developer panel ▸ View ▸ **📷 Capture slot N**). Nothing links to them.
See [Taking screenshots](../manual.md#taking-screenshots).
