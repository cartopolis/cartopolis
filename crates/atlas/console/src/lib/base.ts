/**
 * Where the console is mounted: `https://cartopolis.org/console/`, beside the
 * viewer at `/viewer/` and atlas's front page at `/`. It had a hostname of its
 * own (web.cartopolis.org) until 2026-09, retired without a redirect.
 *
 * Every route and every in-app link goes through `at`, so moving it again is
 * this one constant plus the Caddy block, `build.ts`'s dev server and the
 * simulator's `CARTO_PUBLIC_BASE_URL`, which verification links are built on.
 */
export const BASE = '/console'

/** An app path (`/register`) as a URL path (`/console/register`). */
export function at(path: string): string {
    return path === '/' ? `${BASE}/` : `${BASE}${path}`
}
