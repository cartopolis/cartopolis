---
topic: people's pictures
triggers: [faces.rs, pictures.rs, faces, profile picture, "/api/pictures/mine", "/api/pictures/people", texture_for, paint_face, you_picture, face_snapshot, viewfinder, "accounts.picture", FACE_EDGE]
updated: 2026-10-16
---

# People's pictures

A person can give their disc a picture instead of their initial, from the You
page: a file, the camera facing them, or a **polaroid** of the world. Every
disc in the app shows it — lists, chat headers, calls, the map's person
markers — and so does the website's chat. `systems::faces` on the client,
`simulator::pictures` on the server, `accounts.picture` (V36) between them.

## How it is stored

A picture is a **hash in the picture store** (`/api/pictures`), the one chat
pictures and community pictures already use. The account names it with
`PUT /api/pictures/mine {"picture": hash | null}`; a hash the store does not
hold is refused, so a disc can never point at nothing. The picture is a
256-pixel square JPEG (`utils::photo::FACE_EDGE`), not sealed: like a
community's picture it is shown to anybody signed in who sees the name.

Everybody else's are read by name, `GET /api/pictures/people?names=a,b`, up to
100 a request. A name with no account reads the same as a name with no
picture, so this is not a way to find out who exists.

**Both routes are under `/api/pictures/` on purpose**: the public host's
Caddyfile lists every `/api` path by hand and already sends
`/api/pictures/*` to the simulator, so this needed no host change.

## Nothing is pushed

There is no protocol revision. A client asks for the names its discs are
drawing, in batches, and asks again every five minutes (`faces::REFRESH_S`);
that is how a new picture reaches people who already had the old one. Pushing
it would have meant a picture field on half a dozen DTOs (contacts, requests,
people nearby, where people are) and a wire bump, for something that changes
a few times in an account's life.

## How every disc gets it without an edit

`chats::paint_avatar` is the one function every disc goes through, and it
takes a painter and a name — no `World`. So `faces` keeps a small shared table
(`texture_for`) that it rewrites each frame, and a disc asking for a name is
how it learns whom to look up. A name is looked up only if it is a plausible
account name on this server: `name@elsewhere` is left alone. At most
`MAX_TEXTURES` (64) pictures are held as textures; past that the ones nobody
has drawn for 30 s are let go, and come back from the disk cache.

## The polaroid

A viewfinder over the map: the panels close, a square is cut out of a dimmed
screen, and the shutter takes the window with the UI hidden — the same capture
a chat's snapshot makes (`ui::drive_screenshots`, `ScreenshotState::face_snapshot`)
— cropped to the same share of the window the square showed
(`faces::FINDER_SHARE`). **You frame it yourself**: Face me swings the follow
camera round in front of your avatar (the rig sits behind at `facing + π`, so
at `facing` it looks back), and from there it is the ordinary camera. An
automatic selfie that moved the camera and put it back was considered and not
built: putting a Fly or Earth camera back exactly is three modes' worth of
state, and the person cannot see what they are getting.

Every source ends in the same preview, drawn as a polaroid, with Use this or
Again.

## What each platform offers

`platform::photo::offers`: the desktop has a file dialog (Photo) and no camera;
the web has both (Photo with no `capture` attribute, Camera with
`capture="user"`); Android has the camera intent only — `android-activity`
surfaces no activity result, so a gallery's answer could not be read back.
The polaroid is everywhere.

## Not done

- **Moderation.** A picture is shown to strangers, so it is user-generated
  content in the sense a store review asks about. A report names a person and
  an operator can act on the account, but there is no "remove this picture"
  action of its own yet.
- **The fediverse actor's icon** is not set from it.
