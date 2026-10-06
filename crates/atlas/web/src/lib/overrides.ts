import type {BuildingOverride, OverridePlacement, OverrideSummary} from '@/types'

import {$s, api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Building overrides (`docs/notes/building-models.md`): a model, or a generated
 * stadium, standing where the 3DBAG shell of one building used to. Atlas keeps
 * one folder per building and republishes the cell on every save; deleting the
 * override brings the surveyed building back.
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

/** A BAG pand identificatie (sixteen digits); the same test atlas makes. */
export function validPand(pand: string): boolean {
    return /^\d{4,20}$/.test(pand)
}

/** Accepts the bare number or the full `NL.IMBAG.Pand.…` form people copy. */
export function pandOf(text: string): string {
    return text.trim().replace(/^NL\.IMBAG\.Pand\./i, '')
}

export interface OverrideEdit {
    collision?: BuildingOverride['collision']
    interior?: BuildingOverride['interior']
    name?: string
    placement?: OverridePlacement
}

function replace(o: BuildingOverride) {
    const list = ($s.overrides ?? []).filter((x) => x.pand !== o.pand)
    $s.overrides = [...list, o].sort((a, b) => a.name.localeCompare(b.name))
}

function describe(s: OverrideSummary): string {
    return `${s.elements} parts, ${s.triangles.toLocaleString()} triangles in cell ${s.cell[0]}/${s.cell[1]}`
}

export async function loadOverrides(): Promise<void> {
    try {
        const result = await api.get<{overrides: BuildingOverride[]}>('/api/pipeline/overrides')
        $s.overrides = result.overrides.sort((a, b) => a.name.localeCompare(b.name))
    } catch (e) {
        $s.overrides = []
        report(e, 'Could not load the building overrides')
    }
}

/** Upload a GLB for a building: creates the override, or replaces its model. */
export async function uploadModel(pand: string, file: File): Promise<boolean> {
    try {
        const bytes = await file.arrayBuffer()
        const result = await api.upload<{override: BuildingOverride; summary: OverrideSummary}>(
            `/api/pipeline/overrides/${pand}/model`,
            bytes,
            'model/gltf-binary',
        )
        replace(result.override)
        notifier.success(`Published ${result.override.name}: ${describe(result.summary)}`)
        return true
    } catch (e) {
        report(e, 'Could not upload that model')
        return false
    }
}

export async function saveOverride(pand: string, edit: OverrideEdit): Promise<boolean> {
    try {
        const result = await api.put<{override: BuildingOverride; summary: OverrideSummary}>(
            `/api/pipeline/overrides/${pand}`,
            edit,
        )
        replace(result.override)
        notifier.success(`Saved ${result.override.name}: ${describe(result.summary)}`)
        return true
    } catch (e) {
        report(e, 'Could not save that override')
        return false
    }
}

/** Delete an override: the building goes back to the survey's. */
export async function deleteOverride(o: BuildingOverride): Promise<boolean> {
    try {
        await api.delete(`/api/pipeline/overrides/${o.pand}`)
        $s.overrides = ($s.overrides ?? []).filter((x) => x.pand !== o.pand)
        notifier.success(`Removed ${o.name}; the surveyed building is back`)
        return true
    } catch (e) {
        report(e, 'Could not delete that override')
        return false
    }
}
