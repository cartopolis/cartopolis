/**
 * The scheduler: `crates/simulator/src/scripting.rs`'s `run` and `step`, over
 * instances in one shared realm instead of Luau states.
 *
 * Kept deliberately close to the Rust, rule for rule, so that a difference in a
 * measurement is a difference between the engines and not between two
 * schedulers: one fuel bank per tick, slices from a rotating start, timers
 * coalescing, `Start` never evicted, hands replacing their own, messages
 * delivered after the pass and never back to the sender.
 */
import {ScriptRealm} from './realm'
import {ScriptError} from './rewrite'
import {LIMITS} from './limits'
import {auditScripts} from './audit'

export type ScriptEvent =
    | {kind: 'Start'}
    | {kind: 'Touch'; avatar_id: number; avatar_name: string}
    | {kind: 'Timer'}
    | {kind: 'Message'; channel: number; text: string; sender: number}
    | {kind: 'Hand'; avatar_id: number; avatar_name: string; offset: [number, number, number]; precise: boolean}

/** What the tick loop is told: the Rust `ScriptCommand`, as JSON. */
export type ScriptCommand = {prim_id: number; [kind: string]: unknown}

const HANDLER: Record<ScriptEvent['kind'], string> = {
    Start: 'on_start',
    Touch: 'on_touch',
    Timer: 'on_timer',
    Message: 'on_message',
    Hand: 'on_hand',
}

function args(e: ScriptEvent): unknown[] {
    switch (e.kind) {
        case 'Touch':
            return [e.avatar_id, e.avatar_name]
        case 'Message':
            return [e.channel, e.text, e.sender]
        case 'Hand':
            return [e.avatar_id, e.avatar_name, ...e.offset, e.precise]
        default:
            return []
    }
}

class Slot {
    pending: ScriptEvent[] = []
    running = false
    timer: [number, number] | null = null
    listening = new Set<number>()
    faulted = false
    time = 0

    constructor(
        readonly primId: number,
        readonly handlers: Set<string>,
    ) {}

    /** `Slot::queue`, rule for rule. */
    queue(event: ScriptEvent) {
        if (event.kind === 'Timer' && this.pending.some((e) => e.kind === 'Timer')) return
        if (event.kind === 'Hand') {
            const i = this.pending.findIndex((e) => e.kind === 'Hand' && e.avatar_id === event.avatar_id)
            if (i >= 0) {
                this.pending[i] = event
                return
            }
        }
        if (this.pending.length >= LIMITS.maxPendingEvents) {
            const i = this.pending.findIndex((e) => e.kind !== 'Start')
            if (i < 0) return
            this.pending.splice(i, 1)
        }
        this.pending.push(event)
    }
}

export class ScriptHost {
    private slots = new Map<number, Slot>()
    /**
     * Channel → the prims listening on it. The Rust host walks every script for
     * every message; with 2000 prims that walk was 160 of a 188 ms tick here, so
     * delivery looks the listeners up instead. Which slot hears a message first
     * does not matter: each slot's own queue keeps the messages in order.
     */
    private listeners = new Map<number, Set<number>>()
    private readonly realm = new ScriptRealm()
    private cursor = 0
    private out: ScriptCommand[] = []
    /**
     * The prim whose code is running right now, also written to `blame[0]` when
     * the supervisor shares one: a file both processes map, so it can still be
     * read after this process has been killed mid-slice.
     */
    private _current = 0
    constructor(private readonly blame?: Float64Array) {}
    get current() {
        return this._current
    }
    set current(id: number) {
        this._current = id
        if (this.blame) this.blame[0] = id
    }

    private output(prim_id: number, level: 'Notice' | 'Info' | 'Error', text: string) {
        this.out.push({prim_id, Output: {level, text}})
    }

    load(primId: number, source: string) {
        const old = this.slots.get(primId)
        if (old) this.unlisten(old)
        this.slots.delete(primId)
        this.current = primId
        try {
            this.realm.drop(primId)
            const {handlers, commands} = this.realm.load(primId, source)
            const slot = new Slot(primId, new Set(handlers))
            const named = ['on_start', 'on_touch', 'on_timer', 'on_hand'].filter((h) => slot.handlers.has(h))
            this.output(primId, 'Notice', named.length ? `compiled — ${named.join(', ')}` : 'compiled, but defines no handler (on_start / on_touch / on_timer)')
            this.collect(slot, commands, 0, [])
            slot.queue({kind: 'Start'})
            this.slots.set(primId, slot)
        } catch (e) {
            this.output(primId, 'Error', e instanceof ScriptError ? e.message : `could not load: ${(e as Error).message}`)
        } finally {
            this.current = 0
        }
    }

    unload(primId: number) {
        this.realm.drop(primId)
        const old = this.slots.get(primId)
        if (old) this.unlisten(old)
        if (this.slots.delete(primId)) this.output(primId, 'Notice', 'stopped')
    }

    event(primId: number, event: ScriptEvent) {
        this.slots.get(primId)?.queue(event)
    }

    /** Stop a script and say why; it keeps its place so its author sees the error. */
    fault(primId: number, message: string) {
        const slot = this.slots.get(primId)
        if (!slot) return
        this.realm.drop(primId)
        slot.faulted = true
        slot.pending = []
        slot.timer = null
        this.unlisten(slot)
        slot.running = false
        this.output(primId, 'Error', message)
    }

    private unlisten(slot: Slot) {
        for (const channel of slot.listening) this.listeners.get(channel)?.delete(slot.primId)
        slot.listening.clear()
    }

    get size() {
        return this.slots.size
    }

    /** One tick: `step`. Returns every command the scripts produced. */
    tick(dt: number, views: Map<number, object>): ScriptCommand[] {
        const ids = [...this.slots.keys()].sort((a, b) => a - b)
        if (ids.length) this.step(ids, dt, views)
        const out = this.out
        this.out = []
        return out
    }

    private step(ids: number[], dt: number, views: Map<number, object>) {
        for (const id of ids) {
            const slot = this.slots.get(id)!
            slot.time += dt
            if (slot.timer) {
                slot.timer[1] -= dt
                if (slot.timer[1] <= 0) {
                    slot.timer[1] = slot.timer[0]
                    slot.queue({kind: 'Timer'})
                }
            }
        }

        const posted: [number, number, string][] = []
        let bank = LIMITS.worldFuelPerTick
        const start = this.cursor % ids.length
        for (let i = 0; i < ids.length && bank > 0; i++) {
            const id = ids[(start + i) % ids.length]
            const slot = this.slots.get(id)!
            if (slot.faulted || (!slot.running && slot.pending.length === 0)) continue

            bank -= this.runSlice(slot, Math.min(bank, LIMITS.fuelSlice), views.get(id), posted)
        }
        this.cursor++

        for (const [sender, channel, text] of posted) {
            for (const id of this.listeners.get(channel) ?? []) {
                if (id === sender) continue
                const slot = this.slots.get(id)!
                if (slot.faulted) continue
                slot.queue({kind: 'Message', channel, text, sender})
            }
        }
    }

    /** `Slot::run_slice`: resume, or start the next event that has a handler. */
    private runSlice(slot: Slot, budget: number, view: object | undefined, posted: [number, number, string][]): number {
        let handler = ''
        let event: ScriptEvent | undefined
        if (!slot.running) {
            for (;;) {
                event = slot.pending.shift()
                if (!event) return 0
                handler = HANDLER[event.kind]
                if (slot.handlers.has(handler)) break
            }
            slot.running = true
        }
        // The view and the clock cross into the realm only for a script that is
        // about to run — an idle one costs nothing per tick — and all of it, with
        // what comes back, in one crossing.
        this.current = slot.primId
        const r = this.realm.slice(slot.primId, budget, slot.time, view ? JSON.stringify(view) : '', handler, event ? args(event) : [])
        this.current = 0
        this.collect(slot, r.commands, r.dropped, posted)
        switch (r.status) {
            case 'yield':
                break
            case 'done':
                slot.running = false
                break
            case 'error':
                this.fault(slot.primId, r.message)
                break
            case 'memory':
                this.fault(slot.primId, `used more than ${LIMITS.sliceHeap >> 20} MiB in one slice; stopped`)
                break
        }
        return r.spent
    }

    /** Turn what a slice pushed into commands, and apply the ones that are the host's. */
    private collect(slot: Slot, commands: Record<string, any>[], dropped: number, posted: [number, number, string][]) {
        for (const cmd of commands) {
            const [kind, value] = Object.entries(cmd)[0] ?? []
            switch (kind) {
                case 'Timer': {
                    const s = Number(value)
                    slot.timer = s > 0 && Number.isFinite(s) ? [Math.max(s, LIMITS.minTimerInterval), Math.max(s, LIMITS.minTimerInterval)] : null
                    break
                }
                case 'Listen': {
                    const {channel, on} = value as {channel: number; on: boolean}
                    if (on && slot.listening.size < LIMITS.maxListenChannels) {
                        slot.listening.add(channel)
                        let set = this.listeners.get(channel)
                        if (!set) this.listeners.set(channel, (set = new Set()))
                        set.add(slot.primId)
                    }
                    if (!on) {
                        slot.listening.delete(channel)
                        this.listeners.get(channel)?.delete(slot.primId)
                    }
                    break
                }
                case 'Send': {
                    const {channel, text} = value as {channel: number; text: string}
                    posted.push([slot.primId, channel, String(text).slice(0, LIMITS.maxText)])
                    break
                }
                case 'ResetTime':
                    slot.time = 0
                    break
                default:
                    // Everything in the realm is the author's to replace, the API
                    // included, so what it hands back is checked again here.
                    if (kind && finiteThroughout(value)) this.out.push({prim_id: slot.primId, [kind]: value})
                    else this.output(slot.primId, 'Notice', `a ${kind} command carried a value that is not a finite number and was dropped`)
            }
        }
        if (dropped > 0) this.output(slot.primId, 'Notice', `${dropped} command(s) dropped — over the per-tick output budget`)
    }

    /**
     * The slow-leak check: attribute the heap to each script and stop any script
     * holding more than `scriptRetained`. Seconds of work for hundreds of
     * scripts, so the caller runs it when the process grows, not every tick.
     */
    audit(): {primId: number; bytes: number}[] {
        const sizes = auditScripts([...this.slots.values()].filter((s) => !s.faulted).map((s) => s.primId))
        for (const {primId, bytes} of sizes) {
            if (bytes > LIMITS.scriptRetained) this.fault(primId, `holds ${(bytes / 1048576).toFixed(1)} MiB, more than the ${LIMITS.scriptRetained >> 20} MiB a script may keep; stopped`)
        }
        return sizes
    }
}

function finiteThroughout(v: unknown): boolean {
    if (typeof v === 'number') return Number.isFinite(v)
    if (Array.isArray(v)) return v.every(finiteThroughout)
    if (v && typeof v === 'object') return Object.values(v).every(finiteThroughout)
    return true
}
