/**
 * The script process: a `ScriptHost` driven by newline-delimited JSON on stdin,
 * answering each tick with one line on stdout.
 *
 * It is a process of its own, never a thread of the simulator, for two reasons
 * the engine leaves no way around. JavaScriptCore cannot be made to stop a
 * built-in mid-call — a regular expression that backtracks for minutes, a
 * string doubled to a gigabyte — so the only sure stop is killing the process.
 * And the realm sandbox is ours, not the engine's: if a script ever gets out of
 * it, it should land in a process that holds nothing but scripts (run it with no
 * network, no writable files, its own user — `supervisor.ts`).
 *
 * argv[2] is the blame file the supervisor maps (`ScriptHost`'s `current`) —
 * or `render`, with the content pages' bundle as argv[3]: the same executable
 * is then the pages' render process instead (`render.ts`), so a server ships
 * one Bun, not two.
 */
import {ScriptHost, type ScriptEvent} from './host'
import {serveRenders} from './render'

type Request =
    | {op: 'load'; prim_id: number; source: string}
    | {op: 'unload'; prim_id: number}
    | {op: 'event'; prim_id: number; event: ScriptEvent}
    | {op: 'fault'; prim_id: number; message: string}
    | {op: 'tick'; dt: number; views: [number, object][]}
    | {op: 'audit'}

async function serveScripts(blamePath: string | undefined) {
    const mapped = blamePath ? Bun.mmap(blamePath) : null
    const host = new ScriptHost(mapped ? new Float64Array(mapped.buffer, mapped.byteOffset, 1) : undefined)
    const write = (value: unknown) => process.stdout.write(JSON.stringify(value) + '\n')

    for await (const line of console) {
        if (!line) continue
        const req = JSON.parse(line) as Request
        switch (req.op) {
            case 'load':
                host.load(req.prim_id, req.source)
                break
            case 'unload':
                host.unload(req.prim_id)
                break
            case 'event':
                host.event(req.prim_id, req.event)
                break
            case 'fault':
                host.fault(req.prim_id, req.message)
                break
            case 'tick':
                write({commands: host.tick(req.dt, new Map(req.views))})
                break
            case 'audit':
                write({audit: host.audit()})
                break
        }
    }
}

if (process.argv[2] === 'render') await serveRenders(process.argv[3])
else await serveScripts(process.argv[2])
