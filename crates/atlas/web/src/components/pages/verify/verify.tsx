import m, {MithrilComponent} from '@garage44/mithril'

import {$s} from '@/app'
import {BuildingPicker} from '@/components/elements/buildings/buildings'
import {ADMIN} from '@/lib/base'
import {local} from '@/lib/reactive'
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
    s = local({again: '', phase: 'working' as Phase})

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

    view() {
        const s = this.s

        if (s.phase === 'working') {
            return <div class="splash">Confirming…</div>
        }

        if (s.phase === 'confirmed') {
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
                    <BuildingPicker />
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
                        inputmode="email"
                        oninput={(e: Event) => {
                            s.again = (e.target as HTMLInputElement).value
                        }}
                        type="email"
                        value={s.again}
                    />
                </label>
                <button
                    class="primary"
                    disabled={!s.again.includes('@')}
                    onclick={() => void resend(s.again.trim())}
                    type="button"
                >
                    Send another link
                </button>
                <button class="ghost" onclick={() => m.route.set(ADMIN)} type="button">
                    Back to sign in
                </button>
            </div>
        )
    }
}
