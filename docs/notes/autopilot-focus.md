---
topic: autopilot-focus
triggers: [autopilot.ts, "focus", "focusPlan", "planRevision", "autopilot-plan", "planning ticket", cartopy, "Autopilot tab"]
updated: 2026-09-09
---

# Steering the unattended lane at something, and who is allowed to write the queue

The autopilot had one source of work: the frontier table in `docs/direction.md`,
filed a row at a time for ever. That answers *"what next?"* and it cannot answer
*"spend the next fortnight on X"* — the maintainer's own priority, which is
usually not a row in that table and often not a data source at all. "Get this
solid on the Steam Deck runner" is the case that prompted the feature.

So there is a second queue. `focus` is a sentence in the admin console
(cartopy's Autopilot tab); `focusPlan` is an **ordered list of pieces of work**
under it that the console reorders, rewrites, extends and deletes. While the
focus says something it *replaces* the frontier — the two are not blended,
because a frontier ticket with "…and by the way, the Steam Deck" appended to it
is a ticket about neither thing.

## Who turns a sentence into a plan

Not the autopilot: it holds the merge token and still runs no language model,
which is the whole reason it is a separate container from the watcher. Not
cartopy either — it has no model, no key, and nothing else in it needs one.

It is a **planning ticket**, which is the same trick the QA and retrospective
lanes already use: a session does the judging, on a ticket, and answers in a
form a script can read. It posts a fenced ` ```autopilot-plan ` block of JSON
and the autopilot stores it. The alternatives that lost:

- **cartopy calls an API model directly.** A key on the pipeline host, a cost
  centre, and a second place that knows how to talk to a model — to reproduce
  something a session already does with the whole repository checked out beside
  it.
- **The plan lives in the repository** (`docs/focus.md`, read over the API like
  the frontier is). Nothing new to write, no second writer of the control file —
  and no way for the console to reorder it, since cartopy has no Forgejo token
  and giving it one puts repository write access behind the admin GUI.

## Two writers on one file, and what each owns

`autopilot.json` used to have exactly one writer. It now has two, and the rule
that keeps that safe is ownership, not locking:

| | Ordering, wording, membership of an **unfiled** item | A **filed** item | `issue` |
|---|---|---|---|
| cartopy (`server/autopilot.ts`) | writes | refuses the edit | ignores what is sent |
| autopilot (`tools/autopilot.ts`) | only appends to a **fully filed** list | leaves it | writes |

Both do read-merge-rename through a temp file in the same directory. A collision
can therefore cost at most one stamp, and a lost stamp is recovered on the next
pass: `filedIssue("focus-<id>")` finds the ticket by its own marker and re-stamps
it rather than filing the work twice. **Filing twice is the one mistake this
lane cannot take back**, which is why the marker and not the stamp is the
authority.

**The autopilot's config volume has to be writable by it**, which it did not
have to be before. If it is mounted read-only the planning ticket is still
filed and its answer can never be stored, so the lane sits there filing nothing
— it says so on every pass (`could not store the plan … is <path> writable from
here?`) and does not take the QA and retrospective lanes down with it, which is
the only reason that failure is survivable rather than an hourly stack trace.

A plan is stored only while nothing in the console's list is **unfiled** —
otherwise a second planning session would overwrite an evening's ordering. It
used to be "only while the list is empty", and the console's lock on filed rows
(below) retired that: a filed row can no longer be deleted there, so after the
first ticket the list would never be empty again and a new plan could never be
answered. Filed rows are history — they stay where they are and a new plan is
appended past them, through `planItems` so a fresh item cannot take an id a
filed one already answers to.

Asking for a fresh plan is therefore *clearing the unfiled rows*, plus a
`planRevision` bump so the dedupe key does not recognise the question it
answered last time. The bump is what makes the key the memory here: the
exhausted-plan state is the same state the planning ticket was read in, so
without it every tick after the last item was filed would read that ticket's
block again and append the whole plan a second time. The key goes into the
autopilot's own state file once its answer is stored.

## A filed row is locked, in both writers

An item stops being a plan the moment it becomes a ticket: a session is building
it against the wording that was filed, so rewriting the row afterwards describes
the work as something nobody was asked to do. The console renders a filed row as
static text with a link to its ticket and no remove button, and
`writeAutopilotConfig` enforces the same thing rather than trusting it — a stale
tab is a second writer too. What is enforced is *membership and wording*, not
position: where a row sits is only how the queue reads, and the autopilot keys
everything on the id and the marker in the ticket body.

`issue` moved with it. The console used to echo back whatever it last read,
which is a lost stamp whenever the autopilot files something between that read
and the save; the server now takes the number from the file by id and ignores
what was sent, so the field is the autopilot's alone in fact and not only by
convention.

## What the focus is not

- **Not a licence.** The fences in `shipReady` are untouched: a focus asking for
  a wire-protocol bump files tickets that get built and then parked for a person.
- **Not an instruction to this process.** The text reaches a *session's* prompt,
  by string interpolation, and nothing else. That is a place anyone with a
  Forgejo account already reaches by opening an issue, so it is not an
  escalation — but the autopilot's own sequence must stay fixed, and no future
  field may be read as a command here.
- **Not a way to go faster.** `ticketsPerDay` and `maxOpen` gate the focus lane
  exactly as they gate the frontier. The planning ticket is the one exception:
  it writes no code and holds no feature slot, and it is self-limited to one per
  (focus, revision) by its key.

## The planning pass is a lane, not the first half of the feature lane

It was the first half of `generateFocus`, and so it sat behind every gate in
front of the feature lane — a red `main`, the daily quota, `maxOpen`. All three
are about *code being built*, and this builds none: it opens no branch and files
one ticket that answers in a comment. Behind them it was unreachable in the one
state that reliably produces it — a focus typed while the board is busy — and
the console said "no plan yet" for as long as that lasted, with nothing anywhere
saying why. Measured 2026-09-09: nine open tickets against a limit of two and a
red `main`, a focus and four `planRevision` bumps, and not one planning ticket
filed.

So `planFocus` is its own step of the tick, beside the QA and retrospective
lanes, and runs *before* `generate` — a plan stored on a tick has its first item
filed on that same tick rather than an hour later. `enabled` still gates it,
because that knob means "file no new tickets" and this files a ticket.

The plan block is read from **any** comment by **anybody**, which is deliberate
rather than overlooked — see the escalation argument above — and the last valid
block on the ticket wins, so a session that posts a draft and then a correction
means the correction.
