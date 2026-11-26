---
topic: paired gestures
triggers: [core/src/greet.rs, simulator/src/state/greet.rs, simulator/src/net/greet.rs, systems/player/greet.rs, systems::greet, Greeting, OfferGreeting, GreetingOffered, AcceptGreeting, GreetingStarted, "Emote::Handshake", "Emote::HighFive", partner_at, pair_up, ClipPartner, spawn_clip_partner, handshake, highfive, "high five", "protocol 49"]
updated: 2026-11-15
---

# Greeting another avatar: a handshake and a high five

Asked for by the owner as "more interaction with other avatars" (2026-11-15).
One avatar offers, the other accepts, and the sim starts both bodies at once.
Wire protocol 49: `OfferGreeting` / `AcceptGreeting` from clients (kinds 128,
130), `GreetingOffered` / `GreetingStarted` from the sim (129, 131), and
`Emote::Handshake` / `HighFive` (plus `Throw`, a plain emote). The clips are
in `makehuman-avatars.md`.

## Where it lives

- `cartopolis_core::greet` — the `Greeting` type, the reach and timing
  constants, `Greeting::partner_at` (where the partner stands, from the clip)
  and `pair_up` (the arithmetic both ends share).
- The sim: `state/greet.rs` (`SimWorld::offer_greeting`, `accept_greeting`,
  the withdrawals and the lapse) and `net/greet.rs` (the handlers, the blocks,
  the rate limit, telling the other side).
- The client: `systems::greet` (the open offers, the turn when one starts),
  the person card's two buttons (`contacts::render_person_card`), and the
  interaction prompt's first button (`interact::drive_interaction_prompt`).

## How it goes

1. A taps B's avatar; B's person card has "Shake hands" and "High five".
   Pressing one sends `OfferGreeting` and closes the card; A's prompt line
   reads "Shake hands — waiting for B".
2. The sim checks and sends B `GreetingOffered`. B's prompt shows
   "Shake hands with A [E]" and a ✕, above anything else it would offer. **E
   accepts on a keyboard; on touch the button is the way** — the same prompt
   the bench and the ball use, so there is one place to look. With several
   offers open, the nearest offerer's is shown. ✕ (or doing nothing) lets it
   lapse: nothing is sent, so the offerer is not told "no".
3. On `AcceptGreeting` the sim checks again, stands the two where the clip
   needs them, turns them, and sets both avatars' emote to the greeting with
   **one sequence for the pair** (past both their own), in one call under the
   world lock. The next snapshot shows both starting to everyone, so every
   viewer plays them in step; each of the two also gets `GreetingStarted` with
   the yaw to face, which the client takes as its facing at once.

## What the sim checks

Everything, from its own positions — never the client's word:

- **Reach**: both placed, within `GREET_REACH_M` (2.5 m, flat — the pick-up's
  reach) and `GREET_STEP_M` (0.5 m) in height, at the offer *and* at the
  acceptance. Out of reach is a 422 "walk up to them first".
- **Not to oneself, not seated** (422), **nobody there** (404).
- **Blocks, either way** (403, worded "you cannot greet them" so it says
  nothing about who blocked whom): `ChatHub::blocked_between`, the live block
  sets the chat fan-out uses, so a block made after an offer refuses its
  acceptance too. Unlike chat, where a block only withholds the blocked
  one's lines from the blocker, a greeting is made together, so a block either
  way refuses it (`user-safety.md`'s rule: the server refuses).
- **Lapse**: an offer lives `OFFER_SECS` (10 s); the client counts down the
  same clock and drops it, so a lapse is not announced.
- **Moving cancels**: an intent that moves, jumps or sits withdraws every
  offer the avatar made or was made; the one it had offered is sent
  `GreetingOffered { greeting: None }` and its prompt goes.
- **Only the sim starts a greeting**: `Emote::is_paired` emotes in a
  `ClientIntent` are dropped, or anyone could shake an empty hand beside a
  stranger. (The MCP `emote` tool refuses them for the same reason.)
- **Rate**: offers share a bucket of 3, refilling one per 2 s (429 past it).
  Acceptances are not limited: each can only take an offer somebody else's
  bucket paid for.
- **The facing is held** for `GREET_HOLD_SECS` (2.5 s): intents already on
  their way with the old facing would otherwise turn a body away mid-gesture.
  Walking off lets go at once.

## Placement: the sim moves them, point-symmetric

Both avatars play **the same clip** — one subject's side of a two-person take
— and the sim stands them **point-symmetric about the middle between them**:
each sees the other at `partner_at` in its own frame, and they face opposite
ways. Then each right palm, at half the partner offset, lands on the same spot
halfway between them, by construction (`the_palms_meet_halfway`). The offset
is twice the palm's reach at the moment of contact, measured on the default
man by `human-pack --add-clip` (handshake: 0.85 m apart, partner 18° to the
right; high five: 0.81 m, 31°); the takes' own subjects stood 0.74 m and
0.9 m apart. `greetings_meet_halfway` (the pack's test) holds the pack to
those numbers.

The sim **moves both positions** (`accept_greeting`, about their old middle,
along the line between them, at most ~0.85 m each from 2.5 m apart), rather
than each viewer drawing a display offset as a seat does. Chosen because:
the offset would have to be unwound when the gesture ends — two people
sliding apart again after shaking hands — where a move leaves them standing
where they shook; nothing separates where they are from where they are drawn
(the thing the seat's offset needed policing for, `sitting-down.md`); and it
needed no new wire field or draw code: remote viewers glide both over by
their usual smoothing, and the local client's reconciliation eases its own
avatar the same way. The cost is that slide: up to ~0.8 m over about a
second with the feet still, which a step clip would hide and none is cut.

**What lost:** each viewer computing the pair's layout from both avatars'
positions (needs a partner id on the wire, and the unwind above); one clip per
subject (A plays A's side, B plays B's) — the exact take, but it needs a role
on the wire and twice the clips, and the point-symmetric layout already makes
the right hands meet; facing straight at each other with the clip's own
heading — the hands would then miss sideways by about twice the palm's offset
to the right (0.26 m for the handshake).

## Limits

- The hands meet exactly at one moment; around it they drift with the take.
  Planted, the handshake stays within 0.1 m of its partner's through the
  shake (take frames 90–170) and opens to 0.2 m at the let-go; the high
  five is a slap, together for a frame or two. Bodies of other sizes reach
  differently; the spacing is the default man's.
- No client prediction: the receiver presses E and sees it start a round trip
  later, with everyone else.
- An offer is only seen in Follow (it is the interaction prompt). A person who
  is not looking at their avatar does not see it.
- `--clip handshake` / `--clip highfive` draw a stand-in partner
  (`avatar::ClipPartner`): offline there is nobody else, and the gesture can
  only be judged with both bodies.
