#!/usr/bin/env bun
/**
 * Build the atlas web UI into `dist/`, and optionally serve it.
 *
 * Deliberately `Bun.build` and a handful of file copies rather than a bundler
 * with a config file: the output is three files, and the deploy is an rsync of a
 * directory Caddy serves. Nothing here needs a plugin ecosystem, and the CI lane
 * that runs it should be readable by whoever is debugging it at the time.
 *
 * `--serve` adds a dev server that proxies `/api` to a local simulator, so the
 * app runs against the real Rust API rather than a mock of it.
 *
 * `--serve --mock` is the exception, and only for looking at the UI: `/api/*`
 * is answered in-process by `mock.ts` instead of proxied, so the pages render
 * with realistic data on a machine with no simulator and no atlas — a screenshot,
 * a layout check on a phone. It is not how anything is tested. With
 * `?mock-chat` the page is the same app over a stand-in chat worker
 * (`mock-chat.ts`), so Community and its calendar draw with people in them.
 */
import {existsSync, readdirSync, readFileSync, rmSync, writeFileSync} from 'node:fs'
import path from 'node:path'
import zlib, {brotliCompressSync, gzipSync} from 'node:zlib'

const root = import.meta.dir
const dist = path.join(root, 'dist')
const serve = process.argv.includes('--serve')
const mock = serve && process.argv.includes('--mock')

/** Where `--serve` forwards `/api/*`. The simulator's `--http-listen`. */
const API = process.env.CARTO_ADMIN_API || 'http://127.0.0.1:4748'
/**
 * And where it forwards `/api/pipeline/*` — atlas (`cargo run -p
 * cartopolis_atlas`), which runs the map data pipelines.
 *
 * Two upstreams behind one path prefix, which is exactly what Caddy does in
 * production: the web UI has to see one origin, or the bearer it already holds
 * would need CORS and a second address to send it to.
 */
const PIPELINE = process.env.CARTO_PIPELINE_API || 'http://127.0.0.1:4749'
const PORT = Number(process.env.PORT || 3100)

async function build() {
    if (existsSync(dist)) {
        rmSync(dist, {recursive: true})
    }

    // The workers first: the page names their files, and they are content-hashed.
    const callWorkerUrl = await buildCallWorker()
    await buildChatWorker()

    const result = await Bun.build({
        entrypoints: [path.join(root, 'src/index.ts')],
        // Content-hashed, so Caddy can let a browser keep them for good (a deploy
        // is a new name) instead of asking again on every load: each revalidation
        // was a round trip before the page could start. `[ext]` because the entry
        // imports the stylesheet, which Bun bundles into a second output beside
        // the script; index.html is rewritten below to name both.
        naming: 'app-[hash].[ext]',
        define: {__CALL_WORKER_URL__: JSON.stringify(callWorkerUrl)},
        minify: !serve,
        // The client's typeface (`app.css`'s @font-face), copied into
        // `dist/fonts/` below. External so Bun leaves the URL alone: given a
        // path it can resolve, it inlines the font as base64, and 190 KB of that
        // in the stylesheet would block the first paint of every page.
        external: ['/fonts/*', '/img/*'],
        outdir: dist,
        sourcemap: serve ? 'inline' : 'none',
        target: 'browser',
    })

    if (!result.success) {
        for (const log of result.logs) {
            console.error(log)
        }
        throw new Error('build failed')
    }

    const named = (ext: string) => {
        const out = result.outputs.find((o) => o.path.endsWith(ext))
        if (!out) throw new Error(`the build made no ${ext}`)
        return `/${path.basename(out.path)}`
    }
    const appJs = named('.js')
    const html = (await Bun.file(path.join(root, 'src/index.html')).text())
        .replace('href="/app.css"', `href="${named('.css')}"`)
        .replace('src="/app.js"', `src="${appJs}"`)
    if (!html.includes(appJs)) throw new Error('index.html no longer names /app.js; update build.ts')
    await Bun.write(path.join(dist, 'index.html'), html)
    // The service worker that rings a closed browser for a call
    // (`src/lib/web-push.ts`). Copied as it is, to the root, because a service
    // worker only covers the directory it is served from.
    await Bun.write(path.join(dist, 'sw.js'), Bun.file(path.join(root, 'src/sw.js')))
    // The app's mark, for the favicon and the welcome card: the same files the
    // viewer uses, one per theme.
    await Bun.write(path.join(dist, 'icon.svg'), Bun.file(path.join(root, '../../../assets/icon/cartopolis.svg')))
    await Bun.write(path.join(dist, 'icon-light.svg'), Bun.file(path.join(root, '../../../assets/icon/cartopolis-light.svg')))
    // The same font files the client embeds, so the two cannot drift to
    // different cuts of the face.
    for (const face of ['PlusJakartaSans-Regular.ttf', 'PlusJakartaSans-SemiBold.ttf']) {
        await Bun.write(path.join(dist, 'fonts', face), Bun.file(path.join(root, '../../../assets/fonts', face)))
    }
    // The street map behind the glass (`depth.css`): a mask, made by atlas's
    // backdrop route (`pipeline::backdrop`) and committed.
    await Bun.write(path.join(dist, 'img/groningen.svg'), Bun.file(path.join(root, 'src/img/groningen.svg')))

    const bytes = (await Bun.file(path.join(dist, appJs)).arrayBuffer()).byteLength
    console.log(`built dist/ (${appJs.slice(1)} ${Math.round(bytes / 1024)} KB)`)

    if (mock) await buildMockChat()
    if (!serve) precompress()
}

/**
 * `--mock`'s second app: the same entry behind `mock-chat.ts`, which stands a
 * chat worker in with invented chats, so `?mock-chat` draws Community and its
 * calendar with nobody to chat with. Built only here; it never ships.
 */
async function buildMockChat() {
    const result = await Bun.build({
        entrypoints: [path.join(root, 'mock-chat.ts')],
        external: ['/fonts/*', '/img/*'],
        naming: 'mock-chat.[ext]',
        outdir: dist,
        sourcemap: 'inline',
        target: 'browser',
    })
    if (!result.success) {
        for (const log of result.logs) console.error(log)
        throw new Error('mock chat build failed')
    }
}

/**
 * The call encryption worker the chat page's calls run (`src/lib/call.ts`): the
 * viewer's own file and its SFrame, so the two seal frames identically. A
 * dedicated worker, so a hashed name costs nothing. Returns its URL.
 */
async function buildCallWorker(): Promise<string> {
    const callWorker = await Bun.build({
        entrypoints: [path.join(root, '../../cartopolis/web/call-worker.js')],
        format: 'esm',
        minify: !serve,
        naming: 'call-worker-[hash].js',
        outdir: dist,
        target: 'browser',
    })
    if (!callWorker.success) {
        for (const log of callWorker.logs) console.error(log)
        throw new Error('call worker build failed')
    }
    return `/${path.basename(callWorker.outputs[0].path)}`
}

/**
 * Brotli and gzip beside every script, stylesheet and module, which Caddy's
 * `precompressed` serves instead of compressing the 2.9 MB engine again on every
 * request; brotli at its best setting is also a tenth smaller than Caddy's zstd.
 */
function precompress() {
    for (const name of readdirSync(dist)) {
        if (!/\.(js|css|wasm)$/.test(name)) continue
        const file = path.join(dist, name)
        const raw = readFileSync(file)
        writeFileSync(`${file}.br`, brotliCompressSync(raw, {
            params: {
                [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
                [zlib.constants.BROTLI_PARAM_SIZE_HINT]: raw.length,
            },
        }))
        writeFileSync(`${file}.gz`, gzipSync(raw, {level: 9}))
    }
}

/**
 * The chat worker and the engine it loads (`src/worker/`, `docs/notes/web-chat.md`).
 *
 * The engine is Rust, built by `build-chat-core.ts` into `src/worker/pkg/`. Without
 * it the rest of the site still builds — editing a page must not need a Rust
 * toolchain — unless `--require-chat` says this build is the one being shipped.
 */
async function buildChatWorker() {
    const wasm = path.join(root, 'src/worker/pkg/cartopolis_web_chat_bg.wasm')
    if (!existsSync(wasm)) {
        if (process.argv.includes('--require-chat')) {
            throw new Error('src/worker/pkg/ is missing: run `bun run build:chat-core` first')
        }
        console.warn('no src/worker/pkg/: chat worker not built (bun run build:chat-core)')
        return
    }
    // The engine under a content-hashed name, which Caddy lets a browser keep;
    // the worker is told it. The worker itself keeps a fixed name: tabs share
    // one SharedWorker only while its URL is the same, so a new name per deploy
    // would leave an old tab and a new one in two workers fighting over the keys.
    const wasmBytes = readFileSync(wasm)
    const wasmName = `chat-core-${Bun.hash(wasmBytes).toString(36)}.wasm`
    await Bun.write(path.join(dist, wasmName), wasmBytes)
    const result = await Bun.build({
        entrypoints: [path.join(root, 'src/worker/chat-worker.ts')],
        define: {__CHAT_CORE_URL__: JSON.stringify(`/${wasmName}`)},
        format: 'esm',
        minify: !serve,
        naming: 'chat-worker.js',
        outdir: dist,
        sourcemap: serve ? 'inline' : 'none',
        target: 'browser',
    })
    if (!result.success) {
        for (const log of result.logs) {
            console.error(log)
        }
        throw new Error('chat worker build failed')
    }
    const kb = Math.round(wasmBytes.length / 1024)
    console.log(`built dist/chat-worker.js and dist/${wasmName} (${kb} KB)`)
}

/**
 * What `--mock` adds to `index.html`: a classic inline script, so it runs before
 * the app's module script reads localStorage (`src/lib/store.ts`).
 *
 * `?mock-signin` seeds the stored sessions with the mock's remembered tokens —
 * `admin` (the default) for the operator's `/admin`, `player` for `/buildings`
 * and `/building/{pand}`, `both`, or `none` to sign out — so a headless browser
 * opens a signed-in page with no form to fill. `?mock-tab=Data` then clicks the
 * admin tab with that label, because the open tab is reset on every load
 * (`volatileState`) and so cannot be seeded — matched on the button's text,
 * not a class, so it survives the layout changing around it. Both exist only
 * here: the real server never sees this script.
 */
async function mockIndex(url: URL): Promise<Response> {
    const {MOCK_ADMIN_TOKEN, MOCK_PLAYER, MOCK_PLAYER_TOKEN} = await import('./mock')
    const seed = `<script>
(() => {
  const q = new URLSearchParams(location.search)
  const KEY = 'cartopolis-admin'
  if (q.has('mock-signin')) {
    const who = q.get('mock-signin') || 'admin'
    let s = {}
    try { s = JSON.parse(localStorage.getItem(KEY) || '{}') } catch {}
    const admin = {principal: '', token: ${JSON.stringify(MOCK_ADMIN_TOKEN)}, username: 'viberfox'}
    const player = {token: ${JSON.stringify(MOCK_PLAYER_TOKEN)}, username: ${JSON.stringify(MOCK_PLAYER)}}
    s.profile = who === 'admin' || who === 'both' ? admin : {principal: '', token: '', username: ''}
    s.signup = who === 'player' || who === 'both' ? player : {token: '', username: ''}
    localStorage.setItem(KEY, JSON.stringify(s))
  }
  const tab = q.get('mock-tab')
  if (tab) {
    let tries = 0
    const click = () => {
      const button = [...document.querySelectorAll('nav button, nav a')]
        .find((b) => b.textContent.trim().toLowerCase() === tab.toLowerCase())
      // Clicked again shortly after: the first can land while the app is still
      // settling its first render, and re-selecting the open tab is harmless.
      if (button) [0, 300, 800].forEach((ms) => setTimeout(() => button.click(), ms))
      else if (++tries < 60) setTimeout(click, 50)
    }
    addEventListener('DOMContentLoaded', click)
  }
})()
</script>`
    let html = (await Bun.file(path.join(dist, 'index.html')).text()).replace('<head>', `<head>\n${seed}`)
    if (url.searchParams.has('mock-chat')) html = html.replace(/src="\/app-[^"]+\.js"/, 'src="/mock-chat.js"')
    return new Response(html, {headers: {'Content-Type': 'text/html'}})
}

await build()

if (serve) {
    const {mockApi} = mock ? await import('./mock') : {mockApi: null}

    /**
     * `/ws` is the simulator's WebSocket, which the chat worker opens on its own
     * origin as it does in production. Bridged message for message: every frame
     * is one binary message either way, so nothing here reads them. The mock
     * has no simulator behind it, so under `--mock` the chat stays signed out.
     */
    type Bridge = {upstream: WebSocket; queued: (string | Uint8Array<ArrayBuffer>)[]}
    const wsUpstream = API.replace(/^http/, 'ws') + '/ws'

    Bun.serve<Bridge>({
        port: PORT,
        websocket: {
            open(ws) {
                const up = ws.data.upstream
                up.binaryType = 'arraybuffer'
                up.onopen = () => {
                    for (const m of ws.data.queued) up.send(m)
                    ws.data.queued = []
                }
                up.onmessage = (e) => ws.send(new Uint8Array(e.data as ArrayBuffer))
                up.onclose = () => ws.close()
            },
            message(ws, msg) {
                const up = ws.data.upstream
                const bytes = typeof msg === 'string' ? msg : new Uint8Array(msg)
                if (up.readyState === WebSocket.OPEN) up.send(bytes)
                else ws.data.queued.push(bytes)
            },
            close(ws) {
                ws.data.upstream.close()
            },
        },
        async fetch(req, server) {
            const url = new URL(req.url)

            if (url.pathname === '/ws' && !mock) {
                const upstream = new WebSocket(wsUpstream)
                if (server.upgrade(req, {data: {queued: [], upstream}})) return undefined
                upstream.close()
                return new Response('expected a WebSocket', {status: 400})
            }

            // Answered here rather than proxied, pipeline prefix included: the
            // mock stands in for both upstreams. A route it does not know is a
            // 404 in the server's own shape, so the UI reports it as it would.
            if (mockApi && url.pathname.startsWith('/api/')) {
                return (await mockApi(req, url)) ??
                    Response.json({error: `The mock has no ${req.method} ${url.pathname}`}, {status: 404})
            }

            // Proxy to the simulator, headers and body intact — including the
            // Authorization bearer the app sends. The pipeline prefix is tested
            // first because it is the more specific of the two.
            if (url.pathname.startsWith('/api/')) {
                const upstream = url.pathname.startsWith('/api/pipeline/') ? PIPELINE : API
                return fetch(`${upstream}${url.pathname}${url.search}`, {
                    body: req.body,
                    // Bun requires this for a streamed request body.
                    duplex: 'half',
                    headers: req.headers,
                    method: req.method,
                } as RequestInit)
            }

            // The app's web build, which World frames. Caddy serves it from its
            // own deploy; this server has none, and answering with the web UI
            // (the catch-all below) would draw the page inside its own World.
            if (url.pathname.startsWith('/viewer/')) {
                return new Response(
                    '<!doctype html><body style="margin:0;display:grid;place-items:center;height:100vh;' +
                        'background:#0f1416;color:#aac2c6;font:15px system-ui">' +
                        `<p>The app runs here on cartopolis.org${url.search ? ` (${url.search})` : ''}.</p></body>`,
                    {headers: {'Content-Type': 'text/html'}},
                )
            }

            // Mounted as in production (`src/lib/base.ts`): at the site root.
            const file = url.pathname === '/' ? '/index.html' : url.pathname
            const candidate = Bun.file(path.join(dist, file))
            if (await candidate.exists() && file !== '/index.html') {
                return new Response(candidate)
            }
            // Single-page app: an unknown path is a route, not a 404.
            if (mock) {
                return mockIndex(url)
            }
            return new Response(Bun.file(path.join(dist, 'index.html')), {
                headers: {'Content-Type': 'text/html'},
            })
        },
    })
    console.log(`web UI on http://localhost:${PORT}/ (api → ${mock ? 'mock.ts' : API})`)
}
