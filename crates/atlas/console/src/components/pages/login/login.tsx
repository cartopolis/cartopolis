import {route} from 'preact-router'
import {useState} from 'preact/hooks'

import {$s} from '@/app'
import {at} from '@/lib/base'
import {signIn} from '@/lib/accounts'

export const Login = () => {
    const [username, setUsername] = useState('')
    const [password, setPassword] = useState('')
    const [busy, setBusy] = useState(false)

    const submit = async (e: Event) => {
        e.preventDefault()
        if (busy || !username || !password) {
            return
        }
        setBusy(true)
        const ok = await signIn(username, password)
        setBusy(false)
        if (ok) {
            setPassword('')
        }
    }

    return (
        // A real <form>, so the phone keyboard shows a Go key and Enter submits.
        <form class="card signin" onSubmit={submit}>
            <h1>Sign in</h1>
            <p class="hint">
                Use a Cartopolis account with the admin flag. Accounts without it cannot sign in here, and the
                answer is the same either way.
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
                <span>Password</span>
                <input
                    autocomplete="current-password"
                    disabled={busy}
                    onInput={(e) => setPassword((e.target as HTMLInputElement).value)}
                    type="password"
                    value={password}
                />
            </label>
            <button class="primary" disabled={busy || !username || !password} type="submit">
                {busy ? 'Signing in…' : 'Sign in'}
            </button>
            {/*
              * Only when the server says it takes them. A sign-up link on a
              * private world is a button that leads to a refusal.
              */}
            {$s.registration?.open && (
                <button class="ghost" disabled={busy} onClick={() => route(at('/register'))} type="button">
                    Create an account
                </button>
            )}
            {/*
              * The player's one errand on this origin. Gated the same way: a
              * server that does not hand out houses should not offer the page.
              */}
            {$s.registration?.houses_open && (
                <button class="ghost" disabled={busy} onClick={() => route(at('/house'))} type="button">
                    Claim your house
                </button>
            )}
        </form>
    )
}
