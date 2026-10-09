import m, {MithrilComponent} from '@garage44/mithril'
import {Badge, Button, Card, Field, FieldNumber, FieldSelect, FieldText, uid} from '@garage44/mithril/components'
import {required, validation, validations} from '@garage44/mithril/validation'

import type {BuildingOverride} from '@/types'

import {$s} from '@/app'
import {closeContext, contextShows, openContext} from '@/lib/layout'
import {
    deleteOverride,
    loadOverrides,
    pandOf,
    requestedPand,
    saveOverride,
    uploadModel,
    validPand,
} from '@/lib/overrides'
import {bind, local} from '@/lib/reactive'

const COLLISION: {value: BuildingOverride['collision']; label: string}[] = [
    {label: 'Every part (each one a solid block)', value: 'parts'},
    {label: 'Only parts named wall, column or pillar', value: 'walls'},
    {label: 'Nothing — walk through it', value: 'none'},
]

const INTERIOR: {value: BuildingOverride['interior']; label: string}[] = [
    {label: 'None (the model is the inside)', value: 'none'},
    {label: "The building's rooms and storeys", value: 'design'},
]

/** The context panel's key for one building's override form. */
const editKey = (pand: string) => `override:${pand}`

interface UploadButtonAttrs {
    pand: string
    label: string
    disabled?: boolean
    id?: string
    variant?: 'primary' | 'secondary'
}

/**
 * A model file picker that uploads as soon as a file is chosen.
 *
 * The label is what is pressed, drawn as a library button; the input stays in
 * the page, visually hidden rather than `display: none`, so it can still take
 * keyboard focus and the label shows the focus ring for it.
 */
class UploadButton extends MithrilComponent<UploadButtonAttrs> {
    s = local({busy: false})

    view(vnode: m.Vnode<UploadButtonAttrs>) {
        const {pand, label, disabled, id, variant} = vnode.attrs
        const s = this.s
        const off = s.busy || !!disabled
        return (
            <label
                aria-busy={s.busy ? 'true' : undefined}
                aria-disabled={off ? 'true' : undefined}
                class="m-button overrides-upload"
                data-size="md"
                data-variant={variant ?? 'secondary'}
            >
                {s.busy ? 'Uploading…' : label}
                <input
                    accept=".glb,model/gltf-binary"
                    disabled={off}
                    id={id}
                    onchange={async (e: Event) => {
                        const input = e.target as HTMLInputElement
                        const file = input.files?.[0]
                        input.value = ''
                        if (!file) {
                            return
                        }
                        s.busy = true
                        await uploadModel(pand, file)
                        s.busy = false
                    }}
                    type="file"
                />
            </label>
        )
    }
}

/**
 * The override being edited in the context panel: a draft of the stored one, so
 * "Undo changes" has something to go back to and Save knows what changed.
 */
interface Draft {
    busy: boolean
    collision: BuildingOverride['collision']
    confirming: boolean
    interior: BuildingOverride['interior']
    name: string
    pand: string
    placement: BuildingOverride['placement']
}

interface NewOverrideAttrs {
    onClose: () => void
}

class NewOverride extends MithrilComponent<NewOverrideAttrs> {
    // Arriving from the viewer's building card fills in that building.
    s = local({text: requestedPand()})
    v = validations({
        text: validation(bind(this.s, 'text'), [
            required,
            (text: string) => (validPand(pandOf(text)) ? null : 'That is not a BAG building number'),
        ]),
    })
    id = uid('override-model')

    view(vnode: m.Vnode<NewOverrideAttrs>) {
        const s = this.s
        const pand = pandOf(s.text)
        const exists = ($s.overrides ?? []).some((o) => o.pand === pand)

        return (
            <Card
                footer={
                    <Button onclick={vnode.attrs.onClose} variant="ghost">
                        Cancel
                    </Button>
                }
                title="Replace a building"
            >
                <div class="overrides-form">
                    <FieldText
                        help="The building card in the viewer shows the number."
                        label="BAG building number"
                        model={bind(s, 'text')}
                        placeholder="0014100010953806 or NL.IMBAG.Pand.0014100010953806"
                        validation={this.v.text}
                    />
                    <Field
                        error={exists ? 'That building already has an override; replace its model from its row.' : null}
                        for={this.id}
                        help={
                            "A binary glTF (.glb, Y up, one unit a metre, the front facing −Z). It is centred on the " +
                            "building's registered outline with its lowest point on the ground, and can be turned " +
                            'and moved afterwards.'
                        }
                        label="Model"
                    >
                        <div>
                            <UploadButton
                                disabled={!this.v.$valid || exists}
                                id={this.id}
                                label="Choose a .glb…"
                                pand={pand}
                                variant="primary"
                            />
                        </div>
                    </Field>
                </div>
            </Card>
        )
    }
}

/**
 * The overrides page (`docs/notes/building-models.md`): buildings whose
 * surveyed shell is replaced by a model. Every save republishes the one cell the
 * building is in; viewers pick it up within half a minute.
 *
 * A row opens its override's form in the context panel; the draft lives here,
 * on the page, because the panel is drawn by the frame and outlives no row of
 * its own.
 */
export class Overrides extends MithrilComponent {
    // Arriving from the viewer's building card opens the form on that building.
    s = local({adding: requestedPand() !== ''})
    draft: Draft = local<Draft>({
        busy: false,
        collision: 'parts',
        confirming: false,
        interior: 'none',
        name: '',
        pand: '',
        placement: {base_m: 0, heading_deg: 0, lat: null, lng: null, scale: 1},
    })
    /** The `revision` the draft was last reset from. */
    private seenRevision = 0

    /** One placement number as a field model; empty falls back to `fallback`. */
    private placementModel<K extends keyof BuildingOverride['placement']>(
        key: K,
        fallback: (v: number | null) => BuildingOverride['placement'][K],
    ) {
        const draft = this.draft
        return {
            get value(): number | null {
                return draft.placement[key] as number | null
            },
            set value(v: number | null) {
                draft.placement = {...draft.placement, [key]: fallback(v)}
            },
        }
    }

    heading = this.placementModel('heading_deg', (v) => v ?? 0)
    scale = this.placementModel('scale', (v) => v ?? 1)
    base = this.placementModel('base_m', (v) => v ?? 0)
    lat = this.placementModel('lat', (v) => v)
    lng = this.placementModel('lng', (v) => v)

    v = validations({
        name: validation(bind(this.draft, 'name'), [required]),
        // Both or neither: a centre with only a latitude is not a place.
        pin: validation(this.lng, [
            () =>
                (this.draft.placement.lat === null) !== (this.draft.placement.lng === null)
                    ? 'Give both a latitude and a longitude, or leave both empty.'
                    : null,
        ]),
        scale: validation(this.scale, [
            (v: number | null) => (v !== null && Number.isFinite(v) && v > 0 ? null : 'The scale must be more than 0'),
        ]),
    })

    oninit() {
        void loadOverrides().then(() => {
            // A building that already has an override: go to its form instead.
            const pand = requestedPand()
            const o = ($s.overrides ?? []).find((x) => x.pand === pand)
            if (o) {
                this.s.adding = false
                this.open(o)
                document.getElementById(`override-${pand}`)?.scrollIntoView({block: 'start'})
            }
        })
    }

    // A save or an upload comes back as a new revision; show what was stored.
    onupdate() {
        const draft = this.draft
        if (!draft.pand) {
            return
        }
        const o = ($s.overrides ?? []).find((x) => x.pand === draft.pand)
        if (o && o.revision !== this.seenRevision) {
            this.reset(o)
        }
    }

    onremove() {
        if (this.draft.pand) {
            closeContext(editKey(this.draft.pand))
        }
    }

    reset(o: BuildingOverride) {
        const draft = this.draft
        this.seenRevision = o.revision
        draft.pand = o.pand
        draft.name = o.name
        draft.collision = o.collision
        draft.interior = o.interior
        draft.placement = {...o.placement}
        this.v.$reset()
    }

    open(o: BuildingOverride) {
        const key = editKey(o.pand)
        if (contextShows(key)) {
            closeContext(key)
            return
        }
        this.reset(o)
        this.draft.busy = false
        this.draft.confirming = false
        openContext({
            body: () => this.editorBody(o.pand),
            footer: () => this.editorFooter(o.pand),
            key,
            title: () => this.find(o.pand)?.name ?? o.name,
        })
    }

    find(pand: string): BuildingOverride | undefined {
        return ($s.overrides ?? []).find((x) => x.pand === pand)
    }

    changed(o: BuildingOverride): boolean {
        const {name, collision, interior, placement} = this.draft
        return (
            name !== o.name ||
            collision !== o.collision ||
            interior !== o.interior ||
            JSON.stringify(placement) !== JSON.stringify({...o.placement})
        )
    }

    async act(fn: () => Promise<boolean>): Promise<boolean> {
        this.draft.busy = true
        const ok = await fn()
        this.draft.busy = false
        return ok
    }

    editorBody(pand: string) {
        const o = this.find(pand)
        if (!o) {
            return <p class="overrides-note">This building is no longer replaced.</p>
        }
        const draft = this.draft
        const busy = draft.busy
        return (
            <div class="overrides-editor">
                <div class="overrides-meta">
                    <Badge tone={o.source.kind === 'stadium' ? 'info' : 'neutral'}>
                        {o.source.kind === 'stadium' ? 'generated stadium' : 'uploaded model'}
                    </Badge>
                    <OverrideFacts o={o} />
                </div>
                <fieldset class="overrides-group">
                    <legend>Building</legend>
                    <FieldText disabled={busy} label="Name" model={bind(draft, 'name')} validation={this.v.name} />
                    <FieldSelect
                        disabled={busy}
                        label="What stops a walker"
                        model={bind(draft, 'collision')}
                        options={COLLISION}
                    />
                    <FieldSelect disabled={busy} label="Inside" model={bind(draft, 'interior')} options={INTERIOR} />
                </fieldset>
                {o.source.kind === 'model' && (
                    <fieldset class="overrides-group">
                        <legend>Placement</legend>
                        <FieldNumber
                            disabled={busy}
                            label="Heading (clockwise from north)"
                            model={this.heading}
                            plain
                            unit="°"
                        />
                        <FieldNumber
                            disabled={busy}
                            label="Scale"
                            model={this.scale}
                            step={0.01}
                            validation={this.v.scale}
                        />
                        <FieldNumber
                            disabled={busy}
                            label="Lift above the ground"
                            model={this.base}
                            step={0.1}
                            unit="m"
                        />
                        <FieldNumber
                            disabled={busy}
                            help="Empty: the registered outline's centre."
                            label="Centre latitude"
                            model={this.lat}
                            plain
                            step={0.000001}
                        />
                        {/* The pin's error at once rather than on blur, as the hint
                            it replaces was: typing a latitude is what causes it. */}
                        <FieldNumber
                            disabled={busy}
                            error={this.v.pin.error}
                            label="Centre longitude"
                            model={this.lng}
                            plain
                            step={0.000001}
                        />
                    </fieldset>
                )}
            </div>
        )
    }

    editorFooter(pand: string) {
        const o = this.find(pand)
        if (!o) {
            return (
                <Button onclick={() => closeContext(editKey(pand))} variant="ghost">
                    Close
                </Button>
            )
        }
        const draft = this.draft
        const {busy, confirming} = draft
        const changed = this.changed(o)
        return (
            <div class="overrides-actions">
                <Button
                    disabled={!changed || !this.v.$valid}
                    loading={busy}
                    onclick={() => {
                        if (!this.v.$validate()) return
                        void this.act(() =>
                            saveOverride(o.pand, {
                                collision: draft.collision,
                                interior: draft.interior,
                                name: draft.name.trim(),
                                placement: o.source.kind === 'model' ? {...draft.placement} : undefined,
                            }),
                        )
                    }}
                    variant="primary"
                >
                    Save and publish
                </Button>
                <Button disabled={busy || !changed} onclick={() => this.reset(o)} variant="ghost">
                    Undo changes
                </Button>
                <UploadButton
                    disabled={busy}
                    label={o.source.kind === 'model' ? 'Replace model…' : 'Upload a model instead…'}
                    pand={o.pand}
                />
                {confirming ? (
                    <>
                        <Button
                            disabled={busy}
                            onclick={() => {
                                void this.act(() => deleteOverride(o)).then((ok) => {
                                    draft.confirming = false
                                    if (ok) {
                                        closeContext(editKey(o.pand))
                                    }
                                })
                            }}
                            variant="danger"
                        >
                            Really restore the surveyed building
                        </Button>
                        <Button
                            onclick={() => {
                                draft.confirming = false
                            }}
                            variant="ghost"
                        >
                            Cancel
                        </Button>
                    </>
                ) : (
                    <Button
                        disabled={busy}
                        onclick={() => {
                            draft.confirming = true
                        }}
                        variant="danger"
                    >
                        Delete override
                    </Button>
                )}
            </div>
        )
    }

    view() {
        const s = this.s
        return (
            <div class="overrides">
                <div class="overrides-head">
                    <span class="overrides-count">
                        {$s.overrides === null ? '' : `${$s.overrides.length} replaced`}
                    </span>
                    <span class="overrides-spacer" />
                    {!s.adding && (
                        <Button
                            onclick={() => {
                                s.adding = true
                            }}
                            variant="primary"
                        >
                            Replace a building
                        </Button>
                    )}
                </div>
                <p class="overrides-note">
                    A building listed here is drawn from its model instead of the 3DBAG survey. Deleting an override
                    brings the surveyed building back. Viewers see a change within about half a minute.
                </p>
                {s.adding && (
                    <NewOverride
                        onClose={() => {
                            s.adding = false
                        }}
                    />
                )}
                {$s.overrides === null ? (
                    <p class="overrides-empty">Loading…</p>
                ) : !$s.overrides.length ? (
                    <p class="overrides-empty">No building is replaced.</p>
                ) : (
                    <Card flush>
                        <ul class="overrides-list">
                            {$s.overrides.map((o) => {
                                const open = contextShows(editKey(o.pand))
                                return (
                                    <li
                                        class="overrides-row"
                                        data-selected={open ? '' : undefined}
                                        id={`override-${o.pand}`}
                                        key={o.pand}
                                    >
                                        <div class="overrides-row__main">
                                            <span class="overrides-name">{o.name}</span>
                                            <Badge tone={o.source.kind === 'stadium' ? 'info' : 'neutral'}>
                                                {o.source.kind === 'stadium' ? 'generated stadium' : 'uploaded model'}
                                            </Badge>
                                            <span class="overrides-spacer" />
                                            <Button
                                                onclick={() => this.open(o)}
                                                size="sm"
                                                variant={open ? 'secondary' : 'ghost'}
                                            >
                                                {open ? 'Close' : 'Edit'}
                                            </Button>
                                        </div>
                                        <div class="overrides-meta">
                                            <OverrideFacts o={o} />
                                        </div>
                                    </li>
                                )
                            })}
                        </ul>
                    </Card>
                )}
            </div>
        )
    }
}

/** What is stored about an override, as one meta line. */
class OverrideFacts extends MithrilComponent<{o: BuildingOverride}> {
    view(vnode: m.Vnode<{o: BuildingOverride}>) {
        const o = vnode.attrs.o
        return (
            <span class="overrides-facts">
                BAG {o.pand}
                {o.cell && ` · cell ${o.cell[0]}/${o.cell[1]}`} · saved{' '}
                {new Date(o.revision).toLocaleString(undefined, {dateStyle: 'medium', timeStyle: 'short'})}
                {o.author && ` · by ${o.author}`}
                {o.source.kind === 'model' && ` · ${o.source.triangles.toLocaleString()} triangles`}
            </span>
        )
    }
}
