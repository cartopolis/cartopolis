/**
 * Where each page of the web UI lives on cartopolis.org.
 *
 * The web UI *is* the site: Caddy serves this build for every path it does not
 * route elsewhere — the viewer at `/viewer/`, the APK at `/apk/`, the simulator's
 * `/api` and `/ws`, atlas's `/terrain/`, `/height` and `/api/pipeline/`. So
 * the front page is `/`, the operator's pages are `/admin`, and the players'
 * pages — `/register`, `/verify`, `/house` — sit beside them, where the simulator
 * builds its verification link as `{CARTO_PUBLIC_BASE_URL}/verify?token=…`.
 *
 * The operator's pages were `/console/` until 2026-09, and a separate hostname
 * (web.cartopolis.org) before that; Caddy redirects the old path here.
 */

/** The operator's pages. */
export const ADMIN = '/admin'

/** The player routes: no operator chrome, no sign-out button. */
export const PLAYER_PATHS = ['/register', '/verify', '/house']
