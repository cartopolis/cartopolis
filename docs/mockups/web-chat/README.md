---
topic: web-chat
triggers: [chat.tsx, chat.css, home.css, sideBody, "People and groups", "Home list", "start a group", "leave group", "delete group", e2ee_groups, "Admin › Content"]
updated: 2026-10-17
---

# Web chat: a Home list you can read, and groups you can leave: mockup

A design proposal, not code. It builds on `../web-navigation/` (the page's
frame, agreed 2026-10-11) and `../communities/`, and covers what those left
undrawn: the Home sidebar of people and groups, and what can be done to a
group once it exists.

Two problems prompted it, both visible in `today.png` (2026-10-17). The Home
list is one cramped line per chat, with no last message or time even though the
worker sends both. And a group made by accident (*oosterparkwijk*, a
neighbourhood that should have been a community) can't be removed by anyone:
a plain group has no leave, no rename and no delete on the wire, and Admin has no
page for groups. The server never knows a group's name either, since the name is
sealed inside the group.

## v1

![v1](v1.png)

Open, as drawn in section 4 of v1:

- Who may delete a group for everyone: its maker (drawn), or any member?
- Does Admin get a Groups page (metadata only, under the reports-triage permission)?
- *oosterparkwijk* itself: wait for Delete for everyone. Deleting its rows by hand
  leaves it in every browser's list, because nothing tells a device that a plain
  group is gone.
- Rename in this round?
