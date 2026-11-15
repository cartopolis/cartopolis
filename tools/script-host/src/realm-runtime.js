// The runtime of the one ShadowRealm every script runs in. Plain JavaScript,
// not TypeScript: it is read as text and evaluated in the realm, where nothing
// from the host module exists.
//
// One realm for all scripts, not one each. Measured on Bun 1.4.4: the same
// firebird handler cost 89 µs a run spread over 500 realms and 9 µs in one —
// JavaScriptCore optimises code per realm, so 500 copies of the runtime and the
// script each stayed cold — and 2000 realms held 1.6 GB. Scripts are therefore
// kept apart the way SES compartments are: every intrinsic is frozen before any
// script runs (nothing one script can change is seen by another), and each
// script's state lives in its own closure, never in a global.
//
// Only primitives and functions cross a ShadowRealm's edge, so the host talks
// to this file through `control(op, …)` with numbers and strings.
;(function (__heap, __limitsJson) {
    'use strict'
    const __limits = JSON.parse(__limitsJson)
    const __global = globalThis
    const __apply = Reflect.apply

    // ── What the realm may still reach ──
    //
    // An allowlist, so that anything Bun adds to a realm's global in a future
    // release arrives switched *off* — the rule `Slot::compile` applies to mlua's
    // `require`. Measured on Bun 1.4.4: a fresh ShadowRealm carries `process`,
    // `fetch` and `Bun`, so without this a script holds the server.
    const __keep = new Set([
        'Object', 'Function', 'Array', 'Number', 'parseFloat', 'parseInt', 'Infinity', 'NaN',
        'undefined', 'Boolean', 'String', 'Symbol', 'Date', 'RegExp', 'Error', 'AggregateError',
        'EvalError', 'RangeError', 'ReferenceError', 'SyntaxError', 'TypeError', 'URIError',
        'JSON', 'Math', 'Map', 'Set', 'WeakMap', 'WeakSet', 'Reflect', 'Proxy', 'isFinite',
        'isNaN', 'encodeURIComponent', 'decodeURIComponent', 'encodeURI', 'decodeURI',
    ])
    // `Bun` cannot be deleted: on 1.4.4 it is non-configurable and read-only in
    // every realm, and `Bun.spawnSync(["id"])` from a script ran a program on the
    // host. Every script's code is wrapped in a function with a parameter of that
    // name (`realm.ts`), and `globalThis` goes, so no path is left to the global
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

    // ── Shared state the intrinsics carry, which would be a channel between scripts ──
    //
    // V8's stack-trace hooks (Bun has them too); `RegExp.$1` and the other legacy
    // statics, which hold the last match *any* script made; and the global symbol
    // registry, where one script could leave a name for another to find.
    for (const key of ['prepareStackTrace', 'captureStackTrace', 'stackTraceLimit']) {
        try {
            delete Error[key]
        } catch {}
    }
    for (const key of Object.getOwnPropertyNames(RegExp)) {
        if (key !== 'length' && key !== 'name' && key !== 'prototype' && key !== 'escape') delete RegExp[key]
    }
    delete Symbol.for
    delete Symbol.keyFor

    // No constructor in the realm compiles code: the author's source is meant to
    // be what runs, and code compiled at run time would skip the rewrite — no
    // fuel checks. `Function` stays as a name, for `instanceof`.
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
    // Not complete — the slice memory check and the process backstop catch the rest.
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
                    return __apply(orig, this, a)
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
    // `__s.f` counts loop iterations and function entries, the two places the
    // rewrite puts a check — the same two Luau's interrupt fires on, so the Luau
    // host's budgets carry over as numbers. One counter for the realm: one script
    // runs at a time. Every 1024 steps the check also reads the process's
    // resident memory, and a slice that grew it by more than `sliceHeap` stops.
    //
    // Resident memory, not `bun:jsc`'s `heapSize()`: that walks the heap, 26 µs
    // at 10 MB and 318 µs at 100 MB (1.4.4), and read once per slice it made a
    // tick quadratic in the number of scripts. The resident size is ~9 µs at any
    // size, and is read lazily, at a slice's first check, so a handler shorter
    // than 1024 steps never pays for it; what those steps can allocate is bounded
    // by the built-in caps above, and past that by the supervisor.
    const __s = {f: 0, heapStart: -1, memOver: false}
    const __SLICE_HEAP = __limits.sliceHeap
    const __mem = () => {
        const now = __heap()
        if (__s.heapStart < 0) {
            __s.heapStart = now
            return false
        }
        if (now - __s.heapStart > __SLICE_HEAP) {
            __s.memOver = true
            __s.f = -1
            return true
        }
        return false
    }

    // Generators behind plain functions. The map is the only way from one to the
    // other, and no script can reach it — so a script never holds a generator
    // object and cannot drive one past an exhausted budget.
    const __gens = new WeakMap()
    const __next = Object.getPrototypeOf(function* () {}).prototype.next
    const __fn = (g) => {
        const f = function (...a) {
            // Called from a built-in (`arr.map(f)`) or with `new`: there is no
            // `yield*` chain back to the host, so this runs to the end. Overrunning
            // here is fatal — the Luau host has the same edge at C calls.
            const it = __apply(g, this, a)
            for (;;) {
                const r = __apply(__next, it, [])
                if (r.done) return r.value
                if (__s.f < 0) {
                    throw new RangeError('ran out of time inside a callback given to a built-in function, where a script cannot be paused')
                }
            }
        }
        __gens.set(f, g)
        return f
    }
    // What the rewritten code needs; handed to each script's factory, never a global.
    const __rt = Object.freeze({__fn, __gens, __apply, __mem, __s})

    // ── Freeze every intrinsic ──
    //
    // This is what keeps scripts apart in one realm: `Object.prototype.x = 1`
    // throws instead of reaching every other script. A walk from each global and
    // from the intrinsics no global names (iterator and generator prototypes),
    // freezing every object and accessor it meets.
    {
        const seen = new WeakSet()
        // The global object itself is frozen last, once the registration hook is on it.
        const stack = []
        for (const key of __keep) stack.push(__global[key])
        stack.push(
            Object.getPrototypeOf(function* () {}),
            Object.getPrototypeOf(async function () {}),
            Object.getPrototypeOf(async function* () {}),
            Object.getPrototypeOf((function* () {})()),
            Object.getPrototypeOf([][Symbol.iterator]()),
            Object.getPrototypeOf(new Map()[Symbol.iterator]()),
            Object.getPrototypeOf(new Set()[Symbol.iterator]()),
            Object.getPrototypeOf(''[Symbol.iterator]()),
            Object.getPrototypeOf(/a/[Symbol.matchAll]('')),
        )
        while (stack.length) {
            const o = stack.pop()
            if ((typeof o !== 'object' && typeof o !== 'function') || o === null || seen.has(o)) continue
            seen.add(o)
            Object.freeze(o)
            stack.push(Object.getPrototypeOf(o))
            for (const key of Reflect.ownKeys(o)) {
                const d = Reflect.getOwnPropertyDescriptor(o, key)
                if (!d) continue
                if ('value' in d) stack.push(d.value)
                else stack.push(d.get, d.set)
            }
        }
    }

    // ── One script ──

    // `PrimView::default()`: a script that runs before the first tick still reads numbers.
    const __blank = () => ({pos: [0, 0, 0], rot: [0, 0, 0], color: [1, 1, 1], scale: [1, 1, 1], alpha: 1, glow: 0, full_bright: false, name: '', particles: null, geo: [0, 0], nearby: [], physical: false, velocity: [0, 0, 0], mass: 0})
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

    /**
     * The script API — `cartopolis_core::SCRIPT_API` — for one prim, as closures
     * over that prim's state. Several values come back as an array where Luau
     * returns several values: `const [x, y, z] = get_pos()`. Arguments are
     * checked here so the author gets the error on their line; the host checks
     * what comes out again.
     */
    const __api = (inst) => {
        const push = (cmd) => {
            if (inst.out.length >= __limits.commandsPerTick) {
                inst.dropped++
                return
            }
            inst.out.push(cmd)
        }
        const api = {
            get_pos: () => [...inst.view.pos],
            set_pos: (x, y, z) => {
                const v = __vec(x, y, z, 'set_pos')
                push({SetPos: {pos: v}})
                inst.view.pos = v
            },
            get_geo: () => [...inst.view.geo],
            set_geo: (lat, lng) => {
                if (!Number.isFinite(lat) || !Number.isFinite(lng) || Math.abs(lat) > 90 || Math.abs(lng) > 180) {
                    throw new RangeError('set_geo: expected a latitude in -90..90 and a longitude in -180..180')
                }
                push({SetGeo: {latitude: lat, longitude: lng}})
                inst.view.geo = [lat, lng]
            },
            geo_scale: (lat) => {
                lat = __clamp(__num(lat, 'geo_scale'), -90, 90)
                const step = lat >= 0 ? -1 : 1
                return [Math.max(__ground(lat, 0, lat + step, 0), 1e-3), Math.max(__ground(lat, 0, lat, 1), 1e-3)]
            },
            get_rot: () => inst.view.rot.map((r) => (r * 180) / Math.PI),
            set_rot: (x, y, z) => {
                const v = __vec(x, y, z, 'set_rot').map((d) => (d * Math.PI) / 180)
                push({SetRot: {rot: v}})
                inst.view.rot = v
            },
            get_scale: () => [...inst.view.scale],
            set_scale: (x, y, z) => {
                const v = __vec(x, y, z, 'set_scale').map((s) => __clamp(s, 0.01, 64))
                push({SetScale: {scale: v}})
                inst.view.scale = v
            },
            set_physical: (on) => {
                push({SetPhysical: {on: !!on}})
                inst.view.physical = !!on
            },
            get_physical: () => inst.view.physical,
            get_vel: () => [...inst.view.velocity],
            set_vel: (x, y, z) => push({SetVel: {velocity: __vec(x, y, z, 'set_vel')}}),
            apply_impulse: (x, y, z) => push({ApplyImpulse: {impulse: __vec(x, y, z, 'apply_impulse')}}),
            get_mass: () => inst.view.mass,
            get_color: () => [...inst.view.color],
            set_color: (r, g, b) => {
                const c = __vec(r, g, b, 'set_color').map((v) => __clamp(v, 0, 1))
                push({SetColor: {color: c}})
                inst.view.color = c
            },
            get_alpha: () => inst.view.alpha,
            set_alpha: (a) => {
                const v = __clamp(__num(a, 'set_alpha'), 0, 1)
                push({SetAlpha: {alpha: v}})
                inst.view.alpha = v
            },
            get_glow: () => inst.view.glow,
            set_glow: (g) => {
                const v = __clamp(__num(g, 'set_glow'), 0, 1)
                push({SetGlow: {glow: v}})
                inst.view.glow = v
            },
            set_full_bright: (on) => {
                push({SetFullBright: {on: !!on}})
                inst.view.full_bright = !!on
            },
            particles: (rules) => push({SetParticles: {rules: rules == null ? null : JSON.parse(JSON.stringify(rules)), burst: false}}),
            particle_burst: (rules) => push({SetParticles: {rules: JSON.parse(JSON.stringify(rules ?? {})), burst: true}}),
            play_sound: (name, volume, range) => {
                if (inst.sounds >= __limits.soundsPerTick) {
                    inst.dropped++
                    return
                }
                inst.sounds++
                push({PlaySound: {name: String(name), volume, range}})
            },
            loop_sound: (name, volume, range) => push({SetSoundLoop: {name: String(name), volume, range}}),
            stop_sound: () => push({SetSoundLoop: null}),
            set_tone: (rules) => push({SetTone: JSON.parse(JSON.stringify(rules))}),
            stop_tone: () => push({SetTone: null}),
            log: (text) => push({Output: {level: 'Info', text: __text(text)}}),
            whisper: (text) => push({Say: {text: __text(text), radius: __limits.whisperRadius}}),
            say: (text) => push({Say: {text: __text(text), radius: __limits.sayRadius}}),
            shout: (text) => push({Say: {text: __text(text), radius: __limits.shoutRadius}}),
            set_timer_event: (s) => push({Timer: Math.max(0, __num(s, 'set_timer_event'))}),
            get_time: () => inst.time,
            reset_time: () => {
                inst.time = 0
                push({ResetTime: true})
            },
            sensor: (range) => {
                range = Math.max(0, __num(range, 'sensor'))
                const hit = inst.view.nearby.find((n) => n[2] <= range)
                return hit ? [...hit] : []
            },
            sense_hands: (range) => push({SenseHands: {range: __clamp(__num(range, 'sense_hands'), 0, __limits.maxHandRange)}}),
            send: (channel, text) => {
                if (inst.sends >= __limits.messagesPerTick) {
                    inst.dropped++
                    return
                }
                inst.sends++
                push({Send: {channel: Math.trunc(__num(channel, 'send')), text: __text(text)}})
            },
            listen: (channel, on) => push({Listen: {channel: Math.trunc(__num(channel, 'listen')), on: on === undefined ? true : !!on}}),
            get_prim_id: () => inst.id,
            get_name: () => inst.view.name,
            set_name: (name) => {
                const n = String(name).slice(0, 64)
                push({SetName: {name: n}})
                inst.view.name = n
            },
            die: () => push({Die: true}),
            // `sleep` is the one call that is a generator: it yields until the
            // script clock passes, which the scheduler already resumes every tick —
            // the Luau host's prelude does exactly this with `coroutine.yield`.
            sleep: __fn(function* (s) {
                const wake = inst.time + __num(s, 'sleep')
                while (inst.time < wake) yield
            }),
        }
        return Object.freeze(api)
    }

    // ── Sources and instances ──
    //
    // A source is compiled once (`realm.ts` evaluates it through the hook below)
    // and instantiated once per prim, so a hundred copies of the beacon share one
    // compiled — and soon optimised — function.
    const __factories = []
    // Named so the heap audit can find the runtime's own scope: what it reaches
    // (the frozen intrinsics, the compiled sources) is shared, not any script's.
    const __runtimeMarker = function __script_runtime() {}
    const __token = String(Math.random()).slice(2)
    Object.defineProperty(__global, `__register_${__token}`, {
        value: (outer) => {
            __factories.push(outer(undefined, undefined, __rt))
            return __factories.length - 1
        },
    })
    // Not a deep freeze: `Bun`, shadowed and out of every script's reach, would
    // materialise its lazy properties against globals that no longer hold what
    // they need (`Promise is not defined`).
    Object.freeze(__global)

    const __instances = new Map()
    const __message = (e) => (e && typeof e === 'object' && 'message' in e ? `${e.name}: ${e.message}` : String(e))

    /** One slice of `inst`'s running generator with `budget` fuel. → "status spent message". */
    const __slice = (inst, budget) => {
        __s.f = budget
        __s.memOver = false
        __s.heapStart = -1
        let status
        let message = ''
        try {
            const r = __apply(__next, inst.running, [])
            status = r.done ? 'done' : 'yield'
            if (r.done) {
                if (inst.handlers === null) inst.handlers = r.value
                inst.running = null
            }
        } catch (e) {
            status = 'error'
            message = __message(e)
            inst.running = null
        }
        if (__s.memOver) {
            status = 'memory'
            inst.running = null
        }
        const spent = Math.max(0, budget - Math.max(__s.f, 0))
        return `${status} ${spent} ${message}`
    }

    const __drain = (inst) => {
        const out = JSON.stringify({commands: inst.out, dropped: inst.dropped})
        inst.out = []
        inst.dropped = 0
        inst.sends = 0
        inst.sounds = 0
        return out
    }

    return function control(op, a, b, c, d, e, f) {
        switch (op) {
            case 'token':
                return `${__token} ${__runtimeMarker.name}`
            case 'new': {
                // a: prim id, b: factory, c: load fuel
                const inst = {id: a, view: __blank(), time: 0, out: [], dropped: 0, sends: 0, sounds: 0, handlers: null, running: null}
                // A property named after the prim, so the heap audit (`audit.ts`)
                // can find the instance: the snapshot names property edges, but
                // labels a function only by the name written in its source.
                inst[`__prim_${a}`] = {}
                inst.running = __apply(__gens.get(__factories[b](__api(inst))), undefined, [])
                const top = __slice(inst, c)
                if (top.startsWith('done')) __instances.set(a, inst)
                const handlers = Object.keys(inst.handlers || {}).filter((k) => typeof inst.handlers[k] === 'function')
                return `${top}\n${handlers.join(',')}\n${__drain(inst)}`
            }
            case 'drop':
                __instances.delete(a)
                return ''
            case 'slice': {
                // a: prim id, b: budget, c: time, d: view JSON or '', e: handler or '', f: args JSON
                const inst = __instances.get(a)
                if (!inst) return 'error 0 no such script\n{"commands":[],"dropped":0}'
                inst.time = c
                if (d) inst.view = Object.assign(__blank(), JSON.parse(d))
                if (e) {
                    const h = inst.handlers && inst.handlers[e]
                    if (typeof h !== 'function') return 'done 0 \n{"commands":[],"dropped":0}'
                    inst.running = __apply(__gens.get(h), undefined, JSON.parse(f))
                }
                if (!inst.running) return 'done 0 \n{"commands":[],"dropped":0}'
                return `${__slice(inst, b)}\n${__drain(inst)}`
            }
        }
        return ''
    }
})
