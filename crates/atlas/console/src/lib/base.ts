/**
 * Where each half of the app is mounted on cartopolis.org.
 *
 * The operator's pages live under `/console/`, beside the viewer at `/viewer/`
 * and atlas's front page at `/`. The players' pages — `/register`, `/verify`,
 * `/house` — are at the site root, because they are not the console: a player
 * following a verification email has never heard of it, and the simulator builds
 * that link as `{CARTO_PUBLIC_BASE_URL}/verify?token=…` with the base being just
 * the site. Caddy serves this one build for both.
 *
 * The console had a hostname of its own (web.cartopolis.org) until 2026-09,
 * retired without a redirect.
 */
export const BASE = '/console'

/** The player routes, at the site root. */
export const PLAYER_PATHS = ['/register', '/verify', '/house']

/** A console path (`/`) as a URL path (`/console/`). */
export function at(path: string): string {
    return path === '/' ? `${BASE}/` : `${BASE}${path}`
}
