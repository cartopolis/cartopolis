/**
 * What a meeting leaves in its chat — a plan and its answers, polls and their
 * votes, notes, the record — read and written as the same ActivityStreams JSON
 * the app writes (`crates/core/src/meeting.rs`, which is the reference: its
 * tests pin the shapes, and `send_meeting` in the worker refuses anything that
 * does not read there). Each is a sealed chat message; the server sees none.
 *
 * An answer names what it answers by the object's `id`, never by the chat's
 * log position, and per person the newest answer counts.
 *
 * A plan may repeat (`docs/mockups/calendar/v2-repeat.html`): every week, every
 * other week or every month on the same weekday, with an optional last date and
 * dates taken out. It stays one plan, and every reader works out its dates from
 * the rule ([`dates`]), keeping the first date's clock time in the reader's own
 * time zone. An answer names its date (`cartopolis:date`) or every date
 * (`cartopolis:every`); one with neither is for the first date. Skipping a
 * date or ending the series is an `Update` carrying the whole plan again,
 * believed only from the plan's author ([`currentPlan`]).
 */
import type {Message} from '@/worker/history'

export interface AgendaItem {
    minutes: number
    name: string
}

/** How often a repeating plan comes back. */
export type Every = 'week' | 'month'

export interface Repeat {
    /** `week`: the same weekday every `interval` weeks. `month`: the same
     *  weekday of the month ("the 4th Thursday"); a month without it is passed over. */
    every: Every
    /** 1 to 4. */
    interval: number
    /** The start of the last date, if the series ends. Inclusive. */
    untilMs: number | null
}

export interface Plan {
    agenda: AgendaItem[]
    endMs: number | null
    id: string
    kind: 'plan'
    /** How it comes back, if it does. */
    repeat: Repeat | null
    /** Dates of the series taken out, as the start of each. Empty unless it repeats. */
    skip: number[]
    startMs: number
    text: string | null
    title: string
}

export type Rsvp = 'going' | 'maybe' | 'no'

/** Which of a plan's dates an answer is for: the date starting then, every
 *  date, or `null` — the first date, which is all an older build sends. */
export type AnswerFor = number | 'every' | null

export interface Poll {
    id: string
    kind: 'poll'
    multiple: boolean
    options: string[]
    question: string
}

export type NoteKind = 'note' | 'decision' | 'action'

export interface RecordNote {
    by: string
    kind: NoteKind
    text: string
}

export interface MeetingRecord {
    end_ms: number
    notes: RecordNote[]
    people: string[]
    polls: {question: string; results: [string, number][]}[]
    start_ms: number
    title: string
    transcript: [string, string][]
}

export type MeetingObject =
    | Plan
    | {date: AnswerFor; kind: 'answer'; plan: string; rsvp: Rsvp}
    /** The plan again, changed by its author: a date skipped, the series ended. */
    | {kind: 'update'; plan: Plan}
    | Poll
    | {choices: string[]; kind: 'vote'; poll: string}
    | {kind: 'note'; note: NoteKind; text: string}
    | {kind: 'record'; record: MeetingRecord}

const CONTEXT = 'https://www.w3.org/ns/activitystreams'
const RSVP_TYPE: Record<Rsvp, string> = {going: 'Accept', maybe: 'TentativeAccept', no: 'Reject'}
export const RSVP_WORDS: Record<Rsvp, string> = {going: 'Going', maybe: 'Maybe', no: "Can't come"}
export const NOTE_WORDS: Record<NoteKind, string> = {action: 'Action', decision: 'Decision', note: 'Note'}

/** The bounds the app writes within (`meeting.rs`'s `MAX_*`). */
const MAX_TITLE = 140
const MAX_TEXT = 2000
const MAX_ITEMS = 12
const MAX_SKIP = 100

const DAY_S = 86_400
const DAY_MS = DAY_S * 1000
/** How far [`dates`] looks before giving up on a rule. */
const MAX_STEPS = 2000

/** Trimmed and cut to `max` characters, counted as Rust counts them. */
function clip(s: string, max: number): string {
    return Array.from(s.trim()).slice(0, max).join('')
}

const mod = (a: number, b: number) => ((a % b) + b) % b

/** Days since 1970-01-01 for a date (`point.rs`, Howard Hinnant's algorithm). */
function daysFromCivil(y: number, m: number, d: number): number {
    const yy = m <= 2 ? y - 1 : y
    const era = Math.floor(yy / 400)
    const yoe = yy - era * 400
    const mp = (m + 9) % 12
    const doy = Math.trunc((153 * mp + 2) / 5) + d - 1
    const doe = yoe * 365 + Math.trunc(yoe / 4) - Math.trunc(yoe / 100) + doy
    return era * 146_097 + doe - 719_468
}

/** The inverse of [`daysFromCivil`]: [year, month, day]. */
function civilFromDays(days: number): [number, number, number] {
    const z = days + 719_468
    const era = Math.floor(z / 146_097)
    const doe = z - era * 146_097
    const yoe = Math.trunc((doe - Math.trunc(doe / 1460) + Math.trunc(doe / 36_524) - Math.trunc(doe / 146_096)) / 365)
    const doy = doe - (365 * yoe + Math.trunc(yoe / 4) - Math.trunc(yoe / 100))
    const mp = Math.trunc((5 * doy + 2) / 153)
    const d = doy - Math.trunc((153 * mp + 2) / 5) + 1
    const m = mp < 10 ? mp + 3 : mp - 9
    const y = yoe + era * 400
    return [m <= 2 ? y + 1 : y, m, d]
}

/** Unix ms as the app writes a time: RFC 3339, UTC, whole seconds. */
const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')

/**
 * An RFC 3339 time as the app reads one (`point::parse_rfc3339`): a `Z` or a
 * numeric offset, fractions of a second ignored. By hand rather than
 * `Date.parse`, which takes a dozen other shapes the app would refuse.
 */
export function parseTime(s: unknown): number | null {
    if (typeof s !== 'string') return null
    // As Rust's `i64::from_str`: digits with an optional sign, nothing else.
    const int = (p: string | undefined) => (p !== undefined && /^[+-]?\d+$/.test(p) ? Number(p) : NaN)
    const t = s.trim()
    const at = t.indexOf('T')
    if (at < 0) return null
    const date = t.slice(0, at).split('-').map(int)
    let rest = t.slice(at + 1)
    let offset = 0
    if (rest.endsWith('Z')) {
        rest = rest.slice(0, -1)
    } else {
        const i = Math.max(rest.lastIndexOf('+'), rest.lastIndexOf('-'))
        if (i < 0) return null
        const off = rest.slice(i)
        rest = rest.slice(0, i)
        const [h, mi] = off.slice(1).split(':')
        const hours = int(h)
        const minutes = mi === undefined ? 0 : int(mi)
        if (!Number.isInteger(hours) || !Number.isInteger(minutes)) return null
        offset = (off[0] === '-' ? -1 : 1) * (hours * 3600 + minutes * 60)
    }
    const clock = rest.split('.')[0].split(':')
    const [y, mo, da] = date
    const [h, mi] = [int(clock[0]), int(clock[1])]
    const se = clock[2] === undefined ? 0 : int(clock[2])
    if (date.length < 3 || clock.length < 2 || ![y, mo, da, h, mi, se].every(Number.isInteger)) return null
    if (mo < 1 || mo > 12 || da < 1 || da > 31 || h > 23 || mi > 59 || se > 60 || h < 0 || mi < 0 || se < 0) return null
    const secs = daysFromCivil(y, mo, da) * DAY_S + h * 3600 + mi * 60 + se - offset
    return secs >= 0 ? secs * 1000 : null
}

/** This device's UTC offset in seconds at an instant: what [`dates`] keeps the clock time in. */
export type Offset = (ms: number) => number
export const localOffset: Offset = (ms) => -new Date(ms).getTimezoneOffset() * 60

export function newId(): string {
    return `urn:uuid:${crypto.randomUUID()}`
}

/** A plan as its `Event`, without the `@context` (an `Update` carries it inside one that has it). */
function planJson(p: Plan): Record<string, unknown> {
    const text = p.text === null ? null : clip(p.text, MAX_TEXT)
    const v: Record<string, unknown> = {
        'cartopolis:agenda': p.agenda.slice(0, MAX_ITEMS).map((i) => ({minutes: Math.min(i.minutes, 600), name: clip(i.name, MAX_TITLE)})),
        'cartopolis:call': true,
        content: text || null,
        endTime: p.endMs === null ? null : iso(p.endMs),
        id: p.id,
        name: clip(p.title, MAX_TITLE),
        startTime: iso(p.startMs),
        type: 'Event',
    }
    if (p.repeat) {
        const r = p.repeat
        v['cartopolis:repeat'] = {
            every: r.every,
            interval: Math.min(4, Math.max(1, r.interval)),
            ...(r.untilMs === null ? {} : {until: iso(r.untilMs)}),
        }
        if (p.skip.length) v['cartopolis:skip'] = p.skip.slice(0, MAX_SKIP).map(iso)
    }
    return v
}

/** The rule in a plan's `cartopolis:repeat`, as strictly as the app reads it. */
function repeatFrom(r: unknown): Repeat | null {
    if (!r || typeof r !== 'object' || Array.isArray(r)) return null
    const o = r as Record<string, unknown>
    if (o.every !== 'week' && o.every !== 'month') return null
    const interval = o.interval ?? 1
    if (typeof interval !== 'number' || !Number.isInteger(interval) || interval < 0 || interval > 0xffffffff) return null
    if (o.until !== undefined && o.until !== null && typeof o.until !== 'string') return null
    return {every: o.every, interval: Math.min(4, Math.max(1, interval)), untilMs: parseTime(o.until)}
}

/** The plan in an `Event`; `null` for an event that is not a call's plan. */
function planFrom(v: unknown): Plan | null {
    if (!v || typeof v !== 'object') return null
    const o = v as Record<string, unknown>
    const str = (k: string) => (typeof o[k] === 'string' ? (o[k] as string) : null)
    if (o.type !== 'Event' || !('cartopolis:call' in o) || 'location' in o) return null
    const id = str('id')
    const startMs = parseTime(o.startTime)
    if (!id || startMs === null) return null
    const repeat = repeatFrom(o['cartopolis:repeat'])
    const skip = repeat && Array.isArray(o['cartopolis:skip'])
        ? (o['cartopolis:skip'] as unknown[]).map(parseTime).filter((t): t is number => t !== null).slice(0, MAX_SKIP)
        : []
    // Like serde: one malformed item and there is no agenda at all.
    let agenda: AgendaItem[] = []
    const items = o['cartopolis:agenda']
    if (Array.isArray(items) && items.every((a) => typeof a?.name === 'string' && (a.minutes === undefined || (Number.isInteger(a.minutes) && a.minutes >= 0)))) {
        agenda = items.slice(0, MAX_ITEMS).map((a) => ({minutes: a.minutes ?? 0, name: a.name}))
    }
    const text = str('content')
    return {
        agenda,
        endMs: parseTime(o.endTime),
        id,
        kind: 'plan',
        repeat,
        skip,
        startMs,
        text: text === null ? null : clip(text, MAX_TEXT),
        title: clip(str('name') ?? '', MAX_TITLE),
    }
}

/** The JSON it travels as. */
export function toJson(o: MeetingObject): string {
    switch (o.kind) {
        case 'plan':
            return JSON.stringify({'@context': CONTEXT, ...planJson(o)})
        case 'update':
            return JSON.stringify({'@context': CONTEXT, object: planJson(o.plan), type: 'Update'})
        case 'answer':
            return JSON.stringify({
                '@context': CONTEXT,
                object: o.plan,
                type: RSVP_TYPE[o.rsvp],
                ...(o.date === 'every' ? {'cartopolis:every': true} : o.date === null ? {} : {'cartopolis:date': iso(o.date)}),
            })
        case 'poll':
            return JSON.stringify({
                '@context': CONTEXT,
                [o.multiple ? 'anyOf' : 'oneOf']: o.options.map((name) => ({name, type: 'Note'})),
                id: o.id,
                name: o.question,
                type: 'Question',
            })
        case 'vote':
            return JSON.stringify({'@context': CONTEXT, 'cartopolis:choices': o.choices, inReplyTo: o.poll, type: 'Note'})
        case 'note':
            return JSON.stringify({'@context': CONTEXT, 'cartopolis:meeting': o.note, content: o.text, type: 'Note'})
        case 'record':
            return JSON.stringify({
                '@context': CONTEXT,
                'cartopolis:record': o.record,
                content: recordText(o.record),
                endTime: iso(o.record.end_ms),
                name: o.record.title || 'Meeting record',
                startTime: iso(o.record.start_ms),
                type: 'Article',
            })
    }
}

/** Read one back; `null` for a point or anything else. */
export function parse(json: string | null): MeetingObject | null {
    if (!json) return null
    let v: Record<string, unknown>
    try {
        v = JSON.parse(json)
    } catch {
        return null
    }
    if (!v || typeof v !== 'object' || 'location' in v) return null
    const str = (k: string) => (typeof v[k] === 'string' ? (v[k] as string) : null)
    switch (str('type')) {
        case 'Event':
            return planFrom(v)
        case 'Update': {
            const plan = planFrom(v.object)
            return plan && {kind: 'update', plan}
        }
        case 'Accept':
        case 'TentativeAccept':
        case 'Reject': {
            const plan = str('object')
            if (!plan) return null
            const rsvp: Rsvp = v.type === 'Accept' ? 'going' : v.type === 'TentativeAccept' ? 'maybe' : 'no'
            const date: AnswerFor = v['cartopolis:every'] === true ? 'every' : parseTime(v['cartopolis:date'])
            return {date, kind: 'answer', plan, rsvp}
        }
        case 'Question': {
            const id = str('id')
            const list = (v.anyOf ?? v.oneOf) as {name?: unknown}[] | undefined
            if (!id || !Array.isArray(list)) return null
            return {
                id,
                kind: 'poll',
                multiple: Array.isArray(v.anyOf),
                options: list.map((o) => (typeof o?.name === 'string' ? o.name : '')).filter(Boolean),
                question: str('name') ?? '',
            }
        }
        case 'Note': {
            if (Array.isArray(v['cartopolis:choices']) && str('inReplyTo')) {
                return {choices: (v['cartopolis:choices'] as unknown[]).filter((c): c is string => typeof c === 'string'), kind: 'vote', poll: str('inReplyTo')!}
            }
            const note = v['cartopolis:meeting']
            if (note === 'note' || note === 'decision' || note === 'action') return {kind: 'note', note, text: str('content') ?? ''}
            return null
        }
        case 'Article': {
            const r = v['cartopolis:record'] as MeetingRecord | undefined
            if (!r || typeof r.start_ms !== 'number') return null
            return {
                kind: 'record',
                record: {
                    end_ms: r.end_ms,
                    notes: r.notes ?? [],
                    people: r.people ?? [],
                    polls: r.polls ?? [],
                    start_ms: r.start_ms,
                    title: r.title ?? '',
                    transcript: r.transcript ?? [],
                },
            }
        }
    }
    return null
}

/** A line of text for a notification or a preview; the same words as the app's. */
export function caption(o: MeetingObject): string {
    switch (o.kind) {
        case 'plan':
            return `📅 ${o.title}`
        case 'update':
            return `📅 ${o.plan.title} changed`
        case 'answer':
            return RSVP_WORDS[o.rsvp]
        case 'poll':
            return `📊 ${o.question}`
        case 'vote':
            return o.choices.length ? `voted ${o.choices.join(', ')}` : 'took a vote back'
        case 'note':
            return `${NOTE_WORDS[o.note]}: ${o.text}`
        case 'record': {
            const mins = Math.floor((o.record.end_ms - o.record.start_ms) / 60000)
            return `Meeting record · ${mins} min · ${o.record.people.length} people`
        }
    }
}

/** A record as plain text, as the app writes it in `content`. */
export function recordText(r: MeetingRecord): string {
    const mins = Math.floor((r.end_ms - r.start_ms) / 60000)
    let out = `${mins} min with ${r.people.join(', ')}\n`
    for (const kind of ['decision', 'action', 'note'] as NoteKind[]) {
        const notes = r.notes.filter((n) => n.kind === kind)
        if (!notes.length) continue
        out += `\n${NOTE_WORDS[kind]}s\n`
        for (const n of notes) out += `- ${n.text}\n`
    }
    for (const p of r.polls) {
        out += `\n${p.question}\n`
        for (const [o, n] of p.results) out += `- ${o}: ${n}\n`
    }
    return out
}

// ── The dates of a plan ─────────────────────────────────────────────────────

/** How long each date lasts: its end, else its agenda's minutes, else an hour. */
export function durationMs(plan: Plan): number {
    if (plan.endMs !== null && plan.endMs > plan.startMs) return plan.endMs - plan.startMs
    const agenda = plan.agenda.reduce((n, i) => n + i.minutes, 0)
    return agenda > 0 ? agenda * 60_000 : 3600_000
}

export function isSkipped(plan: Plan, dateMs: number): boolean {
    return plan.skip.includes(dateMs)
}

/**
 * The starts of a plan's dates in `fromMs..toMs`, skipped ones included (they
 * are shown, struck through). The dates keep the first date's local clock time
 * across a change of `offset` — a meeting at 20:00 stays at 20:00 after the
 * change to winter time — which is why the offset is a function and not a
 * number. `Plan::dates` in the app, step for step.
 */
export function dates(plan: Plan, fromMs: number, toMs: number, offset: Offset = localOffset): number[] {
    const rule = plan.repeat
    if (!rule) return plan.startMs >= fromMs && plan.startMs < toMs ? [plan.startMs] : []
    const interval = Math.min(4, Math.max(1, rule.interval))
    const off0 = offset(plan.startMs)
    const local0 = Math.floor(plan.startMs / 1000) + off0
    const day0 = Math.floor(local0 / DAY_S)
    const clock = local0 - day0 * DAY_S
    // Local seconds back to UTC: guess with the first date's offset, then use
    // the offset at the guess.
    const toUtc = (day: number): number | null => {
        const local = day * DAY_S + clock
        const guess = local - off0
        if (guess < 0) return null
        const utc = local - offset(guess * 1000)
        return utc < 0 ? null : utc * 1000 + (plan.startMs % 1000)
    }
    const [y0, m0, d0] = civilFromDays(day0)
    const weekday = (day: number) => mod(day + 4, 7) // 1970-01-01 was a Thursday
    const nth = Math.trunc((d0 - 1) / 7)
    const out: number[] = []
    for (let k = 0; k < MAX_STEPS; k++) {
        let day: number | null
        if (rule.every === 'week') {
            day = day0 + k * 7 * interval
        } else {
            const mi = y0 * 12 + (m0 - 1) + k * interval
            const [y, m] = [Math.floor(mi / 12), mod(mi, 12) + 1]
            const first = daysFromCivil(y, m, 1)
            const d = first + mod(weekday(day0) - weekday(first), 7) + 7 * nth
            day = civilFromDays(d)[1] === m ? d : null
        }
        const at = day === null ? null : toUtc(day)
        if (at === null) continue
        if (at >= toMs || (rule.untilMs !== null && at > rule.untilMs)) break
        if (at >= fromMs) out.push(at)
    }
    return out
}

/** The next date that has not ended by `nowMs` and is not skipped. */
export function nextDate(plan: Plan, nowMs: number, offset: Offset = localOffset): number | null {
    const length = durationMs(plan)
    const found = dates(plan, Math.max(0, nowMs - length), nowMs + 400 * DAY_MS, offset)
        .find((d) => d + length > nowMs && !isSkipped(plan, d))
    return found ?? null
}

/** How many dates the series has, when it ends; `null` for one with no end. */
export function dateCount(plan: Plan, offset: Offset = localOffset): number | null {
    if (!plan.repeat) return 1
    if (plan.repeat.untilMs === null) return null
    return dates(plan, plan.startMs, plan.repeat.untilMs + 1, offset).length
}

// ── Answers and changes ─────────────────────────────────────────────────────

/** An answer to a plan, with who gave it. */
export interface Answer {
    date: AnswerFor
    rsvp: Rsvp
    who: string
}

/**
 * Counted answers for the date of `plan` starting at `dateMs`: per person the
 * newest wins, and an answer for every date counts as much as one for this
 * date — whichever came last. `answers` are in the order they were sent, and
 * only this plan's (`tally_rsvps`).
 */
export function tallyRsvps(plan: Plan, dateMs: number, answers: Iterable<Answer>): Map<string, Rsvp> {
    const out = new Map<string, Rsvp>()
    for (const a of answers) {
        const forThis = a.date === 'every' || (a.date === null ? dateMs === plan.startMs : a.date === dateMs)
        if (!forThis) continue
        out.delete(a.who)
        out.set(a.who, a.rsvp)
    }
    return out
}

/**
 * The plan as its author last left it: the newest update for it sent by
 * `author`, else the plan itself. An update from anybody else is not believed,
 * and neither is one that changes the id. `said` is (who, object), in order.
 */
export function currentPlan(plan: Plan, author: string, said: Iterable<[string, MeetingObject]>): Plan {
    let out = plan
    for (const [who, o] of said) {
        if (o.kind === 'update' && who === author && o.plan.id === plan.id) out = o.plan
    }
    return out
}

/** What an update changed, for the line it leaves in the chat (`PlanChange`). */
export type PlanChange =
    | {dates: number[]; kind: 'skipped'}
    | {dates: number[]; kind: 'restored'}
    | {date: number; kind: 'ends'}
    | {kind: 'no-end'}
    | {kind: 'other'}

/** What `after` changed in `before`. */
export function planChange(before: Plan, after: Plan): PlanChange {
    const skipped = after.skip.filter((d) => !before.skip.includes(d))
    const restored = before.skip.filter((d) => !after.skip.includes(d))
    const until = (p: Plan) => p.repeat?.untilMs ?? null
    // Everything but the skips and the end, which are what the words name.
    const rest = (p: Plan) => JSON.stringify([
        p.id, p.title, p.startMs, p.endMs, p.text, p.agenda.map((i) => [i.name, i.minutes]),
        p.repeat && [p.repeat.every, p.repeat.interval],
    ])
    if (rest(before) !== rest(after)) return {kind: 'other'}
    const sameEnd = until(before) === until(after)
    if (skipped.length && !restored.length && sameEnd) return {dates: skipped, kind: 'skipped'}
    if (!skipped.length && restored.length && sameEnd) return {dates: restored, kind: 'restored'}
    if (!skipped.length && !restored.length && !sameEnd) {
        const u = until(after)
        return u === null ? {kind: 'no-end'} : {date: u, kind: 'ends'}
    }
    return {kind: 'other'}
}

/** A change in words, with `date` naming a date ("Thursday 29 October"). */
export function changeWords(change: PlanChange, title: string, date: (ms: number) => string): string {
    switch (change.kind) {
        case 'skipped':
            return `skipped ${title} on ${change.dates.map(date).join(', ')}`
        case 'restored':
            return `put ${title} back on ${change.dates.map(date).join(', ')}`
        case 'ends':
            return `ended ${title} after ${date(change.date)}`
        case 'no-end':
            return `made ${title} repeat with no end`
        case 'other':
            return `changed ${title}`
    }
}

// ── A chat's meetings ───────────────────────────────────────────────────────

/** A plan as posted in a chat, and as its author last left it. */
export interface PlanLine {
    /** Who posted it: the only person whose updates count. */
    from: string
    msg: Message
    /** With its author's updates applied. */
    plan: Plan
}

/** Everything meeting-shaped in a chat, read once per draw. */
export interface Meetings {
    /** Per plan id: every answer to it, in the order they were sent. */
    answers: Map<string, Answer[]>
    /** Per update line its author sent: the plan before it and after. An
     *  update from anybody else is not here, and changes nothing. */
    changes: Map<Message, {after: Plan; before: Plan}>
    /** Per message: what it carries. */
    objects: Map<Message, MeetingObject>
    /** Per plan id: the first line that posted it. */
    plans: Map<string, PlanLine>
    /** Per poll id: each person's newest choices. */
    votes: Map<string, Map<string, string[]>>
}

export function readMeetings(messages: Message[]): Meetings {
    const out: Meetings = {answers: new Map(), changes: new Map(), objects: new Map(), plans: new Map(), votes: new Map()}
    const updates: [Message, Plan][] = []
    for (const msg of messages) {
        if (msg.deleted) continue
        const o = parse(msg.point)
        if (!o) continue
        out.objects.set(msg, o)
        if (o.kind === 'plan') {
            if (!out.plans.has(o.id)) out.plans.set(o.id, {from: msg.from, msg, plan: o})
        } else if (o.kind === 'update') {
            updates.push([msg, o.plan])
        } else if (o.kind === 'answer') {
            const list = out.answers.get(o.plan) ?? []
            list.push({date: o.date, rsvp: o.rsvp, who: msg.from})
            out.answers.set(o.plan, list)
        } else if (o.kind === 'vote') {
            const m = out.votes.get(o.poll) ?? new Map()
            m.delete(msg.from)
            m.set(msg.from, o.choices)
            out.votes.set(o.poll, m)
        }
    }
    // After every plan is known, so an update read before its plan (a shared
    // history can be out of order) still counts; newest wins, as `currentPlan`.
    for (const [msg, after] of updates) {
        const line = out.plans.get(after.id)
        if (!line || line.from !== msg.from) continue
        out.changes.set(msg, {after, before: line.plan})
        line.plan = after
    }
    return out
}

/** Who answered what for one date of a plan in this chat. */
export function rsvpsFor(meetings: Meetings, plan: Plan, dateMs: number): Map<string, Rsvp> {
    return tallyRsvps(plan, dateMs, meetings.answers.get(plan.id) ?? [])
}

/** A poll's count: per option, who chose it. Stray choices are not counted. */
export function tally(poll: Poll, votes: Map<string, string[]> | undefined): {option: string; who: string[]}[] {
    return poll.options.map((option) => ({
        option,
        who: [...(votes ?? new Map<string, string[]>()).entries()]
            .filter(([, c]) => (poll.multiple ? c : c.slice(0, 1)).includes(option))
            .map(([w]) => w),
    }))
}

/** The bare name in an address (`ann@host` → `ann`). */
const bare = (address: string) => address.split('@')[0]

/**
 * What a meeting from `startMs` to `endMs` came to, from its chat: the notes
 * taken and the polls asked in that time, the people, and the finished lines
 * of captions this device heard.
 */
export function buildRecord(
    messages: Message[],
    startMs: number,
    endMs: number,
    people: string[],
    transcript: {text: string; who: string}[],
    chatName: string,
): MeetingRecord {
    const meetings = readMeetings(messages)
    const during = (m: Message) => m.sentAtMs >= startMs - 60_000 && m.sentAtMs <= endMs
    const notes: RecordNote[] = []
    const polls: MeetingRecord['polls'] = []
    for (const [msg, o] of meetings.objects) {
        if (!during(msg)) continue
        if (o.kind === 'note') notes.push({by: bare(msg.from), kind: o.note, text: o.text})
        if (o.kind === 'poll') {
            polls.push({question: o.question, results: tally(o, meetings.votes.get(o.id)).map((t) => [t.option, t.who.length])})
        }
    }
    const plan = planNow(meetings, startMs)
    return {
        end_ms: endMs,
        notes,
        people: [...new Set(people)].sort(),
        polls,
        start_ms: startMs,
        title: plan?.plan.title ?? (chatName ? `Call in ${chatName}` : 'Meeting record'),
        transcript: transcript.slice(-1500).map((l) => [l.who, l.text]),
    }
}

/** "Budget 10" or "Budget · 10 min" → an agenda item. */
export function agendaLine(line: string): AgendaItem | null {
    const t = line.trim().replace(/^[-*•]\s*/, '')
    if (!t) return null
    const m = /^(.*?)[\s·,:-]+(\d{1,3})\s*(?:m|min|mins|minutes)?$/i.exec(t)
    return m && m[1] ? {minutes: Number(m[2]), name: m[1].trim()} : {minutes: 0, name: t}
}

/**
 * A plan as an iCalendar file, with a reminder ten minutes before. A one-off
 * plan is written in UTC, local times being the calendar's business. A
 * repeating one with `zone` (an IANA name and its offset function) is written
 * in that zone's local time, with its rule and skipped dates, so the calendar
 * repeats it at the same clock time across a change to summer time, as this
 * page does.
 */
export function ics(plan: Plan, chatName: string, zone: {name: string; offset: Offset} | null = null): string {
    const stamp = (ms: number) => iso(ms).replace(/[-:]/g, '')
    const esc = (s: string) => s.replace(/\\/g, '\\\\').replace(/;/g, '\\;').replace(/,/g, '\\,').replace(/\n/g, '\\n')
    const z = plan.repeat ? zone : null
    // `:20261014T180000Z`, or `;TZID=Europe/Amsterdam:20261014T200000`.
    const when = (ms: number) => {
        if (!z) return `:${stamp(ms)}`
        const local = Math.max(0, Math.floor(ms / 1000) + z.offset(ms)) * 1000
        return `;TZID=${z.name}:${stamp(local).replace(/Z$/, '')}`
    }
    const end = plan.startMs + durationMs(plan)
    let desc = `A call in ${chatName} on Cartopolis.`
    if (plan.text) desc += `\n\n${plan.text}`
    if (plan.agenda.length) {
        desc += '\n\nAgenda:'
        for (const i of plan.agenda) desc += `\n- ${i.name}${i.minutes ? ` (${i.minutes} min)` : ''}`
    }
    const rule: string[] = []
    if (plan.repeat) {
        const r = plan.repeat
        const interval = Math.min(4, Math.max(1, r.interval))
        const day = Math.floor((Math.floor(plan.startMs / 1000) + (z ? z.offset(plan.startMs) : 0)) / DAY_S)
        const weekday = ['SU', 'MO', 'TU', 'WE', 'TH', 'FR', 'SA'][mod(day + 4, 7)]
        const nth = Math.trunc((civilFromDays(day)[2] - 1) / 7) + 1
        let line = r.every === 'week'
            ? `RRULE:FREQ=WEEKLY;INTERVAL=${interval}`
            : `RRULE:FREQ=MONTHLY;INTERVAL=${interval};BYDAY=${nth}${weekday}`
        if (r.untilMs !== null) line += `;UNTIL=${stamp(r.untilMs)}`
        rule.push(line)
        for (const d of plan.skip) rule.push(`EXDATE${when(d)}`)
    }
    return [
        'BEGIN:VCALENDAR',
        'VERSION:2.0',
        'PRODID:-//Cartopolis//Meetings//EN',
        'BEGIN:VEVENT',
        `UID:${plan.id.replace(/^urn:uuid:/, '')}@cartopolis`,
        `DTSTAMP:${stamp(plan.startMs)}`,
        `DTSTART${when(plan.startMs)}`,
        `DTEND${when(end)}`,
        ...rule,
        `SUMMARY:${esc(plan.title)}`,
        `DESCRIPTION:${esc(desc)}`,
        `LOCATION:${esc(`Cartopolis · ${chatName}`)}`,
        'BEGIN:VALARM',
        'TRIGGER:-PT10M',
        'ACTION:DISPLAY',
        `DESCRIPTION:${esc(plan.title)}`,
        'END:VALARM',
        'END:VEVENT',
        'END:VCALENDAR',
        '',
    ].join('\r\n')
}

/** This browser's time zone, for a repeating plan's calendar file. */
export function localZone(): {name: string; offset: Offset} | null {
    const name = Intl.DateTimeFormat().resolvedOptions().timeZone
    return name ? {name, offset: localOffset} : null
}

/**
 * The plan a call in this chat now belongs to, and its date: one that starts
 * within the next 15 minutes or started up to 3 hours ago, the nearest first.
 * A repeating plan is looked at date by date; a skipped date is no meeting.
 */
export function planNow(meetings: Meetings, now = Date.now(), offset: Offset = localOffset): {dateMs: number; plan: Plan} | null {
    let best: {dateMs: number; plan: Plan} | null = null
    for (const {plan} of meetings.plans.values()) {
        for (const d of dates(plan, now - 3 * 3600_000, now + 15 * 60000 + 1, offset)) {
            if (isSkipped(plan, d)) continue
            if (!best || Math.abs(d - now) < Math.abs(best.dateMs - now)) best = {dateMs: d, plan}
        }
    }
    return best
}
