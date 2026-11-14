import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, FieldCheckbox, FieldText} from '@garage44/mithril/components'
import {minLength, validation} from '@garage44/mithril/validation'

import type {Account as AccountRow} from '@/types'

import {$s} from '@/app'
import {activity, ago, duration, toggleOpen} from '@/lib/account-stats'
import {activateAccount, deleteAccount, updateAccount} from '@/lib/accounts'
import {closeContext, contextShows, openContext} from '@/lib/layout'
import {bind, local} from '@/lib/reactive'
import {loadRoles, may, rolesOf, setAccountRoles} from '@/lib/roles'
import {formatTimestamp} from '@/lib/utils'
import {placeName} from '@/lib/stats'

/** The server's own minimum (`MIN_PASSWORD_LEN` in the simulator's `http.rs`). */
export const MIN_PASSWORD = 8

/** The context panel's key for one account's form. */
const contextKey = (id: string) => `account:${id}`

/**
 * Open one account's form in the context panel.
 *
 * The form's state lives here, in the closure the panel's functions share, and
 * not on the row: the row can leave the list while the form is open (a search
 * that no longer matches it), and the form has to keep working beside it. Every
 * open starts fresh, as the inline editor did on each "Manage".
 */
function manage(id: string) {
    const s = local({
        busy: false,
        /*
         * Delete is two presses, not a `confirm()`: the native dialog is suppressible,
         * looks nothing like the rest of the app, and on a phone appears somewhere
         * unrelated to the row it is about.
         */
        confirming: false,
        password: '',
        roles: rolesOf(id),
    })
    // Not `required`: an empty field means "leave the password alone", and the
    // button is what stays disabled then.
    const password = validation(bind(s, 'password'), [minLength(MIN_PASSWORD)])

    const act = async (fn: () => Promise<boolean>) => {
        s.busy = true
        const ok = await fn()
        s.busy = false
        return ok
    }

    if (may('roles_manage') && !$s.roles.length) {
        void loadRoles()
    }

    // Read live on every redraw: every action reloads the list, and the panel
    // must show the row the server handed back, not the one that was clicked.
    const current = () => $s.accounts.find((a) => a.id === id)

    const resetPassword = async (account: AccountRow) => {
        if (!s.password || !password.valid) {
            return
        }
        const ok = await act(() =>
            updateAccount(account.id, {password: s.password}, `Set a new password for ${account.username}`),
        )
        if (ok) {
            s.password = ''
            password.reset()
        }
    }

    openContext({
        key: contextKey(id),
        title: () => current()?.username ?? 'Account',
        body: () => {
            const account = current()
            if (!account) {
                return <p class="hint">This account no longer exists.</p>
            }
            const held = rolesOf(account.id)
            const roles = s.roles
            const rolesChanged = roles.slice().sort().join(',') !== held.slice().sort().join(',')
            // Only an admin may change an admin's account or who is an admin; the server
            // refuses it from anybody else, so the controls are not offered.
            const mayTouch = $s.me.admin || !account.admin

            return (
                <div class="account-form">
                    <AccountTags account={account} />
                    <p class="hint">
                        Created {formatTimestamp(account.created_at)}
                        {/* Personal data, and here because the operator is the person
                            who has to answer a deletion request and needs to be able
                            to find the account it names. */}
                        {account.email ? ` · ${account.email}` : ''}
                    </p>

                    {may('roles_manage') && (
                        <section class="account-form-section">
                            <h3>Roles</h3>
                            {$s.roles
                                .filter((r) => !r.built_in)
                                .map((r) => (
                                    <FieldCheckbox
                                        disabled={s.busy}
                                        key={r.id}
                                        label={r.name}
                                        model={{
                                            get value() {
                                                return roles.includes(r.id)
                                            },
                                            set value(on: boolean) {
                                                s.roles = on ? [...roles, r.id] : roles.filter((x) => x !== r.id)
                                            },
                                        }}
                                    />
                                ))}
                            {!$s.roles.some((r) => !r.built_in) && (
                                <p class="hint">No roles besides Player yet — make one on the Roles tab.</p>
                            )}
                            {rolesChanged && (
                                <div class="account-form-actions">
                                    <Button
                                        disabled={s.busy}
                                        onclick={() => act(() => setAccountRoles(account.id, account.username, roles))}
                                        variant="primary"
                                    >
                                        Save roles
                                    </Button>
                                </div>
                            )}
                        </section>
                    )}

                    {!mayTouch ? (
                        <p class="hint">Only an admin can change an admin's account.</p>
                    ) : (
                        <>
                            <section class="account-form-section">
                                <h3>Password</h3>
                                <FieldText
                                    autocomplete="new-password"
                                    disabled={s.busy}
                                    label="New password"
                                    model={bind(s, 'password')}
                                    placeholder={`At least ${MIN_PASSWORD} characters`}
                                    type="password"
                                    validation={password}
                                />
                                <div class="account-form-actions">
                                    <Button
                                        disabled={s.busy || !s.password || !password.valid}
                                        onclick={() => resetPassword(account)}
                                    >
                                        Set password
                                    </Button>
                                </div>
                            </section>

                            <section class="account-form-section">
                                <h3>Access</h3>
                                <div class="account-form-actions">
                                    {account.email_pending && (
                                        <Button disabled={s.busy} onclick={() => act(() => activateAccount(account))}>
                                            Activate and notify
                                        </Button>
                                    )}
                                    <Button
                                        disabled={s.busy}
                                        onclick={() =>
                                            act(() =>
                                                updateAccount(
                                                    account.id,
                                                    {disabled: !account.disabled},
                                                    `${account.disabled ? 'Enabled' : 'Disabled'} ${account.username}`,
                                                ),
                                            )
                                        }
                                    >
                                        {account.disabled ? 'Enable' : 'Disable'}
                                    </Button>
                                    {$s.me.admin && (
                                        <Button
                                            disabled={s.busy}
                                            onclick={() =>
                                                act(() =>
                                                    updateAccount(
                                                        account.id,
                                                        {admin: !account.admin},
                                                        `${account.admin ? 'Demoted' : 'Promoted'} ${account.username}`,
                                                    ),
                                                )
                                            }
                                        >
                                            {account.admin ? 'Remove admin' : 'Make admin'}
                                        </Button>
                                    )}
                                    <Button
                                        disabled={s.busy}
                                        onclick={() =>
                                            act(() =>
                                                updateAccount(
                                                    account.id,
                                                    {agent: !account.agent},
                                                    `${account.username} is ${account.agent ? 'no longer' : 'now'} marked as an agent`,
                                                ),
                                            )
                                        }
                                    >
                                        {account.agent ? 'Not an agent' : 'Mark as agent'}
                                    </Button>
                                </div>
                                <p class="hint">
                                    Disabling stops the next sign-in; someone already connected stays connected until
                                    they disconnect.
                                </p>
                            </section>
                        </>
                    )}
                </div>
            )
        },
        footer: () => {
            const account = current()
            const close = (
                <Button
                    onclick={() => {
                        closeContext(contextKey(id))
                    }}
                    variant="ghost"
                >
                    Close
                </Button>
            )
            if (!account || !($s.me.admin || !account.admin)) {
                return close
            }
            const isSelf = account.id === $s.profile.principal
            return (
                <>
                    {s.confirming ? (
                        <>
                            <Button
                                disabled={s.busy}
                                onclick={async () => {
                                    if (await act(() => deleteAccount(account))) {
                                        closeContext(contextKey(id))
                                    }
                                }}
                                variant="danger"
                            >
                                Really delete
                            </Button>
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    s.confirming = false
                                }}
                                variant="ghost"
                            >
                                Cancel
                            </Button>
                        </>
                    ) : (
                        <>
                            <Button
                                /*
                                 * The server refuses this too (you cannot delete
                                 * the account you are signed in with), but a
                                 * button that exists only to be refused is worse
                                 * than one that is not there.
                                 */
                                disabled={s.busy || isSelf}
                                onclick={() => {
                                    s.confirming = true
                                }}
                                variant="danger"
                            >
                                Delete
                            </Button>
                            <span class="account-form-spacer" />
                            {close}
                        </>
                    )}
                </>
            )
        },
    })
}

/** What an account is, as badges: shared by the row and the form's head. */
class AccountTags extends MithrilComponent<{account: AccountRow; compact?: boolean}> {
    view(vnode: m.Vnode<{account: AccountRow; compact?: boolean}>) {
        const {account, compact} = vnode.attrs
        const isSelf = account.id === $s.profile.principal
        return (
            <span class="account-tags">
                {account.admin && <Badge tone="accent">admin</Badge>}
                {account.agent && <Badge>agent</Badge>}
                {!compact &&
                    rolesOf(account.id).map((r) => (
                    <Badge key={r}>{$s.roles.find((x) => x.id === r)?.name ?? r}</Badge>
                ))}
                {account.disabled && <Badge>disabled</Badge>}
                {/*
                  * Its own badge rather than the `disabled` one. They are different
                  * states with different cures — an operator un-disables, and only
                  * the person themselves can confirm an address — and a row that
                  * showed both as "disabled" would send the operator to the wrong
                  * button.
                  */}
                {account.email_pending && <Badge tone="warning">unconfirmed</Badge>}
                {isSelf && <Badge tone="info">you</Badge>}
            </span>
        )
    }
}

interface Props {
    account: AccountRow
}

/** A tile of the opened line: one number and what it counts. */
const tile = (n: number, one: string, many = `${one}s`) => (
    <div class="account-stat">
        <b>{n}</b>
        <span>{n === 1 ? one : many}</span>
    </div>
)

/** What an opened line shows: the account's numbers, then Manage. */
class AccountDetails extends MithrilComponent<Props> {
    view(vnode: m.Vnode<Props>) {
        const {account} = vnode.attrs
        const stats = activity.stats[account.id]
        const roles = rolesOf(account.id).map((r) => $s.roles.find((x) => x.id === r)?.name ?? r)
        const manageButton = (
            <Button
                onclick={() =>
                    contextShows(contextKey(account.id)) ? closeContext(contextKey(account.id)) : manage(account.id)
                }
                size="sm"
            >
                {contextShows(contextKey(account.id)) ? 'Close manage' : 'Manage…'}
            </Button>
        )
        if (stats === undefined) {
            return <div class="account-more"><p class="hint">Loading…</p>{manageButton}</div>
        }
        const busiest = Math.max(1, ...(stats?.minutes ?? []))
        const total = (stats?.minutes ?? []).reduce((a, b) => a + b, 0)
        return (
            <div class="account-more">
                {/* Personal data, and here because the operator is the person who
                    has to answer a deletion request and needs to be able to find
                    the account it names. */}
                {account.email && (
                    <p class="account-mail">
                        {account.email} · {account.email_pending ? 'not confirmed' : 'confirmed'}
                    </p>
                )}
                {stats && (
                    <>
                        <div class="account-stats">
                            {tile(stats.communities, 'community', 'communities')}
                            {tile(stats.posts, 'post')}
                            {tile(stats.objects, 'object built', 'objects built')}
                            {tile(stats.buildings, 'building owned', 'buildings owned')}
                            {tile(stats.contacts, 'contact')}
                            {tile(stats.days_active, 'day active', 'days active')}
                        </div>
                        <div
                            class="account-bars"
                            title={`${duration(total)} online in the last ${stats.minutes.length} days`}
                        >
                            {stats.minutes.map((n, i) => (
                                <span
                                    class={i === stats.minutes.length - 1 ? 'is-today' : ''}
                                    key={i}
                                    style={{height: `${n ? Math.max(8, (n / busiest) * 100) : 0}%`}}
                                />
                            ))}
                        </div>
                        <p class="account-bars-cap">
                            Time online per day, last {stats.minutes.length} days: {duration(total)}
                        </p>
                        <dl class="account-facts">
                            <dt>Last seen</dt>
                            <dd>
                                {stats.online ? 'online now' : ago(stats.last_seen)}
                                {stats.last_place && ` · last place ${placeName(stats.last_place.lat, stats.last_place.lng)}`}
                            </dd>
                            <dt>Sign-ins</dt>
                            <dd>
                                {stats.sign_ins} in {stats.keep_days} days
                                {stats.last_app && `, last from ${stats.last_app}`}
                                {stats.last_build && ` (${stats.last_build})`}
                            </dd>
                            <dt>Devices</dt>
                            <dd>{stats.devices ? `${stats.devices} with chat keys` : 'none with chat keys'}</dd>
                            <dt>Signs in with</dt>
                            <dd>
                                {['password', ...stats.identities.map((i) => `${i.provider} ${i.handle}`)].join(' · ')}
                            </dd>
                            {roles.length > 0 && (
                                <>
                                    <dt>Roles</dt>
                                    <dd>{roles.join(', ')}</dd>
                                </>
                            )}
                            <dt>Created</dt>
                            <dd>{formatTimestamp(account.created_at)}</dd>
                        </dl>
                    </>
                )}
                <div class="account-form-actions">{manageButton}</div>
            </div>
        )
    }
}

/**
 * One line of the account list: who, whether they are here, since when. A tap
 * opens the line in place with the account's numbers; "Manage…" there opens its
 * form in the context panel as before, so on a wide screen the list stays in
 * view beside it.
 */
export class Account extends MithrilComponent<Props> {
    view(vnode: m.Vnode<Props>) {
        const {account} = vnode.attrs
        const opened = activity.open === account.id
        const managed = contextShows(contextKey(account.id))
        const seen = activity.seen[account.id]

        return (
            <li
                class={`record account-line ${account.disabled ? 'is-disabled' : ''} ${
                    managed ? 'is-selected' : ''
                } ${opened ? 'is-open' : ''}`}
            >
                <button
                    aria-expanded={opened ? 'true' : 'false'}
                    class="account-head"
                    onclick={() => toggleOpen(account.id)}
                    type="button"
                >
                    <span class="account-avatar">{account.username.slice(0, 1).toUpperCase()}</span>
                    <span class="account-who">
                        <span class="account-name">
                            <span class="record-name">{account.username}</span>
                            <AccountTags account={account} compact />
                        </span>
                        <span class="account-seen">
                            <span class={`account-dot ${seen?.online ? 'is-online' : ''}`} />
                            {seen?.online ? 'online' : seen ? ago(seen.last_seen) : '…'} · since{' '}
                            {formatTimestamp(account.created_at)}
                        </span>
                    </span>
                    <span class="account-chevron">{opened ? '⌄' : '›'}</span>
                </button>
                {opened && <AccountDetails account={account} />}
            </li>
        )
    }
}
