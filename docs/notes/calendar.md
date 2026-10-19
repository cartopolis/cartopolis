---
topic: calendar
triggers: [calendar.rs, systems::calendar, meeting.rs, meeting.ts, meeting-vectors.json, "cartopolis:repeat", "cartopolis:skip", "cartopolis:date", "cartopolis:every", "repeating meeting", "docs/mockups/calendar", chats.rs]
updated: 2026-10-19
---

# The calendar: the plans you are in, built on the device

Asked for 2026-10-18; the design and the decisions are `docs/mockups/calendar`
(v1 the calendar, v2 repeating meetings). One view of every meeting
(`meeting::Plan`) and meet-up (a `Point` with a `Meetup`) from every private
chat, group and community topic, with your answers, in the app's Community
panel and on the web page.

## Decided with the person steering (2026-10-19)

- **Only the plans already in chats.** Public community events and personal
  entries were offered and not chosen. A plan always belongs to a chat: those
  are the people who see and answer it.
- **No copy to the phone's own calendar** from the calendar: it would take a
  private plan out of the encryption into Google's or Apple's sync. (The web
  plan card's `.ics` download predates this and stays.)
- **No reminders while the app is closed**, for now. Push only wakes the phone
  for a call and the server cannot know when a sealed plan starts, so it would
  take an alarm the phone sets itself when the plan arrives.
- **Meet-ups keep only "I'm going"**, for now: Maybe and Can't come would be a
  format change.
- **A big community can be limited** to the plans you answered (per community,
  on the calendar's settings).
- **Repeating meetings are in the first version.** Meet-ups do not repeat.

## Why the app keeps a book of its own

The app holds chat lines **in memory only**; after a restart only the keys are
left. So `systems::calendar::Book` copies every plan, update, answer and
meet-up line into `platform::storage` (`calendar/<server>/<user>.json`, Data
namespace, preloaded on the web like `e2ee/`), and drops a plan 30 days after
its last date with its answers. Each device builds its own: a device sees only
what was said after it joined a chat, plus the shared history. The web page
needs no book: its worker already keeps every chat's history in IndexedDB.

## Repeats: the traps

- **A series keeps its local clock time in the reader's time zone**, not the
  planner's: `Plan::dates` takes the device's offset function. 20:00 stays
  20:00 across summer time; somebody abroad sees the series at 20:00 *their*
  time. The plan carries no zone. If that ever matters, add the planner's IANA
  zone to `cartopolis:repeat` rather than storing UTC steps (which drift an hour
  twice a year).
- **`until` is the start of the last date, inclusive.** Step it through the
  series' own dates (`chats::snap_until`), never by adding weeks: a week after
  a summer-time 18:00Z date is a winter-time 19:00Z date, and an `until` of
  18:00Z would drop it.
- **An answer with no date is for the first date**, which is every answer an
  older build sends. Per person per date the newest answer counts, an answer for
  every date included.
- **Skip and stop are an `Update` carrying the whole plan**, believed only from
  the plan's own sender (`meeting::current_plan`). Compare sender names as the
  chat shows them; both devices of one person show the same name.
- The app and the web each implement the rule; `crates/core/testdata/meeting-vectors.json`
  holds them to one format, not to one expansion. The core tests pin the
  dates (4th/5th weekday months, summer time).
