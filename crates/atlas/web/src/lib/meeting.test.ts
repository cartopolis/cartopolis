import {describe, expect, test} from 'bun:test'

import vectors from '../../../../core/testdata/meeting-vectors.json'
import {
    type MeetingObject,
    type Plan,
    type Poll,
    agendaLine,
    caption,
    changeWords,
    currentPlan,
    dateCount,
    dates,
    ics,
    nextDate,
    parse,
    parseTime,
    planChange,
    planNow,
    readMeetings,
    rsvpsFor,
    tally,
    tallyRsvps,
    toJson,
} from './meeting'
import type {Message} from '@/worker/history'

const at = (from: string, o: object | string, sentAtMs = 0): Message => ({
    body: '',
    deleted: false,
    edited: false,
    from,
    local: null,
    parent: null,
    picture: null,
    point: typeof o === 'string' ? o : JSON.stringify(o),
    reactions: {},
    sentAtMs,
    seq: 1,
})

/**
 * Amsterdam's offset in 2026 and early 2027 (`meeting.rs`'s tests, the same
 * numbers): summer time from 2026-03-29T01:00Z to 2026-10-25T01:00Z and again
 * from 2027-03-28T01:00Z. A fixed function, not the machine's zone, so the
 * tests say the same thing wherever they run.
 */
const amsterdam = (ms: number) => ((ms >= 1_774_746_000_000 && ms < 1_792_890_000_000) || ms >= 1_806_195_600_000 ? 7200 : 3600)

/** Thursday 22 October 2026, 20:00 in Amsterdam (18:00Z, summer time). */
const THU_22_OCT = 1_792_692_000_000
const H = 3_600_000
const WEEK = 7 * 24 * H

const plan = (over: Partial<Plan> = {}): Plan => ({
    agenda: [{minutes: 10, name: 'Budget'}],
    endMs: 1_792_004_400_000,
    id: 'urn:uuid:1',
    kind: 'plan',
    repeat: null,
    skip: [],
    startMs: 1_792_000_800_000,
    text: 'Bring the numbers',
    title: 'Q4 planning',
    ...over,
})

const weekly = (untilMs: number | null = null): Plan =>
    plan({endMs: THU_22_OCT + 90 * 60000, repeat: {every: 'week', interval: 1, untilMs}, startMs: THU_22_OCT})

describe('meeting objects', () => {
    test('the vectors the app is tested against read as their kind here too', () => {
        for (const v of vectors as {json: unknown; kind: string | null}[]) {
            const o = parse(JSON.stringify(v.json))
            expect<string | null>(o?.kind ?? null).toBe(v.kind)
            // What this side writes reads back the same.
            if (o) expect(parse(toJson(o))).toEqual(o)
        }
    })

    test('a repeating plan, a dated answer, an every-date answer and an update read as the app wrote them', () => {
        const read = (has: (json: Record<string, unknown>) => boolean) => {
            const v = (vectors as {json: Record<string, unknown>}[]).find((x) => has(x.json))
            return parse(JSON.stringify(v?.json)) as MeetingObject
        }
        const series = read((j) => j.type === 'Event' && 'cartopolis:skip' in j)
        const monthly = read((j) => j.name === 'Monthly')
        const dated = read((j) => 'cartopolis:date' in j)
        const every = read((j) => 'cartopolis:every' in j)
        const update = read((j) => j.type === 'Update' && (j.object as {type: string}).type === 'Event')
        expect(series).toMatchObject({
            repeat: {every: 'week', interval: 1, untilMs: Date.parse('2026-12-17T19:00:00Z')},
            skip: [Date.parse('2026-10-29T19:00:00Z')],
        })
        expect(monthly).toMatchObject({repeat: {every: 'month', interval: 1, untilMs: null}, skip: []})
        expect(dated).toMatchObject({date: Date.parse('2026-11-05T19:00:00Z'), rsvp: 'going'})
        expect(every).toMatchObject({date: 'every', rsvp: 'maybe'})
        expect(update).toMatchObject({kind: 'update', plan: {repeat: {untilMs: Date.parse('2026-11-05T19:00:00Z')}}})
        expect(caption(update)).toBe('📅 Buurtvergadering changed')
        // An answer that names no date stays one: an older build's.
        expect(toJson({date: null, kind: 'answer', plan: 'p', rsvp: 'no'})).not.toContain('cartopolis:')
    })

    test('skipped dates mean nothing without a rule, and are dropped', () => {
        const oneOff = plan({skip: [THU_22_OCT]})
        expect(parse(toJson(oneOff))).toEqual(plan())
    })

    test('times read as the app reads them', () => {
        expect(parseTime('2026-10-06T17:00:00Z')).toBe(1_791_306_000_000)
        expect(parseTime('2026-10-06T19:00:00+02:00')).toBe(1_791_306_000_000)
        expect(parseTime('2026-10-06T17:00:00.250Z')).toBe(1_791_306_000_000)
        expect(parseTime('6 October 2026')).toBeNull()
        expect(parseTime('2026-13-06T17:00:00Z')).toBeNull()
    })

    test('per person the newest answer and vote count, and stray choices do not', () => {
        const poll: Poll = {id: 'p', kind: 'poll', multiple: false, options: ['Fri', 'Sat'], question: '?'}
        const p = plan({id: 'x'})
        const m = readMeetings([
            at('ann', toJson(p)),
            at('ann', toJson(poll)),
            at('bob', toJson({choices: ['Fri'], kind: 'vote', poll: 'p'})),
            at('bob', toJson({choices: ['Sat'], kind: 'vote', poll: 'p'})),
            at('ann', toJson({choices: ['Sun'], kind: 'vote', poll: 'p'})),
            at('bob', toJson({date: null, kind: 'answer', plan: 'x', rsvp: 'no'})),
            at('bob', toJson({date: null, kind: 'answer', plan: 'x', rsvp: 'going'})),
        ])
        expect(tally(poll, m.votes.get('p'))).toEqual([
            {option: 'Fri', who: []},
            {option: 'Sat', who: ['bob']},
        ])
        expect(rsvpsFor(m, p, p.startMs).get('bob')).toBe('going')
    })

    test('an agenda line takes its minutes from the end', () => {
        expect(agendaLine('Budget 10')).toEqual({minutes: 10, name: 'Budget'})
        expect(agendaLine('- Stage and sound · 15 min')).toEqual({minutes: 15, name: 'Stage and sound'})
        expect(agendaLine('Who does what')).toEqual({minutes: 0, name: 'Who does what'})
        expect(agendaLine('  ')).toBeNull()
    })

    test('a calendar file escapes and carries a reminder', () => {
        const f = ics(plan({agenda: [], endMs: null, text: null, title: 'Budget, again; really'}), '#q4')
        expect(f).toContain('SUMMARY:Budget\\, again\\; really\r\n')
        expect(f).toContain('DTSTART:20261014T180000Z\r\n')
        expect(f).toContain('TRIGGER:-PT10M')
        expect(f).not.toContain('RRULE')
    })

    test('a repeating calendar file repeats in local time', () => {
        const p = {...weekly(1_797_534_000_000), skip: [THU_22_OCT + WEEK + H]}
        const zone = {name: 'Europe/Amsterdam', offset: amsterdam}
        const f = ics(p, '#general', zone)
        expect(f).toContain('DTSTART;TZID=Europe/Amsterdam:20261022T200000\r\n')
        expect(f).toContain('DTEND;TZID=Europe/Amsterdam:20261022T213000\r\n')
        expect(f).toContain('RRULE:FREQ=WEEKLY;INTERVAL=1;UNTIL=20261217T190000Z\r\n')
        expect(f).toContain('EXDATE;TZID=Europe/Amsterdam:20261029T200000\r\n')
        const monthly = {...weekly(), repeat: {every: 'month' as const, interval: 1, untilMs: null}}
        expect(ics(monthly, '#general', zone)).toContain('RRULE:FREQ=MONTHLY;INTERVAL=1;BYDAY=4TH\r\n')
        // A one-off plan stays in UTC whatever zone is given.
        expect(ics(plan(), '#q4', zone)).toContain('DTSTART:20261014T180000Z\r\n')
    })
})

describe('plans that repeat', () => {
    test('a weekly plan keeps its clock time across the change to winter time', () => {
        const p = weekly()
        // 20:00 local each time: 18:00Z in summer time, 19:00Z after 25 October.
        expect(dates(p, 0, THU_22_OCT + 3 * WEEK, amsterdam)).toEqual([THU_22_OCT, THU_22_OCT + WEEK + H, THU_22_OCT + 2 * WEEK + H])
        // From a window that starts later, only the dates in it.
        expect(dates(p, THU_22_OCT + 1, THU_22_OCT + WEEK + H + 1, amsterdam)).toEqual([THU_22_OCT + WEEK + H])
        // Every other week.
        const two = {...p, repeat: {every: 'week' as const, interval: 2, untilMs: null}}
        expect(dates(two, 0, THU_22_OCT + 3 * WEEK, amsterdam)).toEqual([THU_22_OCT, THU_22_OCT + 2 * WEEK + H])
        // And back to summer time in March: 20:00 is 18:00Z again.
        const april = dates(p, 1_806_800_000_000, 1_806_800_000_000 + WEEK, amsterdam)
        expect(new Date(april[0]).getUTCHours()).toBe(18)
    })

    test('the last date counts and nothing comes after it', () => {
        // Thursday 17 December, 20:00: nine Thursdays from 22 October.
        const until = 1_797_534_000_000
        const d = dates(weekly(until), 0, Number.MAX_SAFE_INTEGER, amsterdam)
        expect(d.length).toBe(9)
        expect(d[d.length - 1]).toBe(until)
        expect(dateCount(weekly(until), amsterdam)).toBe(9)
        expect(dateCount(weekly(), amsterdam)).toBeNull()
        expect(dateCount(plan(), amsterdam)).toBe(1)
    })

    test('a monthly plan is the same weekday of the month, and a 5th passes over months without one', () => {
        const p = {...weekly(), repeat: {every: 'month' as const, interval: 1, untilMs: null}}
        // 22 October is the 4th Thursday; then 26 November and 24 December.
        expect(dates(p, 0, 1_798_138_800_001, amsterdam)).toEqual([THU_22_OCT, 1_795_719_600_000, 1_798_138_800_000])
        // 29 October is a 5th Thursday: November has none, so the next is 31 December.
        const fifth = {...p, startMs: 1_793_300_400_000}
        expect(dates(fifth, 0, 1_798_743_600_001, amsterdam)).toEqual([1_793_300_400_000, 1_798_743_600_000])
    })

    test('the next date passes over skipped and finished ones', () => {
        const second = THU_22_OCT + WEEK + H
        const p = {...weekly(), skip: [second]}
        expect(nextDate(p, THU_22_OCT - H, amsterdam)).toBe(THU_22_OCT)
        // Half an hour in, the first date has not ended yet.
        expect(nextDate(p, THU_22_OCT + H / 2, amsterdam)).toBe(THU_22_OCT)
        // After it, the skipped one is passed over.
        expect(nextDate(p, THU_22_OCT + 2 * H, amsterdam)).toBe(THU_22_OCT + 2 * WEEK + H)
        // A plan that does not repeat has no next date once it is over.
        expect(nextDate(plan(), plan().startMs + 2 * H, amsterdam)).toBeNull()
    })

    test('an answer is for its date, and the newest one counts', () => {
        const p = weekly()
        const [first, second] = [THU_22_OCT, THU_22_OCT + WEEK + H]
        const every = {date: 'every' as const, rsvp: 'going' as const, who: 'ann'}
        const maybeSecond = {date: second, rsvp: 'maybe' as const, who: 'ann'}
        const oldBuild = {date: null, rsvp: 'no' as const, who: 'bob'}
        // An answer naming no date is for the first one.
        expect([...tallyRsvps(p, first, [every, maybeSecond, oldBuild])]).toEqual([['ann', 'going'], ['bob', 'no']])
        expect([...tallyRsvps(p, second, [every, maybeSecond, oldBuild])]).toEqual([['ann', 'maybe']])
        // Answering every date again afterwards wins over the one date.
        expect([...tallyRsvps(p, second, [maybeSecond, every])]).toEqual([['ann', 'going']])
    })

    test('only the author can skip or end a series', () => {
        const p = weekly()
        const second = THU_22_OCT + WEEK + H
        const skipped: MeetingObject = {kind: 'update', plan: {...p, skip: [second]}}
        const hijack: MeetingObject = {kind: 'update', plan: {...p, title: 'Something else'}}
        const otherId: MeetingObject = {kind: 'update', plan: {...p, id: 'urn:uuid:9'}}
        const now = currentPlan(p, 'ann', [['ann', skipped], ['bob', hijack], ['ann', otherId]])
        expect(now.skip).toEqual([second])
        expect(now.title).toBe(p.title)
        expect(now.id).toBe(p.id)

        // And a chat read as a whole agrees: bob's update is no change at all.
        const m = readMeetings([at('ann', toJson(p)), at('ann', toJson(skipped)), at('bob', toJson(hijack))])
        const line = m.plans.get(p.id)!
        expect(line.plan.skip).toEqual([second])
        expect(line.plan.title).toBe(p.title)
        expect(m.changes.size).toBe(1)
    })

    test('a change says what it changed', () => {
        const p = weekly()
        const d = (ms: number) => new Date(ms).toISOString().slice(0, 10)
        const second = THU_22_OCT + WEEK + H
        const skipped = {...p, skip: [second]}
        expect(changeWords(planChange(p, skipped), 'Buurtvergadering', d)).toBe('skipped Buurtvergadering on 2026-10-29')
        expect(planChange(skipped, p)).toEqual({dates: [second], kind: 'restored'})
        const ended = weekly(second)
        expect(planChange(p, ended)).toEqual({date: second, kind: 'ends'})
        expect(planChange(ended, p)).toEqual({kind: 'no-end'})
        expect(planChange(p, {...p, startMs: THU_22_OCT + H})).toEqual({kind: 'other'})
        expect(changeWords({kind: 'no-end'}, 'X', d)).toBe('made X repeat with no end')
        expect(changeWords({date: second, kind: 'ends'}, 'X', d)).toBe('ended X after 2026-10-29')
    })

    test('a call belongs to the date of a series it is near, not to a skipped one', () => {
        const second = THU_22_OCT + WEEK + H
        const m = readMeetings([at('ann', toJson({...weekly(), skip: [second]}))])
        expect(planNow(m, THU_22_OCT + 2 * WEEK + H - 5 * 60000, amsterdam)?.dateMs).toBe(THU_22_OCT + 2 * WEEK + H)
        expect(planNow(m, second, amsterdam)).toBeNull()
    })
})
