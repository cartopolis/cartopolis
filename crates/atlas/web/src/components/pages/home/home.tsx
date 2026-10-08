import m, {MithrilComponent} from '@garage44/mithril'

import {ADMIN} from '@/lib/base'

/**
 * cartopolis.org itself. It was a page atlas served from a string until
 * 2026-09, beside this app rather than in it, which made the site two things
 * with one header between them.
 *
 * The map and the APK are plain links, not routes: `/viewer/` and `/apk/` are
 * other builds Caddy serves, and the router would otherwise swallow the click.
 */
export class Home extends MithrilComponent {
    view() {
        return (
            <div class="home">
                <h1>Cartopolis</h1>
                <p class="hint">A shared 3D world on the real map.</p>
                <div class="home-actions">
                    <a class="button primary" href="/viewer/">
                        Open the map
                    </a>
                    <a class="button" href="/apk/">
                        Android app
                    </a>
                    <a
                        class="button"
                        href={ADMIN}
                        onclick={(e: Event) => {
                            e.preventDefault()
                            m.route.set(ADMIN)
                        }}
                    >
                        Sign in
                    </a>
                </div>
            </div>
        )
    }
}
