import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Badge, Button, Card, FieldCheckbox, FieldSelect, FieldText} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import type {AccessToken, AccountSignin} from '@/types'

import {$s} from '@/app'
import {ProviderLogo} from '@/components/elements/providers/providers'
import {PlayerSignIn} from '@/components/elements/signin/signin'
import {loadAccountSignin} from '@/lib/auth'
import {bind, local} from '@/lib/reactive'
import {confirmLabel, reauthValid, reauthWith} from '@/lib/signin'
import {playerSignOut} from '@/lib/signup'
import {createToken, EXPIRY_CHOICES, expiryAfter, formatUnix, listTokens, revokeToken} from '@/lib/tokens'

import {confirmWith, Ways} from './ways'

/**
 * Access tokens: what a bot or a script signs in with instead of a password
 * (`docs/notes/access-tokens.md`).
 *
 * The flow it exists for is a handful of steps a person does by hand — sign in as
 * the bot's account here, make a token, paste it on the machine the bot runs on
 * (`midas bot setup`, or `CARTO_TOKEN`) — and revoking is the one click that
 * stops it, including a bot that is already in the world. So the page is three
 * things and no more: the list, the form, and the secret shown once.
 */
export class Tokens extends MithrilComponent {
    s = local({
        busy: false,
        elevated: false,
        expiry: 'never',
        label: '',
        loaded: false,
        password: '',
        /** The secret of the token just made; shown until dismissed, never again. */
        shown: '',
        /** The account's ways in; `null` until asked (and on a server too old to say). */
        signin: null as AccountSignin | null,
        tokens: [] as AccessToken[],
    })

    v = validations({
        label: validation(bind(this.s, 'label'), [required]),
        // Required only of an account that has a password; one without confirms
        // through its provider instead.
        password: validation(bind(this.s, 'password')).rule((value) =>
            this.needsPassword() && !value ? 'Enter your password.' : null,
        ),
    })

    load = async () => {
        const [tokens, signin] = await Promise.all([listTokens(), loadAccountSignin()])
        if (tokens) {
            this.s.tokens = tokens
            this.s.loaded = true
        }
        if (signin) this.s.signin = signin
    }

    loadSignin = async () => {
        const signin = await loadAccountSignin()
        if (signin) this.s.signin = signin
    }

    /** Whether the form asks for the password: unless the server said there is none. */
    needsPassword(): boolean {
        return this.s.signin?.has_password ?? true
    }

    oninit() {
        if ($s.signup.token) {
            void this.load()
        }
    }

    create = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !this.v.$validate()) {
            return
        }
        s.busy = true
        const made = await createToken({
            elevated: s.elevated,
            expires_at: expiryAfter(s.expiry),
            label: s.label.trim(),
            password: this.needsPassword() ? s.password : undefined,
        })
        s.busy = false
        // The password goes whatever happened: it was asked for one action.
        s.password = ''
        this.v.password.reset()
        if (made === 'reauth') {
            // The confirmation lapsed while the form was open: ask again.
            await this.loadSignin()
            return
        }
        if (made) {
            s.shown = made.token
            s.label = ''
            s.elevated = false
            s.expiry = 'never'
            this.v.label.reset()
            await this.load()
        }
    }

    revoke = async (token: AccessToken) => {
        this.s.busy = true
        if (await revokeToken(token.id)) {
            await this.load()
        }
        this.s.busy = false
    }

    /** Whether a token may be made now without a password: a recent confirmation. */
    confirmed(): boolean {
        return this.needsPassword() || reauthValid(this.s.signin?.reauth_until)
    }

    /**
     * In place of the password, for an account without one: confirm through
     * the provider, which comes back here with ten minutes to make the token.
     */
    confirmView() {
        const signin = this.s.signin
        const first = signin ? reauthWith(signin.identities) : null
        if (signin && reauthValid(signin.reauth_until)) {
            const until = new Date(signin.reauth_until! * 1000).toLocaleTimeString(undefined, {
                hour: '2-digit',
                minute: '2-digit',
            })
            return <Alert tone="success">Confirmed. You can make a token until {until}.</Alert>
        }
        if (!first) return null
        return (
            <div class="tokens-page__confirm">
                <p class="tokens-page__hint">Confirm it's you before a new token is made.</p>
                <Button icon={ProviderLogo({id: first.provider, kind: first.kind})} onclick={() => confirmWith(first)}>
                    {confirmLabel(first.provider_name)}
                </Button>
            </div>
        )
    }

    view() {
        const s = this.s
        if (!$s.signup.token) {
            return (
                <PlayerSignIn
                    hint="Sign in as the account the token is for — a bot's own account, usually."
                    title="Access tokens"
                />
            )
        }
        // Signed in on this page after it mounted: `oninit` had no session to load with.
        if (!s.loaded && !s.busy) {
            s.busy = true
            void this.load().finally(() => (s.busy = false))
        }

        return (
            <div class="tokens-page">
                {s.shown && (
                    <Card
                        footer={
                            <Button onclick={() => (s.shown = '')} size="sm" variant="primary">
                                I have copied it
                            </Button>
                        }
                        title="Your new token"
                    >
                        <p class="tokens-page__hint">
                            Copy it now — it is not stored anywhere it can be shown again. Give it to the
                            bot as <code>CARTO_TOKEN</code>, or paste it into <code>midas bot setup</code>.
                        </p>
                        <code class="tokens-page__secret">{s.shown}</code>
                        <Button onclick={() => void navigator.clipboard?.writeText(s.shown)} size="sm" variant="ghost">
                            Copy
                        </Button>
                    </Card>
                )}

                {s.signin && <Ways reload={this.loadSignin} signin={s.signin} />}

                <Card title={`Access tokens for ${$s.signup.username}`}>
                    {s.tokens.length === 0 ? (
                        <p class="tokens-page__hint">{s.loaded ? 'No tokens yet.' : 'Loading…'}</p>
                    ) : (
                        <ul class="tokens-page__list">
                            {s.tokens.map((t) => (
                                <li class="tokens-page__row" key={t.id}>
                                    <div>
                                        <b>{t.label}</b> {t.elevated && <Badge tone="accent">elevated</Badge>}
                                        <div class="tokens-page__meta">
                                            made {formatUnix(t.created_at, '—')} · last used{' '}
                                            {formatUnix(t.last_used, 'never')} · expires {formatUnix(t.expires_at, 'never')}
                                        </div>
                                    </div>
                                    <Button disabled={s.busy} onclick={() => void this.revoke(t)} size="sm" variant="danger">
                                        Revoke
                                    </Button>
                                </li>
                            ))}
                        </ul>
                    )}
                </Card>

                <form onsubmit={this.create}>
                    <Card title="New token">
                        <FieldText
                            disabled={s.busy}
                            label="Name"
                            model={bind(s, 'label')}
                            placeholder="deck-2 bot"
                            validation={this.v.label}
                        />
                        <FieldSelect disabled={s.busy} label="Expires" model={bind(s, 'expiry')} options={EXPIRY_CHOICES} />
                        <FieldCheckbox
                            disabled={s.busy}
                            label="Elevated: may run the server and edit other people's things, if this account may"
                            model={bind(s, 'elevated')}
                        />
                        <p class="tokens-page__hint">
                            Without it the token acts as a player would, whatever this account's roles say — what
                            a bot needs, and all a stolen token could do.
                        </p>
                        {this.needsPassword() ? (
                            <FieldText
                                autocomplete="current-password"
                                disabled={s.busy}
                                label="Your password, to confirm"
                                model={bind(s, 'password')}
                                type="password"
                                validation={this.v.password}
                            />
                        ) : (
                            this.confirmView()
                        )}
                        <Button disabled={!this.v.$valid || !this.confirmed()} loading={s.busy} type="submit" variant="primary">
                            Create token
                        </Button>
                    </Card>
                </form>

                <Card
                    footer={
                        <Button onclick={() => void playerSignOut()} size="sm" variant="ghost">
                            Sign out
                        </Button>
                    }
                >
                    <p class="tokens-page__hint">
                        Revoking a token disconnects anything signed in with it within seconds.
                    </p>
                </Card>
            </div>
        )
    }
}
