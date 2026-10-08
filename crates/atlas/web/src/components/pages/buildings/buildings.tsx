import m, {MithrilComponent} from '@garage44/mithril'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
import {local} from '@/lib/reactive'
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
        if (s.busy || !s.username || !s.password) {
            return
        }
        s.busy = true
        const ok = await playerSignIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
        }
    }

    view() {
        const s = this.s
        const signedIn = Boolean($s.signup.token)

        if (signedIn) {
            return (
                <>
                    <BuildingPicker />
                    <div class="card">
                        <p class="hint">
                            You're signed in as <b>{$s.signup.username}</b>.
                        </p>
                        <div class="row-actions">
                            <button class="ghost" onclick={() => void playerSignOut()} type="button">
                                Sign out
                            </button>
                        </div>
                    </div>
                </>
            )
        }

        return (
            <form class="card signin" onsubmit={this.submit}>
                <h1>Your buildings</h1>
                <p class="hint">
                    Sign in with your Cartopolis account — the one you play with. No admin flag
                    needed here.
                </p>
                <label class="field">
                    <span>Username</span>
                    <input
                        autocomplete="username"
                        disabled={s.busy}
                        oninput={(e: Event) => {
                            s.username = (e.target as HTMLInputElement).value
                        }}
                        type="text"
                        value={s.username}
                    />
                </label>
                <label class="field">
                    <span>Password</span>
                    <input
                        autocomplete="current-password"
                        disabled={s.busy}
                        oninput={(e: Event) => {
                            s.password = (e.target as HTMLInputElement).value
                        }}
                        type="password"
                        value={s.password}
                    />
                </label>
                <button class="primary" disabled={s.busy || !s.username || !s.password} type="submit">
                    {s.busy ? 'Signing in…' : 'Sign in'}
                </button>
            </form>
        )
    }
}
