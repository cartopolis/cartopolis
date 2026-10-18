---
topic: stats
triggers: [stats, analytics, visitors, "page views", telemetry, usage, stats.db, "/api/stats", "/api/admin/stats", heatmap, retention]
updated: 2026-10-18
---

# Stats: how the world is used, and who visits the site: mockup

A design proposal, not code. Everything on the sheet is new, and every figure
is invented.

A fourth admin area, **Stats**, beside People, Content and Data
(`../web-navigation/`), with four pages: **Overview**, **World**, **Web** and
**Places**. It counts what the server can already see and keeps counts, not
people:

- **The simulator** records sessions (account, app, version, start, end, from
  the handshake, which carries neither app nor version today), refused
  handshakes, a count of what it relays by kind (never content: private and
  community messages are sealed), and once a minute the 250 m square each
  avatar stands in.
- **The web pages** — cartopolis.org, about.cartopolis.org and the viewer —
  send `POST /api/stats/hit` on every change of address. A visitor is a hash
  of a daily secret, the IP address, the browser and the site; the secret is
  never written and is replaced at midnight, so no visit can be joined to the
  next day's. No cookies, so no consent banner.
- **Caddy** counts only APK downloads, which have no page to report them.

Kept in `stats.db` beside `world.db`, its own file: per-account sessions for
90 days (what "came back" and "active in 30 days" need), then daily totals;
the web only as daily totals. Never kept: IP addresses, browser ids, message
content, positions finer than a square, or which person read which page. A
square on the map, and a cohort row, needs 5 different people to show.

Drawn in the web UI's palette (`../web-navigation/style.css`); chart colours
are the dataviz reference palette's first three dark slots, validated against
the panel colour.

## v2: a community's own numbers, for its moderators

Drawn 2026-10-18, after v1's questions were answered. A **Stats** page in a
community's Moderation section, for its owner and moderators: members, active
members, joining and leaving per week, messages and people writing per topic,
fediverse followers per server, and where members share places. It counts
nothing new — Admin › Stats's counts cut to one community — and never names a
person; a topic where fewer than 5 people wrote says so instead of showing
numbers.

![v2-community](v2-community.png)

Decided 2026-10-18: it also counts **visitors** who are not members — to the
community's public page and its posts — by the same daily-hash rule as the
site. Not drawn yet.

## v1

![v1](v1.png)

## Decided (2026-10-18)

v1's five open questions:

- **A community's moderators see its own numbers** — drawn as v2.
- **No public numbers** on about.cartopolis.org; v1's "Public numbers" switch goes.
- **No page of a person's own sessions.**
- **Sessions are kept per account for 90 days**, so retention, "active in 30
  days" and the funnel's last two steps stay.
- **Global Privacy Control is not honoured**: nothing counted identifies
  anyone, so every browser is counted the same.

Considered and not proposed: self-hosted Plausible or Umami (another service on
a near-full VPS, and blind to the world), Matomo or Google Analytics (cookies,
a third party), Caddy's access log for page views (the site changes page
without asking Caddy, and the log puts every IP on disk), and client-side
telemetry (the server already sees every session).
