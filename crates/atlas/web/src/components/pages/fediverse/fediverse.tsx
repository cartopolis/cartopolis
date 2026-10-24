import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldText} from '@garage44/mithril/components'

import {$s, store} from '@/app'
import {ProviderLogo} from '@/components/elements/providers/providers'
import {begin} from '@/lib/auth'
import {bind, local} from '@/lib/reactive'
import {fediverseHost, rememberServer, safeReturn, startUrl} from '@/lib/signin'

/**
 * What the page was opened with. Read from the address rather than the route's
 * attrs: the router decodes the whole path before it splits the query, so a
 * `return` that carries its own `?` would come apart.
 */
interface FediverseQuery {
    /** The app's pairing code, when the app opened this page. */
    app?: string
    /** `link` or `reauth` from the account page; absent for signing in. */
    purpose?: string
    return?: string
}

function query(): FediverseQuery {
    const q = new URLSearchParams(location.search)
    return {app: q.get('app') ?? undefined, purpose: q.get('purpose') ?? undefined, return: q.get('return') ?? undefined}
}

/**
 * `/signin/fediverse`: which server are you on? (v2 section 2 of the mockup.)
 *
 * The one provider that cannot go straight to a sign-in page, because there is
 * no single Mastodon: the server registers itself with whichever one is named
 * here, the first time somebody from it arrives. So the page asks one thing,
 * accepts it however it is usually written (`@anna@mastodon.nl`, the server
 * alone, a profile link), and remembers the server — only the server — for the
 * next time.
 */
export class Fediverse extends MithrilComponent {
    s = local({address: '', busy: false})

    /** The Mastodon provider's id; `mastodon` unless the server says otherwise. */
    id() {
        return $s.registration?.providers?.find((p) => p.kind === 'mastodon')?.id ?? 'mastodon'
    }

    go = async (e: Event) => {
        const attrs = query()
        e.preventDefault()
        const host = fediverseHost(this.s.address)
        if (!host || this.s.busy) return
        $s.fediverseServers = rememberServer($s.fediverseServers, host)
        store.save()
        const ret = safeReturn(attrs.return)
        if (attrs.purpose === 'link' || attrs.purpose === 'reauth') {
            // The account page's round trips need the session, so the server
            // makes the address rather than the plain start route.
            this.s.busy = true
            const ok = await begin(this.id(), attrs.purpose, {return: ret, server: host})
            if (!ok) this.s.busy = false
            return
        }
        this.s.busy = true
        location.assign(startUrl(this.id(), ret, {app: attrs.app, server: host}))
    }

    view() {
        const s = this.s
        const host = fediverseHost(s.address)
        const typed = s.address.trim() !== ''
        const recent = $s.fediverseServers
        return (
            <form class="auth" novalidate onsubmit={(e: Event) => void this.go(e)}>
                <Card
                    title={
                        <span class="provider-title">
                            {ProviderLogo({id: 'mastodon', kind: 'mastodon'}, 22)}
                            Sign in with the fediverse
                        </span>
                    }
                >
                    <p class="auth__lede">
                        Mastodon, GoToSocial, Pleroma, Akkoma or any server that works with Mastodon apps.
                    </p>
                    <FieldText
                        autocomplete="username"
                        disabled={s.busy}
                        error={typed && !host ? 'Add your server, like @anna@mastodon.nl or mastodon.nl.' : null}
                        help={host ? `You'll go to ${host} to allow Cartopolis.` : undefined}
                        label="Your fediverse address or server"
                        model={bind(s, 'address')}
                        placeholder="@anna@mastodon.nl"
                    />
                    {recent.length > 0 && (
                        <div class="fediverse-recent">
                            <div class="fediverse-recent__label">Recently used here</div>
                            {recent.map((server) => (
                                <Button
                                    disabled={s.busy}
                                    key={server}
                                    onclick={() => (s.address = server)}
                                    size="sm"
                                    variant="ghost"
                                >
                                    {server}
                                </Button>
                            ))}
                        </div>
                    )}
                    <div class="auth__actions">
                        <Button disabled={!host} loading={s.busy} type="submit" variant="primary">
                            {host ? `Continue to ${host}` : 'Continue'}
                        </Button>
                        <Button disabled={s.busy} onclick={() => history.back()} variant="ghost">
                            Back
                        </Button>
                    </div>
                </Card>
            </form>
        )
    }
}
