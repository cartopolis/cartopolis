/**
 * The content pages' render process (`docs/mockups/cms`, v3): the same
 * executable as the script process, started as `render <bundle>`, where the
 * bundle is the web UI's `pages-ssr.js`. It answers each line
 * `{"id": n, "page": PageData}` on stdin with `{"id": n, "html": "…"}` (or
 * `"error"`) on stdout, one line each. The simulator keeps it running and asks
 * it for every visit to a live page (`crates/simulator/src/pages/render.rs`).
 *
 * It is never the script process. That one runs players' code; this one runs
 * ours on pages already checked, and what it writes is served as cartopolis.org
 * — a script that got out of its realm must not be able to write a page.
 *
 * stdout is the answer channel, so everything else that would print there —
 * Mithril's server logs on import, a component's `console.log` — goes to stderr.
 */

type Renderer = {renderPage: (data: unknown) => Promise<string>}

export async function serveRenders(bundle: string): Promise<void> {
    const toStderr = (...args: unknown[]) => console.error(...args)
    console.log = toStderr
    console.info = toStderr
    console.debug = toStderr

    const {renderPage} = (await import(bundle)) as Renderer
    if (typeof renderPage !== 'function') throw new Error(`${bundle} has no renderPage`)
    const write = (value: unknown) => process.stdout.write(JSON.stringify(value) + '\n')

    for await (const line of console) {
        if (!line) continue
        let id: unknown = null
        try {
            const req = JSON.parse(line) as {id: number; page: unknown}
            id = req.id
            write({html: await renderPage(req.page), id})
        } catch (e) {
            write({error: String(e instanceof Error ? (e.stack ?? e.message) : e), id})
        }
    }
}
