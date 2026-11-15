/**
 * Where each page of the web UI lives on cartopolis.org.
 *
 * The web UI *is* the site: Caddy serves this build for every path it does not
 * route elsewhere — the viewer at `/viewer/`, the APK at `/apk/`, the simulator's
 * `/api` and `/ws`, atlas's `/terrain/`, `/height` and `/api/pipeline/`. So
 * Community is `/chat` (`/` the content page `home`), the operator's pages are `/admin`, and the players'
 * pages — `/register`, `/verify`, `/buildings`, `/building/{pand}` — sit beside them, where the simulator
 * builds its verification link as `{CARTO_PUBLIC_BASE_URL}/verify?token=…`.
 *
 * The operator's pages were `/console/` until 2026-09, and a separate hostname
 * (web.cartopolis.org) before that; Caddy redirects the old path here.
 */

/** The operator's pages. */
export const ADMIN = '/admin'

/**
 * Community, the chat: the switch's first position. It was `/` until
 * 2026-11-15, when the site's front door became the content page `home`
 * (`cms/tree.ts`); `/` still draws it on a server with no home page published,
 * so the route stays, but every way into Community inside the app goes here.
 */
export const COMMUNITY = '/chat'
