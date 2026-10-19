import {describe, expect, test} from 'bun:test'

import {type Settings, type Source, entries, goingJson, readMeetups} from './calendar'
import {type Plan, readMeetings, toJson} from './meeting'
import type {Message} from '@/worker/history'

const line = (from: string, point: string): Message => ({
    body: '',
    deleted: false,
    edited: false,
    from,
    local: null,
    parent: null,
    picture: null,
    point,
    reactions: {},
    sentAtMs: 0,
    seq: 1,
})

/** Amsterdam's offset around the change to winter time on 25 October 2026. */
const amsterdam = (ms: number) => (ms < 1_792_890_000_000 ? 7200 : 3600)
const THU_22_OCT = 1_792_692_000_000
const H = 3_600_000
const WEEK = 7 * 24 * H

const series: Plan = {
    agenda: [],
    endMs: THU_22_OCT + 90 * 60000,
    id: 'urn:uuid:s',
    kind: 'plan',
    repeat: {every: 'week', interval: 1, untilMs: THU_22_OCT + 8 * WEEK + H},
    skip: [THU_22_OCT + WEEK + H],
    startMs: THU_22_OCT,
    text: null,
    title: 'Buurtvergadering',
}

const vismarkt = JSON.stringify({
    'cartopolis:view': {altitude: 120, pitch: -35, yaw: 40},
    location: {latitude: 53.2166, longitude: 6.5708, name: 'Vismarkt', type: 'Place'},
    startTime: '2026-10-19T10:30:00Z',
    type: 'Event',
})

const source = (messages: Message[], community: string | null = null): Source => ({
    channel: 'c1',
    community,
    meetings: readMeetings(messages),
    meetups: readMeetups(messages),
})

const all: Settings = {answeredOnly: [], showDeclined: true}

describe('the calendar', () => {
    test('a series is one entry per date, the skipped one marked, each with its own answers', () => {
        const src = source([
            line('ann', toJson(series)),
            line('me', toJson({date: 'every', kind: 'answer', plan: series.id, rsvp: 'going'})),
            line('me', toJson({date: THU_22_OCT + 2 * WEEK + H, kind: 'answer', plan: series.id, rsvp: 'maybe'})),
        ])
        const e = entries([src], 'me', THU_22_OCT - H, THU_22_OCT + 3 * WEEK, all, amsterdam)
        expect(e.map((x) => x.startMs)).toEqual([THU_22_OCT, THU_22_OCT + WEEK + H, THU_22_OCT + 2 * WEEK + H])
        expect(e.map((x) => x.kind === 'meeting' && x.skipped)).toEqual([false, true, false])
        expect(e.map((x) => x.mine)).toEqual(['going', 'going', 'maybe'])
        expect(e.map((x) => x.kind === 'meeting' && `${x.index} of ${x.count}`)).toEqual(['1 of 9', '2 of 9', '3 of 9'])
        // A window that starts later still counts from the series' first date.
        const later = entries([src], 'me', THU_22_OCT + 2 * WEEK, THU_22_OCT + 3 * WEEK, all, amsterdam)
        expect(later.map((x) => x.kind === 'meeting' && x.index)).toEqual([3])
    })

    test("can't-come plans hide when asked, and a community can show only what was answered", () => {
        const src = source([
            line('ann', toJson(series)),
            line('me', toJson({date: THU_22_OCT, kind: 'answer', plan: series.id, rsvp: 'no'})),
        ], 'buurt')
        const window = [THU_22_OCT - H, THU_22_OCT + WEEK + 2 * H] as const
        expect(entries([src], 'me', ...window, {...all, showDeclined: false}, amsterdam).length).toBe(1)
        // Only answered: the date answered "can't come" stays, the other goes.
        const answered = entries([src], 'me', ...window, {answeredOnly: ['buurt'], showDeclined: true}, amsterdam)
        expect(answered.map((x) => x.startMs)).toEqual([THU_22_OCT])
        // The planner's own plans always count as answered.
        expect(entries([src], 'ann', ...window, {answeredOnly: ['buurt'], showDeclined: true}, amsterdam).length).toBe(2)
    })

    test("a meet-up gathers its I'm going answers, and the proposer counts as going", () => {
        const going = goingJson({going: [], json: vismarkt, latitude: 0, longitude: 0, name: null, proposer: '', startMs: 0, text: null})
        expect(JSON.parse(going)).toMatchObject({content: "I'm going", location: {name: 'Vismarkt'}, type: 'Accept'})
        expect(going).not.toContain('cartopolis:view')
        const elsewhere = going.replace('53.2166', '53.3')
        const m = readMeetups([line('sanne', vismarkt), line('me', going), line('me', going), line('bob', elsewhere)])
        expect(m.length).toBe(1)
        expect(m[0].going).toEqual(['sanne', 'me'])
        const e = entries([source([line('sanne', vismarkt), line('me', going)])], 'me', 0, Number.MAX_SAFE_INTEGER, all)
        expect(e[0]).toMatchObject({endMs: Date.parse('2026-10-19T11:30:00Z'), kind: 'meetup', mine: 'going', title: 'Vismarkt'})
    })
})
