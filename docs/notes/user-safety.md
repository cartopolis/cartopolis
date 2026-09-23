---
topic: user-safety
triggers: [moderation.rs, chat.rs, "block", "report", "App Store", "guideline 1.2", "account deletion", "5.1.1", blocks, reports, SAFETY_CONTACT]
updated: 2026-08-16
---

# What the stores require of an app with user-generated content, and where each
# piece lives

This is an **external-world constraint**, not a design preference: the four
things below are what Apple's App Review Guideline 1.2 asks of any app carrying
user-generated content, plus 5.1.1(v) on account deletion. Google Play's
User Generated Content policy asks for the same shape. Cartopolis has chat,
proximity voice, shared prims and author-written scripts, so it is squarely in
scope, and none of this is optional at submission time.

Checked against the guidelines as published on 2026-08-16.

| Requirement | Where it lives |
|---|---|
| Filter objectionable material | `cartopolis_core::moderation`, applied in `net.rs`'s `ChatSend` arm |
| Report mechanism, with responses | `NetMessage::ReportUser` → `reports` table → `GET/PATCH /api/admin/reports` |
| Block abusive users | `NetMessage::SetBlocked` → `blocks` table → `ChatHub` delivery |
| Published contact information | `settings::SAFETY_CONTACT`, printed in About and in Settings ▸ Account |
| Account deletion inside the app | `NetMessage::DeleteAccount` → Settings ▸ Account |

## The rule the whole surface turns on: the server refuses, not the client

Every one of these is enforced where the message is fanned out, never where it
is drawn. A client can be rewritten; "the block is applied in the UI" is not a
block, and a reviewer who asks the obvious question about a modded client is
right to. The client's copies exist only for things the server cannot do — the
blocked list draws the Unblock control and hides lines that *had already
arrived* when the block was made.

`crates/simulator/tests/moderation.rs` asserts each refusal at the wire for that
reason.

## Things that were decided one way and could plausibly be decided the other

- **A block is keyed by username, not by avatar id.** An avatar id is minted per
  connection and reused, so a persisted block against one would eventually mute a
  stranger. `accounts.username` is `UNIQUE` and is the identity the wire already
  carries.
- **A deleted account's chat scrollback is anonymised, not deleted.** Removing one
  side of a conversation leaves the rest unintelligible for everyone else who took
  part in it. The name is the personal datum, so the name is what goes
  (`db::DELETED_ACCOUNT_NAME`).
- **Blocks *against* a deleted account survive it.** Deleting the account is
  exactly what someone would do to shed a block, and the username can be
  registered again.
- **The word list is replaceable and its file is a hard error if missing.**
  Falling back to the seed list when an operator's `moderation_words_file` cannot
  be read would run different filtering than was configured, and the only person
  to notice would be whoever it failed to protect.
- **The filter masks, it does not refuse the message.** A refused line tells the
  author precisely which word to obfuscate; a masked one reaches the room with the
  slur gone and nothing to iterate against.
- **Reports keep the *unmasked* line.** Masking is for the room. An operator
  deciding what happened needs what was actually typed, and for proximity chat the
  report row is the only copy that exists at all — `Local` is never stored.

## What is still missing before a submission

- **`SAFETY_CONTACT` must be a real, answered mailbox.** It is
  `safety@cartopolis.org` today and nothing routes it. An unrouted address
  satisfies the letter of the guideline and fails the person using it.
- **A published terms/community standards document.** The guideline expects a EULA
  or community standards that reports are judged against; there is none.
- **Nobody is watching the queue.** `/api/health` reports `open_reports` so
  existing monitoring can, and the wire handler logs each report at `warn` — but
  "timely responses to concerns" is a commitment a person makes, not a table.
- **The admin GUI has no reports screen.** The endpoints exist; the atlas console
  (`crates/atlas/console` — see `hosted-simulator.md`) has not been taught about them.
- **Voice is blocked but not filtered**, which is unavoidable: the payload is an
  opaque codec frame the sim never decodes. Reports are the only recourse there,
  and that is worth saying out loud if a reviewer asks.

## The scripting question, and why it is answered by a build flag

Guideline 2.5.2 forbids downloading and executing code that changes an app's
features; 4.7's list of permitted interpreted content (HTML5/JS mini-apps,
chatbots, plug-ins, emulators) does not name Luau. Roblox is a live counter-
example, but there is no need to argue it here: `mlua` is a **simulator-only**
dependency, and since solo mode's removal (2026-08-21) the client ships no
interpreter in any build — `cartopolis_simulator` is only a dev-dependency for
tests. Author scripts run on the server and the app renders the result. Keep it
that way on any store lane, and the question does not arise.
