import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
import {bind, local} from '@/lib/reactive'
import {loadMyBuildings, playerSignIn, playerSignOut} from '@/lib/signup'

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
    s = local({busy: false, password: '', username: ''})

    // Both fields were already required in code; the rules only move here.
    v = validations({
        password: validation(bind(this.s, 'password'), [required]),
        username: validation(bind(this.s, 'username'), [required]),
    })

    oninit() {
        // A remembered session: ask the server which buildings it owns. A dead
        // token answers 401, `signupApi` clears it, and the form below appears.
        if ($s.signup.token) {
            void loadMyBuildings()
        }
    }

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !this.v.$validate()) {
            return
        }
        s.busy = true
        const ok = await playerSignIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
            this.v.password.reset()
        }
    }

    view() {
        const s = this.s
        const signedIn = Boolean($s.signup.token)

        if (signedIn) {
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
                            You're signed in as <b>{$s.signup.username}</b>.
                        </p>
                    </Card>
                </div>
            )
        }

        return (
            // A real <form>, so the phone keyboard shows a Go key and Enter submits.
            <form class="buildings-signin" onsubmit={this.submit}>
                <Card title="Your buildings">
                    <p class="buildings-page__hint">
                        Sign in with your Cartopolis account — the one you play with. No admin flag
                        needed here.
                    </p>
                    <FieldText
                        autocomplete="username"
                        disabled={s.busy}
                        label="Username"
                        model={bind(s, 'username')}
                        validation={this.v.username}
                    />
                    <FieldText
                        autocomplete="current-password"
                        disabled={s.busy}
                        label="Password"
                        model={bind(s, 'password')}
                        type="password"
                        validation={this.v.password}
                    />
                    <Button disabled={!this.v.$valid} loading={s.busy} type="submit" variant="primary">
                        Sign in
                    </Button>
                </Card>
            </form>
        )
    }
}
