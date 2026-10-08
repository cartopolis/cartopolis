import m, {MithrilComponent} from '@garage44/mithril'

import {$s} from '@/app'
import {Account} from '@/components/elements/account/account'
import {createAccount, visibleAccounts} from '@/lib/accounts'
import {local} from '@/lib/reactive'

const MIN_PASSWORD = 8

class NewAccount extends MithrilComponent {
    s = local({admin: false, busy: false, open: false, password: '', username: ''})

    get valid() {
        return this.s.username.trim().length > 0 && this.s.password.length >= MIN_PASSWORD
    }

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (!this.valid || s.busy) {
            return
        }
        s.busy = true
        const ok = await createAccount(s.username.trim(), s.password, s.admin)
        s.busy = false
        if (ok) {
            s.username = ''
            s.password = ''
            s.admin = false
            s.open = false
        }
    }

    view() {
        const s = this.s
        const valid = this.valid

        if (!s.open) {
            return (
                <button
                    class="primary"
                    onclick={() => {
                        s.open = true
                    }}
                    type="button"
                >
                    New account
                </button>
            )
        }

        return (
            <form class="card" onsubmit={this.submit} style="margin-bottom:16px">
                <h2>New account</h2>
                <label class="field">
                    <span>Username</span>
                    <input
                        autocomplete="off"
                        disabled={s.busy}
                        oninput={(e: Event) => {
                            s.username = (e.target as HTMLInputElement).value
                        }}
                        type="text"
                        value={s.username}
                    />
                </label>
                <label class="field">
                    <span>Password</span>
                    <input
                        autocomplete="new-password"
                        disabled={s.busy}
                        oninput={(e: Event) => {
                            s.password = (e.target as HTMLInputElement).value
                        }}
                        placeholder={`At least ${MIN_PASSWORD} characters`}
                        type="password"
                        value={s.password}
                    />
                </label>
                {/* Who is an admin is an admin's call; the server refuses it from
                    anybody a role merely lets manage accounts. */}
                {$s.me.admin && (
                    <label class="check">
                        <input
                            checked={s.admin}
                            disabled={s.busy}
                            onchange={(e: Event) => {
                                s.admin = (e.target as HTMLInputElement).checked
                            }}
                            type="checkbox"
                        />
                        Admin (every permission)
                    </label>
                )}
                <div class="row-actions" style="margin-top:14px">
                    <button class="primary" disabled={!valid || s.busy} type="submit">
                        {s.busy ? 'Creating…' : 'Create'}
                    </button>
                    <button
                        class="ghost"
                        disabled={s.busy}
                        onclick={() => {
                            s.open = false
                        }}
                        type="button"
                    >
                        Cancel
                    </button>
                </div>
            </form>
        )
    }
}

export class Accounts extends MithrilComponent {
    view() {
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
                            oninput={(e: Event) => {
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
}
