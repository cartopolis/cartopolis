import m, {MithrilComponent} from '@garage44/mithril'

import type {Account as AccountRow} from '@/types'

import {$s} from '@/app'
import {activateAccount, deleteAccount, updateAccount} from '@/lib/accounts'
import {local} from '@/lib/reactive'
import {loadRoles, may, rolesOf, setAccountRoles} from '@/lib/roles'
import {formatTimestamp} from '@/lib/utils'

interface Props {
    account: AccountRow
}

export class Account extends MithrilComponent<Props> {
    s: {
        busy: boolean
        /*
         * Delete is two presses, not a `confirm()`: the native dialog is suppressible,
         * looks nothing like the rest of the app, and on a phone appears somewhere
         * unrelated to the row it is about.
         */
        confirming: boolean
        password: string
        roles: string[]
    }

    constructor(vnode: m.Vnode<Props>) {
        super()
        this.s = local({busy: false, confirming: false, password: '', roles: rolesOf(vnode.attrs.account.id)})
    }

    act = async (fn: () => Promise<boolean>) => {
        this.s.busy = true
        const ok = await fn()
        this.s.busy = false
        return ok
    }

    view(vnode: m.Vnode<Props>) {
        const {account} = vnode.attrs
        const s = this.s
        const act = this.act

        const open = $s.editing === account.id
        const isSelf = account.id === $s.profile.principal
        const held = rolesOf(account.id)
        const roles = s.roles
        // Only an admin may change an admin's account or who is an admin; the server
        // refuses it from anybody else, so the buttons are not offered.
        const mayTouch = $s.me.admin || !account.admin

        const toggleOpen = () => {
            $s.editing = open ? null : account.id
            s.confirming = false
            s.password = ''
            s.roles = held
            if (!open && may('roles_manage') && !$s.roles.length) {
                void loadRoles()
            }
        }
        const rolesChanged = roles.slice().sort().join(',') !== held.slice().sort().join(',')

        const resetPassword = async () => {
            if (s.password.length < 8) {
                return
            }
            const ok = await act(() =>
                updateAccount(account.id, {password: s.password}, `Set a new password for ${account.username}`),
            )
            if (ok) {
                s.password = ''
            }
        }

        return (
            <li class={`account ${account.disabled ? 'is-disabled' : ''}`}>
                <div class="account-main">
                    <span class="account-name">{account.username}</span>
                    {account.admin && <span class="tag admin">admin</span>}
                    {held.map((r) => (
                        <span class="tag" key={r}>
                            {$s.roles.find((x) => x.id === r)?.name ?? r}
                        </span>
                    ))}
                    {account.disabled && <span class="tag disabled">disabled</span>}
                    {/*
                      * Its own tag rather than the `disabled` one. They are different
                      * states with different cures — an operator un-disables, and only
                      * the person themselves can confirm an address — and a row that
                      * showed both as "disabled" would send the operator to the wrong
                      * button.
                      */}
                    {account.email_pending && <span class="tag pending">unconfirmed</span>}
                    {isSelf && <span class="tag you">you</span>}
                    <span style="flex:1" />
                    <button class="ghost" onclick={toggleOpen} type="button">
                        {open ? 'Close' : 'Manage'}
                    </button>
                    <span class="account-meta">
                        Created {formatTimestamp(account.created_at)}
                        {/* Personal data, and here because the operator is the person
                            who has to answer a deletion request and needs to be able
                            to find the account it names. */}
                        {account.email ? ` · ${account.email}` : ''}
                    </span>
                </div>

                {open && (
                    <div class="account-edit">
                        {may('roles_manage') && (
                            <>
                                <div class="account-roles">
                                    {$s.roles
                                        .filter((r) => !r.built_in)
                                        .map((r) => (
                                            <label class="check" key={r.id}>
                                                <input
                                                    checked={roles.includes(r.id)}
                                                    disabled={s.busy}
                                                    onchange={() => {
                                                        s.roles = roles.includes(r.id)
                                                            ? roles.filter((x) => x !== r.id)
                                                            : [...roles, r.id]
                                                    }}
                                                    type="checkbox"
                                                />
                                                {r.name}
                                            </label>
                                        ))}
                                    {!$s.roles.some((r) => !r.built_in) && (
                                        <span class="hint">No roles besides Player yet — make one on the Roles tab.</span>
                                    )}
                                </div>
                                {rolesChanged && (
                                    <div class="row-actions">
                                        <button
                                            class="primary"
                                            disabled={s.busy}
                                            onclick={() =>
                                                act(() => setAccountRoles(account.id, account.username, roles))
                                            }
                                            type="button"
                                        >
                                            Save roles
                                        </button>
                                    </div>
                                )}
                            </>
                        )}
                        {!mayTouch ? (
                            <p class="hint">Only an admin can change an admin's account.</p>
                        ) : (
                        <>
                        <label class="field">
                            <span>New password</span>
                            <input
                                autocomplete="new-password"
                                disabled={s.busy}
                                oninput={(e: Event) => {
                                    s.password = (e.target as HTMLInputElement).value
                                }}
                                placeholder="At least 8 characters"
                                type="password"
                                value={s.password}
                            />
                        </label>
                        <div class="row-actions">
                            {account.email_pending && (
                                <button disabled={s.busy} onclick={() => act(() => activateAccount(account))} type="button">
                                    Activate and notify
                                </button>
                            )}
                            <button disabled={s.busy || s.password.length < 8} onclick={resetPassword} type="button">
                                Set password
                            </button>
                            <button
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
                                type="button"
                            >
                                {account.disabled ? 'Enable' : 'Disable'}
                            </button>
                            {$s.me.admin && (
                                <button
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
                                    type="button"
                                >
                                    {account.admin ? 'Remove admin' : 'Make admin'}
                                </button>
                            )}
                            {s.confirming ? (
                                <>
                                    <button
                                        class="danger"
                                        disabled={s.busy}
                                        onclick={() => act(() => deleteAccount(account))}
                                        type="button"
                                    >
                                        Really delete
                                    </button>
                                    <button
                                        class="ghost"
                                        disabled={s.busy}
                                        onclick={() => {
                                            s.confirming = false
                                        }}
                                        type="button"
                                    >
                                        Cancel
                                    </button>
                                </>
                            ) : (
                                <button
                                    class="danger"
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
                                    type="button"
                                >
                                    Delete
                                </button>
                            )}
                        </div>
                        <p class="hint">
                            Disabling stops the next sign-in; someone already connected stays connected until they
                            disconnect.
                        </p>
                        </>
                        )}
                    </div>
                )}
            </li>
        )
    }
}
