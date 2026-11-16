/**
 * The script host under the scenarios in `scenarios.json`, one JSON line per
 * measurement (`bun run bench`). The Luau host these were first run against
 * (`luau/`, until 2026-11-16) is gone; its numbers are in
 * `docs/notes/javascript-prim-scripts.md`.
 */
import {readFileSync} from 'node:fs'
import path from 'node:path'

import {ScriptHost, type ScriptCommand} from '../src/host'
import {Supervisor} from '../src/supervisor'

const here = import.meta.dir
const scenarios = JSON.parse(readFileSync(path.join(here, 'scenarios.json'), 'utf8'))
const content = ['beacon', 'firebird', 'football', 'theremin'].map((n) => readFileSync(path.join(here, '../../../crates/simulator/content', `${n}.js`), 'utf8'))

const rss = () => {
    Bun.gc(true)
    return process.memoryUsage().rss
}
const view = (i: number) => ({name: `prim ${i}`, geo: [53.2194, 6.5665], alpha: 1, scale: [1, 1, 1], color: [1, 1, 1]})
const stats = (ms: number[]) => {
    ms.sort((a, b) => a - b)
    return {mean: ms.reduce((a, b) => a + b, 0) / ms.length, p99: ms[Math.floor(ms.length * 0.99) - 1]}
}
const round = (v: number) => Math.round(v * 1000) / 1000
const errors = (out: ScriptCommand[]) => out.filter((c: any) => c.Output?.level === 'Error')

// One group per process (`bun bench/bench.ts memory 200`): measured in one process, the
// realms the memory runs leave behind slowed the CPU runs after them tenfold.
const only = process.argv[2]
const sizes = process.argv[3] ? [Number(process.argv[3])] : [200, 2000]

if (!only || only === 'memory') {
    for (const n of sizes) {
        for (const [label, sources] of [
            ['idle', [scenarios.idle.js]],
            ['content', content],
            // One pack each: timers every tick or every 0.1 s, and no messages. The
            // mix above is a message storm at this size — the packs are written for
            // one firebird, and 500 read each other's replies as a toggle.
            ['firebird', [content[1]]],
            ['football', [content[2]]],
        ] as const) {
            const host = new ScriptHost()
            const before = rss()
            const t = performance.now()
            for (let i = 1; i <= n; i++) host.load(i, sources[i % sources.length])
            const loadMs = performance.now() - t
            const views = new Map(Array.from({length: n}, (_, i) => [i + 1, view(i + 1)] as [number, object]))
            const ms: number[] = []
            const first = host.tick(0.05, views)
            if (errors(first).length) throw new Error(JSON.stringify(errors(first).slice(0, 3)))
            for (let tick = 0; tick < 200; tick++) {
                if (tick % 20 === 5) host.event(1, {kind: 'Touch', avatar_id: 42, avatar_name: 'Anna'})
                const t0 = performance.now()
                host.tick(0.05, views)
                ms.push(performance.now() - t0)
            }
            const after = rss()
            const {mean, p99} = stats(ms)
            console.log(
                JSON.stringify({
                    engine: `bun ${Bun.version}`,
                    scenario: label,
                    scripts: n,
                    bytes_per_script: Math.round((after - before) / n),
                    load_ms: Math.round(loadMs),
                    tick_mean_ms: round(mean),
                    tick_p99_ms: round(p99),
                }),
            )
        }
    }
}

if (!only || only === 'cpu') {
    for (const label of ['busy', 'calls']) {
        const host = new ScriptHost()
        for (let i = 1; i <= 10; i++) host.load(i, scenarios[label].js)
        host.tick(0.05, new Map())
        const ms: number[] = []
        let most = 0
        for (let tick = 0; tick < 100; tick++) {
            const t0 = performance.now()
            const out = host.tick(0.05, new Map())
            ms.push(performance.now() - t0)
            if (errors(out).length) throw new Error(JSON.stringify(errors(out)))
            for (const c of out as any[]) if (c.Output?.level === 'Info') most = Math.max(most, Number(c.Output.text))
        }
        const {mean, p99} = stats(ms)
        console.log(
            JSON.stringify({
                engine: `bun ${Bun.version}`,
                scenario: label,
                scripts: 10,
                tick_mean_ms: round(mean),
                tick_p99_ms: round(p99),
                work_per_tick_all_scripts: Math.round((most / 100) * 10),
            }),
        )
    }
}

if (!only || only === 'process') {
    // The same content world, but across the process boundary the simulator
    // would use: what the JSON round trip and the separate process add.
    for (const n of sizes) {
        const sup = new Supervisor({tickTimeoutMs: 5000, hardLimitBytes: 4 * 2 ** 30, auditAboveBytes: 2 ** 40, bun: process.execPath})
        for (let i = 1; i <= n; i++) sup.load(i, content[i % content.length])
        const views = new Map(Array.from({length: n}, (_, i) => [i + 1, view(i + 1)] as [number, object]))
        await sup.tick(0.05, views)
        const ms: number[] = []
        for (let tick = 0; tick < 200; tick++) {
            const t0 = performance.now()
            await sup.tick(0.05, views)
            ms.push(performance.now() - t0)
        }
        const {mean, p99} = stats(ms)
        console.log(
            JSON.stringify({
                engine: `bun ${Bun.version}, own process`,
                scenario: 'content',
                scripts: n,
                process_rss_bytes: await childRss(sup.pid),
                tick_mean_ms: round(mean),
                tick_p99_ms: round(p99),
            }),
        )
        await sup.close()
    }
}

async function childRss(pid: number): Promise<number> {
    const out = await new Response(Bun.spawn(['ps', '-o', 'rss=', '-p', String(pid)]).stdout).text()
    return Number(out.trim()) * 1024
}
