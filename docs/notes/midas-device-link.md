---
topic: the Midas device link (the app answering a developer's agent sessions)
triggers: [midas_link, MidasLink, LinkTap, "Developer tools ▸ Midas", "X-Midas-Device", "/v1/device/"]
updated: 2026-11-03
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

**Its own tools, not `cartopolis_mcp`'s.** That crate is Unix desktops only,
and what a phone is asked for is sensors, not editing. Six tools behind one
match in `serve_calls` are less seam than moving the job channel to Android. A
tool a build lacks is refused with "run the session's code on a build node",
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

**Unverified on a phone as of 2026-11-03.** The link was compiled for Android
and its pieces unit-tested, and the hub's side was tested end to end with a
stand-in device. No phone had linked through Caddy yet.
