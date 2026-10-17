/**
 * A stand-in for the simulator's and atlas's HTTP surfaces, for
 * `bun run build.ts --serve --mock`.
 *
 * What it is for: running and photographing the web UI on a machine with no
 * simulator, no atlas and no world database — a design review, a screenshot for
 * a session, a layout change checked on a phone. It is not a test double and must
 * not become one: the Rust tests `web_mirror::*` are what check this UI against
 * the real wire, and a mock that the tests leaned on would be a second, unchecked
 * description of the same wire.
 *
 * Every fixture is annotated with the type from `src/types.ts`, so a field
 * renamed there fails `tsc` here rather than drawing a blank column. Everything
 * lives in memory and is reset when the process restarts (which `--watch` does on
 * every save), so a deleted account comes back on the next edit.
 *
 * Auth is deliberately a formality: any username and password signs in, and
 * every session the mock has handed out is an operator holding every
 * permission. The two remembered tokens below are what `?mock-signin` seeds into
 * localStorage (`build.ts`), so a headless browser can open a signed-in page
 * without typing into a form.
 */
import type {
    AccessToken,
    Account,
    BuildingAddress,
    BuildingOverride,
    LibraryEntry,
    OwnedBuilding,
    PermissionInfo,
    PipelineConfig,
    PipelineCoverage,
    PipelineJob,
    PipelineLogLine,
    PlaceholderModel,
    RegistrationRules,
    Role,
    StoredAsset,
} from '@/types'
import type {BuildingDesign, BuildingFacts, BuildingResponse, P2, Storey, Wall} from '@/lib/design'

import {toLatLng} from '@/lib/design'

/** The operator session `?mock-signin` seeds: `viberfox`, an admin. */
export const MOCK_ADMIN_TOKEN = 'mock-admin-token'
/** The player session `?mock-signin=player` seeds: an ordinary owner. */
export const MOCK_PLAYER_TOKEN = 'mock-player-token'
export const MOCK_PLAYER = 'annekedijkstra'

// ── Helpers ─────────────────────────────────────────────────────────────────

function json(body: unknown, status = 200): Response {
    return Response.json(body, {status})
}

/** The simulator's refusal shape: one sentence, for a person (`src/lib/api.ts`). */
function refuse(status: number, error: string): Response {
    return json({error}, status)
}

async function body<T>(req: Request): Promise<Partial<T>> {
    try {
        return (await req.json()) as Partial<T>
    } catch {
        return {}
    }
}

/** SQLite's `datetime('now')`, which is what the simulator's rows carry. */
function sqlNow(): string {
    return new Date().toISOString().slice(0, 19).replace('T', ' ')
}

function sha256(text: string): string {
    return new Bun.CryptoHasher('sha256').update(text).digest('hex')
}

function uuid(): string {
    return crypto.randomUUID()
}

// ── Accounts, sessions, roles ───────────────────────────────────────────────

const accounts: Account[] = [
    {admin: true, created_at: '2026-07-02 08:14:51', disabled: false, email: null, email_pending: false, id: '6f1c2a9e-3b0d-4c55-9a1e-0d2f7c1b8e01', username: 'viberfox'},
    {admin: true, created_at: '2026-07-02 08:20:07', disabled: false, email: null, email_pending: false, id: '0b9d4e37-5a62-4f18-8c3a-7e4d2b6a9f02', username: 'jeroen'},
    {admin: false, created_at: '2026-08-14 19:42:10', disabled: false, email: 'anneke.dijkstra@example.nl', email_pending: false, id: 'a3e85c10-9d4b-4e2f-b6a7-1c0e9f5d3a03', username: 'annekedijkstra'},
    {admin: false, created_at: '2026-08-21 12:03:44', disabled: false, email: 'sjoerd@bakker.example', email_pending: false, id: '4d2a7b91-0e6c-4a3f-9b58-2f1d8c7e6b04', username: 'sjoerdbakker'},
    {admin: false, created_at: '2026-09-02 07:55:19', disabled: true, email: 'marieke.visser@example.nl', email_pending: false, id: 'c71e0f42-8b3d-4d96-a2e5-6b9c1a0f4d05', username: 'mariekevisser'},
    {admin: false, created_at: '2026-09-18 21:31:02', disabled: false, email: 'thijs.mulder@example.nl', email_pending: true, id: '9e5b3c08-2f7a-4b1d-8e64-3a0c5d9b2e06', username: 'thijsmulder'},
    {admin: false, created_at: '2026-09-27 16:48:36', disabled: false, email: 'femke@deboer.example', email_pending: false, id: '2a8f6d14-7c0e-4e5b-91d3-8b2e4f6a0c07', username: 'femkedeboer'},
    {admin: false, created_at: '2026-08-03 10:12:58', disabled: true, email: null, email_pending: false, id: 'e4c09b7a-1d5f-4a28-b3e6-5f7a9c2d1b08', username: 'bramhuisman'},
    {admin: false, created_at: '2026-09-05 09:00:00', disabled: false, email: null, email_pending: false, id: '7b3e1a56-4c9d-4f02-a8b1-9d6e0c3f5a09', username: 'library'},
]

/** token → username. Every session here is an operator (see the module note). */
/** The access tokens `/tokens` lists, one seeded so the page is not empty. */
const accessTokens: (AccessToken & {username: string})[] = [
    {created_at: 1_791_000_000, elevated: false, expires_at: null, id: '3f2a9c41b7e05d18', label: 'deck-2 bot', last_used: 1_791_990_000, username: 'viberfox'},
]

const sessions = new Map<string, string>([
    [MOCK_ADMIN_TOKEN, 'viberfox'],
    [MOCK_PLAYER_TOKEN, MOCK_PLAYER],
])

function sessionOf(req: Request): string | null {
    const header = req.headers.get('authorization') ?? ''
    const token = header.replace(/^Bearer\s+/i, '')
    return sessions.get(token) ?? null
}

function accountNamed(username: string): Account | undefined {
    return accounts.find((a) => a.username === username)
}

/** `cartopolis_core::permissions::ALL`, in its order, with its labels. */
const permissions: PermissionInfo[] = [
    {group: 'prims', label: 'Build prims', player_default: true, slug: 'prim_create'},
    {group: 'prims', label: 'Change and delete their own prims', player_default: true, slug: 'prim_edit_own'},
    {group: 'prims', label: "Change and delete anybody's prims", player_default: false, slug: 'prim_edit_any'},
    {group: 'prims', label: 'Make prims physical', player_default: true, slug: 'prim_physical'},
    {group: 'prims', label: 'Play videos on prims', player_default: true, slug: 'prim_video'},
    {group: 'scripts', label: 'Script their own prims', player_default: true, slug: 'script_edit_own'},
    {group: 'scripts', label: "Script anybody's prims", player_default: false, slug: 'script_edit_any'},
    {group: 'scripts', label: "Read the scripts on other people's prims", player_default: true, slug: 'script_read_any'},
    {group: 'buildings', label: 'Take ownership of a building', player_default: true, slug: 'building_claim'},
    {group: 'buildings', label: 'Draw the inside of their own buildings', player_default: true, slug: 'building_design_own'},
    {group: 'buildings', label: 'Draw the inside of any building', player_default: false, slug: 'building_design_any'},
    {group: 'buildings', label: "Replace a building's outside with a model", player_default: false, slug: 'building_override'},
    {group: 'server', label: 'Install content packs and author prims', player_default: false, slug: 'content_install'},
    {group: 'server', label: 'Curate the model library', player_default: false, slug: 'library_manage'},
    {group: 'server', label: 'Manage accounts', player_default: false, slug: 'accounts_manage'},
    {group: 'server', label: 'Manage roles', player_default: false, slug: 'roles_manage'},
    {group: 'server', label: 'Handle reports and building claims', player_default: false, slug: 'reports_triage'},
    {group: 'server', label: 'Run the map data pipelines', player_default: false, slug: 'pipeline_run'},
]

const roles: Role[] = [
    {
        built_in: true,
        id: 'player',
        members: 0,
        name: 'Player',
        permissions: permissions.filter((p) => p.player_default).map((p) => p.slug),
    },
    {built_in: false, id: 'bouwteam', members: 0, name: 'Bouwteam', permissions: ['building_design_any', 'building_override', 'content_install', 'prim_edit_any']},
    {built_in: false, id: 'curator', members: 0, name: 'Curator', permissions: ['library_manage']},
    {built_in: false, id: 'moderator', members: 0, name: 'Moderator', permissions: ['reports_triage', 'prim_edit_any', 'script_read_any']},
]

/** account id → the roles it holds explicitly. */
const accountRoles = new Map<string, string[]>([
    [accounts[2].id, ['bouwteam']],
    [accounts[3].id, ['moderator']],
    [accounts[6].id, ['bouwteam', 'moderator']],
    [accounts[8].id, ['curator']],
])

/** The role list with live member counts — `player` is held by everyone. */
function roleList(): Role[] {
    return roles.map((r) => ({
        ...r,
        members: r.id === 'player'
            ? accounts.length
            : [...accountRoles.values()].filter((held) => held.includes(r.id)).length,
    }))
}

const registration: RegistrationRules = {
    houses_open: true,
    max_username_length: 32,
    min_password_length: 10,
    open: true,
    verification_valid_hours: 48,
}

// ── Buildings: the register to search, and who owns what ────────────────────

/**
 * Addresses in the centre of Groningen, in the shape `/api/buildings/search`
 * answers. The ids follow the BAG's scheme (municipality 0014, then the object
 * type and a serial); they are not looked up, only shaped like the real thing.
 */
interface RegisterRow extends BuildingAddress {
    pand_id: string
}

const register: RegisterRow[] = [
    ['Oude Boteringestraat 44', '9712 GL', 53.22063, 6.56492, '0014100010903891'],
    ['Oude Boteringestraat 46', '9712 GL', 53.22071, 6.56495, '0014100010903892'],
    ['Grote Markt 1', '9711 LV', 53.21876, 6.56772, '0014100010911204'],
    ['Herestraat 19', '9711 LA', 53.21612, 6.56751, '0014100010897513'],
    ['Folkingestraat 7', '9711 JS', 53.21524, 6.56968, '0014100010886042'],
    ['Zuiderdiep 82', '9711 HM', 53.21460, 6.56930, '0014100010879377'],
    ['Noorderhaven 34', '9712 VJ', 53.22348, 6.55977, '0014100010924810'],
    ['Prinsenstraat 12', '9711 CM', 53.22124, 6.57106, '0014100010917765'],
    ['Akerkhof 3', '9711 JB', 53.21756, 6.56480, '0014100010902218'],
    ['Lopende Diep 21', '9712 NW', 53.22235, 6.56915, '0014100010920139'],
    ['Nieuwe Ebbingestraat 101', '9712 NJ', 53.22581, 6.56842, '0014100010931466'],
    ['Peperstraat 15', '9711 PB', 53.21788, 6.56988, '0014100010908357'],
].map(([street, postcode, latitude, longitude, pand], i) => ({
    address: `${street}, ${postcode} Groningen`,
    adresseerbaarobject_id: `00140100${String(10_903_891 + i * 7919).padStart(8, '0')}`,
    available: true,
    latitude: latitude as number,
    longitude: longitude as number,
    municipality: 'Groningen',
    nummeraanduiding_id: `00142000${String(10_904_112 + i * 6343).padStart(8, '0')}`,
    pand_id: pand as string,
    postcode: postcode as string,
}))

/** Owned buildings, with the owner's username — the operator's view. */
const owned: (OwnedBuilding & {username: string})[] = []

function own(rowIndex: number, username: string, claimedAt: string) {
    const r = register[rowIndex]
    owned.push({
        address: r.address,
        claimed_at: claimedAt,
        latitude: r.latitude,
        longitude: r.longitude,
        nummeraanduiding_id: r.nummeraanduiding_id,
        pand_id: r.pand_id,
        postcode: r.postcode,
        username,
    })
}

own(0, 'annekedijkstra', '2026-08-15 08:02:13')
own(7, 'annekedijkstra', '2026-09-30 17:26:40')
own(3, 'sjoerdbakker', '2026-08-22 14:11:05')
own(6, 'femkedeboer', '2026-09-28 10:45:57')
own(2, 'viberfox', '2026-09-11 09:30:00')

const BUILDING_LIMIT = 3

// ── The floor-plan editor's building ────────────────────────────────────────

/**
 * One L-shaped Groningen town house, two storeys, in metres east/north of
 * `ORIGIN`: the same shape answers for every pand, which is all a screenshot
 * needs. The exterior walls sit 0.22 m inside the outline, as the generator puts
 * them (`building_design::WALL_INSET`).
 */
const ORIGIN: [number, number] = [53.22063, 6.56492]
const OUTLINE_M: P2[] = [[0, 0], [12, 0], [12, 7], [6.5, 7], [6.5, 15], [0, 15]]
const RING: P2[] = [[0.22, 0.22], [11.78, 0.22], [11.78, 6.78], [6.28, 6.78], [6.28, 14.78], [0.22, 14.78]]

function exteriorWalls(prefix: string, ground: boolean): Wall[] {
    return RING.map((a, i) => {
        const b = RING[(i + 1) % RING.length]
        const openings: Wall['openings'] = []
        if (i === 0) {
            // The south front: a front door on the ground floor, windows either side.
            if (ground) {
                openings.push({at: 3.0, height: 2.3, id: `${prefix}d-front`, kind: 'door', sill: 0, width: 1.0})
            } else {
                openings.push({at: 3.0, height: 1.3, id: `${prefix}w-front`, kind: 'window', sill: 0.9, width: 1.2})
            }
            openings.push({at: 8.8, height: 1.3, id: `${prefix}w-south`, kind: 'window', sill: 0.9, width: 1.6})
        } else if (i === 1) {
            openings.push({at: 3.3, height: 1.3, id: `${prefix}w-east`, kind: 'window', sill: 0.9, width: 1.2})
        } else if (i === 3) {
            openings.push({at: 4.0, height: 1.3, id: `${prefix}w-yard`, kind: 'window', sill: 0.9, width: 1.2})
        } else if (i === 4) {
            openings.push({at: 3.0, height: 1.3, id: `${prefix}w-north`, kind: 'window', sill: 0.9, width: 1.2})
        } else if (i === 5) {
            openings.push({at: 7.0, height: 1.3, id: `${prefix}w-west`, kind: 'window', sill: 0.9, width: 1.2})
        }
        return {a, b, exterior: true, id: `${prefix}x${i}`, openings, thickness: 0}
    })
}

function rect(x0: number, y0: number, x1: number, y1: number): P2[] {
    return [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

function generatedDesign(pand: string): BuildingDesign {
    const ground: Storey = {
        elevation: 0,
        floor: RING,
        height: 3.2,
        id: 's0',
        name: 'Begane grond',
        rooms: [
            {id: 'r-woonkamer', name: 'Woonkamer', outline: rect(0.22, 0.22, 6.28, 10.5)},
            {id: 'r-keuken', name: 'Keuken', outline: rect(6.28, 0.22, 11.78, 6.78)},
            {id: 'r-werkkamer', name: 'Werkkamer', outline: rect(0.22, 10.5, 6.28, 14.78)},
        ],
        stairs: [{dir: [0, 1], foot: [1.5, 5.2], id: 'st0', run: 3.4, width: 0.95}],
        walls: [
            ...exteriorWalls('g', true),
            {
                a: [6.28, 0.22],
                b: [6.28, 6.78],
                exterior: false,
                id: 'g-i0',
                openings: [{at: 4.6, height: 2.1, id: 'g-d0', kind: 'door', sill: 0, width: 0.9}],
                thickness: 0.1,
            },
            {
                a: [0.22, 10.5],
                b: [6.28, 10.5],
                exterior: false,
                id: 'g-i1',
                openings: [{at: 4.2, height: 2.1, id: 'g-d1', kind: 'door', sill: 0, width: 0.9}],
                thickness: 0.1,
            },
        ],
    }
    const first: Storey = {
        elevation: 3.2,
        floor: RING,
        height: 3.0,
        id: 's1',
        name: 'Eerste verdieping',
        rooms: [
            {id: 'r-slaapkamer', name: 'Slaapkamer', outline: rect(0.22, 0.22, 6.28, 9.0)},
            {id: 'r-badkamer', name: 'Badkamer', outline: rect(6.28, 0.22, 11.78, 6.78)},
        ],
        stairs: [],
        walls: [
            ...exteriorWalls('f', false),
            {
                a: [6.28, 0.22],
                b: [6.28, 6.78],
                exterior: false,
                id: 'f-i0',
                openings: [{at: 4.6, height: 2.1, id: 'f-d0', kind: 'door', sill: 0, width: 0.8}],
                thickness: 0.1,
            },
            {
                a: [0.22, 9.0],
                b: [6.28, 9.0],
                exterior: false,
                id: 'f-i1',
                openings: [{at: 3.5, height: 2.1, id: 'f-d1', kind: 'door', sill: 0, width: 0.9}],
                thickness: 0.1,
            },
        ],
    }
    return {origin: ORIGIN, pand, revision: 0, source: 'generated', storeys: [ground, first], version: 1}
}

function latLngRing(ring: P2[]): [number, number][] {
    return ring.map((p) => toLatLng(ORIGIN, p))
}

function facts(pand: string): BuildingFacts {
    return {
        doors: [toLatLng(ORIGIN, [3.0, 0])],
        height: 7.4,
        neighbours: [
            latLngRing(rect(-7.5, 0, 0, 14)),
            latLngRing(rect(12, 0, 19, 9)),
            latLngRing(rect(6.5, 8.5, 12, 15)),
        ],
        outline: latLngRing(OUTLINE_M),
        pand,
    }
}

/** Designs an owner has saved, by pand. */
const savedDesigns = new Map<string, BuildingDesign>()

// ── The model library ───────────────────────────────────────────────────────

function libraryEntry(
    name: string,
    category: string,
    licence: string,
    extra: Partial<LibraryEntry> = {},
): LibraryEntry {
    return {
        asset: sha256(name),
        attribution: licence === 'cc-by' ? `“${name}” by Poly by Google, CC BY 3.0` : '',
        byte_size: 180_000 + (name.length * 37_411) % 2_400_000,
        category,
        created_at: '2026-09-06 11:20:00',
        held_by: 1 + (name.length % 5),
        in_world: name.length % 4,
        licence,
        name,
        shared_by: 'library',
        tags: [category],
        triangles: 1_200 + (name.length * 9_733) % 60_000,
        uploaders: ['library'],
        visibility: 'curated',
        ...extra,
    }
}

const library: LibraryEntry[] = [
    libraryEntry('Groninger lantaarnpaal', 'furniture', 'own', {held_by: 3, in_world: 0, tags: ['street', 'lamp'], uploaders: ['library', 'viberfox']}),
    libraryEntry('Parkbank (hout)', 'furniture', 'cc0', {tags: ['bench', 'park']}),
    libraryEntry('Fietsenrek, 8 plaatsen', 'furniture', 'cc0', {tags: ['bike', 'street']}),
    libraryEntry('Omafiets', 'vehicles', 'cc-by', {tags: ['bicycle']}),
    libraryEntry('Lindeboom', 'vegetation', 'cc0', {tags: ['tree']}),
    libraryEntry('Marktkraam', 'decoration', 'own', {shared_by: 'femkedeboer', tags: ['market', 'grote markt'], uploaders: ['femkedeboer'], visibility: 'shared'}),
    libraryEntry('Woonboot', 'buildings', 'cc-by', {shared_by: 'sjoerdbakker', uploaders: ['sjoerdbakker'], visibility: 'shared'}),
    libraryEntry('Reclamezuil', 'signage', 'other', {held_by: 1, in_world: 2, shared_by: 'bramhuisman', uploaders: ['bramhuisman'], visibility: 'hidden'}),
]

const assets: StoredAsset[] = [
    ...library.map((e, i) => ({
        byte_size: e.byte_size,
        created_at: e.created_at,
        hash: e.asset,
        held_by: e.held_by,
        in_world: e.in_world,
        largest_texture: [512, 1024, 2048, 0][i % 4],
        nodes: 3 + i * 4,
        shared: true,
        textures: i % 4 === 3 ? 0 : 1 + (i % 3),
        triangles: e.triangles,
    })),
    // Orphans: files nothing in the catalogue or the world points at any more.
    {byte_size: 2_843_112, created_at: '2026-08-29 22:14:07', hash: sha256('orphan-scooter'), held_by: 0, in_world: 0, largest_texture: 2048, nodes: 41, shared: false, textures: 3, triangles: 48_210},
    {byte_size: 96_440, created_at: '2026-09-12 13:02:51', hash: sha256('orphan-crate'), held_by: 1, in_world: 0, largest_texture: 256, nodes: 2, shared: false, textures: 1, triangles: 412},
]

const placeholders: PlaceholderModel[] = [
    {asset: library[0].asset, kind: 'lamp', scale: 1, set_by: 'viberfox', yaw_offset: 0},
    {asset: library[1].asset, kind: 'bench', scale: 0.95, set_by: 'viberfox', yaw_offset: 90},
    {asset: library[2].asset, kind: 'bike-rack', scale: 1, set_by: 'jeroen', yaw_offset: 0},
]

/** Inventory ids the upload route hands out, mapped to the hash they hold. */
const inventory = new Map<number, {asset: string; licence: string; name: string; uploader: string}>()
let nextInventory = 400

// ── The data pipeline ───────────────────────────────────────────────────────

const pipelineConfig: PipelineConfig = {
    areas: [
        {bbox: [229_000, 578_000, 238_000, 587_000], id: 'groningen', km2: 81, label: 'Groningen'},
        {bbox: [113_500, 481_500, 130_000, 493_500], id: 'amsterdam', km2: 198, label: 'Amsterdam'},
        {bbox: [87_000, 431_500, 101_000, 442_500], id: 'rotterdam', km2: 154, label: 'Rotterdam'},
        {bbox: [130_500, 452_500, 141_000, 460_000], id: 'utrecht', km2: 78.75, label: 'Utrecht'},
        {bbox: [178_500, 576_000, 186_500, 582_500], id: 'leeuwarden', km2: 52, label: 'Leeuwarden'},
    ],
    datasets: [
        {description: 'The height each canal, river and pond actually stands at, from the Kadaster 3D basisvoorziening.', id: 'water', label: 'Water levels', typicalMinutes: 1},
        {description: 'Footprints and heights from the 3DBAG WFS. The fallback tier under the LoD2.2 meshes.', id: 'buildings', label: 'Buildings (LoD1.2)', typicalMinutes: 1},
        {description: 'Real roof and wall geometry, triangulated from 3DBAG CityJSON.', id: 'lod22', label: 'Roof meshes (LoD2.2)', typicalMinutes: 1},
        {description: 'What each LoD2.2 roof is made of and what colour it is, read off the aerial photograph.', id: 'roofs', label: 'Roof colours (aerial photo)', typicalMinutes: 10},
        {description: 'Year built, use, floor areas, addresses, entrance points, houseboat berths.', id: 'bag', label: 'Building facts (BAG)', typicalMinutes: 1},
        {description: 'The dikes, dunes and dams the water boards register, as named lines.', id: 'dikes', label: 'Flood defences (IMWA)', typicalMinutes: 1},
        {description: 'Surveyed trees, lampposts, benches, fences, hedges and the paving beside the road.', id: 'bgt', label: 'Street objects (BGT)', typicalMinutes: 1},
        {description: 'Every agricultural field with the crop grown on it this year.', id: 'brp', label: 'Crop parcels (BRP)', typicalMinutes: 1},
        {description: 'Cuts every z14 height tile over the area into the terrain store.', id: 'terrain', label: 'Terrain tiles (AHN)', typicalMinutes: 1},
    ],
    out: '/data/cartopolis/tiles',
}

const GRONINGEN_BBOX = pipelineConfig.areas[0].bbox
const DAY = 86_400_000
const now = Date.now()

const coverage: PipelineCoverage[] = [
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'water bodies', n: 1_284}], dataset: 'water', label: 'Water levels', name: 'groningen', updatedAt: now - 9 * DAY},
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'buildings', n: 94_052}], dataset: 'buildings', label: 'Buildings (LoD1.2)', name: 'groningen', updatedAt: now - 9 * DAY},
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'buildings', n: 93_877}, {label: 'triangles', n: 11_402_316}], dataset: 'lod22', label: 'Roof meshes (LoD2.2)', name: 'groningen', updatedAt: now - 8 * DAY},
    {bbox: null, cells: 0, counts: [], dataset: 'roofs', label: 'Roof colours (aerial photo)', name: null, updatedAt: null},
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'buildings', n: 94_118}, {label: 'addresses', n: 131_540}, {label: 'berths', n: 412}], dataset: 'bag', label: 'Building facts (BAG)', name: 'groningen', updatedAt: now - 2 * DAY},
    {bbox: GRONINGEN_BBOX, cells: 21, counts: [{label: 'flood defences', n: 87}], dataset: 'dikes', label: 'Flood defences (IMWA)', name: 'groningen', updatedAt: now - 30 * DAY},
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'trees', n: 61_204}, {label: 'lampposts', n: 18_931}, {label: 'paving polygons', n: 240_118}], dataset: 'bgt', label: 'Street objects (BGT)', name: 'groningen', updatedAt: now - 1 * DAY},
    {bbox: null, cells: 0, counts: [], dataset: 'brp', label: 'Crop parcels (BRP)', name: null, updatedAt: null},
    {bbox: GRONINGEN_BBOX, cells: 36, counts: [{label: 'height tiles', n: 36}], dataset: 'terrain', label: 'Terrain tiles (AHN)', name: 'groningen', updatedAt: now - 12 * DAY},
]

const started = Date.now()

/** The run that is already going when the mock starts, and never finishes. */
const SEEDED_RUNNING = 'job-20261009-0812'

const jobs: PipelineJob[] = [
    {
        area: 'groningen',
        bbox: GRONINGEN_BBOX,
        by: 'viberfox',
        createdAt: started - 95_000,
        id: 'job-20261009-0812',
        startedAt: started - 94_000,
        status: 'running',
        steps: [
            {counts: [{label: 'buildings', n: 94_118}], dataset: 'bag', finishedAt: started - 40_000, progress: {done: 36, phase: 'cells', total: 36}, startedAt: started - 94_000, status: 'done'},
            {dataset: 'bgt', progress: {done: 14, phase: 'paving pages', total: 36}, startedAt: started - 40_000, status: 'running'},
            {dataset: 'terrain', progress: {}, status: 'queued'},
        ],
    },
    {
        area: 'groningen',
        bbox: GRONINGEN_BBOX,
        by: 'jeroen',
        createdAt: now - 1 * DAY - 3_600_000,
        finishedAt: now - 1 * DAY - 2_900_000,
        id: 'job-20261008-0705',
        startedAt: now - 1 * DAY - 3_590_000,
        status: 'done',
        steps: [
            {counts: [{label: 'trees', n: 61_204}, {label: 'lampposts', n: 18_931}], dataset: 'bgt', finishedAt: now - 1 * DAY - 2_900_000, progress: {done: 36, total: 36}, startedAt: now - 1 * DAY - 3_590_000, status: 'done'},
        ],
    },
    {
        area: 'leeuwarden',
        bbox: pipelineConfig.areas[4].bbox,
        by: 'viberfox',
        createdAt: now - 3 * DAY,
        finishedAt: now - 3 * DAY + 210_000,
        id: 'job-20261006-1532',
        startedAt: now - 3 * DAY + 2_000,
        status: 'failed',
        steps: [
            {counts: [{label: 'water bodies', n: 803}], dataset: 'water', finishedAt: now - 3 * DAY + 61_000, progress: {done: 20, total: 20}, startedAt: now - 3 * DAY + 2_000, status: 'done'},
            {dataset: 'lod22', error: 'the 3DBAG tile server answered 503 Service Unavailable for tile 9-284-556 three times', finishedAt: now - 3 * DAY + 210_000, progress: {done: 7, phase: 'tiles', total: 20}, startedAt: now - 3 * DAY + 61_000, status: 'failed'},
        ],
    },
    {
        area: 'groningen',
        bbox: GRONINGEN_BBOX,
        by: 'viberfox',
        createdAt: now - 5 * DAY,
        finishedAt: now - 5 * DAY + 45_000,
        id: 'job-20261004-0918',
        startedAt: now - 5 * DAY + 1_000,
        status: 'cancelled',
        steps: [
            {dataset: 'roofs', finishedAt: now - 5 * DAY + 45_000, progress: {done: 2, phase: 'aerial tiles', total: 36}, startedAt: now - 5 * DAY + 1_000, status: 'cancelled'},
        ],
    },
]

/** Each job's log, as the server keeps it. */
const logs = new Map<string, PipelineLogLine[]>()

function seedLog(job: PipelineJob, lines: [string, string][], base: number) {
    logs.set(job.id, lines.map(([dataset, text], i) => ({at: base + i * 900, dataset, text})))
}

seedLog(jobs[0], [
    ['bag', 'area groningen, bbox 229000,578000 → 238000,587000 (EPSG:28992), 36 z14 cells'],
    ['bag', 'pdok bag wfs: paging at 1000 features'],
    ['bag', 'cell 14/8434/5379: 3,912 panden, 5,604 verblijfsobjecten'],
    ['bag', 'cell 14/8435/5379: 4,187 panden, 6,019 verblijfsobjecten'],
    ['bag', 'cell 14/8434/5380: 2,766 panden, 3,340 verblijfsobjecten, 41 ligplaatsen'],
    ['bag', '… 33 more cells'],
    ['bag', 'wrote bag/coverage.json: 94,118 buildings, 131,540 addresses, 412 berths'],
    ['bgt', 'bgt: 36 cells, collections pand, vegetatieobject, paal, ondersteunendwegdeel'],
    ['bgt', 'cell 14/8434/5379: 2,104 trees, 611 lampposts'],
    ['bgt', 'cell 14/8434/5379: ondersteunendwegdeel 77,202 vertices → 4,792 after 10 cm simplification'],
], started - 94_000)

/** Lines the running job adds as time passes, so a followed log visibly moves. */
const LIVE_LINES = [
    'cell 14/8435/5379: 1,877 trees, 540 lampposts',
    'cell 14/8435/5379: skipped 312 terminated records (termination_date set)',
    'cell 14/8436/5379: 1,402 trees, 488 lampposts',
    'cell 14/8436/5380: paving 61,388 vertices → 3,915 after simplification',
    'cell 14/8433/5380: 2,551 trees, 702 lampposts',
    'cell 14/8434/5381: page 3 of ondersteunendwegdeel',
]

function liveLog(job: PipelineJob): PipelineLogLine[] {
    const lines = logs.get(job.id) ?? []
    if (job.status !== 'running') {
        return lines
    }
    if (job.id !== SEEDED_RUNNING) {
        // A run started from the page finishes twenty seconds later, so the
        // whole cycle — start, follow, done, coverage reloaded — can be watched.
        const at = Date.now()
        if (at - (job.startedAt ?? at) > 20_000) {
            job.status = 'done'
            job.finishedAt = at
            for (const step of job.steps) {
                step.status = 'done'
                step.finishedAt = at
                step.progress = {done: 36, total: 36}
                step.counts = [{label: 'cells', n: 36}]
                lines.push({at, dataset: step.dataset, text: `${step.dataset}: wrote 36 cells`})
            }
        }
        return lines
    }
    // One extra line every three seconds since the mock started, up to the
    // handful there are: enough for a follow to show movement, finite so a
    // screenshot taken later is the same picture.
    const extra = Math.min(LIVE_LINES.length, Math.floor((Date.now() - started) / 3_000))
    const step = job.steps[1]
    step.progress = {done: Math.min(36, 14 + extra), phase: 'paving pages', total: 36}
    return [
        ...lines,
        ...LIVE_LINES.slice(0, extra).map((text, i) => ({at: started + (i + 1) * 3_000, dataset: 'bgt', text})),
    ]
}

const overrides: BuildingOverride[] = [
    {
        author: 'viberfox',
        cell: [8436, 5381],
        collision: 'parts',
        interior: 'none',
        name: 'Euroborg',
        pand: '0014100010967342',
        placement: {base_m: 0, heading_deg: 12.5, lat: null, lng: null, scale: 1},
        revision: 1_790_412_331_000,
        source: {kind: 'stadium', pitch: [[53.20652, 6.59241], [53.20591, 6.59502], [53.20470, 6.59440], [53.20531, 6.59179]]},
    },
    {
        author: 'jeroen',
        cell: [8434, 5379],
        collision: 'walls',
        interior: 'design',
        name: 'Martinitoren',
        pand: '0014100010911207',
        placement: {base_m: -0.4, heading_deg: 0, lat: 53.21933, lng: 6.56831, scale: 1},
        revision: 1_789_201_904_000,
        source: {kind: 'model', sha256: sha256('martinitoren.glb'), triangles: 184_512},
    },
]

// ── Routing ─────────────────────────────────────────────────────────────────

/** Whether the bearer belongs to a mock session; a 401 otherwise. */
function needSession(req: Request): string | Response {
    return sessionOf(req) ?? refuse(401, 'Sign in first')
}

/**
 * Answer one `/api/*` request, or `null` when the mock does not know the route
 * (`build.ts` turns that into a 404).
 */
/** The server's report queue: reports about a person, filed from the app. */
const userReports = [
    {
        created_at: '2026-10-10 21:14:02', id: 3, message: 'Buy my watches at cheap-watches dot biz', note: '',
        reason: 'spam', reporter_username: 'annekedijkstra', resolution: null, resolved: false, resolved_at: null,
        subject_username: 'robverkoper',
    },
    {
        created_at: '2026-10-09 08:40:51', id: 2, message: null, note: 'Keeps following me around the Grote Markt and shouting.',
        reason: 'harassment', reporter_username: 'femkedeboer', resolution: null, resolved: false, resolved_at: null,
        subject_username: 'thijsmulder',
    },
    {
        created_at: '2026-09-30 17:02:11', id: 1, message: 'lol', note: '', reason: 'other', reporter_username: null,
        resolution: 'Nothing wrong with it', resolved: true, resolved_at: '2026-10-01 09:00:00', subject_username: 'sjoerdbakker',
    },
]

export async function mockApi(req: Request, url: URL): Promise<Response | null> {
    const p = url.pathname
    const method = req.method
    const parts = p.split('/').filter(Boolean) // ['api', …]

    // ── Sessions and registration (unauthenticated) ────────────────────────
    if ((p === '/api/login' || p === '/api/admin/login') && method === 'POST') {
        const {username} = await body<{username: string}>(req)
        const name = (username ?? '').trim() || 'viberfox'
        const account = accountNamed(name)
        const token = `mock-${uuid()}`
        sessions.set(token, name)
        return json({principal: account?.id ?? uuid(), token, username: name})
    }
    if ((p === '/api/logout' || p === '/api/admin/logout') && method === 'POST') {
        // The two seeded tokens survive a sign-out, so `?mock-signin` keeps working.
        const token = (req.headers.get('authorization') ?? '').replace(/^Bearer\s+/i, '')
        if (token !== MOCK_ADMIN_TOKEN && token !== MOCK_PLAYER_TOKEN) {
            sessions.delete(token)
        }
        return new Response(null, {status: 204})
    }
    if (p === '/api/register' && method === 'GET') {
        return json(registration)
    }
    if (p === '/api/register' && method === 'POST') {
        const b = await body<{email: string; password: string; username: string}>(req)
        const username = (b.username ?? '').trim()
        if (!username) {
            return refuse(400, 'Pick a username')
        }
        if (accountNamed(username)) {
            return refuse(409, 'That username is taken')
        }
        accounts.push({admin: false, created_at: sqlNow(), disabled: false, email: b.email ?? null, email_pending: true, id: uuid(), username})
        return json({ok: true})
    }
    if (p === '/api/register/verify' && method === 'POST') {
        // Any token confirms the newest pending registration — there is no mail.
        const pending = [...accounts].reverse().find((a) => a.email_pending)
        if (!pending) {
            return refuse(400, 'That link is not valid, or has already been used')
        }
        pending.email_pending = false
        const token = `mock-${uuid()}`
        sessions.set(token, pending.username)
        return json({token, username: pending.username})
    }
    if (p === '/api/register/resend' && method === 'POST') {
        return json({ok: true})
    }

    // ── Read by every client, signed in or not ─────────────────────────────
    if (p === '/api/library/placeholders' && method === 'GET') {
        return json(placeholders)
    }

    // Everything below needs a session.
    const who = needSession(req)
    if (who instanceof Response) {
        return who
    }

    // ── The player's buildings ─────────────────────────────────────────────
    if (p === '/api/buildings/search' && method === 'GET') {
        const q = (url.searchParams.get('q') ?? '').trim().toLowerCase()
        const results: BuildingAddress[] = register
            .filter((r) => r.address.toLowerCase().includes(q) || (r.postcode ?? '').toLowerCase().replace(' ', '').includes(q.replace(' ', '')))
            .slice(0, 8)
            .map(({pand_id, ...r}) => ({...r, available: !owned.some((o) => o.pand_id === pand_id)}))
        return json({results})
    }
    if (p === '/api/buildings/claim' && method === 'POST') {
        const {nummeraanduiding_id} = await body<{nummeraanduiding_id: string}>(req)
        const index = register.findIndex((r) => r.nummeraanduiding_id === nummeraanduiding_id)
        if (index < 0) {
            return refuse(404, 'That address is not in the register')
        }
        if (owned.some((o) => o.pand_id === register[index].pand_id)) {
            return refuse(409, 'Somebody already owns that building')
        }
        if (owned.filter((o) => o.username === who).length >= BUILDING_LIMIT) {
            return refuse(409, `You already own ${BUILDING_LIMIT} buildings, which is the most one account may hold`)
        }
        own(index, who, sqlNow())
        const {username: _, ...building} = owned[owned.length - 1]
        return json({building})
    }
    if (p === '/api/buildings/mine' && method === 'GET') {
        const buildings: OwnedBuilding[] = owned
            .filter((o) => o.username === who)
            .map(({username: _, ...b}) => b)
        return json({buildings, limit: BUILDING_LIMIT})
    }
    if (parts[1] === 'buildings' && parts.length === 3 && method === 'DELETE') {
        const i = owned.findIndex((o) => o.pand_id === parts[2] && o.username === who)
        if (i < 0) {
            return refuse(404, 'You do not own that building')
        }
        owned.splice(i, 1)
        return new Response(null, {status: 204})
    }

    // ── Access tokens (`/tokens`) ────────────────────────────────────────────
    // Any password confirms, as any password signs in: the mock checks shapes,
    // not secrets. The secret is shown once and kept nowhere, as on the server.
    if (p === '/api/tokens' && method === 'GET') {
        return json({tokens: accessTokens.filter((t) => t.username === who).map(({username: _, ...t}) => t)})
    }
    if (p === '/api/tokens' && method === 'POST') {
        const body = (await req.json()) as {elevated?: boolean; expires_at?: number | null; label?: string; password?: string}
        if (!body.password) {
            return refuse(403, 'that password is not right')
        }
        const label = (body.label ?? '').trim()
        if (!label || label.length > 60) {
            return refuse(400, 'give the token a name of 1 to 60 characters')
        }
        const info = {
            created_at: Math.floor(Date.now() / 1000),
            elevated: Boolean(body.elevated),
            expires_at: body.expires_at ?? null,
            id: uuid().replaceAll('-', '').slice(0, 16),
            label,
            last_used: null,
        }
        accessTokens.unshift({...info, username: who})
        return json({info, token: sha256(`${info.id}:${Date.now()}`)})
    }
    if (parts[1] === 'tokens' && parts.length === 3 && method === 'DELETE') {
        const i = accessTokens.findIndex((t) => t.id === parts[2] && t.username === who)
        if (i < 0) {
            return refuse(404, 'no such token')
        }
        accessTokens.splice(i, 1)
        return new Response(null, {status: 204})
    }

    // ── Uploads (the player route the library page posts to) ───────────────
    if (p === '/api/assets' && method === 'POST') {
        const bytes = await req.arrayBuffer()
        const name = url.searchParams.get('name') || 'Uploaded model'
        const hash = sha256(`${name}:${bytes.byteLength}:${Date.now()}`)
        assets.unshift({byte_size: bytes.byteLength, created_at: sqlNow(), hash, held_by: 1, in_world: 0, largest_texture: 1024, nodes: 6, shared: false, textures: 1, triangles: 3_480})
        const id = nextInventory++
        inventory.set(id, {asset: hash, licence: url.searchParams.get('licence') || 'own', name, uploader: who})
        return json({asset: hash, id})
    }
    if (parts[1] === 'inventory' && parts[3] === 'share' && method === 'POST') {
        const item = inventory.get(Number(parts[2]))
        if (!item) {
            return refuse(404, 'No such inventory item')
        }
        const b = await body<{category: string; name: string}>(req)
        const stored = assets.find((a) => a.hash === item.asset)
        if (stored) {
            stored.shared = true
        }
        library.unshift({
            asset: item.asset,
            attribution: '',
            byte_size: stored?.byte_size ?? 0,
            category: b.category ?? 'other',
            created_at: sqlNow(),
            held_by: 1,
            in_world: 0,
            licence: item.licence,
            name: b.name ?? item.name,
            shared_by: item.uploader,
            tags: [],
            triangles: stored?.triangles ?? 0,
            uploaders: [item.uploader],
            visibility: 'shared',
        })
        return json({ok: true})
    }

    // ── The operator's surface ─────────────────────────────────────────────
    if (p === '/api/admin/me' && method === 'GET') {
        return json({
            admin: true,
            permissions: permissions.map((x) => x.slug),
            principal: accountNamed(who)?.id ?? '',
            username: who,
        })
    }
    if (p === '/api/admin/accounts' && method === 'GET') {
        return json({accounts, roles: Object.fromEntries(accountRoles)})
    }
    if (p === '/api/admin/accounts' && method === 'POST') {
        const b = await body<{admin: boolean; password: string; username: string}>(req)
        const username = (b.username ?? '').trim()
        if (!username) {
            return refuse(400, 'A username is required')
        }
        if (accountNamed(username)) {
            return refuse(409, `${username} is taken`)
        }
        if ((b.password ?? '').length < registration.min_password_length) {
            return refuse(400, `The password needs at least ${registration.min_password_length} characters`)
        }
        const account: Account = {admin: Boolean(b.admin), created_at: sqlNow(), disabled: false, email: null, email_pending: false, id: uuid(), username}
        accounts.push(account)
        return json(account, 201)
    }
    if (parts[1] === 'admin' && parts[2] === 'accounts' && parts.length >= 4) {
        const account = accounts.find((a) => a.id === parts[3])
        if (!account) {
            return refuse(404, 'No such account')
        }
        const lastAdmin = account.admin && !account.disabled &&
            accounts.filter((a) => a.admin && !a.disabled).length === 1
        if (parts.length === 4 && method === 'PATCH') {
            const b = await body<{admin: boolean; disabled: boolean; password: string}>(req)
            if (lastAdmin && (b.admin === false || b.disabled === true)) {
                return refuse(409, 'That is the last enabled admin — promote someone else first')
            }
            if (b.admin !== undefined) {
                account.admin = b.admin
            }
            if (b.disabled !== undefined) {
                account.disabled = b.disabled
            }
            return json(account)
        }
        if (parts.length === 4 && method === 'DELETE') {
            if (account.username === who) {
                return refuse(409, 'You cannot delete the account you are signed in with')
            }
            if (lastAdmin) {
                return refuse(409, 'That is the last enabled admin — promote someone else first')
            }
            accounts.splice(accounts.indexOf(account), 1)
            accountRoles.delete(account.id)
            return new Response(null, {status: 204})
        }
        if (parts[4] === 'activate' && method === 'POST') {
            account.email_pending = false
            return json({mailed: account.email !== null})
        }
        if (parts[4] === 'roles' && method === 'PUT') {
            const b = await body<{roles: string[]}>(req)
            const held = (b.roles ?? []).filter((r) => r !== 'player' && roles.some((x) => x.id === r))
            accountRoles.set(account.id, held)
            return json({roles: held})
        }
    }
    if (p === '/api/admin/permissions' && method === 'GET') {
        return json({permissions})
    }
    if (p === '/api/admin/roles' && method === 'GET') {
        return json({roles: roleList()})
    }
    if (p === '/api/admin/roles' && method === 'POST') {
        const b = await body<{id: string; name: string; permissions: string[]}>(req)
        if (!b.id || roles.some((r) => r.id === b.id)) {
            return refuse(409, `A role called ${b.id ?? '(nothing)'} already exists`)
        }
        roles.push({built_in: false, id: b.id, members: 0, name: b.name ?? b.id, permissions: b.permissions ?? []})
        return json({roles: roleList()})
    }
    if (parts[1] === 'admin' && parts[2] === 'roles' && parts.length === 4) {
        const role = roles.find((r) => r.id === parts[3])
        if (!role) {
            return refuse(404, 'No such role')
        }
        if (method === 'PATCH') {
            const b = await body<{name: string; permissions: string[]}>(req)
            if (b.name && !role.built_in) {
                role.name = b.name
            }
            if (b.permissions) {
                role.permissions = b.permissions
            }
            return json({roles: roleList()})
        }
        if (method === 'DELETE') {
            if (role.built_in) {
                return refuse(409, 'The player role is built in and cannot be deleted')
            }
            roles.splice(roles.indexOf(role), 1)
            for (const [id, held] of accountRoles) {
                accountRoles.set(id, held.filter((r) => r !== role.id))
            }
            return json({roles: roleList()})
        }
    }
    if (p === '/api/admin/reports' && method === 'GET') {
        return json(userReports.filter((r) => url.searchParams.get('all') === 'true' || !r.resolved))
    }
    if (parts[1] === 'admin' && parts[2] === 'reports' && parts.length === 4 && method === 'PATCH') {
        const r = userReports.find((x) => x.id === Number(parts[3]))
        if (!r) {
            return refuse(404, 'no such report')
        }
        const b = await body<{resolution: string}>(req)
        r.resolved = true
        r.resolved_at = sqlNow()
        r.resolution = b.resolution ?? ''
        return new Response(null, {status: 204})
    }
    if (p === '/api/admin/claims' && method === 'GET') {
        return json({buildings: owned})
    }
    if (parts[1] === 'admin' && parts[2] === 'claims' && parts.length === 4 && method === 'DELETE') {
        const i = owned.findIndex((o) => o.pand_id === parts[3])
        if (i < 0) {
            return refuse(404, 'Nobody owns that building')
        }
        owned.splice(i, 1)
        return new Response(null, {status: 204})
    }
    if (p === '/api/admin/library' && method === 'GET') {
        return json(library)
    }
    if (p === '/api/admin/assets' && method === 'GET') {
        return json(assets)
    }
    if (parts[1] === 'admin' && parts[2] === 'library' && parts[3] === 'placeholders' && parts.length === 5) {
        const kind = parts[4]
        if (method === 'PUT') {
            const b = await body<{asset: string; scale: number; yaw_offset: number}>(req)
            const entry: PlaceholderModel = {asset: b.asset ?? '', kind, scale: b.scale ?? 1, set_by: who, yaw_offset: b.yaw_offset ?? 0}
            const i = placeholders.findIndex((x) => x.kind === kind)
            if (i >= 0) {
                placeholders[i] = entry
            } else {
                placeholders.push(entry)
            }
            return json(entry)
        }
        if (method === 'DELETE') {
            const i = placeholders.findIndex((x) => x.kind === kind)
            if (i >= 0) {
                placeholders.splice(i, 1)
            }
            return new Response(null, {status: 204})
        }
    }
    if (parts[1] === 'admin' && parts[2] === 'library' && parts.length === 4 && method === 'PATCH') {
        const entry = library.find((e) => e.asset === parts[3])
        if (!entry) {
            return refuse(404, 'No such model in the library')
        }
        const b = await body<Pick<LibraryEntry, 'attribution' | 'category' | 'name' | 'visibility'>>(req)
        Object.assign(entry, b)
        return json(entry)
    }
    if (parts[1] === 'admin' && parts[2] === 'assets' && parts.length === 4 && method === 'DELETE') {
        const stored = assets.find((a) => a.hash === parts[3])
        if (!stored) {
            return refuse(404, 'No such file')
        }
        if (stored.in_world > 0 || stored.held_by > 0) {
            return refuse(409, `That file is still worn by ${stored.in_world} prims and held in ${stored.held_by} inventories`)
        }
        assets.splice(assets.indexOf(stored), 1)
        const entry = library.findIndex((e) => e.asset === stored.hash)
        if (entry >= 0) {
            library.splice(entry, 1)
        }
        return new Response(null, {status: 204})
    }

    // ── Atlas: the pipeline, overrides, building designs ──────────────────
    if (p === '/api/pipeline/config' && method === 'GET') {
        return json(pipelineConfig)
    }
    if (p === '/api/pipeline/coverage' && method === 'GET') {
        return json({coverage})
    }
    if (p === '/api/pipeline/jobs' && method === 'GET') {
        for (const job of jobs) {
            liveLog(job)
        }
        return json({jobs})
    }
    if (p === '/api/pipeline/jobs' && method === 'POST') {
        const b = await body<{area: string; bbox: number[]; datasets: string[]; fresh: boolean}>(req)
        const area = pipelineConfig.areas.find((a) => a.id === b.area)
        const datasets = b.datasets ?? []
        if (!datasets.length) {
            return refuse(400, 'Pick at least one dataset')
        }
        const at = Date.now()
        const job: PipelineJob = {
            area: b.area ?? 'custom',
            bbox: b.bbox ?? area?.bbox ?? GRONINGEN_BBOX,
            by: who,
            createdAt: at,
            id: `job-${at}`,
            startedAt: at,
            status: 'running',
            steps: datasets.map((dataset, i) => ({
                dataset,
                progress: i === 0 ? {done: 0, phase: 'cells', total: 36} : {},
                startedAt: i === 0 ? at : undefined,
                status: i === 0 ? 'running' : 'queued',
            })),
        }
        jobs.unshift(job)
        logs.set(job.id, [{at, dataset: datasets[0], text: `area ${job.area}, ${datasets.length} datasets queued by ${who}`}])
        // Which datasets this run takes from another area's manifest.
        const replaces = coverage
            .filter((c) => datasets.includes(c.dataset) && c.name !== null && c.name !== job.area)
            .map((c) => ({dataset: c.dataset, name: c.name as string}))
        return json({job, replaces})
    }
    if (parts[1] === 'pipeline' && parts[2] === 'jobs' && parts.length === 5) {
        const job = jobs.find((j) => j.id === parts[3])
        if (!job) {
            return refuse(404, 'No such run')
        }
        if (parts[4] === 'cancel' && method === 'POST') {
            if (job.status === 'running' || job.status === 'queued') {
                job.status = 'cancelled'
                job.finishedAt = Date.now()
                for (const step of job.steps) {
                    if (step.status === 'running' || step.status === 'queued') {
                        step.status = 'cancelled'
                    }
                }
                logs.get(job.id)?.push({at: Date.now(), dataset: job.steps[0].dataset, text: `cancelled by ${who}`})
            }
            return json({job})
        }
        if (parts[4] === 'log' && method === 'GET') {
            // Polled, not streamed: `since` is a line count, and the answer is
            // the lines after it plus how many there are in all.
            const all = liveLog(job)
            const since = Math.max(0, Number(url.searchParams.get('since') ?? 0) || 0)
            return json({job, lines: all.slice(since), total: all.length})
        }
    }
    if (p === '/api/pipeline/overrides' && method === 'GET') {
        return json({overrides})
    }
    if (parts[1] === 'pipeline' && parts[2] === 'overrides' && parts.length >= 4) {
        const pand = parts[3]
        const existing = overrides.find((o) => o.pand === pand)
        if (parts[4] === 'model' && method === 'POST') {
            const bytes = await req.arrayBuffer()
            const override: BuildingOverride = existing ?? {
                author: who,
                cell: [8434, 5379],
                collision: 'parts',
                interior: 'none',
                name: register.find((r) => r.pand_id === pand)?.address.split(',')[0] ?? `Pand ${pand}`,
                pand,
                placement: {base_m: 0, heading_deg: 0, lat: null, lng: null, scale: 1},
                revision: 0,
                source: {kind: 'model', sha256: '', triangles: 0},
            }
            const triangles = Math.max(12, Math.round(bytes.byteLength / 60))
            override.source = {kind: 'model', sha256: sha256(`${pand}:${bytes.byteLength}`), triangles}
            // The revision is the save time in milliseconds; the page shows it as a date.
            override.revision = Date.now()
            if (!existing) {
                overrides.push(override)
            }
            return json({override, summary: {cell: override.cell ?? [8434, 5379], cell_found: true, elements: 3, triangles}})
        }
        if (parts.length === 4 && method === 'PUT') {
            if (!existing) {
                return refuse(404, 'No override for that building — upload a model first')
            }
            const b = await body<Pick<BuildingOverride, 'collision' | 'interior' | 'name' | 'placement'>>(req)
            Object.assign(existing, b)
            existing.revision = Date.now()
            const triangles = existing.source.kind === 'model' ? existing.source.triangles : 9_840
            return json({override: existing, summary: {cell: existing.cell ?? [8434, 5379], cell_found: true, elements: 5, triangles}})
        }
        if (parts.length === 4 && method === 'DELETE') {
            if (existing) {
                overrides.splice(overrides.indexOf(existing), 1)
            }
            return new Response(null, {status: 204})
        }
    }
    if (parts[1] === 'pipeline' && parts[2] === 'buildings' && parts.length >= 4) {
        const pand = parts[3]
        const owner = owned.find((o) => o.pand_id === pand)?.username ?? null
        if (parts.length === 4 && method === 'GET') {
            const stored = savedDesigns.get(pand) ?? null
            const generated = generatedDesign(pand)
            const response: BuildingResponse = {
                // Every mock session is an operator, so it may always edit.
                access: {admin: true, can_edit: true, owned_by: owner, owner: owner === who, username: who},
                design: stored ?? generated,
                facts: facts(pand),
                generated,
                pand,
                stored: stored !== null,
            }
            return json(response)
        }
        if (parts[4] === 'design' && method === 'PUT') {
            const b = (await body<BuildingDesign>(req)) as BuildingDesign
            const previous = savedDesigns.get(pand)
            const design: BuildingDesign = {...b, pand, revision: (previous?.revision ?? 0) + 1, source: 'editor'}
            savedDesigns.set(pand, design)
            return json({design})
        }
        if (parts[4] === 'design' && method === 'DELETE') {
            savedDesigns.delete(pand)
            return new Response(null, {status: 204})
        }
    }

    return null
}
