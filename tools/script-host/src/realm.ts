/**
 * The one ShadowRealm the scripts share: the runtime in `realm-runtime.js`,
 * each distinct source compiled once, each prim an instance of it.
 *
 * A ShadowRealm rather than a `node:vm` context because of what crosses the
 * edge. A vm context shares objects with the host, and any one of them is a way
 * out: measured on Bun 1.3.14, `this.constructor.constructor("return process")()`
 * reaches `process` from an empty context, and so does `.constructor` on any host
 * function handed in. A ShadowRealm passes only primitives and wraps callables,
 * so no host object is ever inside.
 */
// Imported as text rather than read from disk, so `bun build --compile` carries it
// inside the executable the simulator runs (`deploy/build/script-host.sh`).
import RUNTIME from './realm-runtime.js' with {type: 'text'}

import {rewrite, ScriptError} from './rewrite'
import {LIMITS, HANDLERS} from './limits'

/** Every name a script gets from the API, as the destructuring that hands them over. */
export const API_NAMES = [
    'get_pos', 'set_pos', 'get_geo', 'set_geo', 'geo_scale', 'get_rot', 'set_rot', 'get_scale', 'set_scale',
    'set_physical', 'get_physical', 'get_vel', 'set_vel', 'apply_impulse', 'get_mass', 'get_color', 'set_color',
    'get_alpha', 'set_alpha', 'get_glow', 'set_glow', 'set_full_bright', 'particles', 'particle_burst',
    'play_sound', 'loop_sound', 'stop_sound', 'set_tone', 'stop_tone', 'log', 'whisper', 'say', 'shout',
    'set_timer_event', 'sleep', 'get_time', 'reset_time', 'sensor', 'sense_hands', 'send', 'listen',
    'get_prim_id', 'get_name', 'set_name', 'die',
]

export type SliceStatus = 'done' | 'yield' | 'error' | 'memory'
export interface Slice {
    status: SliceStatus
    spent: number
    message: string
    commands: Record<string, any>[]
    dropped: number
}

type Control = (op: string, a?: unknown, b?: unknown, c?: unknown, d?: unknown, e?: unknown, f?: unknown) => string

export class ScriptRealm {
    private readonly shadow = new ShadowRealm()
    private readonly control: Control
    private readonly register: string
    /** Source text → compiled factory, so identical scripts share one function. */
    private readonly factories = new Map<string, number>()

    constructor() {
        const factory = this.shadow.evaluate(RUNTIME) as (heap: () => number, limits: string) => Control
        this.control = factory(process.memoryUsage.rss, JSON.stringify(LIMITS))
        this.register = `__register_${this.control('token').split(' ')[0]}`
    }

    private factory(source: string): number {
        const known = this.factories.get(source)
        if (known !== undefined) return known
        if (source.length > LIMITS.maxSourceBytes) throw new ScriptError(`the script is longer than ${LIMITS.maxSourceBytes} bytes`)
        const body = rewrite(source, HANDLERS)
        // `Bun` and `globalThis` are parameters, so the script's code sees
        // `undefined` under both names (see the allowlist in realm-runtime.js).
        const code =
            `${this.register}(function (Bun, globalThis, __rt) { 'use strict'\n` +
            `const {__fn, __gens, __apply, __mem, __s} = __rt\n` +
            `return function (__api) { const {${API_NAMES.join(', ')}} = __api\n` +
            `return __fn(function* () {\n${body}\n}) } })`
        let id: number
        try {
            id = this.shadow.evaluate(code) as number
        } catch (e: any) {
            // The rewrite parsed it, so this is the rewrite's own output failing:
            // a host bug, worth the message as it is.
            throw new ScriptError(`could not compile: ${e.message}`)
        }
        this.factories.set(source, id)
        return id
    }

    /** Compile (or reuse) `source` and run its top level within `LOAD_FUEL`. */
    load(primId: number, source: string): {handlers: string[]; commands: Record<string, any>[]} {
        const reply = this.control('new', primId, this.factory(source), LIMITS.loadFuel)
        const [head, handlers, drained] = reply.split('\n')
        const top = parseHead(head)
        if (top.status === 'yield') {
            throw new ScriptError(
                `the top level of the script did not finish within its load budget (${LIMITS.loadFuel} steps). ` +
                    'Long-running work belongs in a handler, where it is paused and resumed instead of refused.',
            )
        }
        if (top.status === 'error') throw new ScriptError(top.message)
        if (top.status === 'memory') throw new ScriptError(`the top level used more than ${LIMITS.sliceHeap >> 20} MiB at once`)
        return {handlers: handlers.split(',').filter(Boolean), commands: JSON.parse(drained).commands}
    }

    drop(primId: number) {
        this.control('drop', primId)
    }

    /**
     * One slice for one prim, in one crossing: the clock, the view if there is a
     * new one, the handler to start if none is running, and what came out.
     */
    slice(primId: number, budget: number, time: number, view: string, handler: string, args: unknown[]): Slice {
        const reply = this.control('slice', primId, budget, time, view, handler, handler ? JSON.stringify(args) : '')
        const nl = reply.indexOf('\n')
        const head = parseHead(reply.slice(0, nl))
        const {commands, dropped} = JSON.parse(reply.slice(nl + 1))
        return {...head, commands, dropped}
    }
}

function parseHead(s: string) {
    const space = s.indexOf(' ')
    const space2 = s.indexOf(' ', space + 1)
    return {status: s.slice(0, space) as SliceStatus, spent: Number(s.slice(space + 1, space2)), message: s.slice(space2 + 1)}
}
