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
  Its moderators can also switch on **public posts**, the half anybody may
  read, published to the fediverse: `fediverse.md`. The two never mix.

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
description, topic names, moderators, size, picture and place public on the
server; an unlisted one is reached only by being added or by an invite link.

**Invite links** (protocol 40, `community_invites`, V32) are made by a
moderator and let whoever holds one join straight away, whatever the join
rule, for `COMMUNITY_INVITE_DAYS` (14). Two rules: a person a moderator removed
*with a block* stays out even with a link, and looking one up
(`CommunityInviteLookup`) answers the community as a one-entry directory, so an
unlisted community shows its face to somebody holding its link and nobody
else. The link is `https://cartopolis.org/go?community=CODE`: on `/go` because
that is the one path the app claims on Android, so a phone with the app opens
the app; on the desktop the viewer handles it, and the chat page reads
`?community=` too and takes a pasted link or code in Find.

## History for people added later

An MLS group cannot hand a newcomer what was said before they joined: the keys
for those messages are gone by design. So the device that adds somebody — or a
person's own new device — **packs the last `MAX_HISTORY` (300) messages it
holds**, seals them under a one-off key (`pack_history`), stores the sealed
bytes in the picture store and sends a `Payload::History` naming them, in the
chat itself, which only the members can read. The engine says when
(`Event::HistoryWanted`, once its add commit is confirmed); the receiving
device fetches, opens and merges only what is *older* than the first message
it holds, marked "shared by" whoever sent it. It is the sender's view, so it
can only be as complete as that device's history — a device added an hour ago
shares an hour.

## Pictures, places and Near you

A community's **picture** is a hash in the picture store (V32 `picture`),
uploaded as a plain square JPEG. Unlike a chat's pictures it is **not
encrypted**: it is the community's face in the directory, and a listed
community's face is public on the server anyway. The server checks only that
it is a hash; it never fetches anything a client names.

A **place** is a point and a name (`place_lat`/`place_lng`/`place_name`).
`DirectoryOrder::Near` takes the searcher's point (the app's map, the browser's
geolocation), ranks the listed communities that have a place by great-circle
distance (`distance_m`, haversine) and answers the distance with each. The
searcher's point goes to the server for that one query and is not kept.

## Muting a topic

On the device only: the app keeps muted topics in `user.json`
(`muted_topics`), the web page in `localStorage`. A muted topic shows no count,
adds nothing to its community's, and raises no browser notification. Nobody
else is told, and a mention is muted with the rest.

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

Reports about a person across the server are on the operators' pages, not in the
person menu.
