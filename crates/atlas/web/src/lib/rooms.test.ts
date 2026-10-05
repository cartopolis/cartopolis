import {describe, expect, test} from 'bun:test'

import type {P2, Storey} from './design'

import {addWall} from './design'
import {findRooms, labelPoint, nameRoomAt, relabel} from './rooms'

/** A 6 × 12 m storey: four outer walls, nothing inside. */
function box(): Storey {
    const ring: P2[] = [
        [0, 0],
        [6, 0],
        [6, 12],
        [0, 12],
    ]
    return {
        elevation: 0,
        floor: ring,
        height: 3.2,
        id: 's0',
        name: 'Ground floor',
        rooms: [],
        stairs: [],
        walls: ring.map((a, i) => ({
            a,
            b: ring[(i + 1) % 4],
            exterior: true,
            id: `w${i}`,
            openings: [],
            thickness: 0,
        })),
    }
}

const area = (r: P2[]) =>
    Math.abs(r.reduce((s, p, i) => s + p[0] * r[(i + 1) % r.length][1] - r[(i + 1) % r.length][0] * p[1], 0) / 2)

describe('finding rooms', () => {
    test('four walls make one room', () => {
        const rooms = findRooms(box())
        expect(rooms.length).toBe(1)
        expect(area(rooms[0])).toBeCloseTo(72, 6)
    })

    test('a wall across makes two, whichever way it is drawn', () => {
        for (const [a, b] of [
            [[0, 5], [6, 5]],
            [[6, 5], [0, 5]],
        ] as [P2, P2][]) {
            const s = box()
            addWall(s, a, b)
            const rooms = findRooms(s).map(area).sort((x, y) => x - y)
            expect(rooms.length).toBe(2)
            expect(rooms[0]).toBeCloseTo(30, 6)
            expect(rooms[1]).toBeCloseTo(42, 6)
        }
    })

    test('a wall that stops short of the far wall encloses nothing more', () => {
        const s = box()
        addWall(s, [0, 5], [4, 5])
        expect(findRooms(s).length).toBe(1)
    })

    test('a T: a second wall from the middle of the first makes three', () => {
        const s = box()
        addWall(s, [0, 5], [6, 5])
        addWall(s, [3, 5], [3, 12])
        const rooms = findRooms(s).map(area).sort((x, y) => x - y)
        expect(rooms.length).toBe(3)
        expect(rooms[0]).toBeCloseTo(21, 6)
        expect(rooms[2]).toBeCloseTo(30, 6)
    })

    test('two walls crossing make four', () => {
        const s = box()
        addWall(s, [0, 6], [6, 6])
        addWall(s, [3, 0], [3, 12])
        expect(findRooms(s).length).toBe(4)
    })
})

describe('naming rooms', () => {
    test('tapping a room names it, and the name follows the room when a wall moves', () => {
        const s = box()
        const wall = addWall(s, [0, 5], [6, 5])!
        const id = nameRoomAt(s, [3, 2], 'Keuken')
        expect(id).not.toBeNull()
        expect(s.rooms[0].name).toBe('Keuken')
        expect(area(s.rooms[0].outline)).toBeCloseTo(30, 6)
        // Move the dividing wall north: the kitchen grows and keeps its name.
        wall.a = [0, 7]
        wall.b = [6, 7]
        s.rooms = relabel(s)
        expect(s.rooms.length).toBe(1)
        expect(s.rooms[0].name).toBe('Keuken')
        expect(area(s.rooms[0].outline)).toBeCloseTo(42, 6)
        // Take the wall away: the kitchen is the whole floor, still named.
        s.walls = s.walls.filter((w) => w.id !== wall.id)
        s.rooms = relabel(s)
        expect(s.rooms[0].name).toBe('Keuken')
    })

    test('a label lands inside an L-shaped room', () => {
        const l: P2[] = [
            [0, 0],
            [10, 0],
            [10, 2],
            [2, 2],
            [2, 10],
            [0, 10],
        ]
        const p = labelPoint(l)
        // Inside the L, not in the empty corner the vertex average lands in.
        const inside = (p[0] >= 0 && p[0] <= 10 && p[1] >= 0 && p[1] <= 2) || (p[0] >= 0 && p[0] <= 2 && p[1] >= 0 && p[1] <= 10)
        expect(inside).toBe(true)
    })
})
