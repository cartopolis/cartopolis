import type {AdminTab, Me, PermissionInfo, Role} from '@/types'

import {$s, api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Roles and permissions (`docs/notes/permissions.md`): what the signed-in
 * operator may do, the catalogue the server enforces, the roles, and who holds
 * them. The server decides every one of these; the web UI only draws them, so
 * a tab hidden here is a convenience and never the protection.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        if (!e.isUnauthorized) {
            notifier.error(e.message)
        }
        return
    }
    notifier.error(fallback)
}

/** Whether the signed-in operator holds a permission. An admin holds all. */
export function may(slug: string): boolean {
    return $s.me.admin || $s.me.permissions.includes(slug)
}

/** Which tab each permission opens, in the order the tabs are drawn. */
export const TABS: {tab: AdminTab; label: string; needs: string[]}[] = [
    {label: 'Accounts', needs: ['accounts_manage'], tab: 'accounts'},
    {label: 'Roles', needs: ['roles_manage'], tab: 'roles'},
    {label: 'Buildings', needs: ['reports_triage'], tab: 'claims'},
    {label: 'Models', needs: ['library_manage'], tab: 'library'},
    {label: 'Data', needs: ['pipeline_run'], tab: 'pipeline'},
    {label: 'Autopilot', needs: ['pipeline_run'], tab: 'autopilot'},
]

/** The tabs this operator may open. */
export function visibleTabs() {
    return TABS.filter((t) => t.needs.some(may))
}

/** Remember what the server says this operator may do, and move off a tab it no
 * longer allows. */
export function applyMe(me: Me) {
    $s.me = {admin: me.admin, permissions: me.permissions ?? []}
    const tabs = visibleTabs()
    if (tabs.length && !tabs.some((t) => t.tab === $s.adminTab)) {
        $s.adminTab = tabs[0].tab
    }
}

export async function loadMe(): Promise<void> {
    try {
        applyMe(await api.get<Me>('/api/admin/me'))
    } catch (e) {
        report(e, 'Could not ask the server what this account may do')
    }
}

export async function loadRoles(): Promise<void> {
    try {
        const [catalogue, roles] = await Promise.all([
            api.get<{permissions: PermissionInfo[]}>('/api/admin/permissions'),
            api.get<{roles: Role[]}>('/api/admin/roles'),
        ])
        $s.permissionCatalogue = catalogue.permissions
        $s.roles = roles.roles
    } catch (e) {
        report(e, 'Could not load the roles')
    }
}

export async function createRole(id: string, name: string, permissions: string[]): Promise<boolean> {
    try {
        const result = await api.post<{roles: Role[]}>('/api/admin/roles', {id, name, permissions})
        $s.roles = result.roles
        notifier.success(`Created the ${name} role`)
        return true
    } catch (e) {
        report(e, 'Could not create that role')
        return false
    }
}

export async function saveRole(role: Role, permissions: string[], name?: string): Promise<boolean> {
    try {
        const result = await api.patch<{roles: Role[]}>(`/api/admin/roles/${role.id}`, {
            name: name ?? role.name,
            permissions,
        })
        $s.roles = result.roles
        notifier.success(`Saved ${name ?? role.name}`)
        return true
    } catch (e) {
        report(e, 'Could not save that role')
        return false
    }
}

export async function deleteRole(role: Role): Promise<boolean> {
    try {
        const result = await api.delete<{roles: Role[]}>(`/api/admin/roles/${role.id}`)
        $s.roles = result.roles
        notifier.success(`Deleted the ${role.name} role`)
        return true
    } catch (e) {
        report(e, 'Could not delete that role')
        return false
    }
}

export async function setAccountRoles(accountId: string, username: string, roles: string[]): Promise<boolean> {
    try {
        const result = await api.put<{roles: string[]}>(`/api/admin/accounts/${accountId}/roles`, {roles})
        $s.accountRoles = [
            ...$s.accountRoles.filter((a) => a.id !== accountId),
            {id: accountId, roles: result.roles},
        ]
        notifier.success(`Saved ${username}'s roles`)
        return true
    } catch (e) {
        report(e, 'Could not save those roles')
        return false
    }
}

/** The roles an account holds explicitly. */
export function rolesOf(accountId: string): string[] {
    return $s.accountRoles.find((a) => a.id === accountId)?.roles ?? []
}

/** A role id from a name: lowercase, hyphens, as the server requires. */
export function roleId(name: string): string {
    return name
        .toLowerCase()
        .trim()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
        .slice(0, 32)
}
