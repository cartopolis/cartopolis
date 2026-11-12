import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button, Card, FieldText} from '@garage44/mithril/components'

import {bind, local} from '@/lib/reactive'
import {installServerUpdate, loadServerUpdate, type ServerUpdate} from '@/lib/server-update'

/** How often the banner asks while an install runs, and while the server restarts. */
const POLL_MS = 2000
/** A result older than this is not news on the Admin page any more. */
const RESULT_DAYS = 7

/**
 * The server's own update on every Admin page (`docs/mockups/release-approval`,
 * frames 8 and 9): a release the owner signed that this server has downloaded
 * and checked, with **Install**; the steps while it installs; and what happened
 * last. Nothing at all where the server does not update itself, or when there
 * is nothing to say.
 */
export class ServerUpdateBanner extends MithrilComponent {
    s = local({
        busy: false,
        error: '',
        password: '',
        update: null as ServerUpdate | null,
    })

    private timer: ReturnType<typeof setTimeout> | null = null

    oninit() {
        void this.load()
    }

    onremove() {
        if (this.timer) clearTimeout(this.timer)
    }

    private async load() {
        const next = await loadServerUpdate()
        const was = this.s.update
        // While restarting the server is away: keep showing the steps until it
        // answers again, now as the new version.
        if (next || !was || !['installing', 'restarting'].includes(was.phase)) {
            this.s.update = next
        }
        const phase = this.s.update?.phase
        if (phase === 'installing' || phase === 'restarting' || phase === 'checking' || phase === 'downloading') {
            this.timer = setTimeout(() => void this.load(), POLL_MS)
        }
        m.redraw()
    }

    private install = async (event: Event) => {
        event.preventDefault()
        const s = this.s
        s.busy = true
        s.error = ''
        const answer = await installServerUpdate(s.password || undefined)
        s.password = ''
        s.busy = false
        if (answer === 'reauth') {
            s.error = 'This account has no password: sign out, sign in again through its provider, and install within a few minutes.'
        } else if (typeof answer === 'string') {
            s.error = answer
        } else {
            s.update = answer
            this.timer = setTimeout(() => void this.load(), POLL_MS)
        }
        m.redraw()
    }

    private recentResult(u: ServerUpdate) {
        const r = u.last_result
        if (!r) return null
        if (Date.now() / 1000 - r.at > RESULT_DAYS * 86400) return null
        const when = new Date(r.at * 1000).toLocaleString()
        if (r.result === 'rolled back') {
            return (
                <Alert tone="warning">
                    Release {r.code} did not start here and was rolled back to {r.back_to} on {when}. The server waits for a
                    newer release.
                </Alert>
            )
        }
        return (
            <Alert tone="success">
                Release {r.code} installed on {when} by {r.by}.
            </Alert>
        )
    }

    view() {
        const u = this.s.update
        if (!u) return null
        const s = this.s
        if (u.phase === 'ready' && u.available) {
            const a = u.available
            return (
                <form class="server-update" onsubmit={this.install}>
                    <Card title={`Cartopolis server ${a.name} is ready to install`}>
                        <p class="server-update__seal">
                            ✓ Signed with the release key {u.signed_by}, checked by this server. You run {u.installed.name}.
                        </p>
                        {a.changes.length > 0 && (
                            <ul class="server-update__changes">
                                {a.changes.map((c) => (
                                    <li>{c}</li>
                                ))}
                            </ul>
                        )}
                        <FieldText
                            autocomplete="current-password"
                            disabled={s.busy}
                            label="Your password, to confirm"
                            model={bind(s, 'password')}
                            type="password"
                        />
                        {s.error && <Alert tone="danger">{s.error}</Alert>}
                        <Button loading={s.busy} type="submit" variant="primary">
                            Install
                        </Button>
                        <p class="server-update__hint">
                            The server tests the new version on a copy of its database first, keeps this version and its
                            database, and is away for about 20 seconds while it restarts.
                        </p>
                    </Card>
                </form>
            )
        }
        if (['installing', 'restarting', 'failed'].includes(u.phase)) {
            const title =
                u.phase === 'failed'
                    ? 'The update stopped; nothing was changed'
                    : `Installing Cartopolis server ${u.available?.name ?? ''}`
            return (
                <div class="server-update">
                    <Card title={title}>
                        <ol class="server-update__steps">
                            {u.steps.map((step) => (
                                <li>{step}</li>
                            ))}
                            {u.phase === 'restarting' && <li class="is-waiting">Waiting for the new version to answer…</li>}
                        </ol>
                        {u.phase === 'failed' && u.message && <Alert tone="danger">{u.message}</Alert>}
                    </Card>
                </div>
            )
        }
        return this.recentResult(u)
    }
}
