/**
 * The backstop: what no check inside the engine can stop, the supervisor stops
 * by killing the script process — and it has to blame the right script and
 * bring the others back.
 */
import {afterEach, describe, expect, test} from 'bun:test'

import {Supervisor} from '../src/supervisor'
import type {ScriptCommand} from '../src/host'

const errors = (out: ScriptCommand[]) => out.filter((c: any) => c.Output?.level === 'Error').map((c: any) => [c.prim_id, c.Output.text])
const said = (out: ScriptCommand[]) => out.filter((c: any) => c.Say).map((c: any) => [c.prim_id, c.Say.text])

let sup: Supervisor | null = null
afterEach(async () => {
    await sup?.close()
    sup = null
})

const start = (hardLimitBytes = 600 << 20) =>
    (sup = new Supervisor({tickTimeoutMs: 1000, hardLimitBytes, auditAboveBytes: 2 ** 40, bun: process.execPath}))

const OTHER = 'let n = 0; function on_start() { set_timer_event(0.05) } function on_timer() { n++; if (n % 5 === 0) say("still here " + n) }'

describe('the supervisor', () => {
    // JavaScriptCore gives up on a runaway expression by itself after about a
    // second (measured on 1.4.4) — and answers `false`, silently wrong. One call
    // is a one-second stall for every script; a loop of them is a hang, and the
    // fuel check between calls counts each as one step.
    test('a loop of runaway regular expressions is killed, and blamed', async () => {
        const s = start()
        s.load(1, OTHER)
        s.load(2, 'function on_touch() { for (let i = 0; i < 10; i++) /^(a+)+$/.test("a".repeat(40) + "!") }')
        await s.tick(0.05, new Map())
        s.event(2, {kind: 'Touch', avatar_id: 1, avatar_name: 'Anna'})
        const t0 = performance.now()
        const out = await s.tick(0.05, new Map())
        expect(performance.now() - t0).toBeLessThan(3000)
        expect(errors(out)).toEqual([[2, expect.stringContaining('did not finish within 1000 ms')]])
        expect(s.restarts).toBe(1)
        // The other script came back and runs again.
        let later: ScriptCommand[] = []
        for (let i = 0; i < 12; i++) later = later.concat(await s.tick(0.05, new Map()))
        expect(said(later).some(([id]) => id === 1)).toBe(true)
    }, 20_000)

    // 2^29 characters: a 512 MiB string once `indexOf` flattens it, all inside
    // one built-in call, so no check in the engine sees it coming. (2^31 is
    // refused by JavaScriptCore itself, as a RangeError the script gets.)
    test("a string doubled to half a gigabyte is killed, and blamed", async () => {
        const s = start(300 << 20)
        s.load(1, OTHER)
        s.load(2, 'function on_touch() { let s = "x"; for (let i = 0; i < 29; i++) s = s + s; say(String(s.indexOf("y"))) }')
        await s.tick(0.05, new Map())
        s.event(2, {kind: 'Touch', avatar_id: 1, avatar_name: 'Anna'})
        const out = await s.tick(0.05, new Map())
        const blamed = errors(out)
        expect(blamed.length).toBe(1)
        expect(blamed[0][0]).toBe(2)
        expect(s.restarts).toBe(1)
    }, 30_000)
})
