---
topic: app-updates
triggers: [app_update.rs, platform/update.rs, Updater.java, UpdateReceiver.java, "self-update", "too old"]
updated: 2026-11-05
---

# The app tells you about an update

![v1](v1.png)

**v1 (2026-11-05), agreed and built.** The phone app updates itself from
cartopolis.org/apk/ (`docs/notes/android-build.md`, "The app updates itself").
What the person sees of that:

1. **Update ready** — a pill at the top once a newer build is downloaded, with
   one tap to install. Ignored or closed, it installs when they leave the app;
   this version keeps working meanwhile.
2. **Too old** — only when the server refuses this build's wire version. The map
   is dimmed and nothing answers but **Update now**.
3. **Updating** — the same card with the download's progress, skipped when the
   build was already downloaded.
4. **The very first update** — Android's own confirm screens, once per phone;
   from the second update on, on Android 12+, the one tap is all.

The owner asked for: one tap to install; "Update now" only when required, a
notice with tap-to-install otherwise; downloads on mobile data too.

`map.png` is the landing gate's phone render, the backdrop of every frame.
