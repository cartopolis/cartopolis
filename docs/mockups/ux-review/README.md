---
topic: ux-review
triggers: [login_screen.rs, place_card.rs, dock.rs, ui.rs, dismiss.rs, communities.rs, chats.rs, "first run", onboarding, "sign in", "Back button", "destructive"]
updated: 2026-11-04
---

# UX review: what gets in the way

A proposal, not code. Requested 2026-11-04 ("analyse cartopolis ui/ux and
suggest improvements, make mockups first"). The 1 Nov round (`ui-directions/`)
was about how the app looks. This one is about what a person runs into: the
first launch, signing in, going to a place, a community, leaving safely, and
the words. The "Today" screens were captured from `main` with
`docs/phone-ui-shots.toml` (412×900 at scale 1.5, mac-1). Every "After" is
**new**, drawn in today's teal glass and today's dock, so each fix can land
without direction C.

Four problems, in order of cost to a new person:

1. **The social half is invisible.** A signed-out person gets a silent map.
   Signing in is Menu ▸ Sign in…, the Server field starts empty, and the app
   has no sign-up.
2. **Buttons describe the mechanism, not the result.** From / To / Beam,
   "to Route end", "Needs a GPS position", "simulator", "Polaroid".
3. **Chores push the content down.** A community opens on its moderation queue.
4. **Leaving is risky, and Back is unpredictable.** Block, Log out and the
   ring's Delete act at once. On Android, Back with a card open sends the app
   to the background, and on a page stack it drops every page at once.

Nothing decided yet.

![v1](v1.png)
