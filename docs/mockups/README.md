# Mockups

Screen designs drawn before a feature is built, kept for reference: what was
proposed, what was agreed, and what was drawn and then dropped. They are
pictures of intent, not a description of the app — the code is the truth once
a feature lands, and a mockup is not edited to follow it.

## Format

One folder per topic, named for the topic (`chat-world/`). Inside:

- **`README.md`** with the same front-matter as a note, so it is found the same
  way, then the images in order, newest proposal first, and what was decided:

  ```yaml
  ---
  topic: chat-world
  triggers: [chat, private_chat.rs, contacts]   # when to read this
  updated: 2026-09-29
  ---
  ```

- **The `.html` sources.** Plain HTML + one shared `style.css` per folder,
  versioned in the name (`v1.html`, `v5-contacts.html`) rather than overwritten,
  so the history of a design is the folder itself.
- **A rendered `.png` beside each source**, so the README reads without a
  browser. Images are LFS (`.gitattributes` tracks `*.png`/`*.jpg` everywhere).

Render a source headlessly:

```bash
~/bin/chromium --headless --no-sandbox --disable-gpu --hide-scrollbars \
  --window-size=1840,3200 --screenshot=/tmp/m.png "file://$PWD/v5-contacts.html"
convert /tmp/m.png -trim +repage -bordercolor '#0d1214' -border 40 v5-contacts.png
```

Mark anything that does not exist in the app as new — a mockup that passes a
proposal off as current behaviour misleads whoever reads it later.
