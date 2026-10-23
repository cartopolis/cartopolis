import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, FieldCheckbox, FieldText} from '@garage44/mithril/components'
import {minLength, validation} from '@garage44/mithril/validation'

import type {Account as AccountRow} from '@/types'

import {$s} from '@/app'
import {activateAccount, deleteAccount, updateAccount} from '@/lib/accounts'
import {closeContext, contextShows, openContext} from '@/lib/layout'
import {bind, local} from '@/lib/reactive'
import {loadRoles, may, rolesOf, setAccountRoles} from '@/lib/roles'
import {formatTimestamp} from '@/lib/utils'

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
class AccountTags extends MithrilComponent<{account: AccountRow}> {
    view(vnode: m.Vnode<{account: AccountRow}>) {
        const {account} = vnode.attrs
        const isSelf = account.id === $s.profile.principal
        return (
            <span class="account-tags">
                {account.admin && <Badge tone="accent">admin</Badge>}
                {account.agent && <Badge>agent</Badge>}
                {rolesOf(account.id).map((r) => (
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

/**
 * One row of the account list. "Manage" opens the account's form in the context
 * panel, so on a wide screen the list stays in view beside it, and the row shows
 * itself as the one being edited.
 */
export class Account extends MithrilComponent<Props> {
    view(vnode: m.Vnode<Props>) {
        const {account} = vnode.attrs
        const open = contextShows(contextKey(account.id))

        return (
            <li class={`record ${account.disabled ? 'is-disabled' : ''} ${open ? 'is-selected' : ''}`}>
                <div class="record-main">
                    <span class="record-name">{account.username}</span>
                    <AccountTags account={account} />
                    <span class="record-spacer" />
                    <Button
                        onclick={() => (open ? closeContext(contextKey(account.id)) : manage(account.id))}
                        size="sm"
                        variant={open ? 'secondary' : 'ghost'}
                    >
                        {open ? 'Close' : 'Manage'}
                    </Button>
                </div>
                <div class="record-meta">
                    <span>Created {formatTimestamp(account.created_at)}</span>
                    {/* Personal data, and here because the operator is the person
                        who has to answer a deletion request and needs to be able
                        to find the account it names. */}
                    {account.email && <span>{account.email}</span>}
                </div>
            </li>
        )
    }
}
