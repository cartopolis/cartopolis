---
topic: contacts
triggers: [contacts.rs, chat_people.rs, WherePeople, PeopleWhere, "contact request", "message request", RequestContact, ContactsState, PersonInfo, V21__contacts.sql, request_policy, link_code, "Say hi", tap_avatar]
updated: 2026-09-30
---

# Contacts and message requests

Design: `docs/mockups/chat-world/v5-contacts.html`. Server rules:
`crates/simulator/src/contacts.rs`. Client: `systems::contacts` (the screens,
the person card, the tap on an avatar).

Before protocol 27 anybody could open an encrypted chat with anybody by typing
a username. Now a contact is somebody you accepted, both ways round, and
anybody else's first message is a **request** that waits in a tray above your
chats until you accept, decline, block or ignore it.

## The server refuses; the client only draws

Every rule is checked in the simulator, the way blocks are
(`user-safety.md`): who may send you a request (`RequestPolicy`, default
*people you have been near in the last hour, or who share a contact*), how many
a day (5 for an account younger than a week, 30 otherwise), and that a private
chat with somebody who is not a contact holds **one** application message until
the request is answered. A modified client that skips the UI gets the same
answers.

The one-message rule works on sealed bytes the server cannot read: it counts the
sender's non-commit entries in the direct chat's log. Commits are never held —
a device that misses one can read nothing in that chat again (`e2ee.rs`, rule 4).

## A decline is silent, and that shapes three code paths

- The sender's outgoing list shows a declined request exactly as a waiting one.
- A request to somebody who has blocked you is stored as declined on arrival.
- **Claiming keys still works after a decline** (`may_invite`), because
  refusing would be the tell. Instead `silenced_by` takes the decliner out of
  the chat's delivery list (`leave_direct_chat`: membership row and pending
  invitations, **no commit** — a commit is visible to the sender) and the send
  path delivers to the sender alone. Declining an existing chat does the same.

A declined request blocks a resend for 30 days, and withdrawing it does not
reset that, or withdraw-and-resend would be a way round it.

## "Near" is measured on the server, in memory

`SimWorld::note_meetings` records every pair of located avatars within
`MEET_RADIUS_M` (100 m, twice the Here chat's say radius) every 2 s, keyed by
username, pruned to an hour. It is never written to disk: an hour of who stood
where is the kind of thing not to keep. The client is never told whose an
avatar is until it asks (`LookupPerson { avatar_id }`), which is also rate
limited — a lookup says whether a username exists, so an unthrottled one lists
every account.

*Only with my link* hides the account from name lookups and from the *Near you*
list; a refusal to a stranger reads "there is nobody called …" rather than
"only through their link", unless they are standing next to each other.

## A first message waits for its request

*Say hi* sends `RequestContact`, then waits for the server's `ContactsState` to
list the request before starting the chat (`ChatState::private_hello`,
`drive_private_chats` sends it on the chat's `Opened` event). Starting the chat
first would race the request and be refused keys.

## The link reuses `/go`

`https://cartopolis.org/go?contact=name~code` — the path Android already claims
and the web client already serves, so it needed no Caddy or manifest change.
The mockup drew `/u/name`; that would need both. A link lets the holder send a
request whatever the setting says; *Reset link* issues a new code.

## Where the people in a chat are (protocol 30)

A private chat's World view draws each other member at their avatar, with what
they last said floating over them (`systems::chat_people`, design v5 screen B).
The client cannot know this itself: the avatar is usually far outside the
snapshot's area of interest, and its sim position is in its own client's
frame. So the view asks `WherePeople` every 2 s and the server answers from
`ClientIntent.geo`, only for the asker's **contacts who show that they are
online** (`contacts::visible_to`). Where an avatar stands is the same kind of
fact as whether it is online, so it rides the same switch; anybody else is
left out of the answer rather than marked, so it says nothing about them.
This is the avatar, never the phone — the real location is still only what a
person shares with one chat, for a time.
