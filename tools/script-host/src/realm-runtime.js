// The runtime that lives *inside* each script's ShadowRealm, around the
// author's rewritten code. Plain JavaScript, not TypeScript: it is read as text
// and evaluated in the realm, where nothing from the host module exists.
//
// `realm.ts` splices the author's code in at the AUTHOR marker below and evaluates the
// whole as one function expression taking the host callables. Everything named
// `__…` is therefore a *local* of that function, which is what keeps it out of
// the author's reach: the rewrite refuses `__` names, and a local, unlike a
// global, cannot be found through `globalThis["_" + "_f"]`.
//
// Only primitives and functions cross a ShadowRealm's edge, so the host talks to
// this file through `__control(op, a, b)` with numbers and JSON strings.
;(function (__heap, __limitsJson) {
    'use strict'
    const __limits = JSON.parse(__limitsJson)
    // A function named after the prim, held in this scope, so the heap audit
    // (`audit.ts`) can tell which realm a heap node belongs to: the snapshot
    // labels functions by name and nothing else here carries the prim's id.
    const __marker = /*MARKER*/ null

    // ── The sandbox: what the realm may still reach ──
    //
    // An allowlist, so that anything Bun adds to a realm's global in a future
    // release arrives switched *off* — the rule `Slot::compile` applies to mlua's
    // `require`. Measured on Bun 1.4.4: a fresh ShadowRealm carries `process`,
    // `fetch` and `Bun`, so without this a script holds the server.
    const __global = globalThis
    const __keep = new Set([
        'Object', 'Function', 'Array', 'Number', 'parseFloat', 'parseInt', 'Infinity', 'NaN',
        'undefined', 'Boolean', 'String', 'Symbol', 'Date', 'RegExp', 'Error', 'AggregateError',
        'EvalError', 'RangeError', 'ReferenceError', 'SyntaxError', 'TypeError', 'URIError',
        'JSON', 'Math', 'Map', 'Set', 'WeakMap', 'WeakSet', 'Reflect', 'Proxy', 'isFinite',
        'isNaN', 'encodeURIComponent', 'decodeURIComponent', 'encodeURI', 'decodeURI',
    ])
    // `Bun` cannot be deleted: on 1.4.4 it is non-configurable and read-only in
    // every realm, and `Bun.spawnSync(["id"])` from a script ran a program on the
    // host. It is shadowed by a local of the same name instead (just above the
    // author's code), and `globalThis` goes, so no path is left to the global
    // object's own property: strict code has no sloppy `this`, and every
    // constructor that compiles code is closed below.
    const __shadowed = new Set(['Bun'])
    for (const key of Reflect.ownKeys(__global)) {
        if (typeof key !== 'string' || __keep.has(key)) continue
        try {
            delete __global[key]
        } catch {}
    }
    // Fail closed: a name Bun adds that cannot be deleted and is not shadowed
    // would be reachable by every script, so refuse to run any.
    const __stuck = Reflect.ownKeys(__global).filter((k) => typeof k === 'string' && !__keep.has(k) && !__shadowed.has(k))
    if (__stuck.length) throw new Error(`the script realm still holds ${__stuck.join(', ')}; refusing to run scripts`)
    // V8's stack-trace hooks, which Bun also has: call sites hand out functions
    // and receivers. Measured to give nothing away in strict code, and removed
    // anyway, because nothing a script does needs them.
    for (const key of ['prepareStackTrace', 'captureStackTrace', 'stackTraceLimit']) {
        try {
            delete Error[key]
        } catch {}
    }
    // `Function` stays as a name (`instanceof Function`), but no constructor in
    // the realm compiles code any more: the author's source is meant to be what
    // runs, and code compiled at run time would skip the rewrite — no fuel checks.
    const __noCode = function () {
        throw new EvalError('scripts cannot compile code at run time')
    }
    for (const f of [function () {}, function* () {}, async function () {}, async function* () {}]) {
        Object.defineProperty(Object.getPrototypeOf(f), 'constructor', {value: __noCode})
    }
    __noCode.prototype = Function.prototype
    __global.Function = __noCode

    // ── Ceilings inside built-ins ──
    //
    // A built-in runs no fuel check, so one call can allocate a gigabyte before
    // the host looks. These are the calls whose *output* size is an argument.
    // Not complete — the slice heap check and the process backstop catch the rest.
    const __MAX_LEN = __limits.maxLength
    const __big = (n, what) => {
        if (n > __MAX_LEN) throw new RangeError(`${what} would be longer than ${__MAX_LEN}, the most a script may make at once`)
    }
    const __wrap = (obj, name, check) => {
        const orig = obj[name]
        Object.defineProperty(obj, name, {
            value: {
                [name](...a) {
                    check(this, a)
                    return orig.apply(this, a)
                },
            }[name],
            writable: true,
            configurable: true,
        })
    }
    __wrap(String.prototype, 'repeat', (s, [n]) => __big(String(s).length * Number(n), 'repeat'))
    __wrap(String.prototype, 'padStart', (_s, [n]) => __big(Number(n), 'padStart'))
    __wrap(String.prototype, 'padEnd', (_s, [n]) => __big(Number(n), 'padEnd'))
    __wrap(Array.prototype, 'fill', (a) => __big(a.length, 'fill'))
    __wrap(Array.prototype, 'join', (a) => __big(a.length, 'join'))
    __wrap(Array.prototype, 'keys', (a) => __big(a.length, 'keys'))
    __wrap(Array.prototype, 'values', (a) => __big(a.length, 'values'))
    __wrap(Array.prototype, 'entries', (a) => __big(a.length, 'entries'))
    __wrap(Array, 'from', (_A, [src]) => __big(src && typeof src === 'object' ? Number(src.length) || 0 : 0, 'Array.from'))
    {
        const OrigArray = Array
        const SafeArray = new Proxy(OrigArray, {
            apply: (t, self, a) => (a.length === 1 && typeof a[0] === 'number' && __big(a[0], 'Array'), Reflect.apply(t, self, a)),
            construct: (t, a, nt) => (a.length === 1 && typeof a[0] === 'number' && __big(a[0], 'Array'), Reflect.construct(t, a, nt)),
        })
        Object.defineProperty(OrigArray.prototype, 'constructor', {value: SafeArray, writable: true, configurable: true})
        __global.Array = SafeArray
    }

    // ── Fuel ──
    //
    // `__f` counts loop iterations and function entries, the two places the
    // rewrite puts a check — the same two Luau's interrupt fires on, so the Luau
    // host's budgets carry over as numbers. Every 1024 steps the check also reads
    // the process's resident memory: a slice that grew it by more than
    // `sliceHeap` is stopped.
    //
    // Resident memory, not `bun:jsc`'s `heapSize()`: that walks the heap, 26 µs at
    // 10 MB and 318 µs at 100 MB (1.4.4), and read once per slice it made a tick
    // quadratic in the number of scripts — 1000 content scripts took 3.1 s. The
    // resident size is ~9 µs at any size. It is read lazily, at a slice's first
    // check, so a handler shorter than 1024 steps never pays for it; what those
    // steps can allocate is bounded by the built-in caps above, and past that by
    // the supervisor.
    let __f = 0
    let __heapStart = -1
    let __memOver = false
    const __SLICE_HEAP = __limits.sliceHeap
    const __mem = () => {
        const now = __heap()
        if (__heapStart < 0) {
            __heapStart = now
            return false
        }
        if (now - __heapStart > __SLICE_HEAP) {
            __memOver = true
            __f = -1
            return true
        }
        return false
    }

    // Generators behind plain functions. The map is the only way from one to the
    // other, and the author cannot reach it — so the author never holds a
    // generator object and cannot drive one past an exhausted budget.
    const __gens = new WeakMap()
    // Captured before the author's code runs, which may replace
    // `Function.prototype.apply` or a generator's `next` in its own realm. The
    // generator prototypes are frozen too: what the rewritten code delegates
    // through must stay the engine's.
    const __apply = Reflect.apply
    const __GeneratorFunction = Object.getPrototypeOf(function* () {})
    const __next = __GeneratorFunction.prototype.next
    Object.freeze(__GeneratorFunction.prototype)
    Object.freeze(__GeneratorFunction)
    const __fn = (g) => {
        const f = function (...a) {
            // Called from a built-in (`arr.map(f)`) or with `new`: there is no
            // `yield*` chain back to the host, so this runs to the end. Overrunning
            // here is fatal — the Luau host has the same edge at C calls.
            const it = __apply(g, this, a)
            for (;;) {
                const r = __apply(__next, it, [])
                if (r.done) return r.value
                if (__f < 0) {
                    throw new RangeError('ran out of time inside a callback given to a built-in function, where a script cannot be paused')
                }
            }
        }
        __gens.set(f, g)
        return f
    }

    // ── The prim, as the host last told it ──

    // `PrimView::default()`: a script that runs before the first tick still reads numbers.
    const __blank = () => ({pos: [0, 0, 0], rot: [0, 0, 0], color: [1, 1, 1], scale: [1, 1, 1], alpha: 1, glow: 0, full_bright: false, name: "", particles: null, geo: [0, 0], nearby: [], physical: false, velocity: [0, 0, 0], mass: 0})
    let __view = __blank()
    let __time = 0
    let __id = 0
    let __out = []
    let __sends = 0
    let __sounds = 0
    const __push = (cmd) => {
        if (__out.length >= __limits.commandsPerTick) {
            __out.dropped = (__out.dropped || 0) + 1
            return
        }
        __out.push(cmd)
    }
    const __num = (v, what) => {
        if (typeof v !== 'number' || !Number.isFinite(v)) throw new TypeError(`${what}: expected a finite number, got ${v}`)
        return v
    }
    const __vec = (x, y, z, what) => [__num(x, what), __num(y, what), __num(z, what)]
    const __clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v))
    const __text = (t) => String(t).slice(0, __limits.maxText)

    // WGS84 ground distance in a tangent plane — `cartopolis_core::degree_scale_m`.
    const __A = 6378137.0
    const __E2 = 6.69437999014e-3
    const __ecef = (lat, lng) => {
        const p = (lat * Math.PI) / 180
        const l = (lng * Math.PI) / 180
        const n = __A / Math.sqrt(1 - __E2 * Math.sin(p) ** 2)
        return [n * Math.cos(p) * Math.cos(l), n * Math.cos(p) * Math.sin(l), n * (1 - __E2) * Math.sin(p)]
    }
    const __ground = (la, ga, lb, gb) => {
        const [ax, ay, az] = __ecef(la, ga)
        const [bx, by, bz] = __ecef(lb, gb)
        const [dx, dy, dz] = [bx - ax, by - ay, bz - az]
        const p = (la * Math.PI) / 180
        const l = (ga * Math.PI) / 180
        const e = -Math.sin(l) * dx + Math.cos(l) * dy
        const n = -Math.sin(p) * Math.cos(l) * dx - Math.sin(p) * Math.sin(l) * dy + Math.cos(p) * dz
        return Math.hypot(e, n)
    }

    // ── The script API: `cartopolis_core::SCRIPT_API`, in JavaScript ──
    //
    // Several values come back as an array where Luau returns several values:
    // `const [x, y, z] = get_pos()`. Arguments are checked here so the author gets
    // the error on their line; the host checks again, since everything in the
    // realm — these functions included — is the author's to replace.
    const api = {
        get_pos: () => [...__view.pos],
        set_pos: (x, y, z) => {
            const v = __vec(x, y, z, 'set_pos')
            __push({SetPos: {pos: v}})
            __view.pos = v
        },
        get_geo: () => [...__view.geo],
        set_geo: (lat, lng) => {
            if (!Number.isFinite(lat) || !Number.isFinite(lng) || Math.abs(lat) > 90 || Math.abs(lng) > 180) {
                throw new RangeError('set_geo: expected a latitude in -90..90 and a longitude in -180..180')
            }
            __push({SetGeo: {latitude: lat, longitude: lng}})
            __view.geo = [lat, lng]
        },
        geo_scale: (lat) => {
            lat = __clamp(__num(lat, 'geo_scale'), -90, 90)
            const step = lat >= 0 ? -1 : 1
            return [Math.max(__ground(lat, 0, lat + step, 0), 1e-3), Math.max(__ground(lat, 0, lat, 1), 1e-3)]
        },
        get_rot: () => __view.rot.map((r) => (r * 180) / Math.PI),
        set_rot: (x, y, z) => {
            const v = __vec(x, y, z, 'set_rot').map((d) => (d * Math.PI) / 180)
            __push({SetRot: {rot: v}})
            __view.rot = v
        },
        get_scale: () => [...__view.scale],
        set_scale: (x, y, z) => {
            const v = __vec(x, y, z, 'set_scale').map((s) => __clamp(s, 0.01, 64))
            __push({SetScale: {scale: v}})
            __view.scale = v
        },
        set_physical: (on) => {
            __push({SetPhysical: {on: !!on}})
            __view.physical = !!on
        },
        get_physical: () => __view.physical,
        get_vel: () => [...__view.velocity],
        set_vel: (x, y, z) => __push({SetVel: {velocity: __vec(x, y, z, 'set_vel')}}),
        apply_impulse: (x, y, z) => __push({ApplyImpulse: {impulse: __vec(x, y, z, 'apply_impulse')}}),
        get_mass: () => __view.mass,
        get_color: () => [...__view.color],
        set_color: (r, g, b) => {
            const c = __vec(r, g, b, 'set_color').map((v) => __clamp(v, 0, 1))
            __push({SetColor: {color: c}})
            __view.color = c
        },
        get_alpha: () => __view.alpha,
        set_alpha: (a) => {
            const v = __clamp(__num(a, 'set_alpha'), 0, 1)
            __push({SetAlpha: {alpha: v}})
            __view.alpha = v
        },
        get_glow: () => __view.glow,
        set_glow: (g) => {
            const v = __clamp(__num(g, 'set_glow'), 0, 1)
            __push({SetGlow: {glow: v}})
            __view.glow = v
        },
        set_full_bright: (on) => {
            __push({SetFullBright: {on: !!on}})
            __view.full_bright = !!on
        },
        particles: (rules) => __push({SetParticles: {rules: rules == null ? null : JSON.parse(JSON.stringify(rules)), burst: false}}),
        particle_burst: (rules) => __push({SetParticles: {rules: JSON.parse(JSON.stringify(rules ?? {})), burst: true}}),
        play_sound: (name, volume, range) => {
            if (__sounds >= __limits.soundsPerTick) {
                __out.dropped = (__out.dropped || 0) + 1
                return
            }
            __sounds++
            __push({PlaySound: {name: String(name), volume, range}})
        },
        loop_sound: (name, volume, range) => __push({SetSoundLoop: {name: String(name), volume, range}}),
        stop_sound: () => __push({SetSoundLoop: null}),
        set_tone: (rules) => __push({SetTone: JSON.parse(JSON.stringify(rules))}),
        stop_tone: () => __push({SetTone: null}),
        log: (text) => __push({Output: {level: 'Info', text: __text(text)}}),
        whisper: (text) => __push({Say: {text: __text(text), radius: __limits.whisperRadius}}),
        say: (text) => __push({Say: {text: __text(text), radius: __limits.sayRadius}}),
        shout: (text) => __push({Say: {text: __text(text), radius: __limits.shoutRadius}}),
        set_timer_event: (s) => __push({Timer: Math.max(0, __num(s, 'set_timer_event'))}),
        get_time: () => __time,
        reset_time: () => {
            __time = 0
            __push({ResetTime: true})
        },
        sensor: (range) => {
            range = Math.max(0, __num(range, 'sensor'))
            const hit = __view.nearby.find((n) => n[2] <= range)
            return hit ? [...hit] : []
        },
        sense_hands: (range) => __push({SenseHands: {range: __clamp(__num(range, 'sense_hands'), 0, __limits.maxHandRange)}}),
        send: (channel, text) => {
            if (__sends >= __limits.messagesPerTick) {
                __out.dropped = (__out.dropped || 0) + 1
                return
            }
            __sends++
            __push({Send: {channel: Math.trunc(__num(channel, 'send')), text: __text(text)}})
        },
        listen: (channel, on) => __push({Listen: {channel: Math.trunc(__num(channel, 'listen')), on: on === undefined ? true : !!on}}),
        get_prim_id: () => __id,
        get_name: () => __view.name,
        set_name: (name) => {
            const n = String(name).slice(0, 64)
            __push({SetName: {name: n}})
            __view.name = n
        },
        die: () => __push({Die: true}),
    }
    for (const [name, f] of Object.entries(api)) {
        Object.defineProperty(__global, name, {value: f, writable: true, configurable: true, enumerable: false})
    }
    // `sleep` is the one call that is a generator: it yields until the script
    // clock passes, which the scheduler already resumes every tick — the Luau
    // host's prelude does exactly this with `coroutine.yield`.
    Object.defineProperty(__global, 'sleep', {
        value: __fn(function* (s) {
            const wake = __time + __num(s, 'sleep')
            while (__time < wake) yield
        }),
        writable: true,
        configurable: true,
        enumerable: false,
    })

    // ── The author's code ──

    // The undeletable global, out of the author's sight (see the allowlist).
    // eslint-disable-next-line no-unused-vars
    const Bun = undefined

    const __top = __fn(function* () {
        /*AUTHOR*/
    })

    // ── What the host can ask for ──

    let __handlers = null
    let __running = null
    const __message = (e) => (e && typeof e === 'object' && 'message' in e ? `${e.name}: ${e.message}` : String(e))

    /** One slice: resume the running generator with `budget` fuel. → "status spent [message]". */
    const __slice = (budget) => {
        __f = budget
        __memOver = false
        __heapStart = -1
        let status
        let message = ''
        try {
            const r = __apply(__next, __running, [])
            status = r.done ? 'done' : 'yield'
            if (r.done) {
                if (__handlers === null) __handlers = r.value
                __running = null
            }
        } catch (e) {
            status = 'error'
            message = __message(e)
            __running = null
        }
        if (__memOver) {
            status = 'memory'
            __running = null
        }
        const spent = Math.max(0, budget - Math.max(__f, 0))
        return `${status} ${spent} ${message}`
    }

    return function __control(op, a, b) {
        switch (op) {
            case 'id':
                __id = a
                return __marker.name
            case 'view':
                __view = Object.assign(__blank(), JSON.parse(a))
                return ''
            case 'time':
                __time = a
                return ''
            case 'load':
                __running = __apply(__gens.get(__top), undefined, [])
                return __slice(a)
            case 'handlers':
                return Object.keys(__handlers || {})
                    .filter((k) => typeof __handlers[k] === 'function')
                    .join(',')
            case 'start': {
                const h = __handlers && __handlers[a]
                if (typeof h !== 'function') return '0'
                __running = __apply(__gens.get(h), undefined, JSON.parse(b))
                return '1'
            }
            case 'running':
                return __running ? '1' : '0'
            case 'run':
                return __slice(a)
            case 'drain': {
                const out = JSON.stringify({commands: __out, dropped: __out.dropped || 0})
                __out = []
                __sends = 0
                __sounds = 0
                return out
            }
        }
        return ''
    }
})
