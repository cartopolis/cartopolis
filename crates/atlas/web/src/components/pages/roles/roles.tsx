import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, Card, FieldCheckbox, FieldText} from '@garage44/mithril/components'
import {check, required, validation} from '@garage44/mithril/validation'

import type {PermissionInfo, Role} from '@/types'

import {$s} from '@/app'
import {bind, local} from '@/lib/reactive'
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

/*
 * Each role stays an open card rather than a row whose "Edit" opens the context
 * panel: the grid *is* the page, and comparing what two roles grant means seeing
 * both at once. Nothing here was an inline expansion to move.
 */
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
        // One box's model: whether the edit holds the permission.
        const box = (slug: string) => ({
            get value() {
                return held.includes(slug)
            },
            set value(on: boolean) {
                s.held = on ? [...held, slug] : held.filter((p) => p !== slug)
            },
        })

        return (
            <Card
                actions={
                    role.built_in ? (
                        <Badge>every account</Badge>
                    ) : (
                        <span class="role-members">
                            {role.members} {role.members === 1 ? 'account' : 'accounts'}
                        </span>
                    )
                }
                class="role"
                footer={
                    <>
                        <Button
                            disabled={s.busy || !changed}
                            onclick={() => this.act(() => saveRole(role, held))}
                            variant="primary"
                        >
                            Save
                        </Button>
                        <Button
                            disabled={s.busy || !changed}
                            onclick={() => {
                                s.held = role.permissions
                            }}
                            variant="ghost"
                        >
                            Undo changes
                        </Button>
                        <span class="role-spacer" />
                        {!role.built_in &&
                            (s.confirming ? (
                                <>
                                    <Button
                                        disabled={s.busy}
                                        onclick={() => this.act(() => deleteRole(role))}
                                        variant="danger"
                                    >
                                        Really delete
                                    </Button>
                                    <Button
                                        onclick={() => {
                                            s.confirming = false
                                        }}
                                        variant="ghost"
                                    >
                                        Cancel
                                    </Button>
                                </>
                            ) : (
                                <Button
                                    disabled={s.busy}
                                    onclick={() => {
                                        s.confirming = true
                                    }}
                                    variant="danger"
                                >
                                    Delete role
                                </Button>
                            ))}
                    </>
                }
                title={role.name}
            >
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
                                    <FieldCheckbox
                                        disabled={s.busy}
                                        key={p.slug}
                                        label={
                                            <span title={p.slug}>
                                                {p.label}
                                                {WARNINGS[p.slug] && held.includes(p.slug) && (
                                                    <span class="role-warning"> — {WARNINGS[p.slug]}</span>
                                                )}
                                            </span>
                                        }
                                        model={box(p.slug)}
                                    />
                                ))}
                        </fieldset>
                    ))}
                </div>
            </Card>
        )
    }
}

class NewRole extends MithrilComponent {
    s = local({busy: false, name: '', open: false})
    // The rule the old disabled-button check was: the id the name turns into
    // must be at least two characters, which is what the server stores it as.
    name = validation(bind(this.s, 'name'), [
        required,
        check((v: string) => roleId(v).length >= 2, 'Use at least two letters or digits'),
    ])

    submit = async (e: Event) => {
        e.preventDefault()
        const s = this.s
        if (!this.name.touch() || s.busy) {
            return
        }
        s.busy = true
        const ok = await createRole(roleId(s.name), s.name.trim(), [])
        s.busy = false
        if (ok) {
            s.name = ''
            s.open = false
            this.name.reset()
        }
    }

    view() {
        const s = this.s
        const id = roleId(s.name)

        if (!s.open) {
            return (
                <Button
                    onclick={() => {
                        s.open = true
                    }}
                    variant="primary"
                >
                    New role
                </Button>
            )
        }
        return (
            <form class="roles-new" onsubmit={this.submit}>
                <Card
                    footer={
                        <>
                            <Button disabled={!this.name.valid} loading={s.busy} type="submit" variant="primary">
                                Create
                            </Button>
                            <Button
                                disabled={s.busy}
                                onclick={() => {
                                    s.open = false
                                    this.name.reset()
                                }}
                                variant="ghost"
                            >
                                Cancel
                            </Button>
                        </>
                    }
                    title="New role"
                >
                    <FieldText
                        disabled={s.busy}
                        help={id ? `Stored as “${id}”. It starts with no permissions; tick them below after.` : undefined}
                        label="Name"
                        model={bind(s, 'name')}
                        placeholder="Builder, Moderator, …"
                        validation={this.name}
                    />
                </Card>
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
            <div class="roles">
                <div class="roles-head">
                    <span class="roles-count">
                        {$s.roles.length} {$s.roles.length === 1 ? 'role' : 'roles'}
                    </span>
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
                    <div class="role-list">
                        {$s.roles.map((role) => (
                            <RoleCard key={role.id} role={role} />
                        ))}
                    </div>
                )}
            </div>
        )
    }
}
