/**
 * How much heap each script holds — the ceiling Luau's allocator gave per
 * state, rebuilt from a heap snapshot.
 *
 * JavaScriptCore counts memory per VM, never per script, so the only per-script
 * figure is reachability. All scripts share one realm, so the walk has two
 * steps. First what is *shared*: everything reachable from the runtime's own
 * scope (`__script_runtime`) — the frozen intrinsics, the compiled sources —
 * without going through the two tables that point at every script (`__instances`
 * and `__gens`). Then, per script, everything reachable from its instance that
 * is not shared. The instance is found by its property `__prim_<id>`: the
 * snapshot names property edges, but labels a function only by the name in its
 * source, so a name made at run time is not there.
 *
 * A snapshot of the whole process is the cost, every script paused meanwhile,
 * so this runs when the process grows, not per tick.
 */
import {generateHeapSnapshotForDebugging} from 'bun:jsc'

const NODE_FIELDS = 7 // id, size, class, flags, label, cell address, wrapped address
const EDGE_FIELDS = 4 // from, to, type, name or index
/** The runtime's tables of every script; the shared walk must not pass through them. */
const PER_SCRIPT_TABLES = new Set(['__instances', '__gens'])

export function auditScripts(primIds: number[]): {primId: number; bytes: number}[] {
    const snap: any = generateHeapSnapshotForDebugging()
    const nodes: any[] = snap.nodes
    const edges: number[] = snap.edges
    const labels: string[] = snap.labels
    const edgeTypes: string[] = snap.edgeTypes
    const edgeNames: string[] = snap.edgeNames

    // Node ids are small integers in no particular order, so a flat array maps
    // id to index.
    const count = nodes.length / NODE_FIELDS
    let maxId = 0
    for (let i = 0; i < count; i++) maxId = Math.max(maxId, nodes[i * NODE_FIELDS])
    const index = new Int32Array(maxId + 1).fill(-1)
    const size = new Float64Array(count)
    const markers = new Map<number, number>()
    let runtime = -1
    const wanted = new Set(primIds.map((id) => `__prim_${id}`))
    for (let i = 0; i < count; i++) {
        const o = i * NODE_FIELDS
        const id = nodes[o]
        if (!(id >= 0) || index[id] !== -1) throw new Error(`heap snapshot node id ${id} is negative or repeated`)
        index[id] = i
        size[i] = nodes[o + 1]
        if (labels[nodes[o + 4]] === '__script_runtime') runtime = i
    }
    if (runtime < 0) throw new Error('the heap snapshot has no __script_runtime; the audit cannot tell shared from per-script')

    // Compressed adjacency, with the edges into the per-script tables marked.
    const pairs = edges.length / EDGE_FIELDS
    const degree = new Uint32Array(count + 1)
    const from = new Uint32Array(pairs)
    const to = new Uint32Array(pairs)
    const cut = new Uint8Array(pairs)
    for (let e = 0; e < pairs; e++) {
        const o = e * EDGE_FIELDS
        from[e] = index[edges[o]]
        to[e] = index[edges[o + 1]]
        const type = edgeTypes[edges[o + 2]]
        if ((type === 'Variable' || type === 'Property') && PER_SCRIPT_TABLES.has(edgeNames[edges[o + 3]])) cut[e] = 1
        if (type === 'Property' && wanted.has(edgeNames[edges[o + 3]])) markers.set(Number(edgeNames[edges[o + 3]].slice(7)), from[e])
        degree[from[e] + 1]++
    }
    for (let i = 0; i < count; i++) degree[i + 1] += degree[i]
    const fill = degree.slice(0, count)
    const adj = new Uint32Array(pairs)
    const adjCut = new Uint8Array(pairs)
    for (let e = 0; e < pairs; e++) {
        const k = fill[from[e]]++
        adj[k] = to[e]
        adjCut[k] = cut[e]
    }

    // 1: shared.
    const shared = new Uint8Array(count)
    const queue: number[] = [runtime]
    shared[runtime] = 1
    for (let q = 0; q < queue.length; q++) {
        const x = queue[q]
        for (let k = degree[x]; k < degree[x + 1]; k++) {
            const y = adj[k]
            if (adjCut[k] || shared[y]) continue
            shared[y] = 1
            queue.push(y)
        }
    }

    // 2: each script, stopping at anything shared.
    const seen = new Uint32Array(count) // stamp per walk, so it is never cleared
    const out: {primId: number; bytes: number}[] = []
    let stamp = 0
    for (const [primId, start] of markers) {
        stamp++
        let bytes = 0
        const walk = [start]
        seen[start] = stamp
        for (let q = 0; q < walk.length; q++) {
            const x = walk[q]
            bytes += size[x]
            for (let k = degree[x]; k < degree[x + 1]; k++) {
                const y = adj[k]
                if (shared[y] || adjCut[k] || seen[y] === stamp) continue
                seen[y] = stamp
                walk.push(y)
            }
        }
        out.push({primId, bytes})
    }
    return out.sort((a, b) => b.bytes - a.bytes)
}
