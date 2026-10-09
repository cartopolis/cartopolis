import {describe, expect, test} from 'bun:test'

import {
    apply,
    changeLocally,
    emptyChat,
    markRead,
    mentions,
    pending,
    reactLocally,
    replyCounts,
    thread,
    topLevel,
    unread,
} from './history.ts'
import type {ChatEvent} from './protocol.ts'

const ME = 'ann@cartopolis.org'
const BOB = 'bob@cartopolis.org'

function line(seq: number, from: string, body: string, parent: number | null = null): ChatEvent {
    return {
        body,
        channel: 'dm:00',
        from,
        kind: 'line',
        parent,
        picture: null,
        point: null,
        sent_at_ms: seq * 1000,
        seq,
    }
}

describe('history', () => {
    test('messages are kept in log order and a replayed one is not doubled', () => {
        const c = emptyChat()
        apply(c, line(2, BOB, 'two'), ME)
        apply(c, line(1, BOB, 'one'), ME)
        apply(c, line(2, BOB, 'two again'), ME)
        expect(c.messages.map((m) => m.body)).toEqual(['one', 'two'])
    })

    test("my own message waits at the end, then takes the place the server gave it", () => {
        const c = emptyChat()
        apply(c, line(1, BOB, 'hi'), ME)
        pending(c, 77, ME, 'hello', null, 0)
        // Bob's next line reached the log before mine did.
        apply(c, line(2, BOB, 'you there?'), ME)
        expect(c.messages.map((m) => m.body)).toEqual(['hi', 'you there?', 'hello'])
        apply(c, {channel: 'dm:00', kind: 'sent', request: 77, seq: 3}, ME)
        expect(c.messages.at(-1)).toMatchObject({body: 'hello', local: null, seq: 3})
        expect(c.readSeq).toBe(3)
    })

    test('a thread hangs under its parent and is counted there', () => {
        const c = emptyChat()
        apply(c, line(1, BOB, 'plan?'), ME)
        apply(c, line(2, BOB, 'idea one', 1), ME)
        apply(c, line(3, ME, 'idea two', 1), ME)
        expect(topLevel(c).map((m) => m.seq)).toEqual([1])
        expect(thread(c, 1).map((m) => m.body)).toEqual(['idea one', 'idea two'])
        expect(replyCounts(c).get(1)).toBe(2)
    })

    test('reactions gather people per emoji and can be taken back', () => {
        const c = emptyChat()
        apply(c, line(1, BOB, 'ship it'), ME)
        apply(c, {channel: 'dm:00', emoji: '👍', from: BOB, kind: 'reaction', remove: false, target: 1}, ME)
        reactLocally(c, 1, '👍', ME, false)
        reactLocally(c, 1, '👍', ME, false)
        expect(c.messages[0].reactions['👍']).toEqual([BOB, ME])
        reactLocally(c, 1, '👍', ME, true)
        apply(c, {channel: 'dm:00', emoji: '👍', from: BOB, kind: 'reaction', remove: true, target: 1}, ME)
        expect(c.messages[0].reactions).toEqual({})
    })

    test('only whoever said something may edit or delete it', () => {
        const c = emptyChat()
        apply(c, line(1, BOB, 'tpyo'), ME)
        expect(apply(c, {body: 'mine now', channel: 'dm:00', from: ME, kind: 'edited', target: 1}, ME)).toBe(false)
        expect(changeLocally(c, 1, ME, 'mine now')).toBe(false)
        apply(c, {body: 'typo', channel: 'dm:00', from: BOB, kind: 'edited', target: 1}, ME)
        expect(c.messages[0]).toMatchObject({body: 'typo', edited: true})
        apply(c, {channel: 'dm:00', from: BOB, kind: 'deleted', target: 1}, ME)
        expect(c.messages[0]).toMatchObject({body: '', deleted: true})
    })

    test("unread counts other people's messages past what was read", () => {
        const c = emptyChat()
        apply(c, line(1, BOB, 'a'), ME)
        apply(c, line(2, ME, 'b'), ME)
        apply(c, line(3, BOB, 'c'), ME)
        expect(unread(c, ME)).toBe(2)
        expect(markRead(c)).toBe(true)
        expect(unread(c, ME)).toBe(0)
        expect(markRead(c)).toBe(false)
    })

    test('a mention is @name or @name@server, not a longer name', () => {
        expect(mentions('hey @ann look', ME)).toBe(true)
        expect(mentions('@ann@cartopolis.org see this', ME)).toBe(true)
        expect(mentions('@anna look', ME)).toBe(false)
        expect(mentions('ann without the at', ME)).toBe(false)
    })
})
