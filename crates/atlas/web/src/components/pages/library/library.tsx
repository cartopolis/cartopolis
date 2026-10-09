import m, {MithrilComponent} from '@garage44/mithril'
import {
    Badge,
    Button,
    Card,
    Collapse,
    Field,
    FieldCheckbox,
    FieldSelect,
    FieldText,
    Table,
    uid,
} from '@garage44/mithril/components'
import {validation, validations} from '@garage44/mithril/validation'

import type {LibraryEntry, StoredAsset} from '@/types'

import {$s} from '@/app'
import {closeContext, contextShows, openContext} from '@/lib/layout'
import {
    CATEGORIES,
    clearPlaceholder,
    curateEntry,
    forgetAsset,
    formatBytes,
    hideEntry,
    licenceLabel,
    loadLibrary,
    loadLibraryAssets,
    loadPlaceholders,
    modelNameFromFile,
    PLACEHOLDER_KINDS,
    PLACEHOLDER_LABELS,
    setPlaceholder,
    unhideEntry,
    UPLOAD_ACCEPT,
    uploadFormatHint,
    uploadModel,
} from '@/lib/library'
import {bind, local} from '@/lib/reactive'
import {formatTimestamp} from '@/lib/utils'

/** The licences an upload can claim. `other` can be used and never shared. */
const LICENCES = [
    {label: 'I made it', value: 'own'},
    {label: 'CC0 — public domain', value: 'cc0'},
    {label: 'CC BY — credit required', value: 'cc-by'},
    {label: 'Something else', value: 'other'},
]

/** The context panel's key for one catalogue entry's form. */
const editKey = (asset: string) => `library:${asset}`

/**
 * The shared model library, and the files under it.
 *
 * Two lists because they answer two questions. The catalogue is what players see
 * in the game's Models panel, and this page is where it gets a category, a tidy
 * name and an operator's approval — or gets taken down. The stored-files list
 * below is the other question: which uploads nothing refers to any more.
 *
 * A row is a **file**, not an upload. Two people uploading the same bench share
 * one file and one row, which is why the meta line lists every uploader and why
 * "in world" counts prims rather than owners.
 */
export class Library extends MithrilComponent {
    s = local({confirming: '', showFiles: false})
    /**
     * The open entry's form. On the page rather than on its row because the form
     * is drawn in the context panel, which outlives a row the search filters out.
     */
    form = local({attribution: '', category: '', name: '', saving: false})

    oninit() {
        void loadLibrary()
        void loadLibraryAssets()
        void loadPlaceholders()
    }

    onremove() {
        if ($s.libraryEditing) {
            closeContext(editKey($s.libraryEditing))
        }
        $s.libraryEditing = null
    }

    /** Edit one entry in the context panel; pressing Edit on the open one closes it. */
    toggleEditor(entry: LibraryEntry) {
        const key = editKey(entry.asset)
        if (contextShows(key)) {
            this.closeEditor(entry.asset)
            return
        }
        // Reset the fields on every open, so a form abandoned half-edited does
        // not reopen holding the abandoned text.
        this.form.name = entry.name
        this.form.category = entry.category
        this.form.attribution = entry.attribution
        $s.libraryEditing = entry.asset
        openContext({
            body: () => this.editorBody(entry.asset),
            footer: () => this.editorFooter(entry.asset),
            key,
            title: () => $s.library.find((e) => e.asset === entry.asset)?.name ?? entry.name,
        })
    }

    closeEditor(asset: string) {
        closeContext(editKey(asset))
        $s.libraryEditing = null
    }

    editorBody(asset: string) {
        const form = this.form
        const entry = $s.library.find((e) => e.asset === asset)
        if (!entry) {
            return <p class="library-note">This model is no longer in the catalogue.</p>
        }
        return (
            <div class="library-editor">
                <div class="library-meta">
                    <span>{licenceLabel(entry.licence)}</span>
                    <span>{entry.triangles.toLocaleString()} triangles</span>
                    <span>{formatBytes(entry.byte_size)}</span>
                </div>
                <FieldText label="Name" model={bind(form, 'name')} />
                <FieldSelect label="Category" model={bind(form, 'category')} options={CATEGORIES} />
                <FieldText
                    label="Credit line"
                    model={bind(form, 'attribution')}
                    placeholder={entry.licence === 'cc-by' ? 'Required for CC BY' : 'Not required for this licence'}
                />
                <span class="library-mono">{entry.asset.slice(0, 16)}</span>
            </div>
        )
    }

    editorFooter(asset: string) {
        const form = this.form
        return (
            <>
                {/*
                  * Saving marks it curated as well as writing the fields. The two
                  * are the same act: an operator who has typed a name and picked a
                  * category has reviewed it, and a separate "approve" button would
                  * be a second click for a decision already made.
                  */}
                <Button
                    loading={form.saving}
                    onclick={() => {
                        form.saving = true
                        void curateEntry(asset, {
                            attribution: form.attribution,
                            category: form.category,
                            name: form.name,
                            visibility: 'curated',
                        }).then((ok) => {
                            form.saving = false
                            if (ok) {
                                this.closeEditor(asset)
                            }
                        })
                    }}
                    variant="primary"
                >
                    Save and approve
                </Button>
                <Button onclick={() => this.closeEditor(asset)} variant="ghost">
                    Cancel
                </Button>
            </>
        )
    }

    view() {
        const s = this.s
        const needle = $s.librarySearch.trim().toLowerCase()
        const entries = $s.library.filter((entry) =>
            needle === ''
                ? true
                : entry.name.toLowerCase().includes(needle) ||
                  entry.tags.some((tag) => tag.toLowerCase().includes(needle)),
        )
        // An operator opening this page is here to review, so what has never been
        // reviewed goes first; hidden entries go last, since acting on one is rare.
        const ordered = [...entries].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name))
        const waiting = $s.library.filter((entry) => entry.visibility === 'shared').length
        const orphans = $s.libraryAssets.filter((asset) => asset.in_world === 0 && asset.held_by === 0)

        return (
            <div class="library">
                <div class="library-head">
                    <span class="library-count">{$s.library.length} in the catalogue</span>
                    {waiting > 0 && <Badge tone="warning">{waiting} to review</Badge>}
                    <FieldText
                        class="library-search"
                        model={bind($s, 'librarySearch')}
                        placeholder="Search names and tags"
                        type="search"
                    />
                </div>

                <Upload />

                <Card flush title="Catalogue">
                    {$s.library.length === 0 ? (
                        <p class="library-empty">Nobody has shared a model yet.</p>
                    ) : ordered.length === 0 ? (
                        <p class="library-empty">No model matches that search.</p>
                    ) : (
                        <ul class="library-list">
                            {ordered.map((entry) => (
                                <Row
                                    confirming={s.confirming === entry.asset}
                                    editing={contextShows(editKey(entry.asset))}
                                    entry={entry}
                                    key={entry.asset}
                                    onConfirm={(on: boolean) => {
                                        s.confirming = on ? entry.asset : ''
                                    }}
                                    onEdit={() => this.toggleEditor(entry)}
                                />
                            ))}
                        </ul>
                    )}
                </Card>

                <StreetObjects />

                <Collapse
                    actions={orphans.length > 0 && <Badge tone="warning">{orphans.length} unused</Badge>}
                    class="library-files"
                    model={bind(s, 'showFiles')}
                    title={`Stored files (${$s.libraryAssets.length})`}
                >
                    <StoredFiles />
                </Collapse>
            </div>
        )
    }
}

/** Unreviewed first, then curated, then hidden. */
function rank(entry: LibraryEntry): number {
    switch (entry.visibility) {
    case 'shared':
        return 0
    case 'curated':
        return 1
    default:
        return 2
    }
}

interface RowAttrs {
    confirming: boolean
    editing: boolean
    entry: LibraryEntry
    onConfirm: (on: boolean) => void
    onEdit: () => void
}

class Row extends MithrilComponent<RowAttrs> {
    view(vnode: m.Vnode<RowAttrs>) {
        const {confirming, editing, entry, onConfirm, onEdit} = vnode.attrs

        return (
            <li class="library-row" data-selected={editing ? '' : undefined}>
                <div class="library-row__main">
                    <span class="library-name">{entry.name}</span>
                    {entry.visibility === 'hidden' && <Badge tone="neutral">hidden</Badge>}
                    {entry.visibility === 'shared' && <Badge tone="warning">unreviewed</Badge>}
                    <span class="library-spacer" />
                    <Button onclick={onEdit} size="sm" variant={editing ? 'secondary' : 'ghost'}>
                        {editing ? 'Close' : 'Edit'}
                    </Button>
                    {entry.visibility === 'hidden' ? (
                        <Button onclick={() => void unhideEntry(entry)} size="sm" variant="ghost">
                            Restore
                        </Button>
                    ) : confirming ? (
                        <>
                            {/*
                              * Two presses. Hiding a model stops the server serving
                              * the file at all, so every prim in the world wearing it
                              * goes to a placeholder — that is the intended effect and
                              * it is not a small one.
                              */}
                            <Button
                                onclick={() => {
                                    onConfirm(false)
                                    void hideEntry(entry)
                                }}
                                size="sm"
                                variant="danger"
                            >
                                Take down
                            </Button>
                            <Button onclick={() => onConfirm(false)} size="sm" variant="ghost">
                                Cancel
                            </Button>
                        </>
                    ) : (
                        <Button onclick={() => onConfirm(true)} size="sm" variant="ghost">
                            Take down
                        </Button>
                    )}
                </div>

                <div class="library-meta">
                    <span>{entry.category}</span>
                    <span>{entry.triangles.toLocaleString()} triangles</span>
                    <span>{formatBytes(entry.byte_size)}</span>
                    <span>{licenceLabel(entry.licence)}</span>
                    <span>
                        {entry.in_world} in world · {entry.held_by} held
                    </span>
                    <span>shared by {entry.shared_by ?? 'the server'}</span>
                    {/*
                      * Everyone who uploaded these exact bytes. Plural because the
                      * file is deduplicated by content — and it is who an operator
                      * acts on when a model turns out not to be shareable.
                      */}
                    {entry.uploaders.length > 0 && <span>uploaded by {entry.uploaders.join(', ')}</span>}
                    <span>{formatTimestamp(entry.created_at)}</span>
                </div>

                {/* The credit line a CC-BY model must be shown with, always visible
                    rather than only in the edit form: it is the thing most likely to
                    be wrong, and a wrong credit is worse than a missing model. */}
                {entry.attribution !== '' && <div class="library-meta library-credit">{entry.attribution}</div>}
            </li>
        )
    }
}

/**
 * Every stored file, for finding the ones nothing refers to. A table because it
 * is one: the same seven facts about every file, compared down the column.
 */
class StoredFiles extends MithrilComponent {
    view() {
        return (
            <Table<StoredAsset>
                columns={[
                    {
                        cell: (asset) => <span class="library-mono">{asset.hash.slice(0, 12)}</span>,
                        key: 'hash',
                        label: 'File',
                    },
                    {
                        align: 'end',
                        cell: (asset) => asset.triangles.toLocaleString(),
                        key: 'triangles',
                        label: 'Triangles',
                    },
                    {align: 'end', cell: (asset) => formatBytes(asset.byte_size), key: 'size', label: 'Size'},
                    {
                        cell: (asset) =>
                            asset.textures === 0
                                ? 'none'
                                : `${asset.textures}, ${asset.largest_texture} px`,
                        key: 'textures',
                        label: 'Textures',
                    },
                    {
                        cell: (asset) => (
                            <>
                                {asset.in_world} in world · {asset.held_by} held
                                {!asset.shared && <> · not shared</>}
                            </>
                        ),
                        key: 'use',
                        label: 'Use',
                    },
                    {cell: (asset) => formatTimestamp(asset.created_at), key: 'created', label: 'Uploaded'},
                    {
                        align: 'end',
                        /*
                         * Offered only for a file nothing refers to. The server
                         * refuses the rest and says what is holding them, so this
                         * is a courtesy rather than the guard.
                         */
                        cell: (asset) =>
                            asset.in_world === 0 &&
                            asset.held_by === 0 && (
                                <Button onclick={() => void forgetAsset(asset)} size="sm" variant="ghost">
                                    Remove
                                </Button>
                            ),
                        key: 'actions',
                        label: '',
                    },
                ]}
                empty="Nothing has been uploaded to this server."
                rowKey={(asset) => asset.hash}
                rows={$s.libraryAssets}
            />
        )
    }
}

/** A native file picker inside the fields' frame; the library has no file field. */
function filePicker(
    id: string,
    label: string,
    onpick: (file: File | null) => void,
    keep?: (dom: HTMLInputElement) => void,
) {
    return (
        <Field for={id} label={label}>
            <input
                accept={UPLOAD_ACCEPT}
                class="library-file"
                id={id}
                onchange={(e: Event) => onpick((e.target as HTMLInputElement).files?.[0] ?? null)}
                oncreate={keep ? (v: m.VnodeDOM) => keep(v.dom as HTMLInputElement) : undefined}
                type="file"
            />
        </Field>
    )
}

/**
 * Adding a model to this server from here.
 *
 * The same route the game client posts to, because uploading is an ordinary
 * account's act — an operator is an ordinary account with a flag. What is
 * different is the intent: a curator at this screen is nearly always uploading
 * *for* the library, so sharing and the category are asked here instead of being
 * a second visit to the row that appears.
 *
 * The licence question is not a formality and is not skippable. Using a model in
 * a world and handing copies of it to everybody else are different acts under
 * almost every asset licence, and a model claimed as "something else" can be used
 * and can never be shared — the server refuses, this form only has to not lie
 * about it.
 */
class Upload extends MithrilComponent {
    // A plain field, not state: `state()` would wrap the File in a proxy, and a
    // proxied File is refused by FormData. Every write to it happens in an event
    // handler or beside a state write, either of which redraws.
    file: File | null = null
    /** The native input, so a finished upload can empty it along with the form. */
    input: HTMLInputElement | null = null
    id = uid('library-upload')
    s = local({attribution: '', busy: false, category: 'other', licence: 'own', name: '', share: true})

    view() {
        const s = this.s
        const file = this.file
        const mayShare = s.licence !== 'other'
        const needsCredit = s.licence === 'cc-by'

        return (
            <Card title="Add a model">
                <div class="library-form">
                    {/*
                      * What the simulator accepts, said before a large upload finds out.
                      * Built from `UPLOAD_LIMITS` (see `uploadFormatHint`), which a Rust
                      * test pins to the numbers the server enforces.
                      */}
                    <p class="library-note">{uploadFormatHint()}</p>
                    {filePicker(
                        this.id,
                        'Model file',
                        (picked) => {
                            this.file = picked
                            if (picked && s.name === '') {
                                s.name = modelNameFromFile(picked.name)
                            }
                        },
                        (dom) => {
                            this.input = dom
                        },
                    )}
                    {file && (
                        <>
                            <FieldText label="Name" model={bind(s, 'name')} placeholder="What is it?" />
                            <FieldSelect label="Licence" model={bind(s, 'licence')} options={LICENCES} />
                            {needsCredit && (
                                <FieldText
                                    label="Credit line"
                                    model={bind(s, 'attribution')}
                                    placeholder="e.g. “Bench by A. N. Other”"
                                />
                            )}
                            {mayShare ? (
                                <FieldCheckbox label="Share it in the library" model={bind(s, 'share')} />
                            ) : (
                                <p class="library-note">
                                    This can be used on this server and can never be shared or handed on.
                                </p>
                            )}
                            {s.share && mayShare && (
                                <FieldSelect label="Category" model={bind(s, 'category')} options={CATEGORIES} />
                            )}
                            <div class="library-actions">
                                <Button
                                    loading={s.busy}
                                    onclick={() => {
                                        s.busy = true
                                        void uploadModel(file, {
                                            attribution: s.attribution,
                                            category: s.category,
                                            licence: s.licence,
                                            name: s.name,
                                            share: s.share,
                                        }).then((ok) => {
                                            if (ok) {
                                                this.file = null
                                                if (this.input) {
                                                    this.input.value = ''
                                                }
                                            }
                                            s.busy = false
                                            if (ok) {
                                                s.name = ''
                                                s.attribution = ''
                                            }
                                        })
                                    }}
                                    variant="primary"
                                >
                                    Upload
                                </Button>
                            </div>
                        </>
                    )}
                </div>
            </Card>
        )
    }
}

/**
 * What this server's street objects look like.
 *
 * A lamppost, a bin or a bench is drawn from a register — the BGT's columns, the
 * city's own, OpenStreetMap's — so nobody placed one and there is nothing in the
 * world to select and improve. The client builds each out of a handful of boxes,
 * which is what makes a city of them affordable and is also why they look like a
 * handful of boxes.
 *
 * This is where that is answered, once, for everybody. It is deliberately not a
 * per-player setting: two people standing on the same street seeing different
 * lampposts is not a preference, it is two different worlds.
 *
 * Every client rebuilds the streets around it when this changes, so it is a
 * heavier action than curating a name — worth knowing before flipping through
 * fifteen of them.
 */
class StreetObjects extends MithrilComponent {
    // The one row whose upload form is open, if any.
    s = local({uploadingFor: ''})

    view() {
        const s = this.s
        const set = new Map($s.placeholders.map((p) => [p.kind, p]))
        // Only what has actually been published: a placeholder pointing at a model
        // nobody can fetch is a street with nothing in it.
        const usable = $s.library.filter((entry) => entry.visibility !== 'hidden')
        const options = [
            {label: 'Built-in shape', value: ''},
            ...usable.map((entry) => ({label: entry.name, value: entry.asset})),
        ]

        return (
            <Card flush subtitle={`${$s.placeholders.length} replaced`} title="Street objects">
                <ul class="library-list">
                    {PLACEHOLDER_KINDS.map((kind) => {
                        const current = set.get(kind)
                        const label = PLACEHOLDER_LABELS[kind] ?? kind
                        // Choosing writes straight to the server, as the plain select
                        // did: the model is a view of the stored placeholder, never a
                        // draft of it.
                        const choice = {
                            get value(): string {
                                return current?.asset ?? ''
                            },
                            set value(asset: string | null) {
                                if (!asset) {
                                    void clearPlaceholder(kind)
                                    return
                                }
                                void setPlaceholder(kind, {
                                    asset,
                                    scale: current?.scale ?? 1,
                                    yaw_offset: current?.yaw_offset ?? 0,
                                })
                            },
                        }
                        return (
                            <li class="library-row" key={kind}>
                                <div class="library-row__main">
                                    <span class="library-name">{label}</span>
                                    <span class="library-spacer" />
                                    <FieldSelect class="library-street-model" model={choice} options={options} />
                                    <Button
                                        onclick={() => {
                                            s.uploadingFor = s.uploadingFor === kind ? '' : kind
                                        }}
                                        size="sm"
                                        variant={s.uploadingFor === kind ? 'secondary' : 'ghost'}
                                    >
                                        {s.uploadingFor === kind ? 'Cancel' : 'Upload…'}
                                    </Button>
                                </div>
                                {s.uploadingFor === kind && (
                                    <StreetUpload
                                        kind={kind}
                                        onDone={() => {
                                            s.uploadingFor = ''
                                        }}
                                    />
                                )}
                                {current && (
                                    <div class="library-street-fit">
                                        {/*
                                          * Scale and angle are here rather than at
                                          * upload because they are about *this* use of
                                          * the model: the same bench may be the right
                                          * size for a prim and the wrong one for a
                                          * street, and a model whose front faces +x
                                          * sits sideways on every pavement in the city
                                          * until this is turned.
                                          *
                                          * Native inputs in the fields' frame rather
                                          * than `FieldNumber`, which writes on every
                                          * keystroke: each write here makes every client
                                          * rebuild its streets, so it waits for `change`.
                                          */}
                                        <Field for={`street-scale-${kind}`} label="Scale">
                                            <div class="m-input">
                                                <input
                                                    class="m-input__native"
                                                    id={`street-scale-${kind}`}
                                                    inputmode="decimal"
                                                    onchange={(e: Event) => {
                                                        const scale = Number((e.target as HTMLInputElement).value)
                                                        void setPlaceholder(kind, {
                                                            asset: current.asset,
                                                            scale,
                                                            yaw_offset: current.yaw_offset,
                                                        })
                                                    }}
                                                    step="0.05"
                                                    type="number"
                                                    value={current.scale}
                                                />
                                            </div>
                                        </Field>
                                        <Field for={`street-turn-${kind}`} label="Turn (°)">
                                            <div class="m-input">
                                                <input
                                                    class="m-input__native"
                                                    id={`street-turn-${kind}`}
                                                    inputmode="decimal"
                                                    onchange={(e: Event) => {
                                                        const yaw = Number((e.target as HTMLInputElement).value)
                                                        void setPlaceholder(kind, {
                                                            asset: current.asset,
                                                            scale: current.scale,
                                                            yaw_offset: yaw,
                                                        })
                                                    }}
                                                    step="15"
                                                    type="number"
                                                    value={current.yaw_offset}
                                                />
                                            </div>
                                        </Field>
                                        <span class="library-meta">set by {current.set_by}</span>
                                    </div>
                                )}
                            </li>
                        )
                    })}
                </ul>
            </Card>
        )
    }
}

/** Where a street object's model is filed in the library. */
const STREET_CATEGORY: Record<string, string> = {
    bicycle: 'vehicles',
    boat: 'vehicles',
    car: 'vehicles',
    houseboat: 'vehicles',
    person: 'other',
    yacht: 'vehicles',
}

interface StreetUploadAttrs {
    kind: string
    onDone: () => void
}

/**
 * Upload a model straight into one street object: it is stored, shared and set
 * as that object's model in one go, at scale 1 and no turn.
 *
 * It is always shared, because a street object is drawn on every client and a
 * model nobody else can fetch is a street with nothing in it — which is also why
 * "something else" is not offered as a licence here: that one can never be
 * shared. The general form above still takes those.
 */
class StreetUpload extends MithrilComponent<StreetUploadAttrs> {
    // A plain field for the reason `Upload.file` is one.
    file: File | null = null
    id = uid('street-upload')
    s = local({attribution: '', busy: false, licence: 'own'})
    // A CC BY model has to be shown with its credit, and this form shares it at
    // once, so the line is asked for before the button lets go.
    v = validations({
        attribution: validation(bind(this.s, 'attribution'), [
            (value: string) =>
                this.s.licence === 'cc-by' && value.trim() === '' ? 'A CC BY model needs its credit line' : null,
        ]),
    })

    view(vnode: m.Vnode<StreetUploadAttrs>) {
        const {kind, onDone} = vnode.attrs
        const s = this.s
        const file = this.file
        const label = PLACEHOLDER_LABELS[kind] ?? kind

        return (
            <div class="library-street-upload">
                {filePicker(this.id, 'Model file', (picked) => {
                    this.file = picked
                })}
                <FieldSelect label="Licence" model={bind(s, 'licence')} options={LICENCES.slice(0, 3)} />
                {s.licence === 'cc-by' && (
                    <FieldText
                        label="Credit line"
                        model={bind(s, 'attribution')}
                        required
                        validation={this.v.attribution}
                    />
                )}
                <div class="library-actions">
                    <Button
                        disabled={!file || !this.v.$valid}
                        loading={s.busy}
                        onclick={() => {
                            if (!file || !this.v.$validate()) return
                            s.busy = true
                            void uploadModel(file, {
                                attribution: s.attribution,
                                category: STREET_CATEGORY[kind] ?? 'furniture',
                                licence: s.licence,
                                name: modelNameFromFile(file.name),
                                share: true,
                            })
                                .then((asset) => (asset ? setPlaceholder(kind, {asset, scale: 1, yaw_offset: 0}) : false))
                                .then((ok) => {
                                    s.busy = false
                                    if (ok) onDone()
                                })
                        }}
                        variant="primary"
                    >
                        Use for {label.toLowerCase()}
                    </Button>
                </div>
            </div>
        )
    }
}
