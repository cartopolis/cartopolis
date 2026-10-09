import m, {MithrilComponent} from '@garage44/mithril'
import {Button, Card, FieldCheckbox, FieldText} from '@garage44/mithril/components'
import {minLength, required, validation, validations} from '@garage44/mithril/validation'

import {$s} from '@/app'
import {Account, MIN_PASSWORD} from '@/components/elements/account/account'
import {createAccount, visibleAccounts} from '@/lib/accounts'
import {bind, local} from '@/lib/reactive'

class NewAccount extends MithrilComponent {
    s = local({admin: false, busy: false, open: false, password: '', username: ''})
    // `required` trims, so a username of spaces is refused here as it was by the
    // old `trim().length > 0` check; the server has the rest of the rules.
    v = validations({
        password: validation(bind(this.s, 'password'), [required, minLength(MIN_PASSWORD)]),
        username: validation(bind(this.s, 'username'), [required]),
    })

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (!this.v.$validate() || s.busy) {
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
            this.v.$reset()
        }
    }

    view() {
        const s = this.s

        if (!s.open) {
            return (
                <Button
                    onclick={() => {
                        s.open = true
                    }}
                    variant="primary"
                >
                    New account
                </Button>
            )
        }

        return (
            <form class="accounts-new" onsubmit={this.submit}>
                <Card
                    footer={
                        <>
                            <Button disabled={!this.v.$valid} loading={s.busy} type="submit" variant="primary">
                                Create
                            </Button>
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    s.open = false
                                    this.v.$reset()
                                }}
                                variant="ghost"
                            >
                                Cancel
                            </Button>
                        </>
                    }
                    title="New account"
                >
                    <FieldText
                        autocomplete="off"
                        disabled={s.busy}
                        label="Username"
                        model={bind(s, 'username')}
                        validation={this.v.username}
                    />
                    <FieldText
                        autocomplete="new-password"
                        disabled={s.busy}
                        label="Password"
                        model={bind(s, 'password')}
                        placeholder={`At least ${MIN_PASSWORD} characters`}
                        type="password"
                        validation={this.v.password}
                    />
                    {/* Who is an admin is an admin's call; the server refuses it from
                        anybody a role merely lets manage accounts. */}
                    {$s.me.admin && (
                        <FieldCheckbox
                            disabled={s.busy}
                            label="Admin (every permission)"
                            model={bind(s, 'admin')}
                        />
                    )}
                </Card>
            </form>
        )
    }
}

export class Accounts extends MithrilComponent {
    view() {
        const rows = visibleAccounts()
        const total = $s.accounts.length

        return (
            <div class="accounts">
                <div class="accounts-head">
                    <span class="accounts-count">
                        {rows.length === total ? `${total}` : `${rows.length} of ${total}`}{' '}
                        {total === 1 ? 'account' : 'accounts'}
                    </span>
                    <NewAccount />
                </div>

                {total > 0 && (
                    <FieldText
                        label="Search"
                        model={bind($s, 'search')}
                        placeholder="Filter by username"
                        type="search"
                    />
                )}

                {$s.loading && !total ? (
                    <div class="empty">Loading…</div>
                ) : rows.length === 0 ? (
                    <div class="empty">
                        {total === 0 ? 'No accounts yet.' : `Nothing matches “${$s.search}”.`}
                    </div>
                ) : (
                    <ul class="record-list">
                        {rows.map((account) => (
                            <Account account={account} key={account.id} />
                        ))}
                    </ul>
                )}
            </div>
        )
    }
}
