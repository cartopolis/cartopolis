---
topic: admin-accounts
triggers: [accounts.tsx, "elements/account", "crates/atlas/web", AccountRow, admin accounts]
updated: 2026-11-14
---

# Admin › Accounts: a compact list with numbers on opening

The accounts list on a phone was a tall card per account (five on a screen),
showing only the creation date and the address. The proposal: one line per
account, sorted and filtered by chips, and a tap that opens the line in place
with what the account has done.

## v1

![v1](v1.png)

Most numbers are already in `world.db` and only need counting (communities,
posts, prims by owner, buildings owned, contacts, chat devices, linked sign-ins,
last seen from `sessions.touched`, last place from `accounts.last_lat/lng`).
Drawn dashed: a sign-in count and time online per day, which nothing records yet.

## Decided (2026-11-14)

**A2**: the compact list and the opened line as drawn, *with* the new tracking —
sign-ins counted, and time online kept per day, so the 30-day bar and "days
active" can be drawn. That history starts empty on the day it lands.
