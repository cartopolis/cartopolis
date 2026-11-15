/**
 * How much heap each script holds — the ceiling Luau's allocator gave per
 * state, rebuilt from a heap snapshot.
 *
 * JavaScriptCore counts memory per VM, never per realm, so the only per-script
 * figure is reachability: walk from each realm's marker function (named
 * `__prim_<id>`, held in the runtime's scope) and add up what is reachable
 * without crossing into another global object. The first global the walk meets
 * is the realm's own (two edges away, through `__global`); any later one is
 * reached through a wrapped host function and belongs to someone else.
 *
 * A snapshot of the whole process is the cost: measured at 4–5 seconds for
 * 200 realms on Bun 1.4.4, every script paused meanwhile, so this runs when the process
 * grows, not per tick.
 */
import {generateHeapSnapshotForDebugging} from 'bun:jsc'

const NODE_FIELDS = 7 // id, size, class, flags, label, cell address, wrapped address
const EDGE_FIELDS = 4 // from, to, type, name
const BOUNDARY = new Set(['GlobalObject', 'JSGlobalProxy', 'ShadowRealm', 'Realm'])

export function auditRealms(primIds: number[]): {primId: number; bytes: number}[] {
    const snap: any = generateHeapSnapshotForDebugging()
    const nodes: any[] = snap.nodes
    const edges: number[] = snap.edges
    const classes: string[] = snap.nodeClassNames
    const labels: string[] = snap.labels

    // Node ids are small integers in no particular order, so a flat array maps
    // id to index.
    const count = nodes.length / NODE_FIELDS
    let maxId = 0
    for (let i = 0; i < count; i++) maxId = Math.max(maxId, nodes[i * NODE_FIELDS])
    const index = new Int32Array(maxId + 1).fill(-1)
    const size = new Float64Array(count)
    const boundary = new Uint8Array(count)
    const markers = new Map<number, number>()
    const wanted = new Set(primIds.map((id) => `__prim_${id}`))
    for (let i = 0; i < count; i++) {
        const o = i * NODE_FIELDS
        const id = nodes[o]
        if (!(id >= 0) || index[id] !== -1) throw new Error(`heap snapshot node id ${id} is negative or repeated`)
        index[id] = i
        size[i] = nodes[o + 1]
        if (BOUNDARY.has(classes[nodes[o + 2]])) boundary[i] = classes[nodes[o + 2]] === 'GlobalObject' ? 2 : 1
        const label = labels[nodes[o + 4]]
        if (label && wanted.has(label)) markers.set(Number(label.slice(7)), i)
    }
    const degree = new Uint32Array(count + 1)
    const pairs = edges.length / EDGE_FIELDS
    const from = new Uint32Array(pairs)
    const to = new Uint32Array(pairs)
    for (let e = 0; e < pairs; e++) {
        from[e] = index[edges[e * EDGE_FIELDS]]
        to[e] = index[edges[e * EDGE_FIELDS + 1]]
        degree[from[e] + 1]++
    }
    for (let i = 0; i < count; i++) degree[i + 1] += degree[i]
    const fill = degree.slice(0, count)
    const adj = new Uint32Array(pairs)
    for (let e = 0; e < pairs; e++) adj[fill[from[e]]++] = to[e]

    const seen = new Uint32Array(count) // stamp per walk, so it is never cleared
    const out: {primId: number; bytes: number}[] = []
    let stamp = 0
    for (const [primId, start] of markers) {
        stamp++
        let bytes = 0
        let ownGlobal = -1
        const queue = [start]
        seen[start] = stamp
        for (let q = 0; q < queue.length; q++) {
            const x = queue[q]
            bytes += size[x]
            for (let k = degree[x]; k < degree[x + 1]; k++) {
                const y = adj[k]
                if (seen[y] === stamp) continue
                if (boundary[y]) {
                    if (boundary[y] !== 2 || ownGlobal !== -1) continue
                    ownGlobal = y
                }
                seen[y] = stamp
                queue.push(y)
            }
        }
        out.push({primId, bytes})
    }
    return out.sort((a, b) => b.bytes - a.bytes)
}
