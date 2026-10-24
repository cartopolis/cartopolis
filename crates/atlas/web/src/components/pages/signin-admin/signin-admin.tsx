import m, {MithrilComponent} from '@garage44/mithril'
import {Alert, Badge, Button, FieldSegmented, FieldSwitch, FieldText} from '@garage44/mithril/components'

import type {AdminProvider, AdminSignin, NewAccounts} from '@/types'

import {ProviderLogo} from '@/components/elements/providers/providers'
import {begin, deleteProvider, loadAdminSignin, orderProviders, type ProviderChange, saveProvider} from '@/lib/auth'
import {ADMIN} from '@/lib/base'
import {Icon} from '@/lib/icons'
import {bind, local} from '@/lib/reactive'
import {
    customId,
    hasKeys,
    mayEnable,
    moved,
    newAccountChoices,
    providerLine,
    providerStatus,
    setupSteps,
} from '@/lib/signin'

/** The open row's form, filled from the provider when it opens. */
interface Draft {
    client_id: string
    /** Empty means "keep the stored one": a secret is never sent back to be shown. */
    client_secret: string
    issuer: string
    name: string
    new_accounts: NewAccounts
}

/**
 * Admin › People › Sign-in (`docs/mockups/external-sign-in/v3-communities.html`,
 * sections 2 and 3): which providers people may sign in with, in what order,
 * and each one's keys.
 *
 * The rule the page is built around: a provider says "working" only after a
 * test came back with the operator's own account, and its switch stays grey
 * until then. A wrong redirect address looks exactly like a right one until a
 * member hits the provider's error page, and the test is the only thing that
 * catches it first. The server enforces it too (a 409 on enabling untested).
 */
export class SigninAdmin extends MithrilComponent {
    s = local({
        adding: false,
        busy: false,
        data: null as AdminSignin | null,
        draft: {client_id: '', client_secret: '', issuer: '', name: '', new_accounts: 'anyone'} as Draft,
        newIssuer: '',
        newName: '',
        open: '',
    })

    load = async () => {
        const data = await loadAdminSignin()
        if (data) this.s.data = {...data, providers: [...data.providers].sort((a, b) => a.position - b.position)}
    }

    oninit() {
        void this.load()
    }

    toggleOpen(p: AdminProvider) {
        const s = this.s
        if (s.open === p.id) {
            s.open = ''
            return
        }
        s.open = p.id
        s.draft = {
            client_id: p.client_id,
            client_secret: '',
            issuer: p.issuer ?? '',
            name: p.name,
            new_accounts: p.new_accounts,
        }
    }

    /** Only what changed: new keys or a new issuer switch a provider off and untest it. */
    change(p: AdminProvider): ProviderChange {
        const d = this.s.draft
        const out: ProviderChange = {}
        if (d.new_accounts !== p.new_accounts) out.new_accounts = d.new_accounts
        if (!p.needs_keys) return out
        if (d.client_id.trim() !== p.client_id) out.client_id = d.client_id.trim()
        if (d.client_secret) out.client_secret = d.client_secret
        if (p.custom && d.issuer.trim() !== (p.issuer ?? '')) out.issuer = d.issuer.trim()
        if (p.custom && d.name.trim() && d.name.trim() !== p.name) out.name = d.name.trim()
        return out
    }

    save = async (p: AdminProvider, thenTest: boolean) => {
        const s = this.s
        s.busy = true
        const change = this.change(p)
        const saved = Object.keys(change).length === 0 || (await saveProvider(p.id, change, thenTest))
        if (saved && thenTest) {
            // A real round trip with the operator's own account; `/welcome`
            // says how it went and links back here.
            if (await begin(p.id, 'test', {return: ADMIN})) return
        }
        s.busy = false
        if (saved) {
            s.draft.client_secret = ''
            await this.load()
        }
    }

    setEnabled = async (p: AdminProvider, enabled: boolean) => {
        this.s.busy = true
        await saveProvider(p.id, {enabled}, true)
        await this.load()
        this.s.busy = false
    }

    move = async (p: AdminProvider, by: -1 | 1) => {
        const s = this.s
        if (!s.data) return
        const ids = moved(s.data.providers.map((x) => x.id), p.id, by)
        s.busy = true
        if (await orderProviders(ids)) await this.load()
        s.busy = false
    }

    remove = async (p: AdminProvider) => {
        const what = p.custom ? `Remove ${p.name}?` : `Clear the keys for ${p.name} and switch it off?`
        if (!confirm(`${what} Accounts that sign in only with it will need another way in.`)) return
        this.s.busy = true
        if (await deleteProvider(p)) {
            this.s.open = ''
            await this.load()
        }
        this.s.busy = false
    }

    addCustom = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        const id = customId(s.newName)
        if (!id || !s.newIssuer.trim() || s.busy) return
        s.busy = true
        if (await saveProvider(id, {issuer: s.newIssuer.trim(), name: s.newName.trim()}, true)) {
            s.adding = false
            s.newName = ''
            s.newIssuer = ''
            await this.load()
            const added = s.data?.providers.find((p) => p.id === id)
            if (added) this.toggleOpen(added)
        }
        s.busy = false
    }

    view() {
        const s = this.s
        const data = s.data
        if (!data) return <p class="signin-admin__hint">Loading…</p>
        return (
            <div class="signin-admin">
                {!data.key_ok && (
                    <Alert tone="warning">
                        Keys can't be saved yet: the server has nothing to lock them with. Set CARTO_SIGNIN_KEY,
                        or let the server make its signin.key file, and restart it.
                    </Alert>
                )}
                <div class="signin-admin__list">
                    {data.providers.map((p, i) => this.row(p, i, data))}
                </div>
                {s.adding ? (
                    <form class="signin-admin__add" onsubmit={this.addCustom}>
                        <FieldText disabled={s.busy} label="Name" model={bind(s, 'newName')} placeholder="Garage44 Forgejo" />
                        <FieldText
                            disabled={s.busy}
                            help="The address its OpenID configuration lives under."
                            label="Issuer"
                            model={bind(s, 'newIssuer')}
                            placeholder="https://code.garage44.eu"
                        />
                        <div class="signin-admin__actions">
                            <Button disabled={s.busy} onclick={() => (s.adding = false)} size="sm" variant="ghost">
                                Cancel
                            </Button>
                            <Button
                                disabled={!customId(s.newName) || !s.newIssuer.trim()}
                                loading={s.busy}
                                size="sm"
                                type="submit"
                                variant="primary"
                            >
                                Add
                            </Button>
                        </div>
                    </form>
                ) : (
                    <Button icon={Icon('add')} onclick={() => (s.adding = true)} size="sm" variant="ghost">
                        Another OpenID server (a community's own Keycloak, Nextcloud, Forgejo…)
                    </Button>
                )}
                <Alert tone="info">
                    Switching a provider off stops new sign-ins with it. Accounts that also have a password or
                    another way in keep working.
                </Alert>
            </div>
        )
    }

    row(p: AdminProvider, i: number, data: AdminSignin) {
        const s = this.s
        const status = providerStatus(p)
        const open = s.open === p.id
        const allowed = mayEnable(p)
        const setEnabled = this.setEnabled
        return (
            <div class={open ? 'signin-admin__row is-open' : 'signin-admin__row'} key={p.id}>
                <div class="signin-admin__head">
                    <span class="signin-admin__order">
                        <Button
                            aria-label={`Move ${p.name} up`}
                            disabled={s.busy || i === 0}
                            icon={Icon('keyboard_arrow_up')}
                            iconOnly
                            onclick={() => void this.move(p, -1)}
                            size="sm"
                            title="Move up"
                            variant="ghost"
                        />
                        <Button
                            aria-label={`Move ${p.name} down`}
                            disabled={s.busy || i === data.providers.length - 1}
                            icon={Icon('keyboard_arrow_down')}
                            iconOnly
                            onclick={() => void this.move(p, 1)}
                            size="sm"
                            title="Move down"
                            variant="ghost"
                        />
                    </span>
                    {ProviderLogo(p, 22)}
                    <button class="signin-admin__name" onclick={() => this.toggleOpen(p)} type="button">
                        <b>{p.name}</b>
                        <small>{providerLine(p)}</small>
                    </button>
                    {status ? (
                        <Badge tone={status.tone}>{status.label}</Badge>
                    ) : (
                        <Button onclick={() => this.toggleOpen(p)} size="sm">
                            Set up
                        </Button>
                    )}
                    <FieldSwitch
                        disabled={s.busy || (!allowed && !p.enabled)}
                        label={<span class="signin-admin__switch">{p.enabled ? 'On' : 'Off'}</span>}
                        model={{
                            get value() {
                                return p.enabled
                            },
                            set value(next: boolean) {
                                void setEnabled(p, next)
                            },
                        }}
                    />
                </div>
                {open && this.setup(p, data)}
            </div>
        )
    }

    setup(p: AdminProvider, data: AdminSignin) {
        const s = this.s
        const d = s.draft
        const change = this.change(p)
        const dirty = Object.keys(change).length > 0
        const keysChanging = 'client_id' in change || 'client_secret' in change || 'issuer' in change
        const ready = p.needs_keys ? !!d.client_id.trim() && (p.secret_set || !!d.client_secret) : true
        return (
            <div class="signin-admin__setup">
                {p.needs_keys ? (
                    <>
                        <ol class="signin-admin__steps">
                            {setupSteps(p).map((step, i) => (
                                <li key={i}>
                                    {step}
                                    {i === 0 && p.setup_url && (
                                        <>
                                            {' '}
                                            <a href={p.setup_url} rel="noopener noreferrer" target="_blank">
                                                Open it
                                            </a>
                                        </>
                                    )}
                                </li>
                            ))}
                        </ol>
                        <div class="signin-admin__redirect">
                            <FieldText
                                disabled
                                label="Redirect address"
                                model={{value: p.redirect_url}}
                            />
                            <Button
                                onclick={() => void navigator.clipboard?.writeText(p.redirect_url)}
                                size="sm"
                                variant="ghost"
                            >
                                Copy
                            </Button>
                        </div>
                        {p.custom && <FieldText disabled={s.busy} label="Name" model={bind(d, 'name')} />}
                        {p.custom && <FieldText disabled={s.busy} label="Issuer" model={bind(d, 'issuer')} />}
                        <FieldText
                            autocomplete="off"
                            disabled={s.busy || !data.key_ok}
                            label="Client ID"
                            model={bind(d, 'client_id')}
                        />
                        <FieldText
                            autocomplete="new-password"
                            disabled={s.busy || !data.key_ok}
                            help={p.secret_set ? 'Leave it empty to keep the one that is set.' : undefined}
                            label="Client secret"
                            model={bind(d, 'client_secret')}
                            placeholder={p.secret_set ? 'set · replace' : ''}
                            type="password"
                        />
                    </>
                ) : (
                    <p class="signin-admin__hint">
                        Nothing to set up: the server registers itself with each fediverse server the first time
                        somebody from it signs in. Servers on the federation blocklist are refused.
                    </p>
                )}
                <FieldSegmented
                    disabled={s.busy}
                    label="New accounts"
                    model={bind(d, 'new_accounts')}
                    options={newAccountChoices(p.kind)}
                    size="sm"
                />
                {d.new_accounts === 'none' && (
                    <p class="signin-admin__hint">Only accounts that already linked {p.name} may sign in with it.</p>
                )}
                {keysChanging && p.tested_at && (
                    <p class="signin-admin__hint">New keys switch {p.name} off until it is tested again.</p>
                )}
                <div class="signin-admin__actions">
                    {(p.custom || hasKeys(p)) && p.needs_keys && (
                        <Button disabled={s.busy} onclick={() => void this.remove(p)} size="sm" variant="danger">
                            {p.custom ? 'Remove' : 'Clear keys'}
                        </Button>
                    )}
                    <span class="spacer" />
                    <Button disabled={s.busy} onclick={() => (s.open = '')} size="sm" variant="ghost">
                        Cancel
                    </Button>
                    <Button disabled={s.busy || !dirty} onclick={() => void this.save(p, false)} size="sm">
                        Save
                    </Button>
                    {p.needs_keys && (
                        <Button
                            disabled={s.busy || !ready || !data.key_ok}
                            onclick={() => void this.save(p, true)}
                            size="sm"
                            variant="primary"
                        >
                            Save and test with my {p.name}
                        </Button>
                    )}
                </div>
            </div>
        )
    }
}
