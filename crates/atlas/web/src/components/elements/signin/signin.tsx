import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import {bind, local} from '@/lib/reactive'
import {playerSignIn} from '@/lib/signup'

interface SignInAttrs {
    /** What signing in leads to, under the title. */
    hint: string
    title: string
}

/**
 * A player's sign-in card: the person's own session (`$s.signup`), for the pages
 * that are the account's rather than the operator's — `/buildings` and `/tokens`.
 * Lifted out of the buildings page when the tokens page needed the same form.
 */
export class PlayerSignIn extends MithrilComponent<SignInAttrs> {
    s = local({busy: false, password: '', username: ''})

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
        const ok = await playerSignIn(s.username, s.password)
        s.busy = false
        if (ok) {
            s.password = ''
            this.v.password.reset()
        }
    }

    view(vnode: m.Vnode<SignInAttrs>) {
        const s = this.s
        return (
            // A real <form>, so the phone keyboard shows a Go key and Enter submits.
            <form class="buildings-signin" onsubmit={this.submit}>
                <Card title={vnode.attrs.title}>
                    <p class="buildings-page__hint">{vnode.attrs.hint}</p>
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
