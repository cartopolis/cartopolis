/**
 * What a meeting leaves in its chat — a plan and its answers, polls and their
 * votes, notes, the record — read and written as the same ActivityStreams JSON
 * the app writes (`crates/core/src/meeting.rs`, which is the reference: its
 * tests pin the shapes, and `send_meeting` in the worker refuses anything that
 * does not read there). Each is a sealed chat message; the server sees none.
 *
 * An answer names what it answers by the object's `id`, never by the chat's
 * log position, and per person the newest answer counts.
 */
import type {Message} from '@/worker/history'

export interface AgendaItem {
    minutes: number
    name: string
}

export interface Plan {
    agenda: AgendaItem[]
    endMs: number | null
    id: string
    kind: 'plan'
    startMs: number
    text: string | null
    title: string
}

export type Rsvp = 'going' | 'maybe' | 'no'

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
    | {kind: 'answer'; plan: string; rsvp: Rsvp}
    | Poll
    | {choices: string[]; kind: 'vote'; poll: string}
    | {kind: 'note'; note: NoteKind; text: string}
    | {kind: 'record'; record: MeetingRecord}

const CONTEXT = 'https://www.w3.org/ns/activitystreams'
const RSVP_TYPE: Record<Rsvp, string> = {going: 'Accept', maybe: 'TentativeAccept', no: 'Reject'}
export const RSVP_WORDS: Record<Rsvp, string> = {going: 'Going', maybe: 'Maybe', no: "Can't come"}
export const NOTE_WORDS: Record<NoteKind, string> = {action: 'Action', decision: 'Decision', note: 'Note'}

const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, 'Z')

export function newId(): string {
    return `urn:uuid:${crypto.randomUUID()}`
}

/** The JSON it travels as. */
export function toJson(o: MeetingObject): string {
    switch (o.kind) {
        case 'plan':
            return JSON.stringify({
                '@context': CONTEXT,
                'cartopolis:agenda': o.agenda,
                'cartopolis:call': true,
                content: o.text || null,
                endTime: o.endMs === null ? null : iso(o.endMs),
                id: o.id,
                name: o.title,
                startTime: iso(o.startMs),
                type: 'Event',
            })
        case 'answer':
            return JSON.stringify({'@context': CONTEXT, object: o.plan, type: RSVP_TYPE[o.rsvp]})
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
    const time = (k: string) => {
        const s = str(k)
        const t = s ? Date.parse(s) : NaN
        return Number.isFinite(t) ? t : null
    }
    switch (str('type')) {
        case 'Event': {
            const id = str('id')
            const startMs = time('startTime')
            if (!('cartopolis:call' in v) || !id || startMs === null) return null
            const agenda = Array.isArray(v['cartopolis:agenda']) ? (v['cartopolis:agenda'] as AgendaItem[]) : []
            return {
                agenda: agenda.filter((a) => typeof a?.name === 'string').map((a) => ({minutes: Number(a.minutes) || 0, name: a.name})),
                endMs: time('endTime'),
                id,
                kind: 'plan',
                startMs,
                text: str('content'),
                title: str('name') ?? '',
            }
        }
        case 'Accept':
        case 'TentativeAccept':
        case 'Reject': {
            const plan = str('object')
            if (!plan) return null
            const rsvp: Rsvp = v.type === 'Accept' ? 'going' : v.type === 'TentativeAccept' ? 'maybe' : 'no'
            return {kind: 'answer', plan, rsvp}
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

/** Everything meeting-shaped in a chat, read once per draw. */
export interface Meetings {
    /** Per plan id: each person's newest answer. */
    answers: Map<string, Map<string, Rsvp>>
    /** Per message: what it carries. */
    objects: Map<Message, MeetingObject>
    /** Per poll id: each person's newest choices. */
    votes: Map<string, Map<string, string[]>>
}

export function readMeetings(messages: Message[]): Meetings {
    const out: Meetings = {answers: new Map(), objects: new Map(), votes: new Map()}
    for (const msg of messages) {
        if (msg.deleted) continue
        const o = parse(msg.point)
        if (!o) continue
        out.objects.set(msg, o)
        if (o.kind === 'answer') {
            const m = out.answers.get(o.plan) ?? new Map()
            m.delete(msg.from)
            m.set(msg.from, o.rsvp)
            out.answers.set(o.plan, m)
        } else if (o.kind === 'vote') {
            const m = out.votes.get(o.poll) ?? new Map()
            m.delete(msg.from)
            m.set(msg.from, o.choices)
            out.votes.set(o.poll, m)
        }
    }
    return out
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
    const plan = currentPlan(meetings, startMs)
    return {
        end_ms: endMs,
        notes,
        people: [...new Set(people)].sort(),
        polls,
        start_ms: startMs,
        title: plan?.title ?? (chatName ? `Call in ${chatName}` : 'Meeting record'),
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

/** A plan as an iCalendar file, written in UTC with a reminder ten minutes before. */
export function ics(plan: Plan, chatName: string): string {
    const stamp = (ms: number) => iso(ms).replace(/[-:]/g, '')
    const esc = (s: string) => s.replace(/\\/g, '\\\\').replace(/;/g, '\\;').replace(/,/g, '\\,').replace(/\n/g, '\\n')
    const end = plan.endMs ?? plan.startMs + 3600_000
    let desc = `A call in ${chatName} on Cartopolis.`
    if (plan.text) desc += `\n\n${plan.text}`
    if (plan.agenda.length) {
        desc += '\n\nAgenda:'
        for (const i of plan.agenda) desc += `\n- ${i.name}${i.minutes ? ` (${i.minutes} min)` : ''}`
    }
    return [
        'BEGIN:VCALENDAR',
        'VERSION:2.0',
        'PRODID:-//Cartopolis//Meetings//EN',
        'BEGIN:VEVENT',
        `UID:${plan.id.replace(/^urn:uuid:/, '')}@cartopolis`,
        `DTSTAMP:${stamp(plan.startMs)}`,
        `DTSTART:${stamp(plan.startMs)}`,
        `DTEND:${stamp(end)}`,
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

/** The plan a call in this chat now belongs to: one that starts within the
 * next 15 minutes or started up to 3 hours ago, the nearest first. */
export function currentPlan(meetings: Meetings, now = Date.now()): Plan | null {
    let best: Plan | null = null
    for (const o of meetings.objects.values()) {
        if (o.kind !== 'plan') continue
        if (o.startMs - now > 15 * 60000 || now - o.startMs > 3 * 3600_000) continue
        if (!best || Math.abs(o.startMs - now) < Math.abs(best.startMs - now)) best = o
    }
    return best
}
