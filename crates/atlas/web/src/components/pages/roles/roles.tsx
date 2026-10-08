import m, {MithrilComponent} from '@garage44/mithril'

import type {PermissionInfo, Role} from '@/types'

import {$s} from '@/app'
import {local} from '@/lib/reactive'
import {createRole, deleteRole, loadRoles, roleId, saveRole} from '@/lib/roles'

const GROUPS: {group: PermissionInfo['group']; label: string}[] = [
    {group: 'prims', label: 'Prims'},
    {group: 'scripts', label: 'Scripts'},
    {group: 'buildings', label: 'Buildings'},
    {group: 'server', label: 'Running the server'},
]

/**
 * Two permissions are trust, not features: whoever may manage roles can grant
 * themselves anything, and whoever may manage accounts can lock other people
 * out. The grid says so next to the box rather than in a manual.
 */
const WARNINGS: Record<string, string> = {
    accounts_manage: 'Can disable and delete accounts (but not admins).',
    roles_manage: 'Can give any role any permission — including to themselves. Treat as an admin.',
}

interface RoleCardAttrs {
    role: Role
}

class RoleCard extends MithrilComponent<RoleCardAttrs> {
    s: {busy: boolean; confirming: boolean; held: string[]}
    /** The `role.permissions` the last `sync` followed. */
    private seen: string

    constructor(vnode: m.Vnode<RoleCardAttrs>) {
        super()
        this.s = local({busy: false, confirming: false, held: vnode.attrs.role.permissions})
        this.seen = vnode.attrs.role.permissions.join(',')
    }

    // A save elsewhere (another tab, the server's answer) replaces the role;
    // follow it rather than keep showing the edit that was saved.
    private sync(role: Role) {
        const key = role.permissions.join(',')
        if (key === this.seen) {
            return
        }
        this.seen = key
        this.s.held = role.permissions
    }

    onupdate(vnode: m.Vnode<RoleCardAttrs>) {
        this.sync(vnode.attrs.role)
    }

    act = async (fn: () => Promise<boolean>) => {
        this.s.busy = true
        await fn()
        this.s.busy = false
    }

    view(vnode: m.Vnode<RoleCardAttrs>) {
        const {role} = vnode.attrs
        const s = this.s
        const held = s.held

        const changed = held.slice().sort().join(',') !== role.permissions.slice().sort().join(',')
        const toggle = (slug: string) => {
            s.held = held.includes(slug) ? held.filter((p) => p !== slug) : [...held, slug]
        }

        return (
            <li class="account role">
                <div class="account-main">
                    <span class="account-name">{role.name}</span>
                    {role.built_in && <span class="tag">every account</span>}
                    {!role.built_in && (
                        <span class="account-meta">
                            {role.members} {role.members === 1 ? 'account' : 'accounts'}
                        </span>
                    )}
                </div>
                {role.built_in && (
                    <p class="hint">
                        Every signed-in account holds this role. Taking a permission away here takes it away from
                        everybody who is not given it by another role.
                    </p>
                )}
                <div class="role-grid">
                    {GROUPS.map(({group, label}) => (
                        <fieldset class="role-group" key={group}>
                            <legend>{label}</legend>
                            {$s.permissionCatalogue
                                .filter((p) => p.group === group)
                                .map((p) => (
                                    <label class="check" key={p.slug} title={p.slug}>
                                        <input
                                            checked={held.includes(p.slug)}
                                            disabled={s.busy}
                                            onchange={() => toggle(p.slug)}
                                            type="checkbox"
                                        />
                                        <span>
                                            {p.label}
                                            {WARNINGS[p.slug] && held.includes(p.slug) && (
                                                <span class="warning"> — {WARNINGS[p.slug]}</span>
                                            )}
                                        </span>
                                    </label>
                                ))}
                        </fieldset>
                    ))}
                </div>
                <div class="row-actions">
                    <button
                        class="primary"
                        disabled={s.busy || !changed}
                        onclick={() => this.act(() => saveRole(role, held))}
                        type="button"
                    >
                        Save
                    </button>
                    <button
                        class="ghost"
                        disabled={s.busy || !changed}
                        onclick={() => {
                            s.held = role.permissions
                        }}
                        type="button"
                    >
                        Undo changes
                    </button>
                    {!role.built_in &&
                        (s.confirming ? (
                            <>
                                <button
                                    class="danger"
                                    disabled={s.busy}
                                    onclick={() => this.act(() => deleteRole(role))}
                                    type="button"
                                >
                                    Really delete
                                </button>
                                <button
                                    class="ghost"
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
                                disabled={s.busy}
                                onclick={() => {
                                    s.confirming = true
                                }}
                                type="button"
                            >
                                Delete role
                            </button>
                        ))}
                </div>
            </li>
        )
    }
}

class NewRole extends MithrilComponent {
    s = local({busy: false, name: '', open: false})

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        const id = roleId(s.name)
        if (id.length < 2 || s.busy) {
            return
        }
        s.busy = true
        const ok = await createRole(id, s.name.trim(), [])
        s.busy = false
        if (ok) {
            s.name = ''
            s.open = false
        }
    }

    view() {
        const s = this.s
        const id = roleId(s.name)

        if (!s.open) {
            return (
                <button
                    class="primary"
                    onclick={() => {
                        s.open = true
                    }}
                    type="button"
                >
                    New role
                </button>
            )
        }
        return (
            <form class="card" onsubmit={this.submit} style="margin-bottom:16px">
                <h2>New role</h2>
                <label class="field">
                    <span>Name</span>
                    <input
                        disabled={s.busy}
                        oninput={(e: Event) => {
                            s.name = (e.target as HTMLInputElement).value
                        }}
                        placeholder="Builder, Moderator, …"
                        value={s.name}
                    />
                </label>
                {id && <p class="hint">Stored as “{id}”. It starts with no permissions; tick them below after.</p>}
                <div class="row-actions">
                    <button class="primary" disabled={s.busy || id.length < 2} type="submit">
                        Create
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

/**
 * The roles page (`docs/notes/permissions.md`). Admins are not a role — they
 * hold everything, and are made on the Accounts page.
 */
export class Roles extends MithrilComponent {
    oninit() {
        void loadRoles()
    }

    view() {
        return (
            <>
                <div class="section-head">
                    <h1>Roles</h1>
                    <span class="count">{$s.roles.length}</span>
                    <span style="flex:1" />
                    <NewRole />
                </div>
                <p class="hint">
                    A role is a set of permissions. Every account holds Player; give an account more roles on the
                    Accounts page. Admins hold every permission and are not listed here. A change applies to an
                    account's next sign-in in the game, and to its next request everywhere else.
                </p>
                {!$s.permissionCatalogue.length ? (
                    <div class="empty">Loading…</div>
                ) : (
                    <ul class="account-list">
                        {$s.roles.map((role) => (
                            <RoleCard key={role.id} role={role} />
                        ))}
                    </ul>
                )}
            </>
        )
    }
}
