import type {RoutableProps} from 'preact-router'

import {route} from 'preact-router'
import {useEffect, useState} from 'preact/hooks'

import {$s} from '@/app'
import {HousePicker} from '@/components/elements/house/house'
import {ADMIN} from '@/lib/base'
import {resend, tokenFromLocation, verify} from '@/lib/signup'

type Phase = 'confirmed' | 'failed' | 'working'

/**
 * Where the link in the mail lands.
 *
 * It runs the confirmation exactly once, on mount, and it has to: `useEffect`
 * with an empty dependency list is what keeps a re-render — a toast arriving, a
 * signal changing — from spending the token a second time and turning a
 * successful sign-up into "that link is not valid".
 */
export const Verify = (_props: RoutableProps) => {
    const [phase, setPhase] = useState<Phase>('working')
    const [again, setAgain] = useState('')

    useEffect(() => {
        const search = window.location.search
        const token = tokenFromLocation(search)
        // `/verify?resend=…` is the other way in: the "send another" link on the
        // check-your-inbox panel, which has an address and no token.
        const prefill = new URLSearchParams(search).get('resend')
        if (prefill) {
            setAgain(prefill)
        }
        if (!token) {
            setPhase('failed')
            return
        }
        void verify(token).then((ok) => setPhase(ok ? 'confirmed' : 'failed'))
    }, [])

    if (phase === 'working') {
        return <div class="splash">Confirming…</div>
    }

    if (phase === 'confirmed') {
        return (
            <>
                <div class="card">
                    <h1>You're in, {$s.signup.username}</h1>
                    <p class="hint">
                        Your account is confirmed. Sign in to the world at{' '}
                        <a href="https://cartopolis.org">cartopolis.org</a> with the username and
                        password you just chose — this page is the account admin, and it only lets
                        operators in.
                    </p>
                </div>
                <HousePicker />
            </>
        )
    }

    return (
        <div class="card signin">
            <h1>That link didn't work</h1>
            <p class="hint">
                It may have been used already, replaced by a newer one, or simply expired. Ask for
                another and we'll send it.
            </p>
            <label class="field">
                <span>Email</span>
                <input
                    autocomplete="email"
                    inputMode="email"
                    onInput={(e) => setAgain((e.target as HTMLInputElement).value)}
                    type="email"
                    value={again}
                />
            </label>
            <button
                class="primary"
                disabled={!again.includes('@')}
                onClick={() => void resend(again.trim())}
                type="button"
            >
                Send another link
            </button>
            <button class="ghost" onClick={() => route(ADMIN)} type="button">
                Back to sign in
            </button>
        </div>
    )
}
