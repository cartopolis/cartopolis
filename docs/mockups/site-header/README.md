---
topic: site-header
triggers: [crates/atlas/web, frame.tsx, cms-top, cms-nav, home-top, home-seg, home-crumbs, topBar, chat.tsx, "content pages", navigation]
updated: 2026-11-17
---

# One top bar for the content pages and the app: mockup

A design proposal, not code. Everything marked **new** does not exist today.

Since the content pages arrived (`../cms/`), cartopolis.org draws two top bars:
the content pages' frame (`src/cms/frame.tsx`: the wordmark, Community and
Wereld as plain links, NL | EN) and the app's (`chat.tsx` `topBar`: the mark and
wordmark, the Community | World | Admin switch, the breadcrumb, the account or
theme switch). The app's logo goes to `/chat`, so nothing in the app leads back
to the front page, and the site's own pages (`over`, `server`) are in neither
bar. v1 proposes one bar, the same on every page: the mark to the front page,
the app's places (Community, Wereld, Beheer in gold), a divider, the content
pages marked "in menu", then language and the account. The app's breadcrumb
moves to a thin second line that only the app has. On a phone the bar keeps
the mobile-header B layout (the mark, Community, Wereld) and puts the rest
behind one menu button.

Drawn in the web UI's light palette (`crates/atlas/web/src/css/theme.css`). The
"Now" strips are captures of the live site, 2026-11-17.

## v2

![v2](v2.png)

**v2 (2026-11-17), proposed.** On v1 the owner asked how the logo going to
the front page fits the navigation. v2 draws the site map first: the front page
is the root and the logo is its link, underlined when you are there. Under it
are two kinds of place: the site (CMS pages marked "in menu") on the left of the
bar and the app (Community, Wereld, Beheer) on the right. Everything deeper is
the second line, a path that starts at the logo on every page but the front
page. The phone menu lists the same structure: the site, the app, you.

## v1

**v1 (2026-11-17), superseded by v2.** Every link in one row, the app's places
first, with the breadcrumb on a second line only inside the app. It did not
show where the logo fits.

![v1](v1.png)

Open: the app's own words are English only, so the bar's language switch would
change the bar and the content pages but not the chat.
