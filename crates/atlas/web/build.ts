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
 */
import {existsSync, rmSync} from 'node:fs'
import path from 'node:path'

const root = import.meta.dir
const dist = path.join(root, 'dist')
const serve = process.argv.includes('--serve')

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

    const result = await Bun.build({
        entrypoints: [path.join(root, 'src/index.ts')],
        // Content hashing is pointless here and actively unhelpful: index.html
        // is written by hand and would have to be templated to name the hash.
        // The site is ~30 KB and Caddy is told not to cache it.
        naming: 'app.js',
        minify: !serve,
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

    await Bun.write(path.join(dist, 'index.html'), Bun.file(path.join(root, 'src/index.html')))
    await Bun.write(path.join(dist, 'app.css'), Bun.file(path.join(root, 'src/css/app.css')))

    const bytes = (await Bun.file(path.join(dist, 'app.js')).arrayBuffer()).byteLength
    console.log(`built dist/ (app.js ${Math.round(bytes / 1024)} KB)`)
}

await build()

if (serve) {
    Bun.serve({
        port: PORT,
        async fetch(req) {
            const url = new URL(req.url)

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

            // Mounted as in production (`src/lib/base.ts`): at the site root.
            const file = url.pathname === '/' ? '/index.html' : url.pathname
            const candidate = Bun.file(path.join(dist, file))
            if (await candidate.exists()) {
                return new Response(candidate)
            }
            // Single-page app: an unknown path is a route, not a 404.
            return new Response(Bun.file(path.join(dist, 'index.html')), {
                headers: {'Content-Type': 'text/html'},
            })
        },
    })
    console.log(`web UI on http://localhost:${PORT}/ (api → ${API})`)
}
