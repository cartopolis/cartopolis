---
topic: chat
triggers: [chat.rs, chat_overlay.rs, "speech bubble", "transcript", "composer", "nearby chat", dock.rs]
updated: 2026-08-22
---

# Chat is not a panel

Chatting used to mean opening `RightPanel::Chat`. On a compact viewport a right
panel is the **whole screen** (`egui_manager::responsive_right_panel`), so in a
client whose Local channel is scoped by *how far away someone is standing*,
saying anything meant covering the world and the people in it. That is the
sentence the whole redesign follows from.

`systems::chat_overlay` is chat with no panel: a bubble over whoever spoke, an
ambient transcript that fades, and a composer that exists only while you write.
`systems::chat` keeps the state and one panel that is now a **log and its
settings** — the backlog, the rooms, the blocks, the microphone, the report form
— reached from ☰ rather than from the dock.

## What was measured against the old panel, and lost

**The composer was behind the keys.** `chat.rs` was the only text-entry surface
that never consulted `OnScreenKeyboard`; `hud`, `dock`, `place_pins`, `map_osd`,
`place_card` and `login_screen` all did. Worse than merely being covered:
`onscreen_keyboard::layout` flips the slab to the **top** edge when the bottom
one would cover the focused field, so tapping the chat field threw a 45 %-tall
keyboard over the tabs and the upper half of the backlog. You lost the message
you were replying to the moment you started replying.

That cannot be fixed inside a panel, and the keyboard's own module note says why:
*"we cannot scroll the panel or shrink its layout, because by the time this runs
the panel has already composed."* An `Area` can move. This is the load-bearing
argument for the overlay — see `onscreen_keyboard::expected_height`, which the
composer calls to clear the slab **before the slab exists**, because the frame
the keyboard first lays out is the only frame that decides its edge.

**Four rows of chrome stood between the history and the thumb.** Bottom-up:
Join-room row, separator, voice mode switcher, separator, Shout checkbox, then
the composer. All of it permanent, most of it used once a session. Shout is a
chip in the composer row now, voice and rooms are in the panel, and Join is not
in the send path at all.

**The tabs were desktop widgets.** `ui.selectable_value` in a
`horizontal_wrapped`: text-height targets well under the 48 dp the theme enforces
everywhere else, and they *wrapped onto new lines* as rooms were joined, silently
pushing the history down. There is one scope chip now and it cycles.

**`ChatState::my_id` was set by the handshake and read by nothing.** It had
carried the comment "to tag own lines" since it was written, so every line in the
app looked alike. Own lines are amber in the transcript.

## What the wire already gave us for free

`NetMessage::ChatMessage` carries `sender_id: u64` and `RemoteAvatar` carries
`sim_id: u64` — the same number. A bubble finds the entity that said the line
with no name matching, no lookup table and **no protocol revision**; the field
was simply being dropped on the floor by `network.rs`. `distance_m` and
`sent_at_ms` were likewise already there.

A side effect worth knowing: this is the first thing in the client that
identifies a nearby person at all. There are no nameplates on remote avatars.

## Three rules that are not preferences

**An `Area` that is faded out is still a live click target.** The transcript is
`interactable(false)` unless the composer is open — the rule being "you can act
on chat while you are in the conversation, and the map has the taps the rest of
the time". This is the same trap `place_pins` culls for (a clipped-away pin still
eats the tap) and `map_osd` hides for. The durable path to Block and Report is
therefore the panel, which is always reachable; the transcript's name menu is a
convenience, not the only route.

**A bubble is painted and never interactive**, for the sharper version of the
same reason: it floats *over the world*, where a tap means pick, walk or place a
pin. A chat balloon is not allowed to be in the way of that.

**Everything ages on `ChatState::clock`, not `sent_at_ms`.** Every clock in the
overlay measures how long *this screen* has shown a line. A server clock is
unsynchronised and skewed by flight time, so a bubble would vanish early or
linger depending on whose watch you asked.

## What was considered and not done

- **A half-height bottom sheet on compact** instead of the full-screen panel.
  Cheaper, and it would have kept the map — but it keeps chat as a *place you go*
  and still eats the bottom half of the screen while you type. The bubbles are
  what actually answer "who said that, and where are they".
- **A tappable bubble** opening the person menu, which the first sketch had. It
  loses to the rule above: over the world, the tap belongs to the world.
- **An unread badge on the Say disc.** Unnecessary once the transcript wakes on
  arrival — the notification *is* the message, which is most of the point of
  making chat ambient. Reconsider only if the transcript is ever suppressed in a
  state where messages still arrive.
- **Swipe-up on the transcript for scrollback.** The panel's log does this
  already and costs no new gesture. If the panel ever stops being the place you
  read back from, this is the thing to build.

## How to look at it

`--ui chat` (`shot_harness`) is the only way to photograph any of this: all three
pieces need a session and every scripted run is offline, so the flag seeds an
exchange, claims `my_id` for the local avatar and stands one more speaker in
front of the camera.

```bash
WGPU_BACKEND=vulkan cargo run -p cartopolis -- \
    --shot /tmp/chat.png --at 53.2194,6.5665,14 --look=-30,0 --ui chat
```

Two things it had to be taught, both facts about the *harness* rather than about
chat. The seeded lines are **re-stamped every frame**, because a capture waits
~15 virtual seconds for the tile streams to go quiet while a bubble is
deliberately short-lived — the first run photographed the demo speaker standing
in the street with nothing over her head. And the speaker **follows the camera**
rather than the avatar, because `--look`'s pitch does not reliably reach the
rendered camera (three different elevations in `--shot-mode avatar` produced one
identical frame), so a fixed offset photographs an empty street about half the
time.

## Verified, and not

Verified in the container, on lavapipe: the bubble lands over the speaker's head
with the name and body in it; the transcript renders bottom-left with own lines
in amber and non-local lines prefixed by their channel; the composer pill stacks
above the dock with the scope chip, Shout, Send and ✕. The layout arithmetic
(`stack_bottoms`, `map_osd::reserved_left_bottom`,
`onscreen_keyboard::expected_height`, `clear_of_keyboard`) is unit-tested, and
the projection is `place_pins`'s.

**Not** verified: anything on a phone. Nothing here has run on glass, so the two
halves that only a handset exercises — the keyboard reservation actually keeping
the slab at the bottom edge, and whether the transcript clears the thumbstick in
practice — are arguments from arithmetic. Colour and lighting are never evidence
on the software rasteriser (`docs/notes/headless-shots-software-renderer.md`).
And the *feel* — whether a 10-second hold is right, whether four transcript lines
is enough, whether the bubble should follow a walking avatar or stay put — is a
judgement only a person on glass can make.
