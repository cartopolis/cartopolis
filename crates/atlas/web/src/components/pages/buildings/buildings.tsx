import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card} from '@garage44/mithril/components'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
import {PlayerSignIn} from '@/components/elements/signin/signin'
import {loadMyBuildings, playerSignOut} from '@/lib/signup'

/**
 * Your buildings, for the account that already exists.
 *
 * The verify screen mounts `BuildingPicker` for the person who signed up a minute
 * ago; this page is the way back for everyone else — registered players never
 * held a browser session before `/api/login` existed, because the admin login
 * refuses them by design. The session lands in `$s.signup`, same slot the
 * verification link fills, so the picker itself needs no second variant.
 */
export class Buildings extends MithrilComponent {
    oninit() {
        // A remembered session: ask the server which buildings it owns. A dead
        // token answers 401, `signupApi` clears it, and the form below appears.
        if ($s.signup.token) {
            void loadMyBuildings()
        }
    }

    view() {
        if (!$s.signup.token) {
            return (
                <PlayerSignIn
                    hint="Sign in with your Cartopolis account — the one you play with. No admin flag needed here."
                    title="Your buildings"
                />
            )
        }
        return (
            <div class="buildings-page">
                <BuildingPicker />
                <Card
                    footer={
                        <Button onclick={() => void playerSignOut()} size="sm" variant="ghost">
                            Sign out
                        </Button>
                    }
                >
                    <p class="buildings-page__hint">
                        You're signed in as <b>{$s.signup.username}</b>. Bots and scripts sign in
                        with an <a href="/tokens">access token</a> instead of your password.
                    </p>
                </Card>
            </div>
        )
    }
}
