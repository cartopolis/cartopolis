import {useEffect, useState} from 'preact/hooks'

import type {BuildingOverride} from '@/types'

import {$s} from '@/app'
import {
    deleteOverride,
    loadOverrides,
    pandOf,
    requestedPand,
    saveOverride,
    uploadModel,
    validPand,
} from '@/lib/overrides'

const COLLISION: {value: BuildingOverride['collision']; label: string}[] = [
    {label: 'Every part (each one a solid block)', value: 'parts'},
    {label: 'Only parts named wall, column or pillar', value: 'walls'},
    {label: 'Nothing — walk through it', value: 'none'},
]

const INTERIOR: {value: BuildingOverride['interior']; label: string}[] = [
    {label: 'None (the model is the inside)', value: 'none'},
    {label: "The building's rooms and storeys", value: 'design'},
]

/** A number input; empty reads as `null`. */
const NumberField = ({
    label,
    value,
    onChange,
    disabled,
    step,
}: {
    label: string
    value: number | null
    onChange: (v: number | null) => void
    disabled: boolean
    step?: string
}) => (
    <label class="field">
        <span>{label}</span>
        <input
            disabled={disabled}
            inputMode="decimal"
            onInput={(e) => {
                const text = (e.target as HTMLInputElement).value.trim()
                onChange(text === '' ? null : Number(text))
            }}
            step={step}
            type="number"
            value={value ?? ''}
        />
    </label>
)

/** A model file picker that uploads as soon as a file is chosen. */
const UploadButton = ({pand, label, disabled}: {pand: string; label: string; disabled?: boolean}) => {
    const [busy, setBusy] = useState(false)
    return (
        <label class={busy ? 'button is-busy' : 'button'}>
            {busy ? 'Uploading…' : label}
            <input
                accept=".glb,model/gltf-binary"
                disabled={busy || disabled}
                onChange={async (e) => {
                    const input = e.target as HTMLInputElement
                    const file = input.files?.[0]
                    input.value = ''
                    if (!file) {
                        return
                    }
                    setBusy(true)
                    await uploadModel(pand, file)
                    setBusy(false)
                }}
                style="display:none"
                type="file"
            />
        </label>
    )
}

const OverrideCard = ({o}: {o: BuildingOverride}) => {
    const [name, setName] = useState(o.name)
    const [collision, setCollision] = useState(o.collision)
    const [interior, setInterior] = useState(o.interior)
    const [placement, setPlacement] = useState({...o.placement})
    const [busy, setBusy] = useState(false)
    const [confirming, setConfirming] = useState(false)

    const reset = () => {
        setName(o.name)
        setCollision(o.collision)
        setInterior(o.interior)
        setPlacement({...o.placement})
    }
    // A save comes back as a new revision; show what was stored.
    useEffect(reset, [o.revision])

    const changed =
        name !== o.name ||
        collision !== o.collision ||
        interior !== o.interior ||
        JSON.stringify(placement) !== JSON.stringify({...o.placement})
    const halfPinned = (placement.lat === null) !== (placement.lng === null)
    const placementValid =
        Number.isFinite(placement.scale) &&
        placement.scale > 0 &&
        Number.isFinite(placement.heading_deg) &&
        Number.isFinite(placement.base_m) &&
        !halfPinned

    const act = async (fn: () => Promise<boolean>) => {
        setBusy(true)
        await fn()
        setBusy(false)
    }
    const set = (patch: Partial<BuildingOverride['placement']>) => setPlacement({...placement, ...patch})

    return (
        <li class="account role" id={`override-${o.pand}`}>
            <div class="account-main">
                <span class="account-name">{o.name}</span>
                <span class="tag">{o.source.kind === 'stadium' ? 'generated stadium' : 'uploaded model'}</span>
                <span class="account-meta">
                    BAG {o.pand}
                    {o.cell && ` · cell ${o.cell[0]}/${o.cell[1]}`} · saved{' '}
                    {new Date(o.revision).toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'})}
                    {o.author && ` · by ${o.author}`}
                    {o.source.kind === 'model' && ` · ${o.source.triangles.toLocaleString()} triangles`}
                </span>
            </div>
            <div class="role-grid">
                <fieldset class="role-group">
                    <legend>Building</legend>
                    <label class="field">
                        <span>Name</span>
                        <input
                            disabled={busy}
                            onInput={(e) => setName((e.target as HTMLInputElement).value)}
                            type="text"
                            value={name}
                        />
                    </label>
                    <label class="field">
                        <span>What stops a walker</span>
                        <select
                            disabled={busy}
                            onChange={(e) =>
                                setCollision((e.target as HTMLSelectElement).value as BuildingOverride['collision'])
                            }
                            value={collision}
                        >
                            {COLLISION.map((c) => (
                                <option key={c.value} value={c.value}>
                                    {c.label}
                                </option>
                            ))}
                        </select>
                    </label>
                    <label class="field">
                        <span>Inside</span>
                        <select
                            disabled={busy}
                            onChange={(e) =>
                                setInterior((e.target as HTMLSelectElement).value as BuildingOverride['interior'])
                            }
                            value={interior}
                        >
                            {INTERIOR.map((c) => (
                                <option key={c.value} value={c.value}>
                                    {c.label}
                                </option>
                            ))}
                        </select>
                    </label>
                </fieldset>
                {o.source.kind === 'model' && (
                    <fieldset class="role-group">
                        <legend>Placement</legend>
                        <NumberField
                            disabled={busy}
                            label="Heading (° clockwise from north)"
                            onChange={(v) => set({heading_deg: v ?? 0})}
                            value={placement.heading_deg}
                        />
                        <NumberField
                            disabled={busy}
                            label="Scale"
                            onChange={(v) => set({scale: v ?? 1})}
                            step="0.01"
                            value={placement.scale}
                        />
                        <NumberField
                            disabled={busy}
                            label="Lift above the ground (m)"
                            onChange={(v) => set({base_m: v ?? 0})}
                            step="0.1"
                            value={placement.base_m}
                        />
                        <NumberField
                            disabled={busy}
                            label="Centre latitude (empty: the registered outline's)"
                            onChange={(v) => set({lat: v})}
                            step="0.000001"
                            value={placement.lat}
                        />
                        <NumberField
                            disabled={busy}
                            label="Centre longitude"
                            onChange={(v) => set({lng: v})}
                            step="0.000001"
                            value={placement.lng}
                        />
                        {halfPinned && <p class="hint">Give both a latitude and a longitude, or leave both empty.</p>}
                    </fieldset>
                )}
            </div>
            <div class="row-actions">
                <button
                    class="primary"
                    disabled={busy || !changed || !name.trim() || !placementValid}
                    onClick={() =>
                        act(() =>
                            saveOverride(o.pand, {
                                collision,
                                interior,
                                name: name.trim(),
                                placement: o.source.kind === 'model' ? placement : undefined,
                            }),
                        )
                    }
                    type="button"
                >
                    Save and publish
                </button>
                <button class="ghost" disabled={busy || !changed} onClick={reset} type="button">
                    Undo changes
                </button>
                <UploadButton
                    disabled={busy}
                    label={o.source.kind === 'model' ? 'Replace model…' : 'Upload a model instead…'}
                    pand={o.pand}
                />
                {confirming ? (
                    <>
                        <button
                            class="danger"
                            disabled={busy}
                            onClick={() => act(() => deleteOverride(o))}
                            type="button"
                        >
                            Really restore the surveyed building
                        </button>
                        <button class="ghost" onClick={() => setConfirming(false)} type="button">
                            Cancel
                        </button>
                    </>
                ) : (
                    <button class="danger" disabled={busy} onClick={() => setConfirming(true)} type="button">
                        Delete override
                    </button>
                )}
            </div>
        </li>
    )
}

const NewOverride = () => {
    // Arriving from the viewer's building card opens the form on that building.
    const [open, setOpen] = useState(() => requestedPand() !== '')
    const [text, setText] = useState(requestedPand)
    const pand = pandOf(text)
    const exists = ($s.overrides ?? []).some((o) => o.pand === pand)

    if (!open) {
        return (
            <button class="primary" onClick={() => setOpen(true)} type="button">
                Replace a building
            </button>
        )
    }
    return (
        <div class="card" style="margin-bottom:16px">
            <h2>Replace a building</h2>
            <label class="field">
                <span>BAG building number</span>
                <input
                    onInput={(e) => setText((e.target as HTMLInputElement).value)}
                    placeholder="0014100010953806 or NL.IMBAG.Pand.0014100010953806"
                    type="text"
                    value={text}
                />
            </label>
            <p class="hint">
                The building card in the viewer shows the number. Upload a binary glTF (.glb, Y up, one unit a
                metre, the front facing −Z); it is centred on the building's registered outline with its lowest
                point on the ground, and can be turned and moved afterwards.
            </p>
            {exists && <p class="hint">That building already has an override; replace its model below.</p>}
            <div class="row-actions">
                {validPand(pand) && !exists ? (
                    <UploadButton label="Choose a .glb…" pand={pand} />
                ) : (
                    <button class="primary" disabled type="button">
                        Choose a .glb…
                    </button>
                )}
                <button class="ghost" onClick={() => setOpen(false)} type="button">
                    Cancel
                </button>
            </div>
        </div>
    )
}

/**
 * The overrides page (`docs/notes/building-models.md`): buildings whose
 * surveyed shell is replaced by a model. Every save republishes the one cell the
 * building is in; viewers pick it up within half a minute.
 */
export const Overrides = () => {
    useEffect(() => {
        void loadOverrides().then(() => {
            // A building that already has an override: go to its card instead.
            const pand = requestedPand()
            if (pand) {
                document.getElementById(`override-${pand}`)?.scrollIntoView({block: 'start'})
            }
        })
    }, [])

    return (
        <>
            <div class="section-head">
                <h1>Overrides</h1>
                <span class="count">{$s.overrides?.length ?? ''}</span>
                <span style="flex:1" />
                <NewOverride />
            </div>
            <p class="hint">
                A building listed here is drawn from its model instead of the 3DBAG survey. Deleting an override
                brings the surveyed building back. Viewers see a change within about half a minute.
            </p>
            {$s.overrides === null ? (
                <div class="empty">Loading…</div>
            ) : !$s.overrides.length ? (
                <div class="empty">No building is replaced.</div>
            ) : (
                <ul class="account-list">
                    {$s.overrides.map((o) => (
                        <OverrideCard key={o.pand} o={o} />
                    ))}
                </ul>
            )}
        </>
    )
}
