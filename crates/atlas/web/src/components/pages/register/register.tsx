import type {RoutableProps} from 'preact-router'

import {route} from 'preact-router'
import {useState} from 'preact/hooks'

import {$s} from '@/app'
import {ADMIN} from '@/lib/base'
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
export const Register = (_props: RoutableProps) => {
    const rules = $s.registration
    const [username, setUsername] = useState('')
    const [email, setEmail] = useState('')
    const [password, setPassword] = useState('')
    const [busy, setBusy] = useState(false)
    const [sent, setSent] = useState(false)

    if (!rules) {
        return (
            <div class="card signin">
                <h1>Sign up</h1>
                <p class="hint">
                    This server did not answer when we asked whether it takes registrations. It may be
                    down, or older than this page.
                </p>
            </div>
        )
    }

    if (!rules.open) {
        return (
            <div class="card signin">
                <h1>Sign up</h1>
                <p class="hint">
                    This server does not take registrations — accounts on it are made by its operator.
                </p>
                <button class="ghost" onClick={() => route(ADMIN)} type="button">
                    Back to sign in
                </button>
            </div>
        )
    }

    if (sent) {
        return (
            <div class="card signin">
                <h1>Check your inbox</h1>
                <p class="hint">
                    We've sent a link to <b>{email.trim()}</b>. Open it to finish signing up — it works
                    once and expires in {rules.verification_valid_hours} hours.
                </p>
                <p class="hint">
                    Nothing arrived? It may be in a spam folder.{' '}
                    <a href={`/verify?resend=${encodeURIComponent(email.trim())}`}>Send another</a>.
                </p>
            </div>
        )
    }

    const missing = whatIsMissing(
        username,
        email,
        password,
        rules.min_password_length,
        rules.max_username_length,
    )

    const submit = async (e: Event) => {
        e.preventDefault()
        if (missing || busy) {
            return
        }
        setBusy(true)
        const result = await register(username.trim(), email.trim(), password)
        setBusy(false)
        if (result.ok) {
            // Out of the component's state as well as off the screen. It is the
            // one field here worth not leaving in a form somebody may walk away
            // from.
            setPassword('')
            setSent(true)
        }
    }

    return (
        // A real <form>, so the phone keyboard shows a Go key and Enter submits.
        <form class="card signin" onSubmit={submit}>
            <h1>Sign up</h1>
            <p class="hint">
                One account for the world and for this page. We'll send a link to confirm your address
                before it works.
            </p>
            <label class="field">
                <span>Username</span>
                <input
                    autocomplete="username"
                    disabled={busy}
                    onInput={(e) => setUsername((e.target as HTMLInputElement).value)}
                    type="text"
                    value={username}
                />
            </label>
            <label class="field">
                <span>Email</span>
                <input
                    autocomplete="email"
                    disabled={busy}
                    inputMode="email"
                    onInput={(e) => setEmail((e.target as HTMLInputElement).value)}
                    type="email"
                    value={email}
                />
            </label>
            <label class="field">
                <span>Password</span>
                <input
                    autocomplete="new-password"
                    disabled={busy}
                    onInput={(e) => setPassword((e.target as HTMLInputElement).value)}
                    placeholder={`At least ${rules.min_password_length} characters`}
                    type="password"
                    value={password}
                />
            </label>
            {/*
              * Shown only once something has been typed. On an untouched form
              * "Pick a username" is not advice, it is the form telling somebody
              * off for not having filled it in yet.
              */}
            {missing && (username || email || password) ? (
                <p class="hint form-problem">{missing}</p>
            ) : null}
            <button class="primary" disabled={!!missing || busy} type="submit">
                {busy ? 'Sending…' : 'Sign up'}
            </button>
            <button class="ghost" disabled={busy} onClick={() => route(ADMIN)} type="button">
                I already have an account
            </button>
        </form>
    )
}
