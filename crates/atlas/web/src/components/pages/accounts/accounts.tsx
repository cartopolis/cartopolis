import {useState} from 'preact/hooks'

import {$s} from '@/app'
import {Account} from '@/components/elements/account/account'
import {createAccount, visibleAccounts} from '@/lib/accounts'

const MIN_PASSWORD = 8

const NewAccount = () => {
    const [open, setOpen] = useState(false)
    const [username, setUsername] = useState('')
    const [password, setPassword] = useState('')
    const [admin, setAdmin] = useState(false)
    const [busy, setBusy] = useState(false)

    const valid = username.trim().length > 0 && password.length >= MIN_PASSWORD

    const submit = async (e: Event) => {
        e.preventDefault()
        if (!valid || busy) {
            return
        }
        setBusy(true)
        const ok = await createAccount(username.trim(), password, admin)
        setBusy(false)
        if (ok) {
            setUsername('')
            setPassword('')
            setAdmin(false)
            setOpen(false)
        }
    }

    if (!open) {
        return (
            <button class="primary" onClick={() => setOpen(true)} type="button">
                New account
            </button>
        )
    }

    return (
        <form class="card" onSubmit={submit} style="margin-bottom:16px">
            <h2>New account</h2>
            <label class="field">
                <span>Username</span>
                <input
                    autocomplete="off"
                    disabled={busy}
                    onInput={(e) => setUsername((e.target as HTMLInputElement).value)}
                    type="text"
                    value={username}
                />
            </label>
            <label class="field">
                <span>Password</span>
                <input
                    autocomplete="new-password"
                    disabled={busy}
                    onInput={(e) => setPassword((e.target as HTMLInputElement).value)}
                    placeholder={`At least ${MIN_PASSWORD} characters`}
                    type="password"
                    value={password}
                />
            </label>
            {/* Who is an admin is an admin's call; the server refuses it from
                anybody a role merely lets manage accounts. */}
            {$s.me.admin && (
                <label class="check">
                    <input
                        checked={admin}
                        disabled={busy}
                        onChange={(e) => setAdmin((e.target as HTMLInputElement).checked)}
                        type="checkbox"
                    />
                    Admin (every permission)
                </label>
            )}
            <div class="row-actions" style="margin-top:14px">
                <button class="primary" disabled={!valid || busy} type="submit">
                    {busy ? 'Creating…' : 'Create'}
                </button>
                <button class="ghost" disabled={busy} onClick={() => setOpen(false)} type="button">
                    Cancel
                </button>
            </div>
        </form>
    )
}

export const Accounts = () => {
    const rows = visibleAccounts()
    const total = $s.accounts.length

    return (
        <>
            <div class="section-head">
                <h1>Accounts</h1>
                <span class="count">
                    {rows.length === total ? `${total}` : `${rows.length} of ${total}`}
                </span>
                <span style="flex:1" />
                <NewAccount />
            </div>

            {total > 0 && (
                <label class="field">
                    <span>Search</span>
                    <input
                        onInput={(e) => {
                            $s.search = (e.target as HTMLInputElement).value
                        }}
                        placeholder="Filter by username"
                        type="search"
                        value={$s.search}
                    />
                </label>
            )}

            {$s.loading && !total ? (
                <div class="empty">Loading…</div>
            ) : rows.length === 0 ? (
                <div class="empty">
                    {total === 0 ? 'No accounts yet.' : `Nothing matches “${$s.search}”.`}
                </div>
            ) : (
                <ul class="account-list">
                    {rows.map((account) => (
                        <Account account={account} key={account.id} />
                    ))}
                </ul>
            )}
        </>
    )
}
