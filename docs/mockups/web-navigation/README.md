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

## Decided (2026-10-11)

The sheet's three open questions, settled when v1 was agreed:

- **Reports split by what they are about.** A report on a line in a
  community's topic goes to that community's moderators, as it does today. A
  report about a *person* (the app's Report, `/api/admin/reports`) goes to the
  server's admins, in Admin › People › Reports, which is new: until now that
  queue had no page at all.
- **World works signed out.** It is the map: the switch reads Community |
  World, Community is the sign-in card, World is the viewer with no rail and no
  chat beside it. Nobody has to sign in to look at the map.
- **One sign-in.** Admin uses the session the chat signed in with; the separate
  admin sign-in form goes. The simulator already issues both from one session
  store, and still checks the account's permissions on every admin call.
