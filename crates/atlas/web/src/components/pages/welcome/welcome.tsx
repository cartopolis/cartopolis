import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button, Card, FieldCheckbox, FieldRadio, FieldText} from '@garage44/mithril/components'

import type {Identity, Offer, SigninTicket, TicketSession} from '@/types'

import {$s, notifier} from '@/app'
import {ProviderLogo} from '@/components/elements/providers/providers'
import {
    adoptTicketSession,
    communityFromTicket,
    createFromTicket,
    joinFromTicket,
    linkFromTicket,
    readTicket,
    sentence,
} from '@/lib/auth'
import {ADMIN} from '@/lib/base'
import {bind, local} from '@/lib/reactive'
import {createLabel, safeReturn, welcomeQuery} from '@/lib/signin'

/** Where "Back to Cartopolis" goes when the app asked: the app link Android claims. */
const APP_LINK = 'https://cartopolis.org/go'

/** How long a short confirmation stays before the page moves on. */
const SETTLE_MS = 1200

/** "Discord server", or the group's own name when it differs from the community's. */
function groupWords(offer: Offer): string {
    const kind = offer.provider === 'reddit' ? 'Subreddit' : `${offer.provider_name} server`
    return offer.group_name && offer.group_name !== offer.name ? `${kind} ${offer.group_name}` : kind
}

/** Which communities are ticked, as a list the form fields can bind to. */
interface Pick {
    join: boolean
    offer: Offer
}

/**
 * `/welcome`: where every round trip to a provider ends
 * (`docs/mockups/external-sign-in/`).
 *
 * The server sends the browser here with `?ticket=…` whatever happened, and
 * the ticket says what that was — signed in, a name to pick, a password to
 * link with, a test that passed, groups to link — so this page is a switch
 * over the ticket's kind and nothing more. The ticket is read once, in
 * `oninit` (a second read after it was spent would turn a sign-in into an
 * error), and taken out of the address bar: for a sign-in it is as good as a
 * session for the ten minutes it lives.
 */
export class Welcome extends MithrilComponent {
    s = local({
        busy: false,
        /** The app's code, once the account it asked for is signed in. */
        appDone: '' as string,
        /** A short "done" line before moving on. */
        done: '',
        /** A refusal from the last step, shown in the card. */
        error: '',
        group: '',
        name: '',
        password: '',
        picks: [] as Pick[],
        /** The server's sentence when there is no ticket to read. */
        problem: '',
        signedInAs: '',
        ticket: '',
        what: null as SigninTicket | null,
    })

    oninit() {
        const q = welcomeQuery(location.search)
        history.replaceState(null, '', '/welcome')
        if (!q) {
            this.s.problem = 'There is nothing to finish here. Start again from the sign-in page.'
            return
        }
        if ('error' in q) {
            this.s.problem = q.error
            return
        }
        this.s.ticket = q.ticket
        void this.read(q.ticket)
    }

    async read(ticket: string) {
        const s = this.s
        s.busy = true
        try {
            const what = await readTicket(ticket)
            s.what = what
            await this.arrive(what)
        } catch (e) {
            s.problem = sentence(e, 'Could not finish signing in')
        } finally {
            s.busy = false
        }
    }

    /** What happens as soon as the ticket is known, before anything is drawn for it. */
    async arrive(what: SigninTicket) {
        const s = this.s
        switch (what.kind) {
            case 'signed_in':
                await adoptTicketSession(what)
                s.signedInAs = what.username
                if (what.app_code) {
                    s.appDone = what.app_code
                } else if (what.offers.length) {
                    s.picks = what.offers.map((offer) => ({join: true, offer}))
                } else {
                    m.route.set(safeReturn(what.return))
                }
                break
            case 'choose_name':
                s.name = what.suggestion
                s.picks = what.offers.map((offer) => ({join: true, offer}))
                break
            case 'linked':
                this.settle(`${what.provider_name} is now a way into your account.`, what.return)
                break
            case 'reauthed':
                this.settle(`Confirmed with ${what.provider_name}.`, what.return)
                break
            case 'community_groups':
                s.group = what.groups[0]?.group_id ?? ''
                break
        }
    }

    /** Say it briefly, then go back to where the person was. */
    settle(done: string, ret: string) {
        this.s.done = done
        setTimeout(() => m.route.set(safeReturn(ret)), SETTLE_MS)
    }

    /** A session from create or link: kept, and then the app's message or the way back. */
    async signedIn(session: TicketSession, ret: string | undefined) {
        await adoptTicketSession(session)
        this.s.signedInAs = session.username
        if (session.app_code) {
            this.s.appDone = session.app_code
        } else {
            m.route.set(safeReturn(ret))
        }
    }

    joining(): string[] {
        return this.s.picks.filter((p) => p.join).map((p) => p.offer.community)
    }

    create = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        const what = s.what
        if (s.busy || what?.kind !== 'choose_name' || !s.name.trim()) return
        s.busy = true
        s.error = ''
        try {
            await this.signedIn(await createFromTicket(s.ticket, s.name.trim(), this.joining()), what.return)
        } catch (err) {
            // Inline, under the name: "that name is taken" is about the field
            // the person is looking at.
            s.error = sentence(err, 'Could not make that account')
        } finally {
            s.busy = false
        }
    }

    link = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        const what = s.what
        if (s.busy || what?.kind !== 'link_existing' || !s.password) return
        s.busy = true
        s.error = ''
        try {
            await this.signedIn(await linkFromTicket(s.ticket, s.password), what.return)
        } catch (err) {
            s.error = sentence(err, 'Could not link that account')
        } finally {
            s.password = ''
            s.busy = false
        }
    }

    join = async (ret: string) => {
        const s = this.s
        const join = this.joining()
        if (join.length) {
            s.busy = true
            const joined = await joinFromTicket(s.ticket, join)
            s.busy = false
            if (!joined) return
            notifier.success(joined.length === 1 ? 'You joined 1 community' : `You joined ${joined.length} communities`)
        }
        m.route.set(safeReturn(ret))
    }

    linkGroup = async (community: {id: string; name: string}) => {
        const s = this.s
        if (s.busy || !s.group) return
        s.busy = true
        s.error = ''
        try {
            const link = await communityFromTicket(s.ticket, s.group)
            notifier.success(`${link.group_name} is linked to ${community.name}`)
            m.route.set('/', {show: community.id})
        } catch (err) {
            s.error = sentence(err, 'Could not link that group')
        } finally {
            s.busy = false
        }
    }

    view() {
        const s = this.s
        if (s.problem) return this.problemView(s.problem)
        if (s.appDone) return this.appView()
        if (s.done) {
            return (
                <div class="auth">
                    <Card title="Done">
                        <Alert tone="success">{s.done}</Alert>
                    </Card>
                </div>
            )
        }
        const what = s.what
        if (!what) {
            return (
                <div class="auth">
                    <Card title="Signing in">
                        <p class="auth__working">One moment…</p>
                    </Card>
                </div>
            )
        }
        switch (what.kind) {
            case 'signed_in':
                return this.offersView(what.offers.length ? what.return : null)
            case 'choose_name':
                return this.chooseView(what)
            case 'link_existing':
                return this.linkView(what)
            case 'tested':
                return (
                    <div class="auth">
                        <Card title="It works">
                            <Alert tone="success">
                                Signing in with {what.provider_name} works. You can switch it on now.
                            </Alert>
                            <div class="auth__actions">
                                <Button
                                    onclick={() => {
                                        $s.adminTab = 'signin'
                                        m.route.set(ADMIN)
                                    }}
                                    variant="primary"
                                >
                                    Back to Admin › Sign-in
                                </Button>
                            </div>
                        </Card>
                    </div>
                )
            case 'community_groups':
                return this.groupsView(what)
            case 'refused':
                return this.problemView(what.message)
            default:
                // `linked` and `reauthed` have set `done`; this is the instant before.
                return null
        }
    }

    problemView(message: string) {
        return (
            <div class="auth">
                <Card title="That didn't work">
                    <Alert tone="warning">{message}</Alert>
                    <div class="auth__actions">
                        <Button onclick={() => m.route.set('/')} variant="primary">
                            Back to sign in
                        </Button>
                    </div>
                </Card>
            </div>
        )
    }

    /** The app asked: say so, and give the way back to it. Nothing else to do here. */
    appView() {
        return (
            <div class="auth">
                <Card title={`Signed in as ${this.s.signedInAs}`}>
                    <Alert tone="success">
                        The app asking with code <b>{this.s.appDone}</b> is now signed in.
                    </Alert>
                    <div class="auth__actions">
                        <Button href={APP_LINK} variant="primary">
                            Back to Cartopolis
                        </Button>
                    </div>
                </Card>
            </div>
        )
    }

    /** Who came back: the provider's name for them and how it was confirmed. */
    who(identity: Identity) {
        const confirmed = identity.email
            ? `${identity.email} · ${identity.email_verified ? 'verified' : 'given'} by ${identity.provider_name}`
            : `${identity.handle} · confirmed by ${identity.provider_name}`
        return (
            <div class="welcome-who">
                <span class="welcome-who__avatar">{identity.avatar_letter}</span>
                <div class="welcome-who__text">
                    <b>{identity.display_name || identity.handle}</b>
                    <small>{confirmed}</small>
                </div>
                {ProviderLogo({id: identity.provider, kind: identity.kind}, 22)}
            </div>
        )
    }

    /** The "your groups are here too" list, every box ticked to start with. */
    picks() {
        const s = this.s
        if (!s.picks.length) return null
        return (
            <div class="welcome-groups">
                <div class="welcome-groups__label">Your groups are here too</div>
                {s.picks.map((p) => (
                    <div class="welcome-group" key={p.offer.community}>
                        <FieldCheckbox
                            disabled={s.busy}
                            label={
                                <span class="welcome-group__name">
                                    {ProviderLogo({id: p.offer.provider}, 18)}
                                    <span>
                                        {p.offer.name}
                                        <small>
                                            {groupWords(p.offer)} · {p.offer.members}{' '}
                                            {p.offer.members === 1 ? 'member' : 'members'} already here
                                        </small>
                                    </span>
                                </span>
                            }
                            model={bind(p, 'join')}
                        />
                    </div>
                ))}
                <p class="auth__lede">
                    Only groups with a Cartopolis community are shown. We don't keep the list of your other groups.
                </p>
            </div>
        )
    }

    /** Signed in, with communities on offer; `ret` null when there are none (the page has moved on). */
    offersView(ret: string | null) {
        const s = this.s
        if (ret === null) return null
        const n = this.joining().length
        return (
            <div class="auth">
                <Card title={`Signed in as ${s.signedInAs}`}>
                    {this.picks()}
                    <div class="auth__actions">
                        <Button disabled={!n} loading={s.busy} onclick={() => void this.join(ret)} variant="primary">
                            {n ? `Join ${n}` : 'Join'}
                        </Button>
                        <Button disabled={s.busy} onclick={() => m.route.set(safeReturn(ret))} variant="ghost">
                            Not now
                        </Button>
                    </div>
                </Card>
            </div>
        )
    }

    chooseView(what: Extract<SigninTicket, {kind: 'choose_name'}>) {
        const s = this.s
        const max = $s.registration?.max_username_length
        const tooLong = max !== undefined && s.name.trim().length > max
        return (
            <form class="auth" novalidate onsubmit={this.create}>
                <Card title="Welcome to Cartopolis">
                    {this.who(what.identity)}
                    <FieldText
                        autocomplete="username"
                        disabled={s.busy}
                        error={tooLong ? `That name is longer than ${max} characters.` : null}
                        help="Free. This is how people find you in chat and calls."
                        label="Your name here"
                        model={bind(s, 'name')}
                        suffix={`@${location.hostname}`}
                    />
                    {!what.identity.email && (
                        <p class="auth__lede">
                            {what.identity.provider_name} doesn't share your e-mail address, so this account has none.
                        </p>
                    )}
                    {(what.identity.kind === 'mastodon' || what.identity.kind === 'reddit') && (
                        <Alert tone="info">
                            Your profile will show <b>{what.identity.handle}</b> as confirmed, so people who know you
                            there know it's you.
                        </Alert>
                    )}
                    {this.picks()}
                    {s.error && <Alert tone="danger">{s.error}</Alert>}
                    <div class="auth__actions">
                        <Button disabled={!s.name.trim() || tooLong} loading={s.busy} type="submit" variant="primary">
                            {createLabel(this.joining().length)}
                        </Button>
                        <Button disabled={s.busy} onclick={() => m.route.set('/')} variant="ghost">
                            That's not me — use another account
                        </Button>
                    </div>
                </Card>
            </form>
        )
    }

    linkView(what: Extract<SigninTicket, {kind: 'link_existing'}>) {
        const s = this.s
        const provider = what.identity.provider_name
        return (
            <form class="auth" novalidate onsubmit={this.link}>
                <Card title="You already have an account">
                    {this.who(what.identity)}
                    <p>
                        That address belongs to <b>{what.username}@{location.hostname}</b>. Enter its password once to add{' '}
                        {provider} to it, and you can then sign in either way.
                    </p>
                    <FieldText
                        autocomplete="current-password"
                        disabled={s.busy}
                        label={`Password for ${what.username}`}
                        model={bind(s, 'password')}
                        type="password"
                    />
                    {s.error && <Alert tone="danger">{s.error}</Alert>}
                    <div class="auth__actions">
                        <Button disabled={!s.password} loading={s.busy} type="submit" variant="primary">
                            Add {provider} and sign in
                        </Button>
                        <Button disabled={s.busy} onclick={() => m.route.set('/')} variant="ghost">
                            Not now
                        </Button>
                    </div>
                </Card>
            </form>
        )
    }

    groupsView(what: Extract<SigninTicket, {kind: 'community_groups'}>) {
        const s = this.s
        const kind = what.provider === 'reddit' ? 'subreddit' : 'server'
        return (
            <div class="auth">
                <Card title={`Link a group to ${what.community.name}`}>
                    {what.groups.length === 0 ? (
                        <Alert tone="info">
                            {what.provider_name} lists no {kind} you {kind === 'subreddit' ? 'moderate' : 'manage'}, so
                            there is nothing to link.
                        </Alert>
                    ) : (
                        <FieldRadio
                            disabled={s.busy}
                            label={`Which ${what.provider_name} ${kind}?`}
                            model={bind(s, 'group')}
                            options={what.groups.map((g) => ({label: g.name, value: g.group_id}))}
                        />
                    )}
                    {s.error && <Alert tone="danger">{s.error}</Alert>}
                    <div class="auth__actions">
                        {what.groups.length > 0 && (
                            <Button
                                disabled={!s.group}
                                loading={s.busy}
                                onclick={() => void this.linkGroup(what.community)}
                                variant="primary"
                            >
                                Link it
                            </Button>
                        )}
                        <Button
                            disabled={s.busy}
                            onclick={() => m.route.set('/', {show: what.community.id})}
                            variant="ghost"
                        >
                            Back to {what.community.name}
                        </Button>
                    </div>
                </Card>
            </div>
        )
    }
}
