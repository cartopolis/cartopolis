# QA targets

The standing functionality list the autopilot's QA lane rotates through
(`tools/autopilot.ts`, `qaPerDay` in its config). One row per user-facing
functionality; the second column is what a QA session starts from, not a spec.
Rows are cycled at most once per day each, after recently shipped tickets have
been walked first.

Add a row when a feature ships that a player can reach; drop one only when the
functionality itself is gone. Keep rows at the size of "one session can walk
this in one sitting".

| Functionality | Where a player meets it |
|---|---|
| Register and sign in | the login card (☰ ▸ Sign in…), a fresh account against the public sim |
| Claim and own a house | a registered account claiming, re-claiming, and losing a claim |
| Walk, run, stroll | WASD, Shift to run, Alt to stroll — speeds, animation, collision with buildings |
| Jump and flight | Space to jump, held Space / F to fly, R/Q to climb and descend, landing again |
| Sit on a bench | E near surveyed street furniture, standing back up, being seen sitting by others |
| Emotes | keys 1–6, visible to another connected client, joining mid-emote |
| Chat | the chat panel, message delivery to a second client, history on arrival |
| Proximity voice | capture and playback seams, attenuation with distance (audio hardware permitting) |
| Block and report | blocking a user, what the blocked party can still see, filing a report |
| Delete account | the password re-check, what happens to owned prims and sessions |
| Create and edit a prim | context menu, gizmo drag, edit dialog, another client seeing the change |
| Script a prim | the script store, ScriptOutput on errors, send/listen between two prims |
| Touch and interact | left-click on a scripted prim, E on a nearby one, the prompt choosing prim over place |
| Export, import, transfer builds | /api/content routes as a plain player and as an admin |
| Bookmarks and place pins | folders, colours, drag-and-drop, rename, pins culled to the map area |
| Place photos | attaching, viewing and removing a photo on a saved place |
| Place links | https://cartopolis.org/go?place=… into the web client, camera arriving at the place |
| Route planning | picking two points, the drawn line, turn instructions, a route too far |
| Driver mode | starting a drive, minimal map mode, leaving it with layers restored |
| Data layer toggles | each Map panel toggle actually stopping its producer and coming back |
| Street labels | toggle on, names present at street level, flipping with camera heading |
| Terrain relief | --height over a hilly area, buildings and route staying on the ground |
| Aerial imagery | the toggle, coverage edge behaviour outside the Netherlands |
| Settings persistence | graphics, sky, volume surviving a restart; offline default on first run |
| Offline and online switching | entering a server mid-run, leaving it, the map surviving both |
| Avatar appearance | mixing slots across styles, the choice replicating and persisting |
