---
topic: message-wake-ups
triggers: [push.rs, simulator::push, wake_for_message, NOTICES_PREFIX, PushReceiver.java, Push.java, sw.js, web-push.ts, systems::push, "Web Push", UnifiedPush, "New message"]
updated: 2026-11-17
---

# A closed app or page is woken for a private message

A private message or topic line for somebody with **no chat device connected**
wakes their phone's app or their browser through the push channel that rings
calls (`calls.md`, "Ringing a closed app"): the phone shows "New message", the
browser the same, and opening it opens the chat. Built 2026-11-17.

## What a push says

One word: which of the two it is. Never who wrote or in which chat — the
server cannot read a sealed message anyway, and the push's carrier has no
business knowing.

- **A browser** gets the word encrypted to it (RFC 8291, `push::seal`, checked
  against the RFC's own example). The page registers its subscription's
  `p256dh` and `auth` keys after the address: `webpush:<url>#<p256dh>.<auth>`.
  An empty push is a call, which is what an older server sends.
- **A phone** gets it plain: `1` for a call (as before), `message` for a
  message. Its distributor is the person's own choice and can be self-hosted,
  and RFC 8291 in `PushReceiver.java` would be hand-written crypto with no
  library (no Gradle, `android-build.md`). So the distributor learns "call" from
  "message" — which their rhythm already gives away.

## Only a device that can tell them apart

An app or page from before this shows **every** push as "Incoming call". So a
message wakes only registrations that say they know better: a phone's address
prefixed `notices:` (`systems::push`), a page's carrying its keys. A device
that registers its address in the new form has the old form dropped
(`db::set_push_endpoint`), or a call would wake it twice. No protocol revision:
the marker rides in `SetPushEndpoint`'s string, as `webpush:` does.

## Who is woken, and how often

- Every member of the chat but the writer, minus whoever blocked the writer,
  and **minus every account with a chat device connected** (`ChatHub::
  wake_for_message`) — they got the message. An account connected only without
  a device (the world page, say) is still woken.
- Commits (membership changes) wake nobody.
- **Once per account per two minutes** (`MESSAGE_QUIET`), counted before any
  database work: a busy topic sends one ring per line. The notice is one per
  device and replaced quietly (`setOnlyAlertOnce`, `renotify: false`), so a
  second push would only post it again.
- `TTL` a day and `Urgency: normal`, against a call's minute and `high`: a
  message can wait for a phone that is off, and need not break Doze.
- Messages from somebody elsewhere wake too (`remote::deliver_here`).
- The push task and the server's web push key now run whether or not calls are
  on; `/api/push/key` answers on every server.

## Not done, and why

- **Muted topics still wake.** The web page keeps its mutes in the browser,
  told to nobody (`chat.ts`), and the push names no chat, so neither the server
  nor `sw.js` can tell. Carrying the chat id encrypted to the browser would let
  `sw.js` check — but it cannot read `localStorage`, only IndexedDB.
- **Mentions are not told apart.** A mention is inside the sealed message.
- **Fediverse notices and public Here/Everyone lines do not wake.** Notices are
  read when the app or page asks (`fediverse.md`); public lines are not stored.
- **Calendar reminders**: the server cannot see when a sealed plan starts.
- **The desktop app** registers no push address; a closed desktop app hears
  nothing, as for calls.

## Unverified

The server half is tested with a database, the encryption against RFC 8291's
example. **Not seen on a device or in a real browser** from this container: a
real distributor's `bytesMessage`, the Android notice, and Chrome/Firefox
decrypting the push and `sw.js` showing "New message". The check is a phone
with ntfy and the app closed, and a browser with the chat tab closed, each sent
a private message from another account.
