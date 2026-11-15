/**
 * The simulator's side of the script process, as the simulator would hold it:
 * spawn `child.ts`, feed it ticks, and kill it when it stops answering or grows
 * past its ceiling — then blame the prim that was running, restart the process
 * without that script, and load the others again.
 *
 * In the real thing this is Rust inside the simulator; it is TypeScript here so
 * the prototype runs on its own. What it costs is part of the measurement: a
 * restart loses every script's state (they start again from `on_start`), which
 * the Luau host never has to do, since its allocator refuses the allocation
 * instead of the operating system killing the process.
 */
import {mkdtempSync, readFileSync, writeFileSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'

import type {ScriptCommand, ScriptEvent} from './host'
import {LIMITS} from './limits'

export interface SupervisorOptions {
    /** A tick that has not answered by then is a hung script. */
    tickTimeoutMs: number
    /** Resident memory past which the process is killed outright. */
    hardLimitBytes: number
    /** Resident memory past which, between ticks, the heap audit runs. */
    auditAboveBytes: number
    /** The Bun to run the child with. */
    bun: string
    /** Run under a systemd scope with no network and a read-only filesystem (needs systemd-run). */
    confine?: boolean
}

const CHILD = path.join(import.meta.dir, 'child.ts')

export class Supervisor {
    private child!: ReturnType<typeof Bun.spawn>
    private lines!: AsyncIterator<string>
    private readonly sources = new Map<number, string>()
    private readonly faulted = new Set<number>()
    private readonly blame: Float64Array
    private readonly blamePath: string
    private pendingOut: ScriptCommand[] = []
    restarts = 0

    constructor(private readonly options: SupervisorOptions) {
        const dir = mkdtempSync(path.join(tmpdir(), 'script-host-'))
        this.blamePath = path.join(dir, 'blame')
        writeFileSync(this.blamePath, new Uint8Array(8))
        const mapped = Bun.mmap(this.blamePath)
        this.blame = new Float64Array(mapped.buffer, mapped.byteOffset, 1)
        this.spawn()
    }

    private spawn() {
        const argv = [this.options.bun, CHILD, this.blamePath]
        const cmd = this.options.confine
            ? ['systemd-run', '--user', '--scope', '--quiet', '-p', 'PrivateNetwork=yes', '-p', 'ProtectSystem=strict', '-p', 'ProtectHome=read-only', ...argv]
            : argv
        this.child = Bun.spawn(cmd, {stdin: 'pipe', stdout: 'pipe', stderr: 'inherit'})
        this.lines = readLines(this.child.stdout as ReadableStream<Uint8Array>)
        this.blame[0] = 0
        for (const [id, source] of this.sources) {
            if (!this.faulted.has(id)) this.send({op: 'load', prim_id: id, source})
        }
    }

    private send(req: object) {
        const stdin = this.child.stdin as import('bun').FileSink
        stdin.write(JSON.stringify(req) + '\n')
        stdin.flush()
    }

    get pid() {
        return this.child.pid
    }

    /** Resident memory of the script process: /proc on Linux, `ps` elsewhere. */
    rss(): number {
        try {
            if (process.platform === 'linux') {
                const kb = /VmRSS:\s+(\d+) kB/.exec(readFileSync(`/proc/${this.child.pid}/status`, 'utf8'))
                return kb ? Number(kb[1]) * 1024 : 0
            }
            const ps = Bun.spawnSync(['ps', '-o', 'rss=', '-p', String(this.child.pid)])
            return Number(ps.stdout.toString().trim()) * 1024 || 0
        } catch {
            return 0
        }
    }

    load(primId: number, source: string) {
        this.sources.set(primId, source)
        this.faulted.delete(primId)
        this.send({op: 'load', prim_id: primId, source})
    }

    unload(primId: number) {
        this.sources.delete(primId)
        this.send({op: 'unload', prim_id: primId})
    }

    event(primId: number, event: ScriptEvent) {
        this.send({op: 'event', prim_id: primId, event})
    }

    async tick(dt: number, views: Map<number, object>): Promise<ScriptCommand[]> {
        this.send({op: 'tick', dt, views: [...views]})
        const reply = await this.answer()
        let out = this.pendingOut
        this.pendingOut = []
        if (reply === null) return out
        out = out.concat(JSON.parse(reply).commands)
        if (this.rss() > this.options.auditAboveBytes) {
            this.send({op: 'audit'})
            const audit = await this.answer()
            if (audit !== null) {
                const sizes: {primId: number; bytes: number}[] = JSON.parse(audit).audit
                for (const s of sizes) if (s.bytes > LIMITS.scriptRetained) this.faulted.add(s.primId)
            }
        }
        return out
    }

    /** The next line from the child, or null if it had to be killed first. */
    private async answer(): Promise<string | null> {
        const started = performance.now()
        const next = this.lines.next()
        let killed: string | null = null
        const watch = setInterval(() => {
            if (killed) return
            if (performance.now() - started > this.options.tickTimeoutMs) killed = `did not finish within ${this.options.tickTimeoutMs} ms`
            else if (this.rss() > this.options.hardLimitBytes) killed = `took the script process past ${this.options.hardLimitBytes >> 20} MiB`
            if (killed) this.child.kill(9)
        }, 20)
        const r = await next
        clearInterval(watch)
        if (!r.done && !killed) return r.value
        await this.child.exited
        const blamed = this.blame[0]
        const reason = killed ?? `the script process exited (${this.child.exitCode ?? this.child.signalCode})`
        if (blamed) {
            this.faulted.add(blamed)
            this.pendingOut.push({prim_id: blamed, Output: {level: 'Error', text: `${reason} while this script was running; stopped`}})
        }
        this.restarts++
        this.spawn()
        // Every other script was restarted with the process; it says so.
        for (const id of this.sources.keys()) {
            if (!this.faulted.has(id)) this.pendingOut.push({prim_id: id, Output: {level: 'Notice', text: 'restarted: the script process was restarted because of another script'}})
        }
        return null
    }

    async close() {
        this.child.kill()
        await this.child.exited
    }
}

async function* readLines(stream: ReadableStream<Uint8Array>): AsyncGenerator<string> {
    const decoder = new TextDecoder()
    let buffer = ''
    for await (const chunk of stream) {
        buffer += decoder.decode(chunk, {stream: true})
        let nl: number
        while ((nl = buffer.indexOf('\n')) >= 0) {
            yield buffer.slice(0, nl)
            buffer = buffer.slice(nl + 1)
        }
    }
}
