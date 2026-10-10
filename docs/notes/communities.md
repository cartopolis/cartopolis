---
topic: communities
triggers: [community, communities, topic, topics, "crates/simulator/src/communities.rs", "net/communities.rs", "systems/gui/communities.rs", V31__communities.sql, CommunityDto, TopicAddPeople, TopicRemovePeople, E2eeRedacted, ModerateDelete, moderator, workspace, channel, rooms, ChatScope::Room]
updated: 2026-10-10
---

# Communities: being in one is being in every one of its topics

Protocol 39 replaced three overlapping ideas — the app's **rooms** (public,
unencrypted, joined by typing a name), the web's **workspaces** and their
public and private **channels** (joined by asking per channel) — with one
(`docs/mockups/communities/` v1–v3 has the design and the decisions). The
kinds of chat are now:

- **Here** and **Everyone**: the world's public chats. Not encrypted, by nature:
  the server chooses who hears a nearby line by where people stand.
- **A person** and **a group**: private chats with people you pick. Unchanged.
- **A community**: a named set of people with a few **topics**. Each topic is an
  ordinary encrypted group chat, and **every member is in every topic** — no
  per-topic asking, no private topic (something private is a group).

## What the server knows, and why it is the one deciding

The name, the description, the topic names, who is in it with which role, who
is asking to join and what was reported to its moderators. What is said stays
sealed like any private chat. Because the server holds membership in the clear,
it is the server that decides who must be added to or taken out of each topic;
but it holds no key, so it cannot do either. It **queues the work** —
`topic_joins` and `topic_removals` (V31) — and hands it to a device already in
the topic (`TopicAddPeople` with key packages it claimed itself, and
`TopicRemovePeople`): the asking device when it holds the topic, else any
connected one, else the next one to sync. A job handed out is not handed out
again for two minutes (`WORK_AGAIN_MS`), because each spends key packages.
This is the same machinery as adding a member's later devices
(`encrypted-chat.md`, "Every device of every member"), and the device checks
what it is asked against the community list it was last sent: it adds only
people who are members (`Device::add_allowed`).

A join settles when a commit whose invitation names them is accepted
(`e2ee::append`); a removal when the device that committed it says so
(`TopicRemoved`), since the server cannot read a commit. Delivery to a removed
person stops at once, before the commit: `e2ee_members` is the routing list and
is changed first. Their devices are told (`TopicLeft`) and forget the topic; a
device that was away is told on its next sync (`e2ee::topics_left`).

## Roles, and the line between a community and the server

- **Owner** (exactly one): appoints and removes moderators, hands the community
  over, deletes it. Cannot leave without handing it over; an owner whose
  account is deleted passes it to the longest-serving moderator, else member,
  else it is deleted (`communities::erase_account`).
- **Moderators**: let people in, add and remove them (from one topic or the
  whole community, with or without a block), delete messages, edit the
  community and its topics. Cannot remove the owner or each other.
- **Members**: talk, start topics when the community allows it, report a line
  to the moderators.
- **Server admins** have no say inside a community they are not running —
  they cannot read it. They act on *accounts* (suspend, delete), from the same
  person menu as a section only they see, through the existing
  `/api/admin/accounts` routes; and one who triages reports may delete a whole
  community (`ReportsTriage`).

## Joining

Open (join at once), asks first (a moderator confirms; a refusal stands for a
week, `REFUSAL_MS`), or invite only (only a moderator adds). A **listed**
community is in the directory (`CommunityDirectory`), with its name,
description, topic names, moderators and size public on the server; an
unlisted one is reached only by being added. Somebody added reads from the
commit that added them, not before: history for newcomers still needs the
backup that is not built.

## Deleting a message is more than a request

A moderator's delete (`ModerateDelete`) is enforced by the server, which is
what makes moderation mean something in an encrypted chat: the log entry keeps
its place (sequence numbers must stay gapless, or devices resync forever) but
its bytes are overwritten and `redacted_by` is set (V31), and every later sync
replays an `E2eeRedacted` in its place. Commits can never be deleted — a device
that misses one can read nothing in that chat again. A device that had already
shown the line hides it on the live `E2eeRedacted`; what it already decrypted
was on its screen, which no protocol can take back.

## Not built

A place a community is pinned to (the mockup's "Near you"), invite links,
pictures for communities, muting a topic, and history for newcomers. Reports
about a person across the server are on the operators' pages, not in the
person menu.
