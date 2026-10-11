---
topic: web-navigation
triggers: [atlas web, "crates/atlas/web", "main.tsx", "chat.tsx", Community, World, admin, Shell, rail, breadcrumb, viewer]
updated: 2026-10-11
---

# The web page: Community | World, Admin beside them, and four levels: mockup

A design proposal, not code. Everything marked **new** does not exist today.

The web page gets the app's Chats | World switch, worded **Community | World**.
World is the app's own WebAssembly build (today at `/viewer/`), running inside
the page. An account that can manage anything also sees a third segment,
**Admin**, in gold, so the admin pages stop being a separate site with their
own sign-in. Today the page is one flat list with its links at the bottom; this
gives it four levels: the switch in a top bar, a rail (Home and one tile per
community; the admin areas People, Content and Data), a sidebar (topics and
moderation; an area's pages) and a side pane (a thread, a person, a record),
with a breadcrumb of all four. Moderator and admin actions show where the thing
is, under a gold line: a spam line in a thread, a building's card in World.

Drawn in the web UI's palette as it is today (`crates/atlas/web/src/css/theme.css`,
dark half). Builds on `../workspace/style.css`.

## v1

![v1](v1.png)

Open questions are on the sheet: who receives which reports, what World is for
somebody signed out, and whether admin keeps its own sign-in.
