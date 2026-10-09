import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {signIn} from '@/lib/accounts'
import {bind, local} from '@/lib/reactive'

export class Login extends MithrilComponent {
    s = local({busy: false, password: '', username: ''})

    // Both fields are all the client can know about: whether the account exists
    // and has the admin flag is the server's answer, and it gives the same one
    // either way.
    v = validations({
        password: validation(bind(this.s, 'password'), [required]),
        username: validation(bind(this.s, 'username'), [required]),
    })

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !this.v.$validate()) {
            return
        }
        s.busy = true
        const ok = await signIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
            this.v.$reset()
        }
    }

    view() {
        const s = this.s
        return (
            // A real <form>, so the phone keyboard shows a Go key and Enter submits.
            <form class="auth" novalidate onsubmit={this.submit}>
                <Card title="Sign in">
                    <p class="auth__lede">
                        Use a Cartopolis account with the admin flag. Accounts without it cannot sign in here, and
                        the answer is the same either way.
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
                    <div class="auth__actions">
                        <Button disabled={!this.v.$valid} loading={s.busy} type="submit" variant="primary">
                            Sign in
                        </Button>
                        {/*
                          * Only when the server says it takes them. A sign-up link on a
                          * private world is a button that leads to a refusal.
                          */}
                        {$s.registration?.open && (
                            <Button disabled={s.busy} onclick={() => m.route.set('/register')} variant="ghost">
                                Create an account
                            </Button>
                        )}
                        {/*
                          * The player's one errand on this origin. Gated the same way: a
                          * server that does not hand out buildings should not offer the page.
                          */}
                        {$s.registration?.houses_open && (
                            <Button disabled={s.busy} onclick={() => m.route.set('/buildings')} variant="ghost">
                                Your buildings
                            </Button>
                        )}
                    </div>
                </Card>
            </form>
        )
    }
}
