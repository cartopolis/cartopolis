---
topic: the Midas device link (the app answering a developer's agent sessions)
triggers: ["midas_link.rs", "systems::midas_link", MidasLink, LinkTap, "Developer tools ▸ Midas", "X-Midas-Device", "mcp/film.rs", "mcp_server/src/local.rs", LocalTools]
updated: 2026-11-04
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
build as that person, under the same server checks a bot meets. The chip at
the top of the map shows a robot while the link is up, so it is never on
unseen. What the agent may *read* is still `mcp::agent::Audience`, empty by
default and set in Settings ▸ General ▸ Editor access, and `view` and `film`
drop the chrome while that list is narrower than everyone.

A tool a build lacks is refused with "run the session's code on a build node",
which is what the agent should do then (the hub's bridge says the same when the
phone is offline).

**Camera frames are kept only when asked for.** `high-precision-mode.md` keeps
nothing the camera sees, and said frames would come back "as a developer
switch, not in the menu". This is that switch: `HighPrecision::tap` copies one
frame, or records for a while, only for a pending call. A call that needs the
camera switches precision mode on for itself, and off after if it was off.

**The token is `X-Midas-Device`, never `Authorization`.** Caddy strips
`Authorization` from every request to the hub, so a bearer token arrives as
nothing and the link reads as unpaired. A 401 on the link or a push means
Forget was pressed on the page: the app drops its token and asks to be paired
again.

**First linked from a phone on 2026-11-04** (a OnePlus CPH2747, through
Caddy): a session's `screenshot` came back as the app's window. The MCP tools
on the link were compiled for Android and unit-tested that day, and had not
yet run on a phone.
