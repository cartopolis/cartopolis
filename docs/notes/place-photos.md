---
topic: place-photos
triggers: [photo.rs, photo_store.rs, bookmarks.rs, user_store.rs, android.rs, "photo", "camera", "thumbnail", "picture", "image capture", "MediaStore", "rfd", "file picker", "context menu", "long_touched", marker_slot, "place row", "pin colour"]
updated: 2026-08-23
---

# A saved place can carry a photo

Places are now `[photo] name` in the panel, with everything the row used to offer
as buttons moved into a context menu. Three separate constraints shaped this and
none of them is visible from the feature description.

## Android has no activity result, so the camera is watched, not awaited

The ordinary way to take a photo is `startActivityForResult(ACTION_IMAGE_CAPTURE)`
and read the answer in `onActivityResult`. **Neither half of that is reachable
here.** `android-activity` 0.6 surfaces no activity result at all, and delivering
one would need a Java class — hence a `classes.dex`, hence (it was assumed)
Gradle, which is the one thing `docs/notes/android-build.md` exists to avoid.
**That last step turned out to be false**: the dex lane landed 2026-08-23 as
javac + d8 with no Gradle, so an activity that reports its result *is* now
buildable. The watched-MediaStore design below is what ships and is not urgent to
replace — but it is a workaround now, not the only option.

What replaces it: the app inserts an empty row into the `MediaStore`, hands the
camera app that `content://` URI as `EXTRA_OUTPUT` with a write grant, starts the
intent fire-and-forget, and then *watches the row's `_size` column*. The camera
app never tells us anything; the file filling up is the signal.

Consequences worth knowing before touching it:

- **Completion is "non-zero and unchanged since the last look"**, not "non-zero".
  A poll can land inside the camera app's write. Being wrong there costs an error
  message for a photo that was taken perfectly well.
- **A cancelled capture is indistinguishable from a slow one.** Nothing reports a
  Back press, so the row simply stays empty forever — which is why the panel
  grows a Cancel next to the spinner, and why that Cancel deletes the MediaStore
  row (the alternative is an empty file in the user's gallery per attempt).
- **`android.permission.CAMERA` is deliberately *not* declared.**
  `ACTION_IMAGE_CAPTURE` needs no permission from the sender — but an app that
  *declares* CAMERA must also hold it before the intent is honoured. Declaring it
  would buy a runtime prompt for something already permitted.
- `WRITE_EXTERNAL_STORAGE` is declared with `maxSdkVersion="28"`: `RELATIVE_PATH`
  (scoped storage) only exists from API 29, and minSdk here is 28.

**It compiles for aarch64 and has never run.** The container has no Android
runtime — see `docs/notes/container-android-toolchain.md` for the local target
check, which catches type errors but says nothing about behaviour. Every claim
above is read off the API contract.

## The web build captures photos, and now keeps them

`<input type="file" accept="image/*" capture="environment">` is the whole mobile
web story — a phone browser answers it with the rear camera, a desktop browser
with a file picker, from the same element.

Photos persist there like everything else: both sizes go into the Data namespace
of `platform::storage`, which is IndexedDB on the web (`docs/notes/web-persistence.md`).
Thumbnails are preloaded into the synchronous view before the app is built, because
a row draws one inside an egui pass; the full-size photo is fetched when someone
opens the viewer, which is why `read_full` answers `Cached::Loading` and the viewer
has a spinner state distinct from its "could not be opened" one.

One constraint the web arm cannot violate: `input.click()` needs **transient user
activation**, which a browser grants for about five seconds after a real gesture.
Reached from a menu item tapped a frame or two ago; not reachable from a timer or
a network callback.

## The desktop dialog is `rfd` with GTK deliberately switched off

`default-features = false, features = ["xdg-portal"]`. rfd's default Linux
backend is GTK3, which needs `libgtk-3-dev` at build time — a new system
prerequisite for every Linux build of this client, including CI and the agent
container, neither of which has it. This is the same trap `bevy_audio`/alsa-sys
documents. The portal backend is DBus and pure Rust.

## Why the row's actions moved into a context menu

The row ended in four small buttons (route, update, rename, delete). On the
300 pt panel that left about half the width for the name; on a phone, where the
panel is the whole screen and the buttons are still finger-sized, it left a name
column too narrow to tell two long street names apart. The thumbnail needed
28 pt more.

egui reaches the menu from a touch screen with no work on our side:
`Response::secondary_clicked` is true for a **long touch** as well as a
right-click, and this client feeds egui real `Event::Touch`es
(`egui_manager::pointer_events`), which is the condition
`InputState::any_touches` — and therefore the whole long-press path — depends on.

**The trap in that path:** egui's press-and-hold handler sets `clicked()` *as
well as* `secondary_clicked()`, so a long press on a place would open the menu
*and* fly the camera to the place behind it. `place_row` guards the jump with
`!name_response.long_touched()`. Anything else that grows a context menu on a
clickable widget needs the same guard.

## A photo must not move the row it is in

The row opens with one fixed 28 pt square (`bookmarks::marker_slot`): the photo
tile when there is one, the place's map pin painted at the same size when there
isn't. It was a 28 pt tile against a 12 pt text glyph, so whether a place happened
to have a photo shifted its name — and its kebab, and its whole row — half a slot
sideways. A list of saved places is scanned down its left edge; rows that do not
share a margin read as broken before anyone works out which of them is the odd
one. The folder colour a place carries moved with it: it is the pin's fill when
there is no photo, and a hairline frame *inside* the tile when there is (outside,
it made the photo rows a shade wider than the pin rows — the same misalignment
one step smaller).

The pin is painted, not a glyph (`place_pins::paint_pin_glyph`). The slot used to
call `theme::first_drawable(&["📍", "⌖", "•"])`, which probes the bundled font and
had settled on `⌖` — a crosshair, next to a teardrop standing over the same place
on the map. Painting it means the panel and the map cannot disagree.

**Closing the viewer used to freeze the app**, and that is a renderer constraint
rather than a photo one: [`egui-texture-lifetime`](egui-texture-lifetime.md).

## Storage shape

Two entries per photo in the **Data** namespace of `platform::storage` (not the
Cache one — clearing tile cache must not take the user's pictures), keyed
`photos/<stem>.jpg` and `photos/<stem>.thumb.jpg`; `user.json` stores only the
stem. That is a file under the app data root natively and an IndexedDB record on
the web, from one code path.

- **Bytes are never in `user.json`.** `UserStore::save` re-serialises the whole
  document on every autosave tick and compares it to the last one; a base64 photo
  would make each tick a multi-megabyte serialise-and-compare.
- **The stem carries a serial (`place-<id>-<seq>`), not just the place id.** egui
  caches a texture by name, so replacing a photo under the same name would leave
  the old picture in the row until the app restarted.
- **A stem is validated on the way out of the store.** `user.json` is a plain
  file anything can edit, so a stem is input; `../..` in one would otherwise aim
  the reader — and the `remove` on delete — outside the photo namespace (a path
  escape natively; merely a strange key on the web, but the two platforms have no
  reason to disagree about what a stem is).
- **EXIF orientation is baked into the pixels**, once, at capture. Phone cameras
  write the sensor's landscape frame plus a rotation tag; carrying the tag would
  make every later consumer honour it independently.
