#!/usr/bin/env bun
/**
 * The World tab against a real simulator: the page signs the app's web build
 * in (`lib/world-frame.ts`, `framed_session.rs`), and a browser without
 * WebGPU is told so instead (`docs/notes/web-client-is-the-world.md`).
 *
 *   bun run build:chat-core --dev && bun run build
 *   bun world-browser.ts --sim "$CARGO_TARGET_DIR/debug/simulator" \
 *       --viewer ../../cartopolis/dist --shots /tmp/world
 *
 * The dev server has no web build of the app and no cross-origin isolation,
 * which the build's threads need, so a small front sits before it: `/viewer/`
 * from `--viewer`, `/ws` bridged, the rest proxied, COOP and COEP on all of it
 * as Caddy sets them; and the map's own files, which the dev server answers
 * with its index page, from `--upstream` (cartopolis.org).
 */
import {mkdirSync} from 'node:fs'
import {tmpdir} from 'node:os'
import path from 'node:path'

import {Browser, bootSim, bootWeb, freePort, procs, shots, waitFor} from './browser-harness'

const arg = (name: string) => {
    const i = process.argv.indexOf(name)
    return i > 0 ? process.argv[i + 1] : undefined
}
const viewer = arg('--viewer') ?? '../../cartopolis/dist'
const upstream = arg('--upstream') ?? 'https://cartopolis.org'

/** What Caddy sets on the whole site: without it the build's workers never start. */
const isolation = {
    'Cross-Origin-Embedder-Policy': 'require-corp',
    'Cross-Origin-Opener-Policy': 'same-origin',
}

function isolated(r: Response): Response {
    const headers = new Headers(r.headers)
    for (const [k, v] of Object.entries(isolation)) headers.set(k, v)
    // Bun has already decoded a proxied body, so these would lie.
    headers.delete('content-encoding')
    headers.delete('content-length')
    return new Response(r.body, {headers, status: r.status})
}

/** The front: the site as Caddy serves it, with this branch's web build in it. */
function bootFront(web: number): number {
    const port = freePort()
    type Bridge = {up: WebSocket; queued: (string | Uint8Array<ArrayBuffer>)[]}
    Bun.serve<Bridge>({
        async fetch(req, server) {
            const url = new URL(req.url)
            if (url.pathname === '/ws') {
                const up = new WebSocket(`ws://127.0.0.1:${web}/ws`)
                if (server.upgrade(req, {data: {queued: [], up}})) return undefined
                up.close()
                return new Response('expected a WebSocket', {status: 400})
            }
            if (url.pathname.startsWith('/viewer')) {
                let rest = url.pathname.replace(/^\/viewer/, '')
                if (rest === '' || rest.endsWith('/')) rest += 'index.html'
                const file = Bun.file(path.join(viewer, rest))
                if (await file.exists()) return isolated(new Response(file))
            }
            const headers = new Headers(req.headers)
            headers.delete('accept-encoding')
            const local = await fetch(`http://127.0.0.1:${web}${url.pathname}${url.search}`, {
                body: req.body,
                duplex: 'half',
                headers,
                method: req.method,
            } as RequestInit)
            // The dev server answers a path it does not know with its index
            // page; a file the app asked for (a tile, a height) is the site's.
            const html = (local.headers.get('content-type') ?? '').includes('text/html')
            const page = (req.headers.get('accept') ?? '').includes('text/html')
            if (html && !page && !url.pathname.startsWith('/api/')) {
                for (const h of ['host', 'origin', 'referer', 'cookie', 'authorization']) headers.delete(h)
                return isolated(await fetch(`${upstream}${url.pathname}${url.search}`, {headers}))
            }
            return isolated(local)
        },
        hostname: '127.0.0.1',
        port,
        websocket: {
            close(ws) {
                ws.data.up.close()
            },
            message(ws, msg) {
                const bytes = typeof msg === 'string' ? msg : new Uint8Array(msg)
                if (ws.data.up.readyState === WebSocket.OPEN) ws.data.up.send(bytes)
                else ws.data.queued.push(bytes)
            },
            open(ws) {
                const up = ws.data.up
                up.binaryType = 'arraybuffer'
                up.onopen = () => {
                    for (const m of ws.data.queued) up.send(m)
                    ws.data.queued = []
                }
                up.onmessage = (e) => ws.send(new Uint8Array(e.data as ArrayBuffer))
                up.onclose = () => ws.close()
            },
        },
    })
    return port
}

/** Choose a top-bar mode by its word, as a person does. */
function chooseMode(b: Browser, mode: string): Promise<boolean> {
    return b.eval(`(() => {
        const el = [...document.querySelectorAll('nav button, nav a, header button, header a')]
            .find((x) => x.innerText.trim().toLowerCase() === ${JSON.stringify(mode.toLowerCase())})
        if (!el) return false
        el.click()
        return true
    })()`)
}

/**
 * A browser with WebGPU on a machine with no GPU: SwiftShader's adapter. The
 * ANGLE-on-Vulkan flags answer a null adapter on a build node (2026-11-22,
 * Chromium 158); these answer `google swiftshader`.
 */
const WEBGPU = ['--enable-unsafe-webgpu', '--use-webgpu-adapter=swiftshader', '--enable-unsafe-swiftshader',
    '--ignore-gpu-blocklist']

const browsers: Browser[] = []
const started = Date.now()
const step = (s: string) => console.log(`${((Date.now() - started) / 1000).toFixed(1).padStart(5)} s  ${s}`)
const signedInLine = "signing the world in with the page's session"

try {
    if (!(await Bun.file(path.join(viewer, 'index.html')).exists())) throw new Error(`no web build in ${viewer}`)
    const simHttp = await bootSim()
    const web = await bootWeb(simHttp)
    const front = bootFront(web)
    const url = `http://127.0.0.1:${front}/chat`
    step(`simulator, web UI and front up (${url})`)
    if (shots) mkdirSync(shots, {recursive: true})

    // Without WebGPU: World says what it needs; the chat goes on working.
    const plain = new Browser('plain')
    browsers.push(plain)
    await plain.launch(url)
    await plain.signIn('bob')
    if (!(await chooseMode(plain, 'World'))) throw new Error('no World in the top bar')
    await plain.until('This browser cannot show the world', 15000)
    if (await plain.eval('!!document.querySelector("iframe.home-world-frame")')) {
        throw new Error('a frame was made in a browser without WebGPU')
    }
    if (shots) await plain.shot(path.join(shots, 'world-no-webgpu.png'))
    step('without WebGPU: World says so, and makes no frame')

    // With WebGPU: the frame is made, says ready, and is signed in as the page.
    const ann = new Browser('ann')
    browsers.push(ann)
    await ann.launch(url, undefined, WEBGPU)
    await ann.signIn('ann')
    if (!(await chooseMode(ann, 'World'))) throw new Error('no World in the top bar')
    await waitFor('the World frame', () => ann.eval('!!document.querySelector("iframe.home-world-frame")'), 20000)
    step('with WebGPU: the frame is made')
    await waitFor('the world signed in by the page', async () => ann.logs.some((l) => l.includes(signedInLine)), 240000)
    step('the world took the page\'s session')
    // The simulator saw ann say hello twice: the chat worker, and the world.
    const log = () => Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text()
    await waitFor('a second connection for ann', async () =>
        ((await log()).match(/authenticated[^\n]*ann|ann[^\n]*authenticated|signed in[^\n]*ann/gi) ?? []).length >= 2, 60000)
    step('the simulator holds two connections for ann: the page\'s chat and the world')
    await Bun.sleep(20000)
    if (shots) await ann.shot(path.join(shots, 'world-signed-in.png'))

    console.log('ok')
} catch (e) {
    console.error(`FAILED: ${(e as Error).message}`)
    for (const b of browsers) {
        const text = await b.eval<string>('document.body.innerText').catch(() => '(page gone)')
        console.error(`--- ${b.name}'s page:\n${text.slice(0, 800)}`)
        console.error(`--- ${b.name}'s console:\n${b.logs.slice(-40).join('\n')}`)
        if (shots) await b.shot(path.join(shots, `${b.name}-failed.png`)).catch(() => undefined)
    }
    const log = await Bun.file(path.join(tmpdir(), 'chat-browser-sim.log')).text().catch(() => '')
    console.error(`--- simulator:\n${log.split('\n').filter((l) => /ann|hello|auth|warn|error/i.test(l)).slice(-30).join('\n')}`)
    process.exitCode = 1
} finally {
    for (const p of procs) p.kill()
}
