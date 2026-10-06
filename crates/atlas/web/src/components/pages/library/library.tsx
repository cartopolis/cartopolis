import {useEffect, useState} from 'preact/hooks'

import type {LibraryEntry} from '@/types'

import {$s} from '@/app'
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
import {formatTimestamp} from '@/lib/utils'

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
export const Library = () => {
    const [confirming, setConfirming] = useState('')
    const [showFiles, setShowFiles] = useState(false)

    useEffect(() => {
        void loadLibrary()
        void loadLibraryAssets()
        void loadPlaceholders()
    }, [])

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
    const orphans = $s.libraryAssets.filter(
        (asset) => asset.in_world === 0 && asset.held_by === 0,
    )

    return (
        <>
            <div class="section-head">
                <h1>Models</h1>
                <span class="count">{$s.library.length}</span>
                {waiting > 0 && <span class="count">{waiting} to review</span>}
                <span style="flex:1" />
                <input
                    onInput={(e) => {
                        $s.librarySearch = (e.target as HTMLInputElement).value
                    }}
                    placeholder="Search names and tags"
                    type="search"
                    value={$s.librarySearch}
                />
            </div>

            <Upload />

            {$s.library.length === 0 ? (
                <div class="empty">Nobody has shared a model yet.</div>
            ) : (
                <ul class="account-list">
                    {ordered.map((entry) => (
                        <Row
                            confirming={confirming === entry.asset}
                            entry={entry}
                            key={entry.asset}
                            onConfirm={(on) => setConfirming(on ? entry.asset : '')}
                        />
                    ))}
                </ul>
            )}

            <StreetObjects />

            <div class="section-head">
                <h1>Stored files</h1>
                <span class="count">{$s.libraryAssets.length}</span>
                {orphans.length > 0 && <span class="count">{orphans.length} unused</span>}
                <span style="flex:1" />
                <button class="ghost" onClick={() => setShowFiles(!showFiles)} type="button">
                    {showFiles ? 'Hide' : 'Show'}
                </button>
            </div>

            {showFiles && (
                $s.libraryAssets.length === 0 ? (
                    <div class="empty">Nothing has been uploaded to this server.</div>
                ) : (
                    <ul class="account-list">
                        {$s.libraryAssets.map((asset) => (
                            <li class="account" key={asset.hash}>
                                <div class="account-main">
                                    <span class="mono">{asset.hash.slice(0, 12)}</span>
                                    <span style="flex:1" />
                                    {/*
                                      * Offered only for a file nothing refers to.
                                      * The server refuses the rest and says what
                                      * is holding them, so this is a courtesy
                                      * rather than the guard.
                                      */}
                                    {asset.in_world === 0 && asset.held_by === 0 && (
                                        <button
                                            class="ghost"
                                            onClick={() => void forgetAsset(asset)}
                                            type="button"
                                        >
                                            Remove
                                        </button>
                                    )}
                                </div>
                                <div class="account-meta">
                                    <span>{asset.triangles.toLocaleString()} triangles</span>
                                    <span>{formatBytes(asset.byte_size)}</span>
                                    <span>
                                        {asset.textures === 0
                                            ? 'no textures'
                                            : `${asset.textures} texture(s), ${asset.largest_texture} px`}
                                    </span>
                                    <span>
                                        {asset.in_world} in world · {asset.held_by} held
                                    </span>
                                    {!asset.shared && <span>not shared</span>}
                                    <span>{formatTimestamp(asset.created_at)}</span>
                                </div>
                            </li>
                        ))}
                    </ul>
                )
            )}
        </>
    )
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

const Row = (props: {
    confirming: boolean
    entry: LibraryEntry
    onConfirm: (on: boolean) => void
}) => {
    const {confirming, entry, onConfirm} = props
    const editing = $s.libraryEditing === entry.asset
    const [name, setName] = useState(entry.name)
    const [category, setCategory] = useState(entry.category)
    const [attribution, setAttribution] = useState(entry.attribution)

    return (
        <li class="account">
            <div class="account-main">
                <span class="account-name">{entry.name}</span>
                {entry.visibility === 'hidden' && <span class="count">hidden</span>}
                {entry.visibility === 'shared' && <span class="count">unreviewed</span>}
                <span style="flex:1" />
                <button
                    class="ghost"
                    onClick={() => {
                        // Reset the fields on every open, so a form abandoned
                        // half-edited does not reopen holding the abandoned text.
                        setName(entry.name)
                        setCategory(entry.category)
                        setAttribution(entry.attribution)
                        $s.libraryEditing = editing ? null : entry.asset
                    }}
                    type="button"
                >
                    {editing ? 'Close' : 'Edit'}
                </button>
                {entry.visibility === 'hidden' ? (
                    <button class="ghost" onClick={() => void unhideEntry(entry)} type="button">
                        Restore
                    </button>
                ) : confirming ? (
                    <>
                        {/*
                          * Two presses. Hiding a model stops the server serving
                          * the file at all, so every prim in the world wearing it
                          * goes to a placeholder — that is the intended effect and
                          * it is not a small one.
                          */}
                        <button
                            class="danger"
                            onClick={() => {
                                onConfirm(false)
                                void hideEntry(entry)
                            }}
                            type="button"
                        >
                            Take down
                        </button>
                        <button class="ghost" onClick={() => onConfirm(false)} type="button">
                            Cancel
                        </button>
                    </>
                ) : (
                    <button class="ghost" onClick={() => onConfirm(true)} type="button">
                        Take down
                    </button>
                )}
            </div>

            <div class="account-meta">
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
                {entry.uploaders.length > 0 && (
                    <span>uploaded by {entry.uploaders.join(', ')}</span>
                )}
                <span>{formatTimestamp(entry.created_at)}</span>
            </div>

            {/* The credit line a CC-BY model must be shown with, always visible
                rather than only in the edit form: it is the thing most likely to
                be wrong, and a wrong credit is worse than a missing model. */}
            {entry.attribution !== '' && (
                <div class="account-meta">
                    <span>{entry.attribution}</span>
                </div>
            )}

            {editing && (
                <div class="account-edit">
                    <label>
                        Name
                        <input
                            onInput={(e) => setName((e.target as HTMLInputElement).value)}
                            type="text"
                            value={name}
                        />
                    </label>
                    <label>
                        Category
                        <select
                            onChange={(e) => setCategory((e.target as HTMLSelectElement).value)}
                            value={category}
                        >
                            {CATEGORIES.map((c) => (
                                <option key={c} value={c}>
                                    {c}
                                </option>
                            ))}
                        </select>
                    </label>
                    <label>
                        Credit line
                        <input
                            onInput={(e) => setAttribution((e.target as HTMLInputElement).value)}
                            placeholder={
                                entry.licence === 'cc-by'
                                    ? 'Required for CC BY'
                                    : 'Not required for this licence'
                            }
                            type="text"
                            value={attribution}
                        />
                    </label>
                    <div class="account-main">
                        {/*
                          * Saving marks it curated as well as writing the fields.
                          * The two are the same act: an operator who has typed a
                          * name and picked a category has reviewed it, and a
                          * separate "approve" button would be a second click for
                          * a decision already made.
                          */}
                        <button
                            onClick={() => {
                                void curateEntry(entry.asset, {
                                    attribution,
                                    category,
                                    name,
                                    visibility: 'curated',
                                }).then((ok) => {
                                    if (ok) {
                                        $s.libraryEditing = null
                                    }
                                })
                            }}
                            type="button"
                        >
                            Save and approve
                        </button>
                        <button
                            class="ghost"
                            onClick={() => {
                                $s.libraryEditing = null
                            }}
                            type="button"
                        >
                            Cancel
                        </button>
                        <span style="flex:1" />
                        <span class="mono">{entry.asset.slice(0, 16)}</span>
                    </div>
                </div>
            )}
        </li>
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
const Upload = () => {
    const [file, setFile] = useState<File | null>(null)
    const [name, setName] = useState('')
    const [licence, setLicence] = useState('own')
    const [attribution, setAttribution] = useState('')
    const [category, setCategory] = useState('other')
    const [share, setShare] = useState(true)
    const [busy, setBusy] = useState(false)

    const mayShare = licence !== 'other'
    const needsCredit = licence === 'cc-by'

    return (
        <div class="panel">
            <div class="section-head">
                <h1>Add a model</h1>
                <span style="flex:1" />
            </div>
            {/*
              * What the simulator accepts, said before a large upload finds out.
              * Built from `UPLOAD_LIMITS` (see `uploadFormatHint`), which a Rust
              * test pins to the numbers the server enforces.
              */}
            <p class="hint">{uploadFormatHint()}</p>
            <input
                accept={UPLOAD_ACCEPT}
                onChange={(e) => {
                    const picked = (e.target as HTMLInputElement).files?.[0] ?? null
                    setFile(picked)
                    if (picked && name === '') {
                        setName(modelNameFromFile(picked.name))
                    }
                }}
                type="file"
            />
            {file && (
                <>
                    <input
                        onInput={(e) => setName((e.target as HTMLInputElement).value)}
                        placeholder="What is it?"
                        type="text"
                        value={name}
                    />
                    <select
                        onChange={(e) => setLicence((e.target as HTMLSelectElement).value)}
                        value={licence}
                    >
                        <option value="own">I made it</option>
                        <option value="cc0">CC0 — public domain</option>
                        <option value="cc-by">CC BY — credit required</option>
                        <option value="other">Something else</option>
                    </select>
                    {needsCredit && (
                        <input
                            onInput={(e) => setAttribution((e.target as HTMLInputElement).value)}
                            placeholder="Credit line, e.g. “Bench by A. N. Other”"
                            type="text"
                            value={attribution}
                        />
                    )}
                    {mayShare ? (
                        <label>
                            <input
                                checked={share}
                                onChange={(e) => setShare((e.target as HTMLInputElement).checked)}
                                type="checkbox"
                            />
                            Share it in the library
                        </label>
                    ) : (
                        <p class="hint">
                            This can be used on this server and can never be shared or handed on.
                        </p>
                    )}
                    {share && mayShare && (
                        <select
                            onChange={(e) => setCategory((e.target as HTMLSelectElement).value)}
                            value={category}
                        >
                            {CATEGORIES.map((c) => (
                                <option key={c} value={c}>
                                    {c}
                                </option>
                            ))}
                        </select>
                    )}
                    <button
                        disabled={busy}
                        onClick={() => {
                            setBusy(true)
                            void uploadModel(file, {
                                attribution,
                                category,
                                licence,
                                name,
                                share,
                            }).then((ok) => {
                                setBusy(false)
                                if (ok) {
                                    setFile(null)
                                    setName('')
                                    setAttribution('')
                                }
                            })
                        }}
                        type="button"
                    >
                        {busy ? 'Uploading…' : 'Upload'}
                    </button>
                </>
            )}
        </div>
    )
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
const StreetObjects = () => {
    const set = new Map($s.placeholders.map((p) => [p.kind, p]))
    // Only what has actually been published: a placeholder pointing at a model
    // nobody can fetch is a street with nothing in it.
    const usable = $s.library.filter((entry) => entry.visibility !== 'hidden')
    // The one row whose upload form is open, if any.
    const [uploadingFor, setUploadingFor] = useState('')

    return (
        <>
            <div class="section-head">
                <h1>Street objects</h1>
                <span class="count">{$s.placeholders.length} replaced</span>
            </div>
            <ul class="account-list">
                {PLACEHOLDER_KINDS.map((kind) => {
                    const current = set.get(kind)
                    return (
                        <li class="account" key={kind}>
                            <div class="account-main">
                                <span class="account-name">
                                    {PLACEHOLDER_LABELS[kind] ?? kind}
                                </span>
                                <span style="flex:1" />
                                <select
                                    onChange={(e) => {
                                        const asset = (e.target as HTMLSelectElement).value
                                        if (asset === '') {
                                            void clearPlaceholder(kind)
                                            return
                                        }
                                        void setPlaceholder(kind, {
                                            asset,
                                            scale: current?.scale ?? 1,
                                            yaw_offset: current?.yaw_offset ?? 0,
                                        })
                                    }}
                                    value={current?.asset ?? ''}
                                >
                                    <option value="">Built-in shape</option>
                                    {usable.map((entry) => (
                                        <option key={entry.asset} value={entry.asset}>
                                            {entry.name}
                                        </option>
                                    ))}
                                </select>
                                <button
                                    onClick={() => setUploadingFor(uploadingFor === kind ? '' : kind)}
                                    type="button"
                                >
                                    {uploadingFor === kind ? 'Cancel' : 'Upload…'}
                                </button>
                            </div>
                            {uploadingFor === kind && (
                                <StreetUpload kind={kind} onDone={() => setUploadingFor('')} />
                            )}
                            {current && (
                                <div class="account-meta">
                                    {/*
                                      * Scale and angle are here rather than at
                                      * upload because they are about *this* use of
                                      * the model: the same bench may be the right
                                      * size for a prim and the wrong one for a
                                      * street, and a model whose front faces +x
                                      * sits sideways on every pavement in the city
                                      * until this is turned.
                                      */}
                                    <label>
                                        scale
                                        <input
                                            onChange={(e) => {
                                                const scale = Number(
                                                    (e.target as HTMLInputElement).value,
                                                )
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
                                    </label>
                                    <label>
                                        turn°
                                        <input
                                            onChange={(e) => {
                                                const yaw = Number(
                                                    (e.target as HTMLInputElement).value,
                                                )
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
                                    </label>
                                    <span>set by {current.set_by}</span>
                                </div>
                            )}
                        </li>
                    )
                })}
            </ul>
        </>
    )
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

/**
 * Upload a model straight into one street object: it is stored, shared and set
 * as that object's model in one go, at scale 1 and no turn.
 *
 * It is always shared, because a street object is drawn on every client and a
 * model nobody else can fetch is a street with nothing in it — which is also why
 * "something else" is not offered as a licence here: that one can never be
 * shared. The general form above still takes those.
 */
const StreetUpload = ({kind, onDone}: {kind: string; onDone: () => void}) => {
    const [file, setFile] = useState<File | null>(null)
    const [licence, setLicence] = useState('own')
    const [attribution, setAttribution] = useState('')
    const [busy, setBusy] = useState(false)
    const label = PLACEHOLDER_LABELS[kind] ?? kind

    return (
        <div class="account-meta">
            <input
                accept={UPLOAD_ACCEPT}
                onChange={(e) => setFile((e.target as HTMLInputElement).files?.[0] ?? null)}
                type="file"
            />
            <select
                onChange={(e) => setLicence((e.target as HTMLSelectElement).value)}
                value={licence}
            >
                <option value="own">I made it</option>
                <option value="cc0">CC0 — public domain</option>
                <option value="cc-by">CC BY — credit required</option>
            </select>
            {licence === 'cc-by' && (
                <input
                    onInput={(e) => setAttribution((e.target as HTMLInputElement).value)}
                    placeholder="Credit line"
                    type="text"
                    value={attribution}
                />
            )}
            <button
                disabled={busy || !file || (licence === 'cc-by' && attribution.trim() === '')}
                onClick={() => {
                    if (!file) return
                    setBusy(true)
                    void uploadModel(file, {
                        attribution,
                        category: STREET_CATEGORY[kind] ?? 'furniture',
                        licence,
                        name: modelNameFromFile(file.name),
                        share: true,
                    })
                        .then((asset) =>
                            asset ? setPlaceholder(kind, {asset, scale: 1, yaw_offset: 0}) : false,
                        )
                        .then((ok) => {
                            setBusy(false)
                            if (ok) onDone()
                        })
                }}
                type="button"
            >
                {busy ? 'Uploading…' : `Use for ${label.toLowerCase()}`}
            </button>
        </div>
    )
}
