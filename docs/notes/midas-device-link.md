---
topic: the Midas device link (the app answering a developer's agent sessions)
triggers: ["midas_link.rs", "MidasCall.java", "systems::midas_link", MidasLink, LinkTap, "Developer tools ▸ Midas", "X-Midas-Device", "mcp/film.rs", "mcp_server/src/local.rs", LocalTools]
updated: 2026-11-11
---

# The Midas device link

Developer tools ▸ Midas pairs the app with a developer's Midas hub, after which
their agent sessions can ask it for a screenshot, a back-camera frame, the
precision log, a sensor recording (motion and fixes as CSV) and the GNSS state
while it is open. The developer can also send those to a session from the
phone. The hub's half and the design are in midas (`src/device.rs`,
`docs/mockups/device-link/design.md`).

**Why the phone dials out, and to Midas rather than the simulator.**
`driving-the-client-from-an-ide.md` rejected relaying the desktop MCP server
through the simulator, and said to revisit that for "a phone behind NAT". This
is the revisit, but the relay is the hub, not the sim. So there is no wire
revision, no megabyte PNG beside the snapshot stream, and it works offline: it
needs the internet, not a sim.

**Its sensors, and every tool of `cartopolis_mcp` beside them (2026-11-04).**
The six sensor tools (`device_state`, `screenshot`, `camera_frame`,
`precision_log`, `record`, `gnss`) are answered in `serve_calls`. Everything
else the link offers is the MCP server's — `view`, `view_here`, `state`,
`film`, `me`, `walk_to`, `read_chat`, `say` and the rest — relayed by the
worker to an in-process session of that server (`cartopolis_mcp::local`, the
seam the voice agent already used). The first version kept only the sensors,
because that crate was Unix desktops only and "a phone is asked for sensors,
not editing"; the owner wanted to watch a session walk the phone's avatar, and
a second table of walk and chat tools mapped onto the link would drift from
the bots' in names, schemas and wording. So the crate now builds for Android
(`build.rs` sets `cfg(mcp)` on all of Unix), and the phone never opens its
socket (`mcp::reconcile_listener`). The sensors' `state` became `device_state`
so the server's `state` keeps its name.

**A session is the account signed in on the phone.** There is no switch for
it (the owner's call): while the link is on, a session can walk, talk and
build as that person, under the same server checks a bot meets. So the map
shows it: a round button in the top-right corner carrying the Midas mark (the
gold M of midas's `web/src/icon.svg`, painted by `theme::paint_midas_mark`)
with a green dot, there exactly while the link is on *and* up — no grey
variant when it is down (the owner's call, 2026-11-04). A call coming in makes
it glow; a tap opens a card with the newest call (`s579 · screenshot · just
now`) and the account sessions act as. It replaced a text chip at the top
centre and, on the phone, the editor's activity box (`mcp::overlay`), which
together covered the top of the map with a socket path and a tool name
wrapped a syllable a line. The phone registers no activity box at all: no
editor drives it there. A desktop with both moves the box under the button. What the agent may *read* is still `mcp::agent::Audience`, empty by
default and set in Settings ▸ General ▸ Editor access, and `view` and `film`
drop the chrome while that list is narrower than everyone.

**A session can speak on the map: `tell` (2026-11-11).** The owner wanted
to follow a session driving the phone without leaving the app for the Midas
page to read what it was up to. `tell` puts a short message (400 characters at
most) in a bubble left of the Midas button, signed with the session, its tail
on the button. It stays until tapped away, replaced by the next `tell`, or its
`seconds` pass; an empty text takes it down. It is the link's own tool, not
the MCP server's: only a session on the hub has a person on the other side of
the glass, and an editor on the desktop has its own window. It is not chat
(`say` speaks in the world) and nobody else sees it. Like the button it stands
down under an open panel and in driver mode, and comes back with the map; the
call's answer says when it was kept but not shown. With the card open it
stands under the card. `--ui midastell` captures it.

A tool a build lacks is refused with "run the session's code on a build node",
which is what the agent should do then (the hub's bridge says the same when the
phone is offline).

**Camera frames are kept only when asked for.** `high-precision-mode.md` keeps
nothing the camera sees, and said frames would come back "as a developer
switch, not in the menu". This is that switch: `HighPrecision::tap` copies one
frame, or records for a while, only for a pending call. A call that needs the
camera switches precision mode on for itself, and off after if it was off.

**`gnss` and `device_state` fall back to the ordinary location
(2026-11-04).** They read only precision mode's fix, so they said "no fix"
whenever that mode was off. A call now opens a location watch and the
compass for itself (kept 120 s after the last call; the phone has one
location subscription, so this joins it when locate or precision mode has it
open) and says which source each value came from. A fix carries no time, so
its age is "at least N s" until the link has seen it change.

**`camera_frame` takes `colour`**: a still from the back camera, opened for
that call only, rather than the tracker's grey frame. It is refused while
precision mode or the camera screen holds the camera, since only one may be
open; it does not close theirs to take its picture.

**The token is `X-Midas-Device`, never `Authorization`.** Caddy strips
`Authorization` from every request to the hub, so a bearer token arrives as
nothing and the link reads as unpaired. A 401 on the link or a push means
Forget was pressed on the page: the app drops its token and asks to be paired
again.

**First linked from a phone on 2026-11-04** (a OnePlus CPH2747, through
Caddy): a session's `screenshot` came back as the app's window. The MCP tools
on the link were compiled for Android and unit-tested that day, and had not
yet run on a phone.

**The link outlives leaving the app (2026-11-04).** The person follows the
session on the Midas page, usually on the same phone, so every result they read
took them out of the app. The process was frozen, the link dropped (fifteen
links that day, most under two minutes) and a call in flight failed with "went
while it worked". So, while the link is on, `systems::background` keeps the
process alive with the same foreground service the "stay connected" setting
uses (`midas_link::holds_process`). The app still answers nothing off screen,
because `bevy_winit` runs no update while suspended (`should_update` asks
`lifecycle.is_active()`). Instead the worker tells the hub
`{"type":"away","away":true}`. The hub then tells the person a session wants
the phone, and the call it holds is answered the moment the app is opened,
with no reconnect.

**Answering off screen was weighed and not built (2026-11-07).** The owner
asked whether the app could answer while the Midas page is in front. Only the
tools that need no picture could (the sensors, chat, walking): Android gives a
backgrounded app no surface and no camera, so `screenshot`, `view`, `film` and
`camera_frame` never can. It would take running Bevy's update while
suspended, which `bevy_winit` does not, plus background-location permission
for `gnss`. The owner chose the notice below instead.

**A held call shows on the phone (2026-11-07).** A call still unanswered 2 s
after it reached the app off screen posts a silent, high-importance
notification (`MidasCall.java`, channel `midas`): "s579 wants the phone",
`screenshot and 1 more. Tap to answer.`. It peeks over the Midas page, and the
tap opens the app, which answers. The 2 s is so a tool answered without a
frame does not flash one. It goes when the app resumes, when every held call is
answered, and after 90 s, when the hub has given up. The worker posts it from
its own thread (`worker::Waiting`), since nothing on the frame loop runs then.

**Each call is acknowledged as it arrives** (`"ack": true` in the hello, then
`{"type":"ack"}` before anything else), so the hub drops a dead socket in 5 s
rather than waiting out the call's 60 s. The worker also gives up a link the
hub has been silent on for 70 s (it pings every 25), opens one again at once
when the app returns after 30 s of quiet, and waits at most 10 s between tries,
spread by a quarter. The hub waits 90 s for a phone that is away, and the old
cap of 60 s on its own was enough to miss that. Screenshots and colour stills
are encoded to base64 off the frame loop, give up after 30 s, and replies are
made into messages on the link's thread.
