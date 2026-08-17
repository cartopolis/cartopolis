---
topic: sharing
triggers: [place_link.rs, AndroidManifest.xml, assetlinks.json, "deep link", "app link", "intent filter", "cartopolis://", "?place=", "/go", "Copy link", launch_url, autoVerify]
updated: 2026-08-16
---

# A link to a place opens the app, and what that costs

`https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0` is one link with
two destinations: on an Android phone with the published APK installed it opens
the app at that camera, and everywhere else it opens the web client, which reads
the same `?place=` it has read since the launch-place path existed. The whole
thing is `systems::place_link` (~200 lines and its tests), one JNI call, two
intent filters, a JSON file on the web root and one Caddy matcher.

## The four constraints that decided the shape

**An intent filter matches scheme, host and path, and never the query.** This is
the single fact the design turns on. `?place=…` therefore cannot be what marks a
link as one the app should answer, so the *path* has to carry that signal, and
the app claims `/go` rather than `/`. Claiming the root would have handed the app
every link to `cartopolis.org` — including `/apk/`, the page whose only job is to
give you the APK in a browser, and the README's "open cartopolis.org". `/go` is a
path with no file behind it: Caddy rewrites it to `index.html`, and the query
survives untouched, so the wasm build needed no change at all. In the deploy
host's `/data/stack/Caddyfile`, inside the `cartopolis.org` block and **before**
the catch-all `handle`:

```caddyfile
@place path /go
handle @place {
	root * /srv/cartopolis
	rewrite * /index.html
	header Cache-Control "no-cache"
	file_server
}
```

**The match is exact, and both looser spellings are wrong in their own way.**
`/go*` would swallow `/gophers`. `/go/*` looks harmless and is the trap: the
published `index.html` addresses the wasm module absolutely but Bevy's asset root
relatively (`src="assets/…"`, and `fetch("assets/…")` from the module), so a page
served at `/go/anything` resolves every asset to `/go/assets/…` and 404s the lot.
The manifest claims exactly the same one path (`android:path="/go"`), and the two
must stay in step, or a link opens the app on a phone and breaks in a browser.
`place_link` still accepts a path-carried place (`cartopolis://go/53.2194,6.5665`)
because the custom scheme never touches the web server — nothing *writes* that
form, and as an https link it is a 404.

Also deliberately **not** a general `try_files` fallback on the main handler:
that turns every missing asset into a 200 with an HTML body, and a wasm module
that 404s is a diagnosis where one that arrives as `index.html` is a mystery.

**App Link verification is keyed to the signing certificate**, so a locally built
APK can never have one. Android fetches `/.well-known/assetlinks.json` at install
time and checks it names both the package and the cert the APK was signed with;
`build-apk.sh` signs a hand build with a per-machine debug keystore, so on a
sideloaded build the https filter is dead by design. That is why the
`cartopolis://` scheme exists alongside it, and why nothing depends on the App
Link alone. Two things that look like bugs and are not: verification happens at
**install** time, so publishing the JSON after installing does not retroactively
fix a phone (`adb shell pm verify-app-links --re-verify eu.garage44.cartopolis`
does, and `pm get-app-links eu.garage44.cartopolis` shows the verdict); and the
fingerprint in `crates/cartopolis/assetlinks.json` is the *deploy host's*
`cartopolis.x509.pem`, so **rotating the release key means editing that file**, or
every install after the rotation silently stops handling links.

**The file has to ride in the trunk dist.** `deploy-web.yml` publishes with
`rsync --delete`, so anything under the web root that trunk did not produce is
removed on the next deploy. `assetlinks.json` is therefore a `data-trunk
rel="copy-file"` in `index.html` rather than a file placed on the host — a file
the site needs and the build does not know about is a file that works until the
next push.

**`android-activity` 0.6 surfaces no intent at all**, so the URL is read by JNI —
`getIntent().getDataString()`, `platform::android::launch_url`. Every class in
that path is a framework one, so the no-dex rule
(`docs/notes/android-build.md`) never threatened it.

## The limit: a link followed while the app is running does nothing

`android.app.NativeActivity` does not override `onNewIntent`, and the default
implementation does not call `setIntent`. So `getIntent()` keeps handing back the
intent that *started* the activity for the life of the process. With
`launchMode="singleTask"` — which the app needs, and which is what routes a second
link to `onNewIntent` rather than to a new activity — tapping a link while the app
is already running brings the existing task forward showing the place it was
already at, and `launch_url` reports the *first* link again.

This is why `geo_nav::launch_place` is documented as called **once, at startup**:
polling it would re-teleport the camera to a link followed hours ago, which is a
worse failure than the one being worked around.

Options considered and why they lost:

| | |
|---|---|
| A trampoline activity that reads the intent and hands it over | The correct fix, and it is a Java class, which is a `classes.dex`, which is the Gradle project this APK exists to not have |
| `launchMode="standard"` so each link starts a fresh activity | A second `NativeActivity` in one process means a second Bevy app against one `ANDROID_APP`, one winit and one surface |
| `noHistory="true"` so every launch is cold | Loses the whole session — map, camera, tile cache warmth — every time the user switches apps |
| Poll `getIntent()` each frame | It never changes; there is nothing to poll |

So the fix waits for the day this APK grows a dex for some other reason. Until
then: cold start honours the link, warm start brings the app forward. Worth
knowing before someone spends an afternoon deciding the JNI call is broken.

## What a link carries, and what it does not

Latitude, longitude, altitude, camera pitch and yaw — `place_link::place_param`,
which is what the Search panel's **Copy link** writes and what `geo_nav::parse_location`
reads. No spaces and no degree signs, so the URL needs no percent-encoding and
survives a chat client that ends a link at the first space; `place_from_link` still
accepts the readable `53.21940, 6.56650, 120m, -12.5°, 45.0°` spelling, either
scheme, any host, a percent-encoded value and a bare coordinate string, because the
ones worth refusing are only the ones nobody can fix.

Two functions that used to exist and deliberately do not:
`hud::format_place`, which wrote that readable spelling — Copy link writes a URL
now, so it had no writer left, and a format with no writer drifts away from the
parser that still reads it. And an `app_link` beside `web_link`: the
`cartopolis://` URL has exactly one writer and it is `index.html`'s loader, in
JavaScript, because the page must offer it *while the module is still
downloading*. A Rust twin would be a second definition of one string. What holds
them together is that Rust only ever reads the scheme, liberally, and
`a_link_round_trips_through_both_schemes` parses the exact literal the loader
writes.

It deliberately does **not** carry the time of day, the weather, the enabled
layers, the route or the session. A link that silently rewrote the receiver's
settings would be a link people learn not to follow, and every one of those is
already persisted per user (`user.json`). "The exact camera" is the whole
promise.

## Trying it without a phone

```bash
# the emulator harness in the android-emulator-on-the-vps memory, then:
adb shell am start -a android.intent.action.VIEW \
    -d 'cartopolis://go?place=53.21940,6.56650,120m,-12.5,45.0'
adb shell am start -a android.intent.action.VIEW \
    -d 'https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0'
adb shell pm get-app-links eu.garage44.cartopolis   # verified / not
```

On the desktop the same link is a `CARTO_PLACE` value — it goes through the same
parser, so a URL and five numbers are interchangeable there:

```bash
CARTO_PLACE='https://cartopolis.org/go?place=53.2194,6.5665,120m,-12.5,45' \
    cargo run -p cartopolis
```

There is no desktop *scheme handler*: registering `cartopolis://` with a desktop
needs an installed `.desktop` file (or the macOS/Windows equivalents), and this
project ships a `cargo run`, not an installer. When it ships one, the manifest is
the place that changes and nothing in `place_link` has to.
