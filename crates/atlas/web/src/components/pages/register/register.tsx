import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button, Card, FieldText} from '@garage44/mithril/components'
import {validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {ADMIN} from '@/lib/base'
import {bind, local} from '@/lib/reactive'
import {register, whatIsMissing} from '@/lib/signup'

/**
 * Make an account.
 *
 * Everything about this screen is arranged around one fact: the account is not
 * usable when the form is submitted. So there is no "you're in" and no redirect
 * to a signed-in page — it swaps itself for the "check your inbox" panel, which
 * is the actual state of the world and the only place the re-send button makes
 * sense.
 */
export class Register extends MithrilComponent {
    s = local({busy: false, email: '', password: '', sent: false, username: ''})

    /*
     * The per-field half of `whatIsMissing`, with its sentences, so each problem
     * shows under the field it is about. `whatIsMissing` stays the submit gate
     * (it is the tested copy of the server's rules); these only decide where the
     * sentence goes. The limits are read from `$s.registration` when a rule runs,
     * not when it is built, because the rules arrive after this component may
     * have been constructed.
     */
    v = validations({
        email: validation(bind(this.s, 'email'))
            .required('That does not look like an email address.')
            .check((value) => {
                // Deliberately shallow — see `whatIsMissing`.
                const address = value.trim()
                const at = address.lastIndexOf('@')
                return at >= 1 && address.slice(at + 1).includes('.')
            }, 'That does not look like an email address.'),
        password: validation(bind(this.s, 'password'))
            .required()
            .rule((value) => {
                const min = $s.registration?.min_password_length ?? 0
                return value.length >= min ? null : `The password needs at least ${min} characters.`
            }),
        username: validation(bind(this.s, 'username'))
            .required('Pick a username.')
            .rule((value) => {
                const max = $s.registration?.max_username_length ?? Infinity
                return value.trim().length <= max ? null : `That username is longer than ${max} characters.`
            }),
    })

    view() {
        const s = this.s
        const rules = $s.registration

        if (!rules) {
            return (
                <div class="auth">
                    <Card title="Sign up">
                        <Alert tone="warning">
                            This server did not answer when we asked whether it takes registrations. It may be
                            down, or older than this page.
                        </Alert>
                    </Card>
                </div>
            )
        }

        if (!rules.open) {
            return (
                <div class="auth">
                    <Card title="Sign up">
                        <Alert tone="info">
                            This server does not take registrations — accounts on it are made by its operator.
                        </Alert>
                        <div class="auth__actions">
                            <Button onclick={() => m.route.set(ADMIN)} variant="ghost">
                                Back to sign in
                            </Button>
                        </div>
                    </Card>
                </div>
            )
        }

        if (s.sent) {
            return (
                <div class="auth">
                    <Card title="Check your inbox">
                        <Alert tone="success">
                            We've sent a link to <b>{s.email.trim()}</b>. Open it to finish signing up — it
                            works once and expires in {rules.verification_valid_hours} hours.
                        </Alert>
                        <p class="auth__lede">
                            Nothing arrived? It may be in a spam folder.{' '}
                            <a href={`/verify?resend=${encodeURIComponent(s.email.trim())}`}>Send another</a>.
                        </p>
                    </Card>
                </div>
            )
        }

        const missing = whatIsMissing(
            s.username,
            s.email,
            s.password,
            rules.min_password_length,
            rules.max_username_length,
        )

        const submit = async (e: Event) => {
            e.preventDefault()
            if (!this.v.$validate() || missing || s.busy) {
                return
            }
            s.busy = true
            const result = await register(s.username.trim(), s.email.trim(), s.password)
            s.busy = false
            if (result.ok) {
                // Out of the component's state as well as off the screen. It is the
                // one field here worth not leaving in a form somebody may walk away
                // from.
                s.password = ''
                s.sent = true
            }
        }

        return (
            // A real <form>, so the phone keyboard shows a Go key and Enter submits.
            <form class="auth" novalidate onsubmit={submit}>
                <Card title="Sign up">
                    <p class="auth__lede">
                        One account for the world and for this page. We'll send a link to confirm your address
                        before it works.
                    </p>
                    {/*
                      * Errors wait until a field is left or the form is submitted
                      * (the library's rule, and the one this form had): on an
                      * untouched form "Pick a username" is not advice, it is the form
                      * telling somebody off for not having filled it in yet.
                      */}
                    <FieldText
                        autocomplete="username"
                        disabled={s.busy}
                        help={`Up to ${rules.max_username_length} characters.`}
                        label="Username"
                        model={bind(s, 'username')}
                        validation={this.v.username}
                    />
                    <FieldText
                        autocomplete="email"
                        disabled={s.busy}
                        label="Email"
                        model={bind(s, 'email')}
                        type="email"
                        validation={this.v.email}
                    />
                    <FieldText
                        autocomplete="new-password"
                        disabled={s.busy}
                        help={`At least ${rules.min_password_length} characters.`}
                        label="Password"
                        model={bind(s, 'password')}
                        type="password"
                        validation={this.v.password}
                    />
                    <div class="auth__actions">
                        <Button disabled={!!missing || !this.v.$valid} loading={s.busy} type="submit" variant="primary">
                            Sign up
                        </Button>
                        <Button disabled={s.busy} onclick={() => m.route.set(ADMIN)} variant="ghost">
                            I already have an account
                        </Button>
                    </div>
                </Card>
            </form>
        )
    }
}
