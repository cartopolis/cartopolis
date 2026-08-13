import {useState} from 'preact/hooks'

import type {Account as AccountRow} from '@/types'

import {$s} from '@/app'
import {deleteAccount, updateAccount} from '@/lib/accounts'
import {formatTimestamp} from '@/lib/utils'

interface Props {
    account: AccountRow
}

export const Account = ({account}: Props) => {
    const [password, setPassword] = useState('')
    const [busy, setBusy] = useState(false)
    /*
     * Delete is two presses, not a `confirm()`: the native dialog is suppressible,
     * looks nothing like the rest of the app, and on a phone appears somewhere
     * unrelated to the row it is about.
     */
    const [confirming, setConfirming] = useState(false)

    const open = $s.editing === account.id
    const isSelf = account.id === $s.profile.principal

    const act = async (fn: () => Promise<boolean>) => {
        setBusy(true)
        const ok = await fn()
        setBusy(false)
        return ok
    }

    const toggleOpen = () => {
        $s.editing = open ? null : account.id
        setConfirming(false)
        setPassword('')
    }

    const resetPassword = async () => {
        if (password.length < 8) {
            return
        }
        const ok = await act(() =>
            updateAccount(account.id, {password}, `Set a new password for ${account.username}`),
        )
        if (ok) {
            setPassword('')
        }
    }

    return (
        <li class={`account ${account.disabled ? 'is-disabled' : ''}`}>
            <div class="account-main">
                <span class="account-name">{account.username}</span>
                {account.admin && <span class="tag admin">admin</span>}
                {account.disabled && <span class="tag disabled">disabled</span>}
                {isSelf && <span class="tag you">you</span>}
                <span style="flex:1" />
                <button class="ghost" onClick={toggleOpen} type="button">
                    {open ? 'Close' : 'Manage'}
                </button>
                <span class="account-meta">Created {formatTimestamp(account.created_at)}</span>
            </div>

            {open && (
                <div class="account-edit">
                    <label class="field">
                        <span>New password</span>
                        <input
                            autocomplete="new-password"
                            disabled={busy}
                            onInput={(e) => setPassword((e.target as HTMLInputElement).value)}
                            placeholder="At least 8 characters"
                            type="password"
                            value={password}
                        />
                    </label>
                    <div class="row-actions">
                        <button disabled={busy || password.length < 8} onClick={resetPassword} type="button">
                            Set password
                        </button>
                        <button
                            disabled={busy}
                            onClick={() =>
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
                        <button
                            disabled={busy}
                            onClick={() =>
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
                        {confirming ? (
                            <>
                                <button
                                    class="danger"
                                    disabled={busy}
                                    onClick={() => act(() => deleteAccount(account))}
                                    type="button"
                                >
                                    Really delete
                                </button>
                                <button class="ghost" disabled={busy} onClick={() => setConfirming(false)} type="button">
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
                                disabled={busy || isSelf}
                                onClick={() => setConfirming(true)}
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
                </div>
            )}
        </li>
    )
}
