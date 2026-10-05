import {describe, expect, test} from 'bun:test'

import type {BuildingDesign, Storey} from './design'

import {
    addOpening,
    addStair,
    addStorey,
    addWall,
    clampOpening,
    flipStair,
    pointInRing,
    problems,
    copyWindowsUp,
    moveCorner,
    removeTopStorey,
    slideOpening,
    snap,
    spreadWindows,
    stairRect,
    toLatLng,
    toLocal,
} from './design'

/** A 6 × 12 m house, one storey, outer walls inset, a front door on the south. */
function house(): BuildingDesign {
    const ring: [number, number][] = [
        [0.22, 0.22],
        [5.78, 0.22],
        [5.78, 11.78],
        [0.22, 11.78],
    ]
    const ground: Storey = {
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
            openings: i === 0 ? [{at: 2.78, height: 2.3, id: 'd0', kind: 'door', sill: 0, width: 1}] : [],
            thickness: 0,
        })),
    }
    return {origin: [53.2194, 6.5665], pand: '0014100010938796', revision: 0, source: 'generated', storeys: [ground], version: 1}
}

describe('the frame', () => {
    test('metres round-trip through lat/lng, east and north positive', () => {
        const o: [number, number] = [53.2194, 6.5665]
        const p = toLocal(o, 53.22, 6.57)
        expect(p[0]).toBeGreaterThan(0)
        expect(p[1]).toBeGreaterThan(0)
        const [lat, lng] = toLatLng(o, p)
        expect(lat).toBeCloseTo(53.22, 9)
        expect(lng).toBeCloseTo(6.57, 9)
    })
})

describe('snapping', () => {
    test('a click near a corner lands on the corner, near a wall on the wall, else on the grid', () => {
        const s = house().storeys[0]
        expect(snap([0.3, 0.3], s, [], 0.3)).toEqual([0.22, 0.22])
        const onWall = snap([3, 0.3], s, [], 0.3)
        expect(onWall[1]).toBeCloseTo(0.22, 6)
        const free = snap([3.012, 6.04], s, [], 0.3)
        expect(free[0]).toBeCloseTo(3, 9)
        expect(free[1]).toBeCloseTo(6.05, 9)
    })
})

describe('openings', () => {
    test('a door goes in the nearest wall and is kept inside it', () => {
        const s = house().storeys[0]
        const inner = addWall(s, [0.22, 6], [5.78, 6])!
        const placed = addOpening(s, [5.6, 6.05], 'door', 0.5)!
        expect(placed.wall.id).toBe(inner.id)
        expect(placed.opening.width).toBe(0.9)
        // Clicked 18 cm from the wall's end: pulled in until the whole door fits.
        expect(placed.opening.at).toBeCloseTo(5.56 - 0.45, 6)
        expect(addOpening(s, [3, 3], 'door', 0.5)).toBeNull()
    })

    test('an outer wall gets the wider door', () => {
        const s = house().storeys[0]
        expect(addOpening(s, [1, 11.8], 'door', 0.5)!.opening.width).toBe(1)
    })

    test('a window has a sill', () => {
        const s = house().storeys[0]
        expect(addOpening(s, [0.2, 4], 'window', 0.5)!.opening.sill).toBeGreaterThan(0.3)
    })

    test('clamping keeps a centre inside', () => {
        expect(clampOpening(-5, 1, 4)).toBe(0.5)
        expect(clampOpening(9, 1, 4)).toBe(3.5)
    })
})

describe('stairs and storeys', () => {
    test('a flight is drawn to the left of its direction, and can be flipped', () => {
        const s = house().storeys[0]
        const stair = addStair(s, [5.78, 2], [5.78, 5.4])!
        const ring = s.floor
        const inside = stairRect(stair).map((c) => pointInRing(ring, c))
        // Up the east wall heading north: left is west, into the room.
        expect(inside.filter(Boolean).length).toBeGreaterThanOrEqual(2)
        expect(stair.run).toBeCloseTo(3.4, 6)
        flipStair(stair)
        expect(stair.foot[1]).toBeCloseTo(5.4, 6)
        expect(stair.dir[1]).toBeCloseTo(-1, 6)
    })

    test('a storey on top copies the outer walls and their windows, not the front door', () => {
        const d = house()
        addOpening(d.storeys[0], [0.2, 4], 'window', 0.5)
        const up = addStorey(d)
        expect(d.storeys.length).toBe(2)
        expect(up.elevation).toBeCloseTo(3.2, 6)
        expect(up.walls.length).toBe(4)
        const kinds = up.walls.flatMap((w) => w.openings.map((o) => o.kind))
        expect(kinds).toEqual(['window'])
        // Copies, not shared: editing the copy leaves the ground floor alone.
        up.walls[0].a[0] = 99
        expect(d.storeys[0].walls[0].a[0]).toBeCloseTo(0.22, 6)
    })

    test('taking the top storey off takes the flights up to it too', () => {
        const d = house()
        addStorey(d)
        addStair(d.storeys[0], [5.78, 2], [5.78, 5.4])
        expect(removeTopStorey(d)).toBe(true)
        expect(d.storeys[0].stairs.length).toBe(0)
        expect(removeTopStorey(d)).toBe(false)
    })
})

describe('problems', () => {
    test('a house with a front door and nothing odd has none', () => {
        expect(problems(house())).toEqual([])
    })

    test('a storey taller than the building is said', () => {
        const d = house()
        expect(problems(d, 4)).toEqual([])
        addStorey(d)
        expect(problems(d, 4).some((p) => p.includes('out of the roof'))).toBe(true)
    })

    test('no front door, and a stair to nowhere, are both said', () => {
        const d = house()
        d.storeys[0].walls[0].openings = []
        addStair(d.storeys[0], [5.78, 2], [5.78, 5.4])
        const said = problems(d)
        expect(said.some((p) => p.includes('leads nowhere'))).toBe(true)
        expect(said.some((p) => p.includes('no outside door'))).toBe(true)
    })
})

describe('editing by hand', () => {
    test('dragging a corner moves both walls that meet there, and the floor with them', () => {
        const d = house()
        const s = d.storeys[0]
        moveCorner(s, [5.78, 11.78], [7, 13])
        const ends = s.walls.flatMap((w) => [w.a, w.b]).filter((p) => p[0] === 7 && p[1] === 13)
        expect(ends.length).toBe(2)
        expect(s.floor.some((p) => p[0] === 7 && p[1] === 13)).toBe(true)
    })

    test('a door slides along its wall and stays inside it', () => {
        const s = house().storeys[0]
        const wall = s.walls[0]
        slideOpening(wall, 'd0', [100, 0])
        expect(wall.openings[0].at).toBeCloseTo(5.56 - 0.5, 6)
    })

    test('windows spread along a wall keep clear of its door', () => {
        const s = house().storeys[0]
        const wall = s.walls[0] // 5.56 m with a door in the middle
        spreadWindows(wall, 3)
        const windows = wall.openings.filter((o) => o.kind === 'window')
        expect(wall.openings.filter((o) => o.kind === 'door').length).toBe(1)
        expect(windows.length).toBe(2)
        for (const w of windows) {
            expect(Math.abs(w.at - 2.78)).toBeGreaterThan(0.5 + w.width / 2)
        }
    })

    test('the same windows go up to the wall on the same line above', () => {
        const d = house()
        addStorey(d)
        spreadWindows(d.storeys[0].walls[1], 4)
        expect(copyWindowsUp(d, 0, d.storeys[0].walls[1].id)).toBe(1)
        const up = d.storeys[1].walls[1].openings.filter((o) => o.kind === 'window')
        expect(up.length).toBe(4)
        expect(up.map((o) => o.at)).toEqual(d.storeys[0].walls[1].openings.map((o) => o.at))
    })
})
