/**
 * A stand-in chat worker for `bun run mock` (`?mock-chat`): the real page over
 * invented chats, so Community and its calendar can be looked at on a machine
 * with no simulator and no chat engine (`docs/mockups/calendar/` has the
 * people and plans; the week is this week, so the page always has a today).
 *
 * It answers the worker's protocol (`src/worker/protocol.ts`) in memory: a
 * sign-in, every chat's history, and what the page sends — a message, an
 * answer, a skip, "I'm going" — appended as this person's line, so clicking
 * through the calendar does what it would. It is not a test double: nothing
 * is checked against it, and the real worker is what `chat-e2e.ts` runs.
 */
import type {ChatSummary, Command, CommunityView, Update} from './src/worker/protocol'
import type {Message} from './src/worker/history'
import {type MeetingObject, type Plan, toJson} from './src/lib/meeting'

const HOST = 'localhost'
const at = (name: string) => `${name}@${HOST}`
const ME = at('annekedijkstra')

/** Monday of this week at midnight, and a moment `day` days and `h:mm` after it. */
const monday = (() => {
    const d = new Date()
    d.setHours(0, 0, 0, 0)
    d.setDate(d.getDate() - ((d.getDay() + 6) % 7))
    return d.getTime()
})()
const on = (day: number, h: number, min = 0) => {
    const d = new Date(monday)
    d.setDate(d.getDate() + day)
    d.setHours(h, min, 0, 0)
    return d.getTime()
}

let seq = 1
const line = (from: string, sentAtMs: number, body: string, point: string | null = null): Message => ({
    body,
    deleted: false,
    edited: false,
    from,
    local: null,
    parent: null,
    picture: null,
    point,
    reactions: {},
    sentAtMs,
    seq: seq++,
})
const meeting = (from: string, sentAtMs: number, o: MeetingObject) => line(from, sentAtMs, '', toJson(o))
const answer = (from: string, sentAtMs: number, plan: string, rsvp: 'going' | 'maybe' | 'no', date: number | 'every' | null = null) =>
    meeting(from, sentAtMs, {date, kind: 'answer', plan, rsvp})

/** A meet-up proposal, as the app sends one (`point.rs`). */
const meetup = (from: string, sentAtMs: number, name: string, latitude: number, longitude: number, startMs: number, text: string | null = null) =>
    line(from, sentAtMs, `📍 ${name}`, JSON.stringify({
        content: text ?? undefined,
        location: {latitude, longitude, name, type: 'Place'},
        startTime: new Date(startMs).toISOString().replace(/\.\d{3}Z$/, 'Z'),
        type: 'Event',
    }))
const going = (from: string, sentAtMs: number, proposal: Message) => {
    const v = JSON.parse(proposal.point!)
    return line(from, sentAtMs, "I'm going", JSON.stringify({...v, content: "I'm going", type: 'Accept'}))
}

// ── The chats ───────────────────────────────────────────────────────────────

const SANNE = 'dm:sanne'
const FIETS = 'dm:fietsclub'
const BANKJES = 'dm:gorecht-bankjes'
const GENERAL = 'dm:oost-general'

const q4: Plan = {
    agenda: [{minutes: 10, name: 'Nieuwe bankjes'}, {minutes: 15, name: 'Budget'}, {minutes: 20, name: 'Wie doet wat'}],
    endMs: on(0, 20, 15),
    id: 'urn:uuid:mock-q4',
    kind: 'plan',
    repeat: null,
    skip: [],
    startMs: on(0, 19, 30),
    text: null,
    title: 'Q4 planning',
}
const overleg: Plan = {...q4, agenda: [], endMs: on(5, 11), id: 'urn:uuid:mock-overleg', startMs: on(5, 10), title: 'Bankjes overleg'}
/** The weekly series, planned by this person, with next week taken out. */
const buurt: Plan = {
    agenda: [{minutes: 30, name: 'Speeltuin'}, {minutes: 30, name: 'Straatfeest'}, {minutes: 30, name: 'Rondvraag'}],
    endMs: on(3, 21, 30),
    id: 'urn:uuid:mock-buurt',
    kind: 'plan',
    repeat: {every: 'week', interval: 1, untilMs: on(3 + 56, 20)},
    skip: [],
    startMs: on(3, 20),
    text: null,
    title: 'Buurtvergadering',
}
const skippedBuurt: Plan = {...buurt, skip: [on(10, 20)]}

const vismarkt = meetup(at('sanne'), on(-1, 18), 'Vismarkt', 53.2166, 6.5641, on(0, 12, 30), 'Koffie bij de kraam?')
const hoofdstation = meetup(at('bas'), on(-2, 9), 'Hoofdstation', 53.2105, 6.5641, on(1, 18), 'Verzamelen voor de avondrit')
const plantsoen = meetup(at('lotte'), on(-1, 20), 'Noorderplantsoen', 53.2234, 6.5577, on(5, 14))
const martini = meetup(at('bas'), on(0, 8), 'Martinitoren', 53.2193, 6.5683, on(6, 9, 30), 'Rondje Hondsrug')

const histories = new Map<string, Message[]>([
    [SANNE, [
        line(at('sanne'), on(-1, 17, 55), 'Morgen lunchen?'),
        vismarkt,
        going(ME, on(-1, 18, 4), vismarkt),
        line(ME, on(-1, 18, 5), 'Leuk, tot dan!'),
    ]],
    [FIETS, [
        hoofdstation,
        going(at('lotte'), on(-2, 10), hoofdstation),
        plantsoen,
        going(ME, on(-1, 20, 30), plantsoen),
        going(at('bas'), on(-1, 21), plantsoen),
        martini,
        line(at('bas'), on(0, 8, 2), 'Wie gaat er mee zondag?'),
    ]],
    [BANKJES, [
        meeting(at('anna'), on(-3, 10), q4),
        ...['karim', 'lotte', 'bas'].map((p, i) => answer(at(p), on(-3, 11 + i), q4.id, 'going')),
        answer(at('ruud'), on(-3, 15), q4.id, 'maybe'),
        answer(at('ilse'), on(-3, 16), q4.id, 'no'),
        answer(at('anna'), on(-3, 10, 1), q4.id, 'going'),
        answer(ME, on(-2, 9), q4.id, 'going'),
        meeting(at('karim'), on(-1, 12), overleg),
        answer(ME, on(-1, 13), overleg.id, 'no'),
        line(at('pieter'), on(-1, 14), 'Ik neem de offertes mee.'),
    ]],
    [GENERAL, [
        line(at('karim'), on(-6, 19), 'Zullen we weer elke week vergaderen?'),
        meeting(ME, on(-5, 20), buurt),
        answer(ME, on(-5, 20, 1), buurt.id, 'going', 'every'),
        ...['karim', 'fatima', 'joris', 'daan', 'mei', 'sanne'].map((p, i) =>
            answer(at(p), on(-5, 21 + (i % 3), i), buurt.id, i < 4 ? 'going' : 'maybe', 'every')),
        answer(at('lotte'), on(-4, 9), buurt.id, 'going', on(3, 20)),
        line(at('karim'), on(-2, 10), '29 oktober ben ik er niet, en Fatima ook niet'),
        meeting(ME, on(-2, 11), {kind: 'update', plan: skippedBuurt}),
        answer(ME, on(-2, 11, 5), buurt.id, 'maybe', on(24, 20)),
        line(at('fatima'), on(-2, 12), 'Top, dank!'),
    ]],
])

const members = (names: string[]) => names.map((n) => ({address: at(n), role: 'member' as const}))
const settings = (name: string) => ({about: '', join: 'ask' as const, listed: true, members_start_topics: true, name, picture: null, place: null})
const communities: CommunityView[] = [
    {
        home: HOST,
        id: 'gorecht',
        members: [{address: ME, role: 'member'}, {address: at('anna'), role: 'owner'}, ...members(['karim', 'lotte', 'bas', 'ruud', 'ilse', 'pieter', 'mo'])],
        reports: [],
        requests: [],
        role: 'member',
        settings: settings('Gorecht buurt'),
        topics: [
            {archived: false, channel: BANKJES, joined: true, name: 'bankjes', position: 0},
        ],
    },
    {
        home: HOST,
        id: 'oost',
        members: [{address: ME, role: 'owner'}, ...members(['karim', 'fatima', 'joris', 'daan', 'mei', 'sanne', 'lotte', 'bas', 'ruud', 'ilse', 'pieter', 'mo', 'anna', 'tom', 'eva', 'sem', 'lisa', 'noah', 'emma', 'finn', 'julia', 'lucas', 'sophie'])],
        reports: [],
        requests: [],
        role: 'owner',
        settings: settings('oosterparkwijk'),
        topics: [{archived: false, channel: GENERAL, joined: true, name: 'general', position: 0}],
    },
]

function summaries(): ChatSummary[] {
    const sum = (channel: string, label: string, group: boolean, people: string[]): ChatSummary => {
        const h = histories.get(channel) ?? []
        const last = h[h.length - 1]
        return {channel, group, label, lastAtMs: last?.sentAtMs ?? 0, mentioned: false, people, preview: last?.body ?? '', unread: 0}
    }
    return [
        sum(SANNE, at('sanne'), false, [at('sanne')]),
        sum(FIETS, 'Fietsclub', true, [at('bas'), at('lotte'), at('tom')]),
        sum(BANKJES, 'bankjes', true, communities[0].members.map((x) => x.address).filter((a) => a !== ME)),
        sum(GENERAL, 'general', true, communities[1].members.map((x) => x.address).filter((a) => a !== ME)),
    ]
}

// ── The worker, in memory ───────────────────────────────────────────────────

class StandInPort {
    onmessage: ((ev: MessageEvent<Update>) => void) | null = null

    start(): void {}

    private post(update: Update): void {
        // A copy, as a real port's structured clone is: the page keys caches on the array.
        const copy = structuredClone(update)
        setTimeout(() => this.onmessage?.({data: copy} as MessageEvent<Update>), 0)
    }

    private state(): void {
        this.post({address: ME, chats: summaries(), communities, directory: [], status: 'connected', type: 'state', username: 'annekedijkstra'})
    }

    private history(channel: string): void {
        this.post({channel, messages: histories.get(channel) ?? [], type: 'history'})
    }

    private say(channel: string, body: string, point: string | null): void {
        const h = histories.get(channel)
        if (!h) return
        h.push(line(ME, Date.now(), body, point))
        this.history(channel)
        this.state()
    }

    postMessage(command: Command): void {
        switch (command.type) {
            case 'sign_in':
                this.state()
                return
            case 'histories':
                for (const channel of histories.keys()) this.history(channel)
                return
            case 'open':
                this.history(command.channel)
                return
            case 'send_text':
                this.say(command.channel, command.body, null)
                return
            case 'send_meeting':
                this.say(command.channel, command.body, command.object)
                return
            case 'send_point':
                this.say(command.channel, command.body, command.point)
                return
        }
    }
}

class StandInWorker {
    port = new StandInPort()
}

Object.assign(globalThis, {SharedWorker: StandInWorker})

// After the stand-in: the app's own entry, exactly as the page loads it.
await import('./src/index')

/**
 * `?mock-click=Week|Q4 planning` clicks, in turn, the first button (or
 * summary) whose text contains each, so a headless browser can photograph a
 * page a click or two in, as `?mock-tab` does for Admin.
 */
const clicks = new URLSearchParams(location.search).get('mock-click')?.split('|') ?? []
let next = 0
const click = () => {
    if (next >= clicks.length) return
    const want = clicks[next]
    const target = [...document.querySelectorAll<HTMLElement>('button, summary, label')]
        .find((b) => b.textContent?.includes(want))
    if (target) {
        target.click()
        next++
    }
    setTimeout(click, 300)
}
setTimeout(click, 500)
