---
topic: calendar
triggers: [calendar, agenda, planning, "repeating meeting", recurrence, meetup.rs, systems::meetup, "meeting::Plan", Meetup, Rsvp, chats.rs, meeting.ts, "crates/atlas/web/src/components/pages/chat"]
updated: 2026-10-19
---

# Calendar: every plan you are in, in one place: mockup

A design proposal, not code. Everything in it is new.

Asked for 2026-10-18 ("add a calendar/planning"). Scope chosen then: **one
view of the plans you are already in**. That means the meet-ups (a place and
a time, `point::Meetup`) and meetings (a call with an agenda, `meeting::Plan`)
from every private chat, group and community topic, together with your
answers. It goes in both the app and the web page. Public community events
and personal entries were offered and not chosen.

The one new piece is in the app. Chat lines live only in memory there, so the
calendar needs its own small list of dated items in `platform::storage`. The
web already keeps every chat's history in IndexedDB and builds the calendar
from that.

## v2: repeating meetings

Decided on v1's questions, 2026-10-19:

1. No copying plans to the phone's own calendar.
2. No reminders while the app is closed, for now.
3. Meet-ups keep only "I'm going", for now.
4. Big communities: each gets the switch "only the plans I answered" (v1, H).
5. **Repeating meetings are in the first version.** This sheet draws them.

Only meetings repeat. A plan carries a rule (every week, every 2 weeks, or
every month on the same weekday, with an optional end date) and every device
works out the dates from it. People answer one date, or every date at once.
The planner can skip a date or stop the series, sent as an ActivityStreams
`Update` that only the plan's author can make. An older app sees the first
date as an ordinary plan. This changes the sealed message only; there is no
server or wire protocol change.

![v2](v2-repeat.png)

## v1

![v1](v1.png)
