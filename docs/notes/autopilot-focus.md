---
topic: autopilot-focus
triggers: [autopilot.ts, "focus", "focusPlan", "planRevision", "autopilot-plan", "planning ticket", "qa-gap", "the queue", atlas, autopilot.rs, "Autopilot tab"]
updated: 2026-09-09
---

# Steering the unattended lane at something, and who is allowed to write the queue

The autopilot had one source of work: the frontier table in `docs/direction.md`,
filed a row at a time for ever. That answers *"what next?"* and it cannot answer
*"spend the next fortnight on X"* — the maintainer's own priority, which is
usually not a row in that table and often not a data source at all. "Get this
solid on the Steam Deck runner" is the case that prompted the feature.

So there is a second queue. `focus` is a sentence in the atlas web UI
(the atlas web UI's Autopilot tab); `focusPlan` is an **ordered list of pieces of work**
that the web UI reorders, rewrites, extends and deletes. While it holds a row
nobody has started, it *replaces* the frontier — the two are not blended,
because a frontier ticket with "…and by the way, the Steam Deck" appended to it
is a ticket about neither thing.

The field is called `focusPlan` and the list is no longer only the focus's: see
*A finding is not a ticket* below. The name stayed because it is a hand-synced
seam — atlas's `crates/atlas/src/autopilot.rs`, the live file on the pipeline host and
this parser all have to agree — and renaming it buys nothing a comment does not.

## Who turns a sentence into a plan

Not the autopilot: it holds the merge token and still runs no language model,
which is the whole reason it is a separate container from the watcher. Not
the web UI's server either (atlas now, a separate TypeScript server when this was built) — it has no model, no key, and nothing else in it needs one.

It is a **planning ticket**, which is the same trick the QA and retrospective
lanes already use: a session does the judging, on a ticket, and answers in a
form a script can read. It posts a fenced ` ```autopilot-plan ` block of JSON
and the autopilot stores it. The alternatives that lost:

- **The web UI's server calls an API model directly.** A key on the pipeline host, a cost
  centre, and a second place that knows how to talk to a model — to reproduce
  something a session already does with the whole repository checked out beside
  it.
- **The plan lives in the repository** (`docs/focus.md`, read over the API like
  the frontier is). Nothing new to write, no second writer of the control file —
  and no way for the web UI to reorder it, since atlas has no Forgejo token
  and giving it one puts repository write access behind the admin GUI.

## Two writers on one file, and what each owns

`autopilot.json` used to have exactly one writer. It now has two, and the rule
that keeps that safe is ownership, not locking:

| | Ordering, wording, membership of an **unfiled** row | A **filed** row | `issue` |
|---|---|---|---|
| the web UI, via atlas (`crates/atlas/src/autopilot.rs`) | writes | refuses the edit | ignores what is sent |
| autopilot (`tools/autopilot.ts`) | appends only, never reorders or rewords | leaves it | writes |

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

**Appending is the only write.** A posted block never replaces the list, never
reorders it and never rewords a row: it adds rows to the end, drops any whose
title slugs to an id already there, and stops at twenty. That is what lets three
kinds of session write to one list without a lock and without an argument about
whose ordering wins — there is no ordering to lose. Two earlier rules are gone
with it: "stored only while the list is empty" (the web UI's lock on filed rows
made the list never empty again, so a new plan could never be answered) and its
successor "only while nothing is unfiled" (appending cannot overwrite, so there
is nothing to protect).

Asking for a fresh plan is *clearing the unfiled rows* plus a `planRevision`
bump, so the dedupe key does not recognise the question it answered last time.
Each harvested ticket is remembered in the lane's own state file under
`plan-from-<n>`, which is what stops a tick re-reading a block it has already
appended — without it, every tick would append the same list again.

## A finding is not a ticket

Until 2026-09-09 a QA pass filed each gap it found as its own `qa-gap` issue and
a retrospective filed a ticket per machinery problem and per direction proposal.
On that date 30 gap issues and 9 retrospective ones were open; **none carried a
label the build lane acts on, and not one had ever been closed** — the lane's own
retrospective had filed a ticket saying so ("34 filed, 0 closed"). They were also
counting against the board, and the board's size is what stops the lane filing
anything: nine open against a limit of two meant six days in which it filed
nothing at all.

The rule now is that **a ticket exists only for work that is about to be built**.
A finding is a row on the queue: the session posts the same `autopilot-plan`
block the planning session posts, on its own ticket, and the autopilot appends
it. The maintainer reorders and deletes rows in the web UI, and a row becomes a
ticket only when the lane reaches it. A direction proposal is not even a row —
it is a paragraph in the retrospective's report, posted on the long-lived
`Autopilot log` issue, because a row is a promise that something gets built.

What this costs is that a deleted row is gone: the queue is not a record. That is
why the report goes to the log thread, which is the copy that survives.

## A filed row is locked, in both writers

An item stops being a plan the moment it becomes a ticket: a session is building
it against the wording that was filed, so rewriting the row afterwards describes
the work as something nobody was asked to do. The web UI renders a filed row as
static text with a link to its ticket and no remove button, and
`writeAutopilotConfig` enforces the same thing rather than trusting it — a stale
tab is a second writer too. What is enforced is *membership and wording*, not
position: where a row sits is only how the queue reads, and the autopilot keys
everything on the id and the marker in the ticket body.

`issue` moved with it. The web UI used to echo back whatever it last read,
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
- **Not a way to go faster.** `ticketsPerDay` and `maxOpen` gate the queue
  exactly as they gate the frontier. The planning ticket is the one exception:
  it writes no code and holds no feature slot, and it is self-limited to one per
  (focus, revision) by its key.

## The planning pass is a lane, not the first half of the feature lane

It was the first half of `generateFocus`, and so it sat behind every gate in
front of the feature lane — a red `main`, the daily quota, `maxOpen`. All three
are about *code being built*, and this builds none: it opens no branch and files
one ticket that answers in a comment. Behind them it was unreachable in the one
state that reliably produces it — a focus typed while the board is busy — and
the web UI said "no plan yet" for as long as that lasted, with nothing anywhere
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
