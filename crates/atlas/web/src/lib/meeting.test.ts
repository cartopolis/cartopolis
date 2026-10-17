import {describe, expect, test} from 'bun:test'

import vectors from '../../../../core/testdata/meeting-vectors.json'
import {agendaLine, ics, parse, readMeetings, tally, toJson, type Poll} from './meeting'
import type {Message} from '@/worker/history'

describe('meeting objects', () => {
    test('the vectors the app is tested against read as their kind here too', () => {
        for (const v of vectors as {json: unknown; kind: string | null}[]) {
            const o = parse(JSON.stringify(v.json))
            expect<string | null>(o?.kind ?? null).toBe(v.kind)
            // What this side writes reads back the same.
            if (o) expect(parse(toJson(o))).toEqual(o)
        }
    })

    test('per person the newest answer and vote count, and stray choices do not', () => {
        const at = (from: string, o: object, sentAtMs = 0): Message => ({
            body: '',
            deleted: false,
            edited: false,
            from,
            local: null,
            parent: null,
            picture: null,
            point: JSON.stringify(o),
            reactions: {},
            sentAtMs,
            seq: 1,
        })
        const poll: Poll = {id: 'p', kind: 'poll', multiple: false, options: ['Fri', 'Sat'], question: '?'}
        const m = readMeetings([
            at('ann', JSON.parse(toJson(poll))),
            at('bob', JSON.parse(toJson({choices: ['Fri'], kind: 'vote', poll: 'p'}))),
            at('bob', JSON.parse(toJson({choices: ['Sat'], kind: 'vote', poll: 'p'}))),
            at('ann', JSON.parse(toJson({choices: ['Sun'], kind: 'vote', poll: 'p'}))),
            at('bob', JSON.parse(toJson({kind: 'answer', plan: 'x', rsvp: 'no'}))),
            at('bob', JSON.parse(toJson({kind: 'answer', plan: 'x', rsvp: 'going'}))),
        ])
        expect(tally(poll, m.votes.get('p'))).toEqual([
            {option: 'Fri', who: []},
            {option: 'Sat', who: ['bob']},
        ])
        expect(m.answers.get('x')?.get('bob')).toBe('going')
    })

    test('an agenda line takes its minutes from the end', () => {
        expect(agendaLine('Budget 10')).toEqual({minutes: 10, name: 'Budget'})
        expect(agendaLine('- Stage and sound · 15 min')).toEqual({minutes: 15, name: 'Stage and sound'})
        expect(agendaLine('Who does what')).toEqual({minutes: 0, name: 'Who does what'})
        expect(agendaLine('  ')).toBeNull()
    })

    test('a calendar file escapes and carries a reminder', () => {
        const f = ics({agenda: [], endMs: null, id: 'urn:uuid:1', kind: 'plan', startMs: 1792000800000, text: null, title: 'Budget, again; really'}, '#q4')
        expect(f).toContain('SUMMARY:Budget\\, again\\; really\r\n')
        expect(f).toContain('DTSTART:20261014T180000Z\r\n')
        expect(f).toContain('TRIGGER:-PT10M')
    })
})
