import type {RoutableProps} from 'preact-router'

import {useEffect, useState} from 'preact/hooks'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
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
export const Buildings = (_props: RoutableProps) => {
    const [username, setUsername] = useState('')
    const [password, setPassword] = useState('')
    const [busy, setBusy] = useState(false)

    const signedIn = Boolean($s.signup.token)

    useEffect(() => {
        // A remembered session: ask the server which buildings it owns. A dead
        // token answers 401, `signupApi` clears it, and the form below appears.
        if ($s.signup.token) {
            void loadMyBuildings()
        }
    }, [])

    if (signedIn) {
        return (
            <>
                <BuildingPicker />
                <div class="card">
                    <p class="hint">
                        You're signed in as <b>{$s.signup.username}</b>.
                    </p>
                    <div class="row-actions">
                        <button class="ghost" onClick={() => void playerSignOut()} type="button">
                            Sign out
                        </button>
                    </div>
                </div>
            </>
        )
    }

    const submit = async (e: Event) => {
        e.preventDefault()
        if (busy || !username || !password) {
            return
        }
        setBusy(true)
        const ok = await playerSignIn(username, password)
        setBusy(false)
        if (ok) {
            setPassword('')
        }
    }

    return (
        <form class="card signin" onSubmit={submit}>
            <h1>Your buildings</h1>
            <p class="hint">
                Sign in with your Cartopolis account — the one you play with. No admin flag
                needed here.
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
        </form>
    )
}
