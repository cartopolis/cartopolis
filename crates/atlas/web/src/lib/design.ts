/**
 * A building design as the floor-plan editor edits it.
 *
 * The document is `cartopolis_geo::building_design::BuildingDesign`, byte for
 * byte: storeys, walls carrying their openings, rooms and straight flights, in
 * metres **east and north** of `origin`. Atlas validates it again on save and
 * the game compiles it into what it draws and walks on, so nothing here has to
 * be the last word — only helpful. Every function is pure (it returns a new
 * value or mutates the draft it is handed explicitly), so the editor's undo is a
 * stack of snapshots and these are testable without a browser.
 */

export type P2 = [number, number]

export type OpeningKind = 'door' | 'window'

export interface Opening {
    at: number
    height: number
    id: string
    kind: OpeningKind
    sill: number
    width: number
}

export interface Wall {
    a: P2
    b: P2
    exterior: boolean
    id: string
    openings: Opening[]
    thickness: number
}

export interface Room {
    id: string
    name: string
    outline: P2[]
}

export interface Stair {
    dir: P2
    foot: P2
    id: string
    run: number
    width: number
}

export interface Storey {
    elevation: number
    floor: P2[]
    height: number
    id: string
    name: string
    rooms: Room[]
    stairs: Stair[]
    walls: Wall[]
}

export interface BuildingDesign {
    origin: [number, number]
    pand: string
    revision: number
    source: 'editor' | 'generated' | 'ifc'
    storeys: Storey[]
    version: number
}

/** What the registers say about the building (`atlas::designs::Facts`). */
export interface BuildingFacts {
    doors: [number, number][]
    height: number | null
    neighbours: [number, number][][]
    outline: [number, number][]
    pand: string
}

export interface BuildingAccess {
    admin: boolean
    can_edit: boolean
    owned_by: null | string
    owner: boolean
    username: string
}

/** `GET /api/pipeline/buildings/{pand}`. */
export interface BuildingResponse {
    access: BuildingAccess
    design: BuildingDesign | null
    facts: BuildingFacts | null
    generated: BuildingDesign | null
    pand: string
    stored: boolean
}

/** The generated layout's numbers, as `building_design` has them. */
export const STOREY_M = 3.2
export const DOOR = {height: 2.1, sill: 0, width: 0.9}
export const OUTER_DOOR = {height: 2.3, sill: 0, width: 1.0}
export const WINDOW = {height: 1.3, sill: 0.9, width: 1.2}
export const STAIR = {run: 3.4, width: 0.95}
export const INNER_WALL_M = 0.1

const M_PER_DEG_LAT = 111_320

/** A lat/lng as metres east/north of `origin`, as `building_design::to_local`. */
export function toLocal(origin: [number, number], lat: number, lng: number): P2 {
    const k = Math.cos((origin[0] * Math.PI) / 180)
    return [(lng - origin[1]) * M_PER_DEG_LAT * k, (lat - origin[0]) * M_PER_DEG_LAT]
}

/** The inverse of [`toLocal`]. */
export function toLatLng(origin: [number, number], p: P2): [number, number] {
    const k = Math.cos((origin[0] * Math.PI) / 180)
    return [origin[0] + p[1] / M_PER_DEG_LAT, origin[1] + p[0] / (M_PER_DEG_LAT * k)]
}

let counter = 0
/** A fresh id for a part. Unique within a document is all that is asked. */
export function newId(prefix: string): string {
    counter += 1
    return `${prefix}${Date.now().toString(36)}${counter.toString(36)}`
}

export const sub = (a: P2, b: P2): P2 => [a[0] - b[0], a[1] - b[1]]
export const add = (a: P2, b: P2): P2 => [a[0] + b[0], a[1] + b[1]]
export const scale = (a: P2, k: number): P2 => [a[0] * k, a[1] * k]
export const dist = (a: P2, b: P2) => Math.hypot(a[0] - b[0], a[1] - b[1])
const dot = (a: P2, b: P2) => a[0] * b[0] + a[1] * b[1]

export function norm(a: P2): P2 | null {
    const l = Math.hypot(a[0], a[1])
    return l > 1e-6 ? [a[0] / l, a[1] / l] : null
}

export const wallLength = (w: Wall) => dist(w.a, w.b)

/** A point along a wall, `t` metres from `a`. */
export function alongWall(w: Wall, t: number): P2 {
    const d = norm(sub(w.b, w.a)) ?? [1, 0]
    return add(w.a, scale(d, t))
}

/** The nearest point of segment `a`–`b` to `p`: where, how far along, how far off. */
export function nearestOnSegment(p: P2, a: P2, b: P2): {along: number; off: number; point: P2} {
    const ab = sub(b, a)
    const len2 = dot(ab, ab)
    const t = len2 > 1e-12 ? Math.max(0, Math.min(1, dot(sub(p, a), ab) / len2)) : 0
    const point = add(a, scale(ab, t))
    return {along: t * Math.sqrt(len2), off: dist(p, point), point}
}

/** Even-odd point in polygon. */
export function pointInRing(ring: P2[], p: P2): boolean {
    let inside = false
    for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
        const [u, v] = [ring[i], ring[j]]
        if (u[1] > p[1] !== v[1] > p[1]) {
            const x = u[0] + ((p[1] - u[1]) / (v[1] - u[1])) * (v[0] - u[0])
            if (p[0] < x) {
                inside = !inside
            }
        }
    }
    return inside
}

/**
 * Where a click should land, given what is already drawn: a wall end or an
 * outline corner within `radius`; failing that a point on a wall or the outline;
 * failing that the `grid`. Snapping to what exists is what makes rooms close —
 * a wall that ends two centimetres short of another is a gap the avatar walks
 * through and nobody can see.
 */
export function snap(p: P2, storey: Storey, outline: P2[], radius: number, grid = 0.05): P2 {
    let best: P2 | null = null
    let bestD = radius
    const corners: P2[] = [...outline, ...storey.walls.flatMap((w) => [w.a, w.b])]
    for (const c of corners) {
        const d = dist(p, c)
        if (d < bestD) {
            best = c
            bestD = d
        }
    }
    if (best) {
        return [best[0], best[1]]
    }
    const segments: [P2, P2][] = [
        ...storey.walls.map((w) => [w.a, w.b] as [P2, P2]),
        ...outline.map((c, i) => [c, outline[(i + 1) % outline.length]] as [P2, P2]),
    ]
    bestD = radius * 0.6
    for (const [a, b] of segments) {
        const n = nearestOnSegment(p, a, b)
        if (n.off < bestD) {
            best = n.point
            bestD = n.off
        }
    }
    if (best) {
        return best
    }
    return [Math.round(p[0] / grid) * grid, Math.round(p[1] / grid) * grid]
}

/** A new inner wall from `a` to `b`, or `null` if it is too short to be one. */
export function addWall(storey: Storey, a: P2, b: P2): Wall | null {
    if (dist(a, b) < 0.1) {
        return null
    }
    const wall: Wall = {a, b, exterior: false, id: newId('w'), openings: [], thickness: INNER_WALL_M}
    storey.walls.push(wall)
    return wall
}

/**
 * Put a door or a window into the wall nearest `p`, centred where the click was
 * and kept inside the wall. `null` when no wall is within `radius`, or the wall
 * is too short for the opening.
 */
export function addOpening(
    storey: Storey,
    p: P2,
    kind: OpeningKind,
    radius: number,
): {opening: Opening; wall: Wall} | null {
    let best: {along: number; wall: Wall} | null = null
    let bestD = radius
    for (const wall of storey.walls) {
        const n = nearestOnSegment(p, wall.a, wall.b)
        if (n.off < bestD) {
            best = {along: n.along, wall}
            bestD = n.off
        }
    }
    if (!best) {
        return null
    }
    const {wall} = best
    const size = kind === 'window' ? WINDOW : wall.exterior ? OUTER_DOOR : DOOR
    const len = wallLength(wall)
    const width = Math.min(size.width, len - 0.1)
    if (width < 0.4) {
        return null
    }
    const opening: Opening = {
        at: clampOpening(best.along, width, len),
        height: size.height,
        id: newId(kind === 'door' ? 'd' : 'n'),
        kind,
        sill: size.sill,
        width,
    }
    wall.openings.push(opening)
    return {opening, wall}
}

/** An opening's centre kept so the whole opening fits in a wall `len` long. */
export function clampOpening(at: number, width: number, len: number): number {
    return Math.max(width / 2, Math.min(len - width / 2, at))
}

/**
 * A flight rising out of `storey` from `foot` toward `toward`. Its width spans to
 * the left of the direction of ascent (`building_design::Stair`), so drawing it
 * from the wall end you want it to start at, along the wall, puts it in the room
 * when the room is on your left.
 */
export function addStair(storey: Storey, foot: P2, toward: P2): Stair | null {
    const dir = norm(sub(toward, foot))
    if (!dir) {
        return null
    }
    const run = Math.max(1, Math.min(15, dist(foot, toward) >= 1 ? dist(foot, toward) : STAIR.run))
    const stair: Stair = {dir, foot, id: newId('s'), run, width: STAIR.width}
    storey.stairs.push(stair)
    return stair
}

/** The four corners of a flight, anticlockwise from the foot. */
export function stairRect(s: Stair): [P2, P2, P2, P2] {
    const d = norm(s.dir) ?? [1, 0]
    const l: P2 = [-d[1], d[0]]
    const r = scale(d, s.run)
    const w = scale(l, s.width)
    return [s.foot, add(s.foot, r), add(add(s.foot, r), w), add(s.foot, w)]
}

/** Turn a flight over to the other side of the line it was drawn along. */
export function flipStair(s: Stair): void {
    const d = norm(s.dir) ?? [1, 0]
    const end = add(s.foot, scale(d, s.run))
    s.foot = end
    s.dir = scale(d, -1)
}

/**
 * A storey on top of the building: the floor and the outer walls of the one
 * below, with its windows but without its doors — the front door is on the
 * ground floor. Inner walls are not copied; a floor plan is not a stack.
 */
export function addStorey(design: BuildingDesign): Storey {
    const top = design.storeys[design.storeys.length - 1]
    const k = design.storeys.length
    const storey: Storey = {
        elevation: top.elevation + top.height,
        floor: top.floor.map((p) => [p[0], p[1]] as P2),
        height: STOREY_M,
        id: newId('s'),
        name: storeyName(k),
        rooms: [],
        stairs: [],
        walls: top.walls
            .filter((w) => w.exterior)
            .map((w) => ({
                ...w,
                a: [w.a[0], w.a[1]] as P2,
                b: [w.b[0], w.b[1]] as P2,
                id: newId('w'),
                openings: w.openings
                    .filter((o) => o.kind === 'window')
                    .map((o) => ({...o, id: newId('n')})),
            })),
    }
    design.storeys.push(storey)
    return storey
}

/** Take the top storey off, and the flights that led up to it. */
export function removeTopStorey(design: BuildingDesign): boolean {
    if (design.storeys.length <= 1) {
        return false
    }
    design.storeys.pop()
    design.storeys[design.storeys.length - 1].stairs = []
    return true
}

/** Restack the storeys so each starts where the one below ends. */
export function restack(design: BuildingDesign): void {
    for (let k = 1; k < design.storeys.length; k++) {
        const below = design.storeys[k - 1]
        design.storeys[k].elevation = below.elevation + below.height
    }
}

export function storeyName(k: number): string {
    return k === 0 ? 'Ground floor' : k === 1 ? '1st floor' : k === 2 ? '2nd floor' : k === 3 ? '3rd floor' : `${k}th floor`
}

/** A thing in the plan the editor can select. */
export type Selection =
    | {id: string; kind: 'wall'}
    | {id: string; kind: 'opening'; wallId: string}
    | {id: string; kind: 'room'}
    | {id: string; kind: 'stair'}

/** Remove a selected thing from its storey. */
export function removeSelected(storey: Storey, sel: Selection): void {
    if (sel.kind === 'wall') {
        storey.walls = storey.walls.filter((w) => w.id !== sel.id)
    } else if (sel.kind === 'opening') {
        const wall = storey.walls.find((w) => w.id === sel.wallId)
        if (wall) {
            wall.openings = wall.openings.filter((o) => o.id !== sel.id)
        }
    } else if (sel.kind === 'room') {
        storey.rooms = storey.rooms.filter((r) => r.id !== sel.id)
    } else {
        storey.stairs = storey.stairs.filter((s) => s.id !== sel.id)
    }
}

/**
 * What the editor can say before a save, in the server's own terms. Atlas says
 * it again and is the one that decides; this is so the Save button can explain
 * itself instead of failing.
 */
export function problems(design: BuildingDesign): string[] {
    const out: string[] = []
    const top = design.storeys.length - 1
    design.storeys.forEach((s, k) => {
        if (s.stairs.length && k === top) {
            out.push(`${s.name}: a stair on the top storey leads nowhere`)
        }
        for (const w of s.walls) {
            const len = wallLength(w)
            for (const o of w.openings) {
                if (o.at - o.width / 2 < -0.01 || o.at + o.width / 2 > len + 0.01) {
                    out.push(`${s.name}: an opening does not fit in its ${len.toFixed(2)} m wall`)
                }
            }
        }
    })
    if (!design.storeys[0]?.walls.some((w) => w.exterior && w.openings.some((o) => o.kind === 'door'))) {
        out.push('The ground floor has no outside door, so nobody can get in')
    }
    return out
}

/** A deep copy, for the undo stack. */
export function clone<T>(v: T): T {
    return JSON.parse(JSON.stringify(v)) as T
}
