/**
 * One script's ShadowRealm: the runtime in `realm-runtime.js` around the
 * author's rewritten code, behind a typed handle.
 *
 * A ShadowRealm rather than a `node:vm` context because of what crosses the
 * edge. A vm context shares objects with the host, and any one of them is a way
 * out: measured on Bun 1.3.14, `this.constructor.constructor("return process")()`
 * reaches `process` from an empty context, and so does `.constructor` on any host
 * function handed in. A ShadowRealm passes only primitives and wraps callables,
 * so no host object is ever inside — the runtime's allowlist then takes away
 * the globals Bun puts in every realm.
 */
import {readFileSync} from 'node:fs'

import {rewrite, ScriptError} from './rewrite'
import {LIMITS, HANDLERS} from './limits'

const RUNTIME = readFileSync(new URL('./realm-runtime.js', import.meta.url), 'utf8')
const [BEFORE, AFTER] = RUNTIME.split('/*AUTHOR*/')
if (AFTER === undefined) throw new Error('realm-runtime.js has lost its /*AUTHOR*/ marker')
const LIMITS_JSON = JSON.stringify(LIMITS)

export type SliceStatus = 'done' | 'yield' | 'error' | 'memory'
export interface Slice {
    status: SliceStatus
    spent: number
    message: string
}

/** Everything a slice of the script asked the world to do. */
export interface Drained {
    commands: Record<string, any>[]
    dropped: number
}

export class Realm {
    private constructor(private readonly control: (op: string, a?: unknown, b?: unknown) => string) {}

    /** Rewrite `source`, build its realm, run its top level within `LOAD_FUEL`. */
    static compile(primId: number, source: string): {realm: Realm; handlers: string[]} {
        if (source.length > LIMITS.maxSourceBytes) throw new ScriptError(`the script is longer than ${LIMITS.maxSourceBytes} bytes`)
        const body = rewrite(source, HANDLERS)
        const shadow = new ShadowRealm()
        let factory: (heap: () => number, limits: string) => (op: string, a?: unknown, b?: unknown) => string
        try {
            const before = BEFORE.replace("/*MARKER*/ null", `function __prim_${Math.trunc(primId)}() {}`)
            factory = shadow.evaluate(`${before}${body}${AFTER}`) as typeof factory
        } catch (e: any) {
            // The rewrite parsed it, so this is the rewrite's own output failing:
            // a host bug, worth the message as it is.
            throw new ScriptError(`could not compile: ${e.message}`)
        }
        const realm = new Realm(factory(process.memoryUsage.rss, LIMITS_JSON))
        realm.control('id', primId)
        realm.view({})
        const top = realm.parse(realm.control('load', LIMITS.loadFuel))
        if (top.status === 'yield') {
            throw new ScriptError(
                `the top level of the script did not finish within its load budget (${LIMITS.loadFuel} steps). ` +
                    'Long-running work belongs in a handler, where it is paused and resumed instead of refused.',
            )
        }
        if (top.status === 'error') throw new ScriptError(top.message)
        if (top.status === 'memory') throw new ScriptError(`the top level used more than ${LIMITS.sliceHeap >> 20} MiB at once`)
        const handlers = realm.control('handlers').split(',').filter(Boolean)
        return {realm, handlers}
    }

    private parse(s: string): Slice {
        const space = s.indexOf(' ')
        const space2 = s.indexOf(' ', space + 1)
        return {
            status: s.slice(0, space) as SliceStatus,
            spent: Number(s.slice(space + 1, space2)),
            message: s.slice(space2 + 1),
        }
    }

    view(view: object) {
        this.control('view', JSON.stringify(view))
    }
    time(t: number) {
        this.control('time', t)
    }
    /** Begin `handler` with `args`; false if the script defines no such handler. */
    start(handler: string, args: unknown[]): boolean {
        return this.control('start', handler, JSON.stringify(args)) === '1'
    }
    get running(): boolean {
        return this.control('running') === '1'
    }
    run(budget: number): Slice {
        return this.parse(this.control('run', budget))
    }
    drain(): Drained {
        return JSON.parse(this.control('drain'))
    }
}
