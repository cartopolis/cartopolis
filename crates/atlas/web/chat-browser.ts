#!/usr/bin/env bun
/**
 * The chat page in two real browsers, against a real simulator.
 *
 *   bun run build:chat-core --dev && bun run build
 *   bun chat-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" --shots /tmp/chat
 *
 * What `chat-e2e.ts` cannot reach: the `SharedWorker`, its Web Lock, IndexedDB,
 * and the page itself. Two headless Chromium profiles (two people, two
 * browsers) sign in through the form, start a workspace and a channel, talk in
 * it, and one reloads to read the history back out of its own storage. Driven
 * over the DevTools Protocol, as `tools/cdp_probe.ts` is, so it needs no npm
 * package. `--shots DIR` writes a screenshot of each browser at the end.
 */
import {mkdirSync, mkdtempSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {Database} from 'bun:sqlite'

const arg = (name: string) => {
    const i = process.argv.indexOf(name)
    return i > 0 ? process.argv[i + 1] : undefined
}
const sim = arg('--sim') ?? path.join(process.env.CARGO_TARGET_DIR ?? 'target', 'debug/simulator')
const shots = arg('--shots')
const chromium = arg('--chromium') ?? `${process.env.HOME}/bin/chromium`
const web = import.meta.dir

function freePort(): number {
    const s = Bun.listen({hostname: '127.0.0.1', port: 0, socket: {data() {}}})
    const port = s.port
    s.stop(true)
    return port
}

async function waitFor(what: string, f: () => Promise<boolean>, ms = 20000): Promise<void> {
    const deadline = Date.now() + ms
    while (Date.now() < deadline) {
        if (await f().catch(() => false)) return
        await Bun.sleep(150)
    }
    throw new Error(`timed out waiting for ${what}`)
}

const procs: {kill(): void}[] = []

async function bootSim(): Promise<number> {
    const dir = mkdtempSync(path.join(tmpdir(), 'chat-browser-'))
    const db = path.join(dir, 'world.db')
    for (const name of ['ann', 'bob']) {
        const p = Bun.spawnSync([sim, '--database-path', db, '--create-admin', name], {
            env: {...process.env, CARTO_ADMIN_PASSWORD: 'pw'},
            stderr: 'pipe',
        })
        if (p.exitCode !== 0) throw new Error(`creating ${name}: ${p.stderr}`)
    }
    const sql = new Database(db)
    sql.run(`INSERT INTO contacts (account, contact, since_ms)
             SELECT a.id, b.id, 0 FROM accounts a, accounts b WHERE a.id <> b.id`)
    sql.close()
    const http = freePort()
    procs.push(Bun.spawn([sim, '--database-path', db, '--listen', '127.0.0.1:0',
        '--http-listen', `127.0.0.1:${http}`, '--calls-listen', '127.0.0.1:0'], {
        env: {...process.env, RUST_LOG: 'info'},
        stderr: Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')),
        stdout: 'ignore',
    }))
    await waitFor('the simulator', async () => (await fetch(`http://127.0.0.1:${http}/api/health`)).ok)
    return http
}

async function bootWeb(simHttp: number): Promise<number> {
    const port = freePort()
    procs.push(Bun.spawn(['bun', 'build.ts', '--serve'], {
        cwd: web,
        env: {...process.env, CARTO_ADMIN_API: `http://127.0.0.1:${simHttp}`, PORT: String(port)},
        stderr: 'ignore',
        stdout: 'ignore',
    }))
    await waitFor('the web UI', async () => (await fetch(`http://127.0.0.1:${port}/chat`)).ok)
    return port
}

/** One browser profile, driven over the DevTools Protocol. */
class Browser {
    private ws!: WebSocket
    private id = 0
    private pending = new Map<number, (v: any) => void>()
    readonly logs: string[] = []

    constructor(readonly name: string) {}

    async launch(url: string): Promise<void> {
        const port = freePort()
        const profile = mkdtempSync(path.join(tmpdir(), `chat-${this.name}-`))
        procs.push(Bun.spawn([chromium, '--headless=new', '--no-sandbox', '--disable-gpu',
            `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, '--window-size=1400,860',
            'about:blank'], {stderr: 'ignore', stdout: 'ignore'}))
        let target: {webSocketDebuggerUrl: string} | undefined
        await waitFor(`${this.name}'s browser`, async () => {
            const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json() as any[]
            target = list.find((t) => t.type === 'page')
            return !!target
        })
        this.ws = new WebSocket(target!.webSocketDebuggerUrl)
        await new Promise((r) => (this.ws.onopen = r))
        this.ws.onmessage = (e) => {
            const msg = JSON.parse(String(e.data))
            if (msg.id && this.pending.has(msg.id)) {
                this.pending.get(msg.id)!(msg)
                this.pending.delete(msg.id)
            } else if (msg.method === 'Runtime.consoleAPICalled') {
                this.logs.push(msg.params.args.map((a: any) => a.value ?? a.description).join(' '))
            }
        }
        await this.send('Runtime.enable')
        await this.send('Page.enable')
        await this.send('Page.navigate', {url})
    }

    send(method: string, params: object = {}): Promise<any> {
        const id = ++this.id
        this.ws.send(JSON.stringify({id, method, params}))
        return new Promise((r) => this.pending.set(id, r))
    }

    /** Run `js` in the page; answers its (awaited) value. */
    async eval<T = unknown>(js: string): Promise<T> {
        const r = await this.send('Runtime.evaluate', {awaitPromise: true, expression: js, returnByValue: true})
        if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.text)
        return r.result?.result?.value as T
    }

    has(text: string): Promise<boolean> {
        return this.eval<boolean>(`document.body.innerText.includes(${JSON.stringify(text)})`)
    }

    until(text: string, ms?: number): Promise<void> {
        return waitFor(`"${text}" in ${this.name}'s page`, () => this.has(text), ms)
    }

    /**
     * Click the first button whose title is `label`, or whose text ends with
     * it: a button that carries an icon beside its words has both in its text.
     */
    click(label: string): Promise<boolean> {
        return this.eval(`(() => {
            const b = [...document.querySelectorAll('button, summary')].find((x) =>
                x.title === ${JSON.stringify(label)} || x.innerText.trim().endsWith(${JSON.stringify(label)}))
            if (!b) return false
            b.click()
            return true
        })()`)
    }

    /** Type into the field matched by `selector`, as a person would. */
    type(selector: string, value: string): Promise<boolean> {
        return this.eval(`(() => {
            const el = document.querySelector(${JSON.stringify(selector)})
            if (!el) return false
            const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
            Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, ${JSON.stringify(value)})
            el.dispatchEvent(new Event('input', {bubbles: true}))
            return true
        })()`)
    }

    submit(selector: string): Promise<boolean> {
        return this.eval(`(() => {
            const f = document.querySelector(${JSON.stringify(selector)})
            if (!f) return false
            f.requestSubmit()
            return true
        })()`)
    }

    async signIn(username: string): Promise<void> {
        await this.until('Sign in')
        await this.type('input[autocomplete=username]', username)
        await this.type('input[autocomplete=current-password]', 'pw')
        await this.submit('form.signin')
        await waitFor(`${this.name} signed in`, () => this.eval('!!document.querySelector(".chat-rail")'))
        await waitFor(`${this.name}'s chat connected`, () => this.eval('!document.querySelector(".chat-status")'))
    }

    async shot(file: string): Promise<void> {
        const r = await this.send('Page.captureScreenshot', {format: 'png'})
        await Bun.write(file, Buffer.from(r.result.data, 'base64'))
    }
}

/** Every browser launched, for the failure report. */
const browsers: Browser[] = []

const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)

try {
    const simHttp = await bootSim()
    const port = await bootWeb(simHttp)
    // The site's front door: the chat, or its welcome card when signed out.
    const url = `http://127.0.0.1:${port}/`
    step(`simulator and web UI up (${url})`)

    const ann = new Browser('ann')
    const bob = new Browser('bob')
    browsers.push(ann, bob)
    await ann.launch(url)
    await bob.launch(url)
    await ann.until('Open the map')
    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'welcome.png'))
    }
    await ann.signIn('ann')
    await bob.signIn('bob')
    step('both signed in through the form; the shared worker holds their keys')

    // Ann: a workspace, Bob in it, a channel.
    await ann.click('New workspace')
    await ann.type('.chat-dialog input', 'Atelier Noord')
    await ann.submit('.chat-dialog')
    await waitFor('the workspace in the rail', () => ann.eval('document.querySelectorAll(".chat-ws").length >= 3'))
    await ann.click('Atelier Noord')
    await ann.click('Add people')
    await ann.type('.chat-dialog input', 'bob')
    await ann.submit('.chat-dialog')
    await ann.until('bob')
    await ann.click('New channel')
    await ann.type('.chat-dialog input', 'ebbinge quarter')
    await ann.submit('.chat-dialog')
    await ann.until('ebbinge-quarter')
    step('ann made a workspace, added bob, started #ebbinge-quarter')

    // Bob: the workspace appears; he asks into the channel.
    await waitFor('the workspace for bob', () => bob.eval('document.querySelectorAll(".chat-ws").length >= 3'))
    await bob.click('Atelier Noord')
    await bob.until('Browse 1 more')
    if (!(await bob.click('Browse 1 more'))) throw new Error('no Browse button')
    await bob.until('Waiting for a member', 1).catch(() => undefined)
    if (!(await bob.click('Join'))) throw new Error('no Join button')
    await waitFor('bob let into the channel', () =>
        bob.eval('[...document.querySelectorAll(".chat-item")].some((b) => b.innerText.includes("ebbinge-quarter"))'), 30000)
    await bob.click('Close')
    step("bob asked into the channel and ann's browser let him in")

    // They talk.
    await ann.eval('[...document.querySelectorAll(".chat-item")].find((b) => b.innerText.includes("ebbinge-quarter")).click()')
    await ann.type('.chat-composer textarea', 'Scaffolding is down on the north side @bob')
    await ann.submit('.chat-composer')
    await bob.eval('[...document.querySelectorAll(".chat-item")].find((b) => b.innerText.includes("ebbinge-quarter")).click()')
    await bob.until('Scaffolding is down on the north side')
    await bob.type('.chat-composer textarea', 'The brick reads darker than the sample.')
    await bob.submit('.chat-composer')
    await ann.until('The brick reads darker than the sample.')
    // A reaction from Ann on Bob's line.
    await ann.eval(`(() => {
        const msg = [...document.querySelectorAll('.chat-msg')].find((m) => m.innerText.includes('brick reads darker'))
        msg.querySelector('.chat-actions button').click()
    })()`)
    await bob.until('👍 1')
    step('a line each way through the page, with a mention and a reaction')

    // Bob reloads: the history comes back out of his browser's own storage.
    await bob.send('Page.reload')
    // Back in the direct-messages view; the workspace returns to the rail
    // once the worker has signed in again.
    await waitFor('bob reconnected', () => bob.eval('document.querySelectorAll(".chat-ws").length >= 3'), 30000)
    await bob.click('Atelier Noord')
    await bob.eval('[...document.querySelectorAll(".chat-item")].find((b) => b.innerText.includes("ebbinge-quarter")).click()')
    await bob.until('Scaffolding is down on the north side')
    step('after a reload bob reads the history back out of IndexedDB')

    if (shots) {
        mkdirSync(shots, {recursive: true})
        await ann.shot(path.join(shots, 'ann.png'))
        await bob.shot(path.join(shots, 'bob.png'))
        step(`screenshots in ${shots}`)
    }
    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    for (const b of browsers) {
        const text = await b.eval<string>('document.body.innerText').catch(() => '(page gone)')
        console.error(`--- ${b.name}'s page:\n${text.slice(0, 1500)}`)
        console.error(`--- ${b.name}'s console:\n${b.logs.slice(-30).join('\n')}`)
    }
    const log = await Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text().catch(() => '')
    console.error(`--- simulator:\n${log.split('\n').filter((l) => /join|channel|e2ee|warn|error/i.test(l)).slice(-30).join('\n')}`)
    if (shots) {
        mkdirSync(shots, {recursive: true})
        for (const b of browsers) await b.shot(path.join(shots, `${b.name}-failed.png`)).catch(() => undefined)
    }
    process.exitCode = 1
} finally {
    for (const p of procs) p.kill()
}
