import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Button, Card, FieldText} from '@garage44/mithril/components'

import type {AccountSignin, LinkedIdentity} from '@/types'

import {$s} from '@/app'
import {ProviderLogo} from '@/components/elements/providers/providers'
import {begin, removeIdentity, setPassword} from '@/lib/auth'
import {Icon} from '@/lib/icons'
import {bind, local} from '@/lib/reactive'
import {asksServer, canRemove, confirmLabel, linkable, reauthWith} from '@/lib/signin'

interface WaysAttrs {
    /** Fetch the account's ways in again, after anything here changed them. */
    reload: () => Promise<void>
    signin: AccountSignin
}

/** The account page's path, where every round trip from here comes back to. */
const HERE = '/tokens'

/**
 * Confirm with a provider: the round trip that stands in for "type your
 * password again" on an account that has none. Mastodon asks which server
 * first, as it does on the sign-in card.
 */
export function confirmWith(identity: Pick<LinkedIdentity, 'kind' | 'provider'>) {
    if (asksServer(identity)) {
        m.route.set('/signin/fediverse', {purpose: 'reauth', return: HERE})
        return
    }
    void begin(identity.provider, 'reauth', {return: HERE})
}

/**
 * "Ways to sign in" (`docs/mockups/external-sign-in/v1.html`, section 5): each
 * linked provider, the ones that could be linked, and the password.
 *
 * Never zero ways in: the last one cannot be removed. The server refuses it
 * too, and its sentence is shown when it does; this greys the button and says
 * why before anybody presses it.
 */
export class Ways extends MithrilComponent<WaysAttrs> {
    s = local({
        busy: false,
        current: '',
        /** The password form: closed, adding, or changing. */
        editing: '' as '' | 'add' | 'change',
        error: '',
        next: '',
        /** The server wants a confirmation through a provider before a first password. */
        reauth: false,
    })

    remove = async (identity: LinkedIdentity, reload: () => Promise<void>) => {
        if (!confirm(`Stop signing in with ${identity.provider_name}?`)) return
        this.s.busy = true
        if (await removeIdentity(identity.id, identity.provider_name)) await reload()
        this.s.busy = false
    }

    savePassword = async (e: Event, attrs: WaysAttrs) => {
        e.preventDefault()
        const s = this.s
        if (s.busy || !s.next) return
        s.busy = true
        s.error = ''
        s.reauth = false
        const result = await setPassword(s.next, s.editing === 'change' ? s.current : undefined)
        s.busy = false
        s.current = ''
        if (result === 'ok') {
            s.next = ''
            s.editing = ''
            await attrs.reload()
        } else if (result === 'reauth') {
            s.reauth = true
        } else {
            s.error = result
        }
    }

    view(vnode: m.Vnode<WaysAttrs>) {
        const s = this.s
        const {signin, reload} = vnode.attrs
        const removable = canRemove(signin)
        const first = reauthWith(signin.identities)
        const min = $s.registration?.min_password_length ?? 0
        return (
            <Card title="Ways to sign in">
                <ul class="ways">
                    {signin.identities.map((identity) => (
                        <li class="ways__row" key={identity.id}>
                            <span class="ways__icon">{ProviderLogo({id: identity.provider, kind: identity.kind}, 20)}</span>
                            <div class="ways__text">
                                <b>{identity.provider_name}</b>
                                <small>
                                    {identity.handle} · since{' '}
                                    {new Date(identity.linked_at * 1000).toLocaleDateString(undefined, {
                                        day: 'numeric',
                                        month: 'short',
                                        year: 'numeric',
                                    })}
                                </small>
                            </div>
                            <Button
                                disabled={s.busy || !removable}
                                onclick={() => void this.remove(identity, reload)}
                                size="sm"
                                variant="danger"
                            >
                                Remove
                            </Button>
                        </li>
                    ))}
                    {linkable(signin).map((p) => (
                        <li class="ways__row" key={`link-${p.id}`}>
                            <span class="ways__icon">{ProviderLogo(p, 20)}</span>
                            <div class="ways__text">
                                <b>{p.name}</b>
                                <small>Not linked</small>
                            </div>
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    if (asksServer(p)) m.route.set('/signin/fediverse', {purpose: 'link', return: HERE})
                                    else void begin(p.id, 'link', {return: HERE})
                                }}
                                size="sm"
                            >
                                Link
                            </Button>
                        </li>
                    ))}
                    <li class="ways__row">
                        <span class="ways__icon">{Icon('lock')}</span>
                        <div class="ways__text">
                            <b>Password</b>
                            <small>
                                {signin.has_password
                                    ? 'Set'
                                    : first
                                      ? `None. You sign in with ${first.provider_name}.`
                                      : 'None.'}
                            </small>
                        </div>
                        {!s.editing && (
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    s.editing = signin.has_password ? 'change' : 'add'
                                    s.error = ''
                                    s.reauth = false
                                }}
                                size="sm"
                            >
                                {signin.has_password ? 'Change' : 'Add a password'}
                            </Button>
                        )}
                    </li>
                </ul>

                {!removable && signin.identities.length === 1 && (
                    <Alert tone="info">
                        {signin.identities[0].provider_name} is your only way in, so it can't be removed until you
                        link another or add a password.
                    </Alert>
                )}

                {s.editing && (
                    <form class="ways__password" onsubmit={(e: Event) => void this.savePassword(e, vnode.attrs)}>
                        {s.editing === 'change' && (
                            <FieldText
                                autocomplete="current-password"
                                disabled={s.busy}
                                label="Current password"
                                model={bind(s, 'current')}
                                type="password"
                            />
                        )}
                        <FieldText
                            autocomplete="new-password"
                            disabled={s.busy}
                            help={min ? `At least ${min} characters.` : undefined}
                            label={s.editing === 'change' ? 'New password' : 'Password'}
                            model={bind(s, 'next')}
                            type="password"
                        />
                        {s.reauth && first && (
                            <Alert tone="info">
                                Confirm it's you with {first.provider_name} first, then add the password here.
                            </Alert>
                        )}
                        {s.error && <Alert tone="danger">{s.error}</Alert>}
                        <div class="ways__actions">
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    s.editing = ''
                                    s.next = ''
                                    s.current = ''
                                }}
                                size="sm"
                                variant="ghost"
                            >
                                Cancel
                            </Button>
                            {s.reauth && first ? (
                                <Button icon={ProviderLogo({id: first.provider, kind: first.kind})} onclick={() => confirmWith(first)} size="sm" variant="primary">
                                    {confirmLabel(first.provider_name)}
                                </Button>
                            ) : (
                                <Button
                                    disabled={!s.next || s.next.length < min || (s.editing === 'change' && !s.current)}
                                    loading={s.busy}
                                    size="sm"
                                    type="submit"
                                    variant="primary"
                                >
                                    Save password
                                </Button>
                            )}
                        </div>
                    </form>
                )}
            </Card>
        )
    }
}
