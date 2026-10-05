/**
 * Rooms from walls: the enclosed areas a storey's walls make, found rather than
 * drawn.
 *
 * Drawing a room as a polygon on top of walls that already enclose it is the
 * same shape twice, and the two drift the moment a wall moves. So the editor
 * finds the rooms itself — every face of the plan the walls cut the floor into —
 * and a person only *names* one, by tapping it. A named room keeps its name
 * while its walls move: the name follows whichever new face contains the old
 * room's centre (`relabel`).
 *
 * The method is the planar-graph one: split every wall where another wall's end
 * or body meets it, merge points closer than a couple of centimetres, then walk
 * each directed edge always turning as far left as possible. Each walk that
 * closes anticlockwise is a bounded face; the one clockwise walk round the
 * outside is the outside. Doors and windows are holes in a wall, not gaps in it:
 * a room with a door is still a room.
 */

import type {P2, Room, Storey} from './design'

import {newId, pointInRing} from './design'

/** Points closer than this are one point, metres. */
const MERGE_M = 0.03

/** A face smaller than this is a sliver between two walls, not a room. */
const MIN_ROOM_M2 = 1.0

function signedArea(ring: P2[]): number {
    let a = 0
    for (let i = 0; i < ring.length; i++) {
        const p = ring[i]
        const q = ring[(i + 1) % ring.length]
        a += p[0] * q[1] - q[0] * p[1]
    }
    return a / 2
}

/** Where segments p–p2 and q–q2 cross, as fractions along each, if they do. */
function cross(p: P2, p2: P2, q: P2, q2: P2): [number, number] | null {
    const r: P2 = [p2[0] - p[0], p2[1] - p[1]]
    const s: P2 = [q2[0] - q[0], q2[1] - q[1]]
    const d = r[0] * s[1] - r[1] * s[0]
    if (Math.abs(d) < 1e-9) {
        return null
    }
    const qp: P2 = [q[0] - p[0], q[1] - p[1]]
    const t = (qp[0] * s[1] - qp[1] * s[0]) / d
    const u = (qp[0] * r[1] - qp[1] * r[0]) / d
    return t >= -1e-6 && t <= 1 + 1e-6 && u >= -1e-6 && u <= 1 + 1e-6 ? [t, u] : null
}

/** How far along segment a–b point p lies, if it lies on it (within MERGE_M). */
function onSegment(p: P2, a: P2, b: P2): number | null {
    const ab: P2 = [b[0] - a[0], b[1] - a[1]]
    const len2 = ab[0] * ab[0] + ab[1] * ab[1]
    if (len2 < 1e-12) {
        return null
    }
    const t = ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2
    if (t <= 0 || t >= 1) {
        return null
    }
    const x = a[0] + ab[0] * t - p[0]
    const y = a[1] + ab[1] * t - p[1]
    return Math.hypot(x, y) < MERGE_M ? t : null
}

/** Every enclosed area of the storey's walls, as rings. */
export function findRooms(storey: Storey): P2[][] {
    const segs: [P2, P2][] = storey.walls.map((w) => [w.a, w.b])
    // Where each segment is cut: its ends, every crossing, every end of another
    // wall that lands on it (a T-junction).
    const cuts: number[][] = segs.map(() => [0, 1])
    for (let i = 0; i < segs.length; i++) {
        for (let j = i + 1; j < segs.length; j++) {
            const c = cross(segs[i][0], segs[i][1], segs[j][0], segs[j][1])
            if (c) {
                cuts[i].push(c[0])
                cuts[j].push(c[1])
            }
        }
        for (let j = 0; j < segs.length; j++) {
            if (i === j) {
                continue
            }
            for (const end of segs[j]) {
                const t = onSegment(end, segs[i][0], segs[i][1])
                if (t !== null) {
                    cuts[i].push(t)
                }
            }
        }
    }

    // The graph: merged points, and undirected edges between them.
    const nodes: P2[] = []
    const node = (p: P2): number => {
        const k = nodes.findIndex((n) => Math.hypot(n[0] - p[0], n[1] - p[1]) < MERGE_M)
        if (k >= 0) {
            return k
        }
        nodes.push(p)
        return nodes.length - 1
    }
    const edges = new Set<string>()
    segs.forEach(([a, b], i) => {
        const ts = cuts[i]
            .map((t) => Math.min(1, Math.max(0, t)))
            .sort((x, y) => x - y)
            .filter((t, k, all) => k === 0 || t - all[k - 1] > 1e-6)
        for (let k = 0; k + 1 < ts.length; k++) {
            const pa: P2 = [a[0] + (b[0] - a[0]) * ts[k], a[1] + (b[1] - a[1]) * ts[k]]
            const pb: P2 = [a[0] + (b[0] - a[0]) * ts[k + 1], a[1] + (b[1] - a[1]) * ts[k + 1]]
            const u = node(pa)
            const v = node(pb)
            if (u !== v) {
                edges.add(u < v ? `${u},${v}` : `${v},${u}`)
            }
        }
    })
    const out: number[][] = nodes.map(() => [])
    for (const e of edges) {
        const [u, v] = e.split(',').map(Number)
        out[u].push(v)
        out[v].push(u)
    }
    // Each node's neighbours by angle, so "the next edge round" is an index step.
    const angle = (u: number, v: number) => Math.atan2(nodes[v][1] - nodes[u][1], nodes[v][0] - nodes[u][0])
    out.forEach((ns, u) => ns.sort((x, y) => angle(u, x) - angle(u, y)))

    const used = new Set<string>()
    const rooms: P2[][] = []
    for (let u = 0; u < nodes.length; u++) {
        for (const v0 of out[u]) {
            if (used.has(`${u}>${v0}`)) {
                continue
            }
            const ring: number[] = []
            let [a, b] = [u, v0]
            let guard = 0
            while (!used.has(`${a}>${b}`) && guard++ < 10_000) {
                used.add(`${a}>${b}`)
                ring.push(a)
                // Arriving at b from a, leave by the edge just clockwise of the
                // way back — the tightest left turn — which traces the face on
                // the left of every edge.
                const ns = out[b]
                const back = ns.indexOf(a)
                const next = ns[(back - 1 + ns.length) % ns.length]
                ;[a, b] = [b, next]
            }
            const pts = ring.map((k) => nodes[k])
            if (pts.length >= 3 && signedArea(pts) > MIN_ROOM_M2) {
                rooms.push(pts)
            }
        }
    }
    return rooms
}

function centroid(ring: P2[]): P2 {
    // The area centroid, which stays inside a convex room and inside most real
    // ones; a label at the vertex average of an L-shaped room can fall outside it.
    let a = 0
    let cx = 0
    let cy = 0
    for (let i = 0; i < ring.length; i++) {
        const p = ring[i]
        const q = ring[(i + 1) % ring.length]
        const f = p[0] * q[1] - q[0] * p[1]
        a += f
        cx += (p[0] + q[0]) * f
        cy += (p[1] + q[1]) * f
    }
    if (Math.abs(a) < 1e-9) {
        return ring[0]
    }
    return [cx / (3 * a), cy / (3 * a)]
}

/** A point to put a room's label at, inside it. */
export function labelPoint(ring: P2[]): P2 {
    const c = centroid(ring)
    if (pointInRing(ring, c)) {
        return c
    }
    // An L or a U: fall back to the middle of the longest horizontal chord
    // through the ring's mid-height, which is always inside.
    const ys = ring.map((p) => p[1])
    const y = (Math.min(...ys) + Math.max(...ys)) / 2
    const xs: number[] = []
    for (let i = 0; i < ring.length; i++) {
        const p = ring[i]
        const q = ring[(i + 1) % ring.length]
        if (p[1] > y !== q[1] > y) {
            xs.push(p[0] + ((y - p[1]) / (q[1] - p[1])) * (q[0] - p[0]))
        }
    }
    xs.sort((a, b) => a - b)
    let best: P2 = c
    let width = 0
    for (let i = 0; i + 1 < xs.length; i += 2) {
        if (xs[i + 1] - xs[i] > width) {
            width = xs[i + 1] - xs[i]
            best = [(xs[i] + xs[i + 1]) / 2, y]
        }
    }
    return best
}

/**
 * The storey's named rooms, moved onto the rooms its walls make now: each name
 * goes to the face that contains the old room's label point, and a name whose
 * room has been walled away is dropped. Unnamed faces are not stored — they are
 * found again whenever they are needed.
 */
export function relabel(storey: Storey): Room[] {
    const faces = findRooms(storey)
    const out: Room[] = []
    for (const room of storey.rooms) {
        if (!room.outline.length) {
            continue
        }
        const at = labelPoint(room.outline)
        const face = faces.find((f) => pointInRing(f, at))
        if (face && !out.some((r) => r.outline === face)) {
            out.push({...room, outline: face})
        }
    }
    return out
}

/** Name the room at `p`, creating it if it has no name yet. Returns its id. */
export function nameRoomAt(storey: Storey, p: P2, name: string): string | null {
    const existing = storey.rooms.find((r) => pointInRing(r.outline, p))
    if (existing) {
        existing.name = name
        return existing.id
    }
    const face = findRooms(storey).find((f) => pointInRing(f, p))
    if (!face) {
        return null
    }
    const id = newId('r')
    storey.rooms.push({id, name, outline: face})
    return id
}
