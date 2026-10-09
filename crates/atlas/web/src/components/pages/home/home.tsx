import m, {MithrilComponent} from '@garage44/mithril'
import {Button} from '@garage44/mithril/components'

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
            <section class="home">
                <h1 class="home__title">
                    carto<b>polis</b>
                </h1>
                <p class="home__tagline">A shared 3D world on the real map.</p>
                <div class="home__actions">
                    <Button href="/viewer/" size="lg" variant="primary">
                        Open the map
                    </Button>
                    <Button href="/apk/" size="lg">
                        Android app
                    </Button>
                    <Button
                        href={ADMIN}
                        onclick={(e: MouseEvent) => {
                            e.preventDefault()
                            m.route.set(ADMIN)
                        }}
                        size="lg"
                        variant="ghost"
                    >
                        Sign in
                    </Button>
                </div>
            </section>
        )
    }
}
