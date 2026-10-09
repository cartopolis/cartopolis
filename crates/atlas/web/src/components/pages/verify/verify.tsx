import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button, Card, FieldText, Spinner} from '@garage44/mithril/components'
import {validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
import {ADMIN} from '@/lib/base'
import {bind, local} from '@/lib/reactive'
import {resend, tokenFromLocation, verify} from '@/lib/signup'

type Phase = 'confirmed' | 'failed' | 'working'

/**
 * Where the link in the mail lands.
 *
 * It runs the confirmation exactly once, on mount, and it has to: `oninit` is
 * what keeps a redraw — a toast arriving, a signal changing — from spending the
 * token a second time and turning a successful sign-up into "that link is not
 * valid".
 */
export class Verify extends MithrilComponent {
    s = local({again: '', phase: 'working' as Phase, sending: false})

    // The check this form always had, and no more: the resend endpoint answers
    // success whatever the address, so a stricter test here would only refuse
    // addresses it would have mailed.
    v = validations({
        again: validation(bind(this.s, 'again'))
            .required()
            .check((value) => value.includes('@'), 'That does not look like an email address.'),
    })

    oninit() {
        const s = this.s
        const search = window.location.search
        const token = tokenFromLocation(search)
        // `/verify?resend=…` is the other way in: the "send another" link on the
        // check-your-inbox panel, which has an address and no token.
        const prefill = new URLSearchParams(search).get('resend')
        if (prefill) {
            s.again = prefill
        }
        if (!token) {
            s.phase = 'failed'
            return
        }
        void verify(token).then((ok) => {
            s.phase = ok ? 'confirmed' : 'failed'
        })
    }

    send = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.sending || !this.v.$validate()) {
            return
        }
        s.sending = true
        await resend(s.again.trim())
        s.sending = false
    }

    view() {
        const s = this.s

        if (s.phase === 'working') {
            return (
                <div class="auth">
                    <Card>
                        <div class="auth__working">
                            <Spinner />
                            Confirming…
                        </div>
                    </Card>
                </div>
            )
        }

        if (s.phase === 'confirmed') {
            return (
                <>
                    <div class="auth">
                        <Card title="Welcome">
                            <Alert title={`You're in, ${$s.signup.username}`} tone="success">
                                Your account is confirmed.
                            </Alert>
                            <p class="auth__lede">
                                Sign in to the world at <a href="https://cartopolis.org">cartopolis.org</a> with the
                                username and password you just chose — this page is the account admin, and it only
                                lets operators in.
                            </p>
                        </Card>
                    </div>
                    <BuildingPicker />
                </>
            )
        }

        return (
            // A form, so Enter or the phone's Go key sends the link.
            <form class="auth" novalidate onsubmit={this.send}>
                <Card title="Confirm your address">
                    <Alert title="That link didn't work" tone="danger">
                        It may have been used already, replaced by a newer one, or simply expired. Ask for another
                        and we'll send it.
                    </Alert>
                    <FieldText
                        autocomplete="email"
                        label="Email"
                        model={bind(s, 'again')}
                        type="email"
                        validation={this.v.again}
                    />
                    <div class="auth__actions">
                        <Button disabled={!this.v.$valid} loading={s.sending} type="submit" variant="primary">
                            Send another link
                        </Button>
                        <Button onclick={() => m.route.set(ADMIN)} variant="ghost">
                            Back to sign in
                        </Button>
                    </div>
                </Card>
            </form>
        )
    }
}
