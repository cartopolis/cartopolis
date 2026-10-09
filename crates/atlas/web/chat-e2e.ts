#!/usr/bin/env bun
/**
 * Two browsers' worth of private chat against a real simulator, from Bun.
 *
 *   bun run build:chat-core --dev
 *   bun chat-e2e.ts --sim "$CARGO_TARGET_DIR/debug/simulator"
 *
 * Boots a throwaway simulator, makes two accounts who are contacts, and drives
 * two `ChatConnection`s — the same class the shared worker runs, the same
 * WebAssembly module, over the simulator's real WebSocket — through a direct
 * chat both ways, then takes one of them offline and brings it back from its
 * saved keys to read what was said meanwhile. Exits non-zero on the first thing
 * that does not arrive.
 *
 * Not part of `bun test`: it needs a simulator binary, which needs a Rust
 * build, and the web UI's own tests must not.
 */
import {mkdtempSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {Database} from 'bun:sqlite'
import {ChatConnection, type ChatCoreModule} from './src/worker/connection.ts'
import {MemoryKeyStore} from './src/worker/keystore.ts'
import type {ChatEvent} from './src/worker/protocol.ts'

const arg = (name: string) => {
    const i = process.argv.indexOf(name)
    return i > 0 ? process.argv[i + 1] : undefined
}
const sim = arg('--sim') ?? path.join(process.env.CARGO_TARGET_DIR ?? 'target', 'debug/simulator')
const pkg = path.join(import.meta.dir, 'src/worker/pkg')

function freePort(): number {
    const s = Bun.listen({hostname: '127.0.0.1', port: 0, socket: {data() {}}})
    const port = s.port
    s.stop(true)
    return port
}

async function boot(): Promise<{port: number; stop: () => void}> {
    const dir = mkdtempSync(path.join(tmpdir(), 'chat-e2e-'))
    const db = path.join(dir, 'world.db')
    for (const name of ['ann', 'bob']) {
        const p = Bun.spawnSync([sim, '--database-path', db, '--create-admin', name], {
            env: {...process.env, CARTO_ADMIN_PASSWORD: 'pw'},
            stderr: 'pipe',
        })
        if (p.exitCode !== 0) throw new Error(`creating ${name}: ${p.stderr}`)
    }
    // Contacts, so a chat between them is a chat and not a request (protocol 27).
    const sql = new Database(db)
    sql.run(`INSERT INTO contacts (account, contact, since_ms)
             SELECT a.id, b.id, 0 FROM accounts a, accounts b WHERE a.id <> b.id`)
    sql.close()

    const port = freePort()
    const proc = Bun.spawn([sim, '--database-path', db, '--listen', '127.0.0.1:0',
        '--http-listen', `127.0.0.1:${port}`, '--calls-listen', '127.0.0.1:0'], {stderr: 'ignore', stdout: 'ignore'})
    for (let i = 0; i < 100; i++) {
        if (await fetch(`http://127.0.0.1:${port}/api/health`).then((r) => r.ok, () => false)) {
            return {port, stop: () => proc.kill()}
        }
        await Bun.sleep(100)
    }
    proc.kill()
    throw new Error('the simulator never answered /api/health')
}

class Peer {
    events: ChatEvent[] = []
    conn: ChatConnection

    constructor(mod: ChatCoreModule, port: number, readonly name: string, readonly store: MemoryKeyStore) {
        this.conn = new ChatConnection(mod, {
            credential: {password: 'pw', username: name},
            onEvent: (e) => this.events.push(e),
            server: `127.0.0.1:${port}`,
            store,
            url: `ws://127.0.0.1:${port}/ws`,
        })
        this.conn.start()
    }

    async until<T>(what: string, find: (e: ChatEvent) => T | undefined, ms = 10000): Promise<T> {
        const deadline = Date.now() + ms
        while (Date.now() < deadline) {
            for (const e of this.events) {
                const hit = find(e)
                if (hit !== undefined) return hit
            }
            await Bun.sleep(25)
        }
        const errors = this.events.filter((e) => e.kind === 'error' || e.kind === 'refused' || e.kind === 'notice')
        throw new Error(`${this.name}: no ${what} within ${ms} ms (${JSON.stringify(errors)})`)
    }

    line(from: string, body: string): Promise<true> {
        return this.until(`line "${body}" from ${from}`, (e) =>
            e.kind === 'line' && e.from === from && e.body === body ? true : undefined)
    }
}

const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)

const glue = await import(path.join(pkg, 'cartopolis_web_chat.js'))
await glue.default({module_or_path: await Bun.file(path.join(pkg, 'cartopolis_web_chat_bg.wasm')).bytes()})
const mod = glue as unknown as ChatCoreModule

const server = await boot()
try {
    step(`simulator up on :${server.port}, protocol ${mod.ChatCore.protocol_version()}`)
    const ann = new Peer(mod, server.port, 'ann', new MemoryKeyStore())
    const bobKeys = new MemoryKeyStore()
    let bob = new Peer(mod, server.port, 'bob', bobKeys)
    // Every person is a full address (protocol 37); the dev simulator is called
    // `localhost` after its public URL.
    const who = await ann.until('sign-in', (e) => (e.kind === 'signed_in' ? e.address : undefined))
    if (who !== 'ann@localhost') throw new Error(`signed in as ${who}, not ann@localhost`)
    await bob.until('sign-in', (e) => (e.kind === 'signed_in' ? true : undefined))
    await ann.conn.settled()
    await bob.conn.settled()
    step('both signed in over WebSocket, keys opened')

    await ann.conn.startDirect('bob')
    const channel = await ann.until('opened chat', (e) => (e.kind === 'opened' ? e.channel : undefined))
    await bob.until('invite', (e) => (e.kind === 'opened' && e.channel === channel ? true : undefined))
    step(`direct chat ${channel.slice(0, 12)}… open on both`)

    await ann.conn.sendText(channel, 'hallo bob, encrypted in wasm')
    await bob.line('ann@localhost', 'hallo bob, encrypted in wasm')
    await bob.conn.sendText(channel, 'hoi ann')
    await ann.line('bob@localhost', 'hoi ann')
    step('a line each way')

    const saved = bobKeys.values.size
    bob.conn.stop()
    await ann.conn.sendText(channel, 'said while you were away')
    await Bun.sleep(300)
    bob = new Peer(mod, server.port, 'bob', bobKeys)
    await bob.line('ann@localhost', 'said while you were away')
    step(`bob came back from ${saved} saved key blob and read what he missed`)

    if (bob.events.some((e) => e.kind === 'opened' && e.channel !== channel)) {
        throw new Error('bob came back as a new device and opened a different chat')
    }
    ann.conn.stop()
    bob.conn.stop()
    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    process.exitCode = 1
} finally {
    server.stop()
}
