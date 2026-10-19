/**
 * The calendar: every plan this person is in, from every chat
 * (`docs/mockups/calendar/`). Nothing here is sent or stored anywhere new —
 * it is read from the histories the chat worker already keeps in this browser
 * (`worker/history.ts`): meetings planned in a chat (`meeting.ts`) and
 * meet-ups, the points with a place and a time (`cartopolis_core::point`).
 *
 * A meeting that repeats is one entry per date, each with its own answers; a
 * date the planner skipped stays, marked, so a regular does not turn up to an
 * empty call. A meet-up has no end, so it is drawn as an hour.
 */
import type {Message} from '@/worker/history'
import {
    type Meetings,
    type Offset,
    type Plan,
    type Rsvp,
    dateCount,
    dates,
    durationMs,
    isSkipped,
    localOffset,
    parseTime,
    rsvpsFor,
} from './meeting'

/** A place and a time somebody proposed in a chat, and who said "I'm going". */
export interface Meetup {
    /** The proposal as it was sent: what "I'm going" copies. */
    json: string
    latitude: number
    longitude: number
    name: string | null
    /** Who proposed it. They count as going (`chats.rs`, `meetup_people`). */
    proposer: string
    /** Everybody going, the proposer first. */
    going: string[]
    startMs: number
    text: string | null
}

/** A point's meet-up, if it is one: an `Event` (proposed) or `Accept` (going)
 *  with a place and a readable time, as `Point::from_activity_json` reads it. */
function meetupOf(json: string | null): {going: boolean; latitude: number; longitude: number; name: string | null; startMs: number; text: string | null; v: Record<string, unknown>} | null {
    if (!json) return null
    let v: Record<string, unknown>
    try {
        v = JSON.parse(json)
    } catch {
        return null
    }
    const place = v?.location as Record<string, unknown> | undefined
    if (!place || place.type !== 'Place' || typeof place.latitude !== 'number' || typeof place.longitude !== 'number') return null
    if (v.type !== 'Event' && v.type !== 'Accept') return null
    const startMs = parseTime(v.startTime)
    if (startMs === null) return null
    const name = typeof place.name === 'string' && place.name.trim() ? Array.from(place.name.trim()).slice(0, 120).join('') : null
    return {
        going: v.type === 'Accept',
        latitude: place.latitude,
        longitude: place.longitude,
        name,
        startMs,
        text: typeof v.content === 'string' ? v.content : null,
        v,
    }
}

/** Same time, and a place within a few metres (`Point::same_meetup`). */
const same = (a: {latitude: number; longitude: number; startMs: number}, b: typeof a) =>
    a.startMs === b.startMs && Math.abs(a.latitude - b.latitude) < 5e-5 && Math.abs(a.longitude - b.longitude) < 5e-5

/** The meet-ups proposed in a chat, each with who is going. */
export function readMeetups(messages: Message[]): Meetup[] {
    const out: Meetup[] = []
    for (const msg of messages) {
        if (msg.deleted) continue
        const p = meetupOf(msg.point)
        if (!p) continue
        if (!p.going) {
            if (out.some((x) => same(x, p))) continue
            out.push({
                going: [msg.from],
                json: msg.point!,
                latitude: p.latitude,
                longitude: p.longitude,
                name: p.name,
                proposer: msg.from,
                startMs: p.startMs,
                text: p.text,
            })
            continue
        }
        const proposal = out.find((x) => same(x, p))
        if (proposal && !proposal.going.includes(msg.from)) proposal.going.push(msg.from)
    }
    return out
}

/**
 * "I'm going" to a meet-up, as the app sends it: the proposal again as an
 * `Accept`, carrying the place and the time itself so an older reader shows it
 * at the right spot (`point.rs`, `Meetup`), with no camera view.
 */
export function goingJson(m: Meetup): string {
    const v = JSON.parse(m.json) as Record<string, unknown>
    v.type = 'Accept'
    v.content = "I'm going"
    delete v['cartopolis:view']
    delete v.url
    return JSON.stringify(v)
}

interface Common {
    channel: string
    endMs: number
    /** Unique within the calendar: chat, plan or place, and date. */
    key: string
    /** This person's answer for this date: a meet-up has only "going". */
    mine: Rsvp | null
    startMs: number
    title: string
}

export interface MeetingEntry extends Common {
    /** How many dates the series has, when it ends. */
    count: number | null
    /** Which date of the series this is, from 1. */
    index: number
    kind: 'meeting'
    /** As its planner last left it. */
    plan: Plan
    planner: string
    rsvps: Map<string, Rsvp>
    skipped: boolean
}

export interface MeetupEntry extends Common {
    kind: 'meetup'
    meetup: Meetup
}

export type Entry = MeetingEntry | MeetupEntry

/** A chat as the calendar reads it. */
export interface Source {
    channel: string
    /** The community it is a topic of, for "only the plans I answered". */
    community: string | null
    meetings: Meetings
    meetups: Meetup[]
}

/** What the calendar shows: chosen here, kept in this browser, told to nobody. */
export interface Settings {
    /** Plans this person can't come to stay, faded and struck through. */
    showDeclined: boolean
    /** Communities whose plans show only once this person answered them. */
    answeredOnly: string[]
}

const SETTINGS_KEY = 'cartopolis.calendar'

export function loadSettings(): Settings {
    const fallback: Settings = {answeredOnly: [], showDeclined: true}
    try {
        const raw = typeof localStorage === 'undefined' ? null : localStorage.getItem(SETTINGS_KEY)
        const v = raw ? (JSON.parse(raw) as Partial<Settings>) : {}
        return {
            answeredOnly: Array.isArray(v.answeredOnly) ? v.answeredOnly.filter((x) => typeof x === 'string') : [],
            showDeclined: typeof v.showDeclined === 'boolean' ? v.showDeclined : true,
        }
    } catch {
        return fallback
    }
}

export function saveSettings(s: Settings): void {
    if (typeof localStorage !== 'undefined') localStorage.setItem(SETTINGS_KEY, JSON.stringify(s))
}

/**
 * Every plan date and meet-up in `fromMs..toMs` from these chats, in time
 * order, as the settings let through. A person's own plans and meet-ups count
 * as answered.
 */
export function entries(
    sources: Source[],
    me: string,
    fromMs: number,
    toMs: number,
    settings: Settings,
    offset: Offset = localOffset,
): Entry[] {
    const out: Entry[] = []
    for (const src of sources) {
        const answeredOnly = src.community !== null && settings.answeredOnly.includes(src.community)
        const keep = (mine: Rsvp | null, own: boolean) =>
            (settings.showDeclined || mine !== 'no') && (!answeredOnly || own || mine !== null)
        for (const {from, plan} of src.meetings.plans.values()) {
            const length = durationMs(plan)
            // From one length back, so a date already going on is in the window.
            const all = dates(plan, fromMs - length, toMs, offset).filter((d) => d + length > fromMs)
            if (!all.length) continue
            const count = dateCount(plan, offset)
            // The series' first date in the window is the nth: count the ones before it once.
            let index = plan.repeat ? dates(plan, plan.startMs, all[0], offset).length : 0
            for (const d of all) {
                index++
                const rsvps = rsvpsFor(src.meetings, plan, d)
                const mine = rsvps.get(me) ?? null
                if (!keep(mine, from === me)) continue
                out.push({
                    channel: src.channel,
                    count,
                    endMs: d + length,
                    index,
                    key: `${src.channel}|${plan.id}|${d}`,
                    kind: 'meeting',
                    mine,
                    plan,
                    planner: from,
                    rsvps,
                    skipped: isSkipped(plan, d),
                    startMs: d,
                    title: plan.title,
                })
            }
        }
        for (const mu of src.meetups) {
            const endMs = mu.startMs + 3600_000
            if (endMs <= fromMs || mu.startMs >= toMs) continue
            const mine: Rsvp | null = mu.going.includes(me) ? 'going' : null
            if (!keep(mine, mu.proposer === me)) continue
            out.push({
                channel: src.channel,
                endMs,
                key: `${src.channel}|${mu.latitude.toFixed(5)},${mu.longitude.toFixed(5)}|${mu.startMs}`,
                kind: 'meetup',
                meetup: mu,
                mine,
                startMs: mu.startMs,
                title: mu.name ?? 'Meet-up',
            })
        }
    }
    return out.sort((a, b) => a.startMs - b.startMs || a.title.localeCompare(b.title))
}

/** Local midnight of the day `ms` falls in. */
export function dayStart(ms: number): number {
    const d = new Date(ms)
    d.setHours(0, 0, 0, 0)
    return d.getTime()
}

/** Local midnight `n` days on: by the calendar, not 24 h steps, so a change of clock does not slip a day. */
export function addDays(ms: number, n: number): number {
    const d = new Date(ms)
    d.setDate(d.getDate() + n)
    d.setHours(0, 0, 0, 0)
    return d.getTime()
}

/** The Monday of the week `ms` falls in, at midnight. */
export function weekStart(ms: number): number {
    const d = new Date(dayStart(ms))
    return addDays(d.getTime(), -((d.getDay() + 6) % 7))
}

/** Midnight on the first of the month `ms` falls in. */
export function monthStart(ms: number): number {
    const d = new Date(dayStart(ms))
    d.setDate(1)
    return d.getTime()
}

// ── In words ────────────────────────────────────────────────────────────────

/** A clock time in this browser's way of writing one: "20:00". */
export function clock(ms: number): string {
    return new Date(ms).toLocaleTimeString([], {hour: '2-digit', minute: '2-digit'})
}

/** A date named in words: "Thursday 29 October". */
export function longDate(ms: number): string {
    return new Date(ms).toLocaleDateString([], {day: 'numeric', month: 'long', weekday: 'long'})
}

/**
 * How a repeating plan comes back, in words: "Every Thursday", "Every 2 weeks
 * on Thursday", "Every month on the 4th Thursday"; with `times`, the clock
 * times too ("Every Thursday, 20:00 – 21:30"), then the end ("until 17
 * December"). Empty for a plan that does not repeat.
 */
export function ruleWords(plan: Plan, times = true): string {
    const r = plan.repeat
    if (!r) return ''
    const start = new Date(plan.startMs)
    const weekday = start.toLocaleDateString([], {weekday: 'long'})
    const nth = ['1st', '2nd', '3rd', '4th', '5th'][Math.floor((start.getDate() - 1) / 7)]
    let out = r.every === 'week'
        ? r.interval === 1 ? `Every ${weekday}` : `Every ${r.interval} weeks on ${weekday}`
        : r.interval === 1 ? `Every month on the ${nth} ${weekday}` : `Every ${r.interval} months on the ${nth} ${weekday}`
    if (times) out += `, ${clock(plan.startMs)} – ${clock(plan.startMs + durationMs(plan))}`
    if (r.untilMs !== null) out += `${times ? ',' : ''} until ${new Date(r.untilMs).toLocaleDateString([], {day: 'numeric', month: 'long'})}`
    return out
}
