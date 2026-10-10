---
topic: communities
triggers: [workspace, workspaces, channels, channel, rooms, ChatScope::Room, community, communities, topic, chats.rs, "crates/atlas/web/src/components/pages/chat"]
updated: 2026-10-10
---

# Communities instead of rooms, workspaces and channels: mockup

A design proposal, not code. Everything in a community is new.

The app had **rooms** (unencrypted, joined by typing a name) and the web had
**workspaces** holding public and private **channels** that people asked to
join. The two never met, and both answered "a named conversation people can
join". This replaces all three with one rule: a **community** is a named set
of people with a few **topics**, and being in the community means being in
every topic. One-to-one chats, groups, Here and Everyone are unchanged.

## v1: the list, starting things, a community, a topic, the web

![v1](v1.png)

Open questions are on the sheet: whether a community can be found and joined
without being added, whether "topic" is the word, and what happens when a
community grows to hundreds.
