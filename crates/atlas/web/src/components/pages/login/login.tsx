import m, {MithrilComponent} from '@garage44/mithril'

import {$s} from '@/app'
import {signIn} from '@/lib/accounts'
import {local} from '@/lib/reactive'

export class Login extends MithrilComponent {
    s = local({busy: false, password: '', username: ''})

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !s.username || !s.password) {
            return
        }
        s.busy = true
        const ok = await signIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
        }
    }

    view() {
        const s = this.s
        return (
            // A real <form>, so the phone keyboard shows a Go key and Enter submits.
            <form class="card signin" onsubmit={this.submit}>
                <h1>Sign in</h1>
                <p class="hint">
                    Use a Cartopolis account with the admin flag. Accounts without it cannot sign in here, and the
                    answer is the same either way.
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
                {/*
                  * Only when the server says it takes them. A sign-up link on a
                  * private world is a button that leads to a refusal.
                  */}
                {$s.registration?.open && (
                    <button class="ghost" disabled={s.busy} onclick={() => m.route.set('/register')} type="button">
                        Create an account
                    </button>
                )}
                {/*
                  * The player's one errand on this origin. Gated the same way: a
                  * server that does not hand out buildings should not offer the page.
                  */}
                {$s.registration?.houses_open && (
                    <button class="ghost" disabled={s.busy} onclick={() => m.route.set('/buildings')} type="button">
                        Your buildings
                    </button>
                )}
            </form>
        )
    }
}
