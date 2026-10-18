/**
 * What the browser tests share (`chat-browser.ts`, `meeting-browser.ts`): a
 * real simulator with ann, bob and carol in it, all contacts of each other; the
 * web UI served against it; and headless Chromium profiles driven over the
 * DevTools Protocol, as `tools/cdp_probe.ts` is, so nothing here needs an npm
 * package.
 *
 *   --sim PATH       the simulator binary (default $CARGO_TARGET_DIR/debug/simulator)
 *   --chromium PATH  the browser (default ~/bin/chromium)
 *   --shots DIR      where a test writes its screenshots
 */
import {mkdirSync, mkdtempSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'
import {Database} from 'bun:sqlite'

const arg = (name: string) => {
    const i = process.argv.indexOf(name)
    return i > 0 ? process.argv[i + 1] : undefined
}
export const sim = arg('--sim') ?? path.join(process.env.CARGO_TARGET_DIR ?? 'target', 'debug/simulator')
export const shots = arg('--shots')
export const chromium = arg('--chromium') ?? `${process.env.HOME}/bin/chromium`
const web = import.meta.dir

export function freePort(): number {
    const s = Bun.listen({hostname: '127.0.0.1', port: 0, socket: {data() {}}})
    const port = s.port
    s.stop(true)
    return port
}

export async function waitFor(what: string, f: () => Promise<boolean>, ms = 20000): Promise<void> {
    const deadline = Date.now() + ms
    while (Date.now() < deadline) {
        if (await f().catch(() => false)) return
        await Bun.sleep(150)
    }
    throw new Error(`timed out waiting for ${what}`)
}

export const procs: {kill(): void}[] = []

export async function bootSim(): Promise<number> {
    const dir = mkdtempSync(path.join(tmpdir(), 'chat-browser-'))
    const db = path.join(dir, 'world.db')
    for (const name of ['ann', 'bob', 'carol', 'dave']) {
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
        env: {...process.env, RUST_LOG: process.env.RUST_LOG ?? 'info'},
        // The log is on stdout (tracing's default); a panic is on stderr.
        stdout: Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')),
        stderr: Bun.file(path.join(tmpdir(), 'chat-browser-sim.err')),
    }))
    await waitFor('the simulator', async () => (await fetch(`http://127.0.0.1:${http}/api/health`)).ok)
    return http
}

export async function bootWeb(simHttp: number): Promise<number> {
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
export class Browser {
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
            // A camera and microphone that need no hardware and no permission prompt.
            '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream',
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
            } else if (msg.method === 'Runtime.exceptionThrown') {
                // An error nobody caught: the page carries on, and a test that
                // only reads the page would never hear of it.
                const d = msg.params.exceptionDetails
                this.logs.push(`uncaught: ${d.exception?.description ?? d.text}`)
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
        await this.submit('form.chat-door')
        await waitFor(`${this.name} signed in`, () => this.eval('!!document.querySelector(".chat-side")'))
        await waitFor(`${this.name}'s chat connected`, () => this.eval('!document.querySelector(".chat-status")'))
    }

    /**
     * Choose the community on the rail, then open its topic: the two levels
     * a person goes through (`docs/mockups/web-navigation/`). The rail's tiles
     * are matched by title, since a wide screen shows only their pictures.
     */
    async openTopic(community: string, topic: string, ms = 30000): Promise<void> {
        const tile = `[...document.querySelectorAll('.home-tile')].find((b) => b.title === ${JSON.stringify(community)})`
        const item = `[...document.querySelectorAll('.chat-side .chat-item')].find((b) => b.innerText.includes(${JSON.stringify(topic)}))`
        await waitFor(`${community} on ${this.name}'s rail`, () => this.eval(`!!${tile}`), ms)
        await this.eval(`${tile}.click()`)
        await waitFor(`#${topic} in ${this.name}'s sidebar`, () => this.eval(`!!${item}`), ms)
        await this.eval(`${item}.click()`)
    }

    /** Choose a community on the rail: its page, on a wide screen. */
    async openCommunity(community: string): Promise<void> {
        await this.eval(`[...document.querySelectorAll('.home-tile')].find((b) => b.title === ${JSON.stringify(community)}).click()`)
    }

    async shot(file: string): Promise<void> {
        const r = await this.send('Page.captureScreenshot', {format: 'png'})
        await Bun.write(file, Buffer.from(r.result.data, 'base64'))
    }
}

