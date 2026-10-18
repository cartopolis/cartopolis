# docs/img

`readme.jpg` is the contact sheet the top-level README shows. It is built in two steps,
and neither involves editing an image by hand:

1. **The captures.** `building.png`, `chat.png`, `layers.png`, `route.png`, `globe.png`
   and `chatworld.png` are the ungrouped shots in [`docs/shots.toml`](../shots.toml):
   real app screens with the UI open, captured headless.

   ```bash
   cargo shots
   ```

2. **The sheet.** [`readme.html`](readme.html) frames them the way `docs/mockups/`
   frames a screen. Render it at 1600 CSS px and save it as a ~2400 px JPEG:

   ```bash
   # Linux (the mockups' renderer)
   ~/bin/chromium --headless --no-sandbox --disable-gpu --hide-scrollbars \
     --force-device-scale-factor=1.5 --window-size=1600,970 \
     --screenshot=/tmp/sheet.png "file://$PWD/docs/img/readme.html"
   convert /tmp/sheet.png -quality 75 docs/img/readme.jpg

   # macOS (no Chromium needed)
   swiftc -O tools/render-html.swift -o /tmp/render-html
   /tmp/render-html docs/img/readme.html /tmp/sheet.png 1600
   sips -s format jpeg -s formatOptions 75 -Z 2400 /tmp/sheet.png --out docs/img/readme.jpg
   ```

To move a view, change its camera in `docs/shots.toml`. The dev panel's View tab has a
**📋 Copy as preset** button that writes the block for you. To change a caption, edit
`readme.html`.

The other directories are shot groups, regenerated with `cargo shots --only <group>`
and not linked from the README:

- `views/` is the scenic set the README used to carry: the city, its landmarks and the port.
- `extras/` has the dusk view, the interface over the city (`chrome.png`, the densest
  settings screen, for checking a palette change) and the atmosphere from 120 km.
