import type {LibraryEntry, PlaceholderModel, StoredAsset} from '@/types'

import {$s, api, notifier} from '@/app'
import {ApiError} from '@/lib/api'

/**
 * Curating the shared model library.
 *
 * Everything a person uploads to a simulator lands here uncurated — a model is
 * visible the moment somebody shares it, because a library that waits for review
 * is empty on the day it opens. What this surface adds is the second half: a
 * tidy name, a category, and an operator's word that it belongs.
 *
 * The unit is the **file**, not the upload. Two people who upload the same bench
 * share one stored file and one catalogue row, which is why a row shows a list of
 * uploaders and why hiding one reaches every prim in the world wearing it.
 *
 * These call `/api/admin/*` on the simulator, same as the accounts and houses
 * surfaces. Note that they are reachable *here* and not from the game host: the
 * Caddy block on `cartopolis.org` proxies only the player-facing model paths, so
 * curation is an operator action on the operator's origin.
 */
function report(e: unknown, fallback: string) {
    if (e instanceof ApiError) {
        // A 401 has already signed the app out; a second toast would be noise on
        // top of a screen that has just changed.
        if (!e.isUnauthorized) {
            notifier.error(e.message)
        }
        return
    }
    notifier.error(fallback)
}

/** The whole catalogue, hidden entries included — this is the review queue. */
export async function loadLibrary() {
    try {
        $s.library = await api.get<LibraryEntry[]>('/api/admin/library')
    } catch (e) {
        report(e, 'Could not load the model library')
    }
}

/**
 * Every stored file with what still refers to it.
 *
 * Fetched alongside the catalogue rather than on its own tab, because the only
 * question it answers is about the catalogue: which files nothing points at any
 * more. A file can outlive its entry — somebody unshares it and keeps their copy
 * — so the two lists genuinely differ.
 */
export async function loadLibraryAssets() {
    try {
        $s.libraryAssets = await api.get<StoredAsset[]>('/api/admin/assets')
    } catch (e) {
        report(e, 'Could not load the stored files')
    }
}

/**
 * Change what an entry says, or how visible it is.
 *
 * A patch, so a field left out is a field left alone — the same rule the prim
 * authoring surface follows. Setting `visibility` to `curated` is what stamps the
 * server's `curated_at`, which is how "an operator has looked at this" is told
 * apart from "nobody has yet".
 */
export async function curateEntry(
    asset: string,
    patch: Partial<Pick<LibraryEntry, 'attribution' | 'category' | 'name' | 'visibility'>>,
): Promise<boolean> {
    try {
        await api.patch(`/api/admin/library/${asset}`, patch)
        await loadLibrary()
        return true
    } catch (e) {
        report(e, 'Could not update that model')
        return false
    }
}

/**
 * Take a model down.
 *
 * Stronger than it looks, and deliberately so: the server refuses to serve a
 * hidden file at all, so every prim already wearing it falls back to a
 * placeholder. Hiding it from this list while still handing it to everybody who
 * walks past is the half-measure that reads as moderation and is not.
 */
export async function hideEntry(entry: LibraryEntry): Promise<boolean> {
    const ok = await curateEntry(entry.asset, {visibility: 'hidden'})
    if (ok) {
        notifier.success(`Took “${entry.name}” down`)
    }
    return ok
}

/** Put a hidden model back, as published-but-unreviewed. */
export async function unhideEntry(entry: LibraryEntry): Promise<boolean> {
    const ok = await curateEntry(entry.asset, {visibility: 'shared'})
    if (ok) {
        notifier.success(`Restored “${entry.name}”`)
    }
    return ok
}

/**
 * Delete a stored file the world has finished with.
 *
 * The server refuses while anything still refers to it and says what does, so
 * this cannot leave a prim in the street that nobody can ever draw. That refusal
 * is the reason the button is offered at all rather than being an ssh job.
 */
export async function forgetAsset(asset: StoredAsset): Promise<boolean> {
    try {
        await api.delete(`/api/admin/assets/${asset.hash}`)
        notifier.success('Removed the file')
        await Promise.all([loadLibrary(), loadLibraryAssets()])
        return true
    } catch (e) {
        report(e, 'Could not remove that file')
        return false
    }
}

/** How a stored licence code reads to a person. */
export function licenceLabel(licence: string): string {
    switch (licence) {
    case 'cc-by':
        return 'CC BY'
    case 'cc0':
        return 'CC0'
    case 'own':
        return 'by its author'
    default:
        return 'licence unknown'
    }
}

/**
 * The categories the in-world picker groups by.
 *
 * A fixed list rather than free text: the picker draws a tab per category, and a
 * curator typing "furniture " once would make a second one nobody can find.
 */
export const CATEGORIES = [
    'furniture',
    'vegetation',
    'vehicles',
    'buildings',
    'signage',
    'decoration',
    'other',
] as const

/**
 * The street objects a model may stand in for, mirroring
 * `cartopolis_core::placeholders::PLACEHOLDER_KINDS`.
 *
 * The **seventh hand-synced seam** between these two repositories. The server
 * refuses a slug that is not in its own list, so a name that drifts here fails
 * loudly rather than quietly — which is why a refusal is shown verbatim.
 */
export const PLACEHOLDER_KINDS = [
    'lamp',
    'bench',
    'bin',
    'recycling',
    'shelter',
    'picnic-table',
    'cabinet',
    'board',
    'fountain',
    'artwork',
    'swing',
    'car',
    'bicycle',
    'person',
    'boat',
] as const

/** What each slug is, in the words a person would use for it. */
export const PLACEHOLDER_LABELS: Record<string, string> = {
    artwork: 'Artwork plinth',
    bench: 'Bench',
    bicycle: 'Parked bicycle',
    bin: 'Litter bin',
    board: 'Information board',
    boat: 'Moored boat',
    cabinet: 'Post box / cabinet',
    car: 'Parked car',
    fountain: 'Fountain',
    lamp: 'Street light',
    'picnic-table': 'Picnic table',
    person: 'Pedestrian',
    recycling: 'Bottle bank',
    shelter: 'Shelter',
    swing: 'Playground swing',
}

/** What this server draws its street objects with, or nothing for the default. */
export async function loadPlaceholders() {
    try {
        $s.placeholders = await api.get<PlaceholderModel[]>('/api/library/placeholders')
    } catch (e) {
        report(e, 'Could not load the street-object models')
    }
}

/**
 * Replace one street object with a model, world-wide.
 *
 * Every client rebuilds the streets it is standing in when this changes, which is
 * why it is one call and not a per-field patch: there is no half-set state worth
 * expressing, and a second request would mean a second rebuild.
 */
export async function setPlaceholder(
    kind: string,
    body: {asset: string; scale: number; yaw_offset: number},
): Promise<boolean> {
    try {
        await api.put(`/api/admin/library/placeholders/${kind}`, body)
        await loadPlaceholders()
        return true
    } catch (e) {
        report(e, 'Could not set that street-object model')
        return false
    }
}

/** Put a street object back to the shape built into the client. */
export async function clearPlaceholder(kind: string): Promise<boolean> {
    try {
        await api.delete(`/api/admin/library/placeholders/${kind}`)
        await loadPlaceholders()
        return true
    } catch (e) {
        report(e, 'Could not clear that street-object model')
        return false
    }
}

/**
 * Upload a model from here, and publish it in the same breath.
 *
 * The same route a player's client posts to (`POST /api/assets`), not an admin
 * one — uploading is an ordinary account's act and an operator is an ordinary
 * account with a flag. What this surface adds is that a curator is usually
 * uploading *for* the library, so sharing and the category are asked here rather
 * than left as a second visit.
 *
 * The licence question is not skippable and not a formality: using a model in a
 * world and handing copies of it to everyone else are different acts under almost
 * every asset licence, and `other` may never be shared.
 */
export async function uploadModel(
    file: File,
    meta: {attribution: string; category: string; licence: string; name: string; share: boolean},
): Promise<boolean> {
    const query = new URLSearchParams({
        attribution: meta.attribution,
        licence: meta.licence,
        name: meta.name || file.name,
    })
    try {
        const bytes = await file.arrayBuffer()
        const item = await api.upload<{id: number}>(
            `/api/assets?${query.toString()}`,
            bytes,
            'model/gltf-binary',
        )
        if (meta.share && meta.licence !== 'other') {
            // The share is a second request because it is a second decision the
            // server checks separately — and it needs the inventory id the upload
            // answers with, which is why it cannot be folded into the first.
            await api.post(`/api/inventory/${item.id}/share`, {
                category: meta.category,
                name: meta.name || file.name,
            })
        }
        await loadLibrary()
        await loadLibraryAssets()
        notifier.success(`Uploaded ${meta.name || file.name}`)
        return true
    } catch (e) {
        report(e, 'Could not upload that model')
        return false
    }
}

/** Bytes as a person reads them. */
export function formatBytes(bytes: number): string {
    if (bytes >= 1024 * 1024) {
        return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
    }
    return `${Math.max(1, Math.round(bytes / 1024))} KB`
}
