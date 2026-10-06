import {useEffect, useState} from 'preact/hooks'

import type {PermissionInfo, Role} from '@/types'

import {$s} from '@/app'
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

const RoleCard = ({role}: {role: Role}) => {
    const [held, setHeld] = useState<string[]>(role.permissions)
    const [busy, setBusy] = useState(false)
    const [confirming, setConfirming] = useState(false)

    // A save elsewhere (another tab, the server's answer) replaces the role;
    // follow it rather than keep showing the edit that was saved.
    useEffect(() => setHeld(role.permissions), [role.permissions.join(',')])

    const changed = held.slice().sort().join(',') !== role.permissions.slice().sort().join(',')
    const toggle = (slug: string) =>
        setHeld(held.includes(slug) ? held.filter((p) => p !== slug) : [...held, slug])

    const act = async (fn: () => Promise<boolean>) => {
        setBusy(true)
        await fn()
        setBusy(false)
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
                                        disabled={busy}
                                        onChange={() => toggle(p.slug)}
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
                    disabled={busy || !changed}
                    onClick={() => act(() => saveRole(role, held))}
                    type="button"
                >
                    Save
                </button>
                <button
                    class="ghost"
                    disabled={busy || !changed}
                    onClick={() => setHeld(role.permissions)}
                    type="button"
                >
                    Undo changes
                </button>
                {!role.built_in &&
                    (confirming ? (
                        <>
                            <button
                                class="danger"
                                disabled={busy}
                                onClick={() => act(() => deleteRole(role))}
                                type="button"
                            >
                                Really delete
                            </button>
                            <button class="ghost" onClick={() => setConfirming(false)} type="button">
                                Cancel
                            </button>
                        </>
                    ) : (
                        <button class="danger" disabled={busy} onClick={() => setConfirming(true)} type="button">
                            Delete role
                        </button>
                    ))}
            </div>
        </li>
    )
}

const NewRole = () => {
    const [open, setOpen] = useState(false)
    const [name, setName] = useState('')
    const [busy, setBusy] = useState(false)
    const id = roleId(name)

    const submit = async (e: Event) => {
        e.preventDefault()
        if (id.length < 2 || busy) {
            return
        }
        setBusy(true)
        const ok = await createRole(id, name.trim(), [])
        setBusy(false)
        if (ok) {
            setName('')
            setOpen(false)
        }
    }

    if (!open) {
        return (
            <button class="primary" onClick={() => setOpen(true)} type="button">
                New role
            </button>
        )
    }
    return (
        <form class="card" onSubmit={submit} style="margin-bottom:16px">
            <h2>New role</h2>
            <label class="field">
                <span>Name</span>
                <input
                    disabled={busy}
                    onInput={(e) => setName((e.target as HTMLInputElement).value)}
                    placeholder="Builder, Moderator, …"
                    value={name}
                />
            </label>
            {id && <p class="hint">Stored as “{id}”. It starts with no permissions; tick them below after.</p>}
            <div class="row-actions">
                <button class="primary" disabled={busy || id.length < 2} type="submit">
                    Create
                </button>
                <button class="ghost" disabled={busy} onClick={() => setOpen(false)} type="button">
                    Cancel
                </button>
            </div>
        </form>
    )
}

/**
 * The roles page (`docs/notes/permissions.md`). Admins are not a role — they
 * hold everything, and are made on the Accounts page.
 */
export const Roles = () => {
    useEffect(() => {
        void loadRoles()
    }, [])

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
