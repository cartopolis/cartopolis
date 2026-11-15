/**
 * What the Luau host promises, held against the Bun one: a script is paused,
 * not killed, when it runs long; it cannot reach the host; and it cannot take
 * the memory the others need.
 */
import {describe, expect, test} from 'bun:test'
import {readFileSync} from 'node:fs'
import path from 'node:path'

import {ScriptHost, type ScriptCommand} from '../src/host'
import {LIMITS} from '../src/limits'

const errors = (out: ScriptCommand[]) => out.filter((c: any) => c.Output?.level === 'Error').map((c: any) => c.Output.text as string)
const said = (out: ScriptCommand[]) => out.filter((c: any) => c.Say).map((c: any) => c.Say.text as string)

/** Run `expr` at a script's top level and say its value, or return the refusal. */
function probe(expr: string): string {
    const host = new ScriptHost()
    host.load(1, `let out; try { out = String(${expr}) } catch (e) { out = "threw " + e.name } function on_start() { say(out) }`)
    const out = host.tick(0.05, new Map())
    return errors(out)[0] ?? said(out)[0]
}

describe('pausing', () => {
    test('a handler that never ends is paused each tick, and the others still run', () => {
        const host = new ScriptHost()
        host.load(1, 'function on_start() { while (true) {} }')
        host.load(2, 'let n = 0; function on_start() { set_timer_event(0.05) } function on_timer() { n++; say("tick " + n) }')
        const ticks = Array.from({length: 10}, () => host.tick(0.05, new Map()))
        expect(ticks.flatMap(errors)).toEqual([])
        expect(said(ticks.flat()).length).toBeGreaterThanOrEqual(8)
    })

    test('deep recursion is spread over ticks and finishes right', () => {
        const host = new ScriptHost()
        host.load(1, 'function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) } function on_start() { say(String(fib(20))) }')
        let out: ScriptCommand[] = []
        let ticks = 0
        while (said(out).length === 0 && ticks < 100) {
            out = host.tick(0.05, new Map())
            ticks++
        }
        expect(said(out)).toEqual(['6765'])
        expect(ticks).toBeGreaterThan(1)
    })

    test('sleep waits on the script clock', () => {
        const host = new ScriptHost()
        host.load(1, 'function on_start() { say("a"); sleep(0.5); say("b") }')
        const out = Array.from({length: 12}, () => said(host.tick(0.05, new Map())))
        expect(out[0]).toEqual(['a'])
        expect(out.findIndex((s) => s.includes('b'))).toBeGreaterThanOrEqual(9)
    })

    test('an endless top level is refused at load, not left running', () => {
        const host = new ScriptHost()
        host.load(1, 'while (true) {}')
        expect(errors(host.tick(0.05, new Map()))[0]).toContain('load budget')
    })

    test('callbacks given to built-ins work', () => {
        expect(probe('[3, 1, 2].map((x) => x * 2).sort((a, b) => a - b).join(",")')).toBe('2,4,6')
    })
})

describe('the sandbox', () => {
    test.each([
        ['typeof process', 'undefined'],
        ['typeof Bun', 'undefined'],
        ['typeof globalThis', 'undefined'],
        ['typeof fetch', 'undefined'],
        ['typeof setTimeout', 'undefined'],
        ['typeof WebAssembly', 'undefined'],
        ['typeof SharedArrayBuffer', 'undefined'],
        ['typeof ShadowRealm', 'undefined'],
        ['typeof eval', 'undefined'],
        ['(function () { return this })()', 'undefined'],
        ['[1].map(function () { return typeof this })[0]', 'undefined'],
        ['(() => {}).constructor("return 1")', 'threw EvalError'],
        ['get_pos.constructor("return 1")', 'threw EvalError'],
        ['Function("return 1")', 'threw EvalError'],
        ['Object.getPrototypeOf(say).constructor("return 1")', 'threw EvalError'],
        ['typeof Error.prepareStackTrace', 'undefined'],
    ])('%s → %s', (expr, want) => {
        expect(probe(expr)).toBe(want)
    })

    test.each([
        ['import("node:fs")', 'cannot import'],
        ['__f = 1e12', 'reserved for the host'],
        ['(async () => 1)()', 'async functions'],
        ['(function* () {})', 'generator functions'],
        ['class A {}', 'classes'],
        ['({get x() { return 1 }})', 'getters and setters'],
    ])('%s is refused', (expr, why) => {
        expect(probe(expr)).toContain(why)
    })

    test('scripts cannot see each other', () => {
        const host = new ScriptHost()
        host.load(1, 'Object.prototype.stolen = "yes"; Math.random = () => 4')
        host.load(2, 'function on_start() { say(String(({}).stolen) + " " + (Math.random() < 1)) }')
        expect(said(host.tick(0.05, new Map()))).toEqual(['undefined true'])
    })
})

// All scripts share one realm (realm-runtime.js says why), so what keeps them
// apart is that every intrinsic is frozen and each script's state is a closure.
// Each test here is one way a script could leave something for another to find.
describe('one realm, scripts apart', () => {
    test.each([
        ['Object.prototype.x = 1', 'TypeError'],
        ['Array.prototype.push = () => 0', 'TypeError'],
        ['Math.random = () => 4', 'TypeError'],
        ['JSON.parse = () => 1', 'TypeError'],
        ['String.prototype.trim = () => "x"', 'TypeError'],
        ['Object.defineProperty(Object.prototype, "x", {value: 1})', 'TypeError'],
        ['Object.getPrototypeOf(function* () {}).prototype.next = 1', 'reserved for the host'],
        ['Object.getPrototypeOf([][Symbol.iterator]()).next = () => ({done: true})', 'TypeError'],
        ['Object.setPrototypeOf(Object.prototype, {})', 'TypeError'],
        ['Error.prototype.message = "x"', 'TypeError'],
    ])('%s is refused', (code, kind) => {
        const host = new ScriptHost()
        host.load(1, code)
        const err = errors(host.tick(0.05, new Map()))[0] ?? ''
        expect(kind === 'TypeError' ? err.split(':')[0] : err).toContain(kind)
    })

    test('the last regular-expression match is not left for the next script', () => {
        const host = new ScriptHost()
        host.load(1, 'function on_start() { /(secret)/.test("a secret") }')
        host.load(2, 'function on_start() { say(String(RegExp.$1) + " " + String(RegExp.lastMatch)) }')
        expect(said(host.tick(0.05, new Map()))).toEqual(['undefined undefined'])
    })

    test('the global symbol registry is gone', () => {
        expect(probe('typeof Symbol.for')).toBe('undefined')
    })

    test('two prims with the same source keep their own state', () => {
        const host = new ScriptHost()
        const src = 'let n = 0; function on_touch() { n++; say(String(n)) }'
        host.load(1, src)
        host.load(2, src)
        host.tick(0.05, new Map())
        for (let i = 0; i < 3; i++) host.event(1, {kind: 'Touch', avatar_id: 1, avatar_name: 'A'})
        host.event(2, {kind: 'Touch', avatar_id: 1, avatar_name: 'A'})
        const out = [...host.tick(0.05, new Map()), ...host.tick(0.05, new Map()), ...host.tick(0.05, new Map())]
        expect(out.filter((c: any) => c.Say && c.prim_id === 1).map((c: any) => c.Say.text)).toEqual(['1', '2', '3'])
        expect(out.filter((c: any) => c.Say && c.prim_id === 2).map((c: any) => c.Say.text)).toEqual(['1'])
    })

    test('replacing an API call breaks only the script that did it', () => {
        const host = new ScriptHost()
        host.load(1, 'say = null')
        host.load(2, 'function on_start() { say("still here") }')
        expect(said(host.tick(0.05, new Map()))).toEqual(['still here'])
    })

    test('a script cannot add a global for another to read', () => {
        const host = new ScriptHost()
        host.load(1, 'try { leaked = 1 } catch (e) {} ; var also = 2')
        host.load(2, 'function on_start() { say(typeof leaked + " " + typeof also) }')
        expect(said(host.tick(0.05, new Map()))).toEqual(['undefined undefined'])
    })
})

describe('memory', () => {
    test('a slice that allocates without end is stopped', () => {
        const host = new ScriptHost()
        host.load(1, 'const keep = []; function on_start() { while (true) keep.push(new Array(1000).fill(0)) }')
        let out: ScriptCommand[] = []
        for (let i = 0; i < 50 && errors(out).length === 0; i++) out = host.tick(0.05, new Map())
        expect(errors(out)[0]).toContain('MiB in one slice')
    })

    test.each([
        ['"x".repeat(2 ** 28).length', 'threw RangeError'],
        ['"x".padEnd(2 ** 28).length', 'threw RangeError'],
        ['new Array(2 ** 28).fill(0).length', 'threw RangeError'],
        ['Array(2 ** 28).length', 'threw RangeError'],
        ['[].constructor(2 ** 28).length', 'threw RangeError'],
        ['Array.from({length: 2 ** 28}).length', 'threw RangeError'],
    ])('%s is refused inside the built-in', (expr, want) => {
        expect(probe(expr)).toBe(want)
    })

    test('the audit finds the script holding memory between ticks', () => {
        const host = new ScriptHost()
        host.load(1, 'function on_start() {}')
        host.load(2, 'const keep = []; function on_start() { set_timer_event(0.05) } function on_timer() { for (let i = 0; i < 4000; i++) keep.push({i, s: "x" + i}) }')
        host.load(3, 'function on_start() {}')
        for (let i = 0; i < 60; i++) host.tick(0.05, new Map())
        const sizes = host.audit()
        expect(sizes[0].primId).toBe(2)
        expect(sizes[0].bytes).toBeGreaterThan(LIMITS.scriptRetained)
        expect(errors(host.tick(0.05, new Map()))[0]).toContain('more than the')
    }, 60_000)
})

describe('the API', () => {
    test('every call in the catalogue in cartopolis_core is there', () => {
        const catalogue = readFileSync(path.join(import.meta.dir, '../../../crates/core/src/script_api.rs'), 'utf8')
        const api = catalogue.slice(catalogue.indexOf('pub const SCRIPT_API'))
        const names = [...api.matchAll(/name: "(\w+)"/g)].map((m) => m[1]).sort()
        // Every catalogued name is a function in the realm.
        const host = new ScriptHost()
        host.load(9, `function on_start() { say([${names.map((n) => `typeof ${n}`).join(',')}].join(",")) }`)
        expect(said(host.tick(0.05, new Map()))[0]).toBe(names.map(() => 'function').join(','))
    })

    test('the four content scripts load', () => {
        const host = new ScriptHost()
        for (const [i, name] of ['beacon', 'firebird', 'football', 'theremin'].entries()) {
            host.load(i + 1, readFileSync(path.join(import.meta.dir, '../content', `${name}.js`), 'utf8'))
        }
        expect(errors(host.tick(0.05, new Map()))).toEqual([])
    })
})
