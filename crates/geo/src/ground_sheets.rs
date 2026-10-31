//! A ground cell's flat sheets — the landuse fills, the surveyed paving, the
//! crop parcels, the road ribbons and the gap fill — cut into one partition, and
//! the file atlas ships them in so the client need not cut them itself.
//!
//! # Why this is here
//!
//! The cuts are most of a street-level cell's worker time: 3.5–4.5 s of a 7–9 s
//! cell on the Steam Deck (2026-10-21), the rest being the drape onto the
//! terrain, which depends on the client's field and stays there. Atlas runs
//! [`build_sheets`] — **this function, not a copy of it** — per cell and writes
//! the result ([`encode`]); the client uses the file when it was built from
//! exactly the inputs the client has ([`fingerprint`], [`GROUND_REVISION`]) and
//! builds the sheets itself otherwise. Nothing else changes for the client: it
//! still reads the vector tile for its streets, names, routing, trees and
//! furniture, and the paving and parcels for their index, kerbs and grass. A
//! pre-cut cell is an optimisation, never the only way to the ground.
//!
//! # The frame
//!
//! Positions are tile-local metres, north-west origin, at the `tile_m` the file
//! was built at — atlas's own, from the cell's latitude. The client's `tile_m`
//! comes from its map anchor and differs by a few hundredths of a per cent, so
//! [`PreCut::sheets`] scales x and z to it. Everything the cut does is linear in
//! the tile's coordinates except a few metric constants (ribbon widths, the gap
//! fill's quads), which a scale of 1.0003 moves by under a millimetre; and the
//! partition is shipped whole, roads included, so it stays a partition.

use crate::crops::CropParcel;
use crate::formats::roofs;
use crate::paving::{PavedArea, PavingKind};
use crate::road_texture::SurveyedCover;
use crate::surveyed_ground::{AtlasDeck, CropCell, SurveyedPaving};
use crate::vector_tiles::{self, SurfaceGroup, SurfaceMesh, TileData};

/// What the client's paving says about the streets, for which OSM ribbons the
/// survey supersedes. Shared so both sides decide it the same way.
#[must_use]
pub fn surveyed_cover(paved: &[PavedArea]) -> SurveyedCover {
    if paved
        .iter()
        .any(|a| matches!(a.kind, PavingKind::Street | PavingKind::LivingStreet))
    {
        SurveyedCover::Streets
    } else if paved.is_empty() {
        SurveyedCover::Nothing
    } else {
        SurveyedCover::Paths
    }
}

/// The decks laid flat — under the clearance a deck is raised at — as meshes the
/// paving is cut out from under (the survey maps a canal bridge's road surface as
/// it does the street, and stood on the ground it lay on the canal bed).
#[must_use]
pub fn flat_decks(
    decks: &[vector_tiles::BridgeDeck],
    tile_m: f32,
    atlas: Option<&[AtlasDeck]>,
) -> Vec<SurfaceMesh> {
    atlas
        .and_then(|a| crate::surveyed_ground::match_atlas_decks(decks, tile_m, a))
        .map(|m| {
            m.iter()
                .zip(decks)
                .filter(|(d, _)| d.height < crate::terrain_grade::MIN_LIFT_M)
                .map(|(_, deck)| SurfaceMesh {
                    group: SurfaceGroup::BridgeDeck,
                    positions: deck.positions.clone(),
                    indices: deck.indices.clone(),
                    uvs: Vec::new(),
                    normals: Vec::new(),
                    colors: Vec::new(),
                    rungs: Vec::new(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Build a cell's flat sheets, cut into each other: the landuse fills out from
/// under the roads and the paving, the paving out from under the roads and the
/// flat decks, the crops into what is left, and the gap fill over whatever
/// nothing covers. Only [`SurfaceGroup::is_flat_ground`] groups come back.
///
/// The order and the inputs are the client's cell job's as they were when this
/// was lifted out of it (2026-10-21); `docs/notes/the-ground-stack.md` has the
/// longer story of each cut.
///
/// # Errors
///
/// When the tile's surfaces cannot be tessellated at all. A road tessellation
/// that fails leaves the paving uncut against roads, as the client always did.
pub fn build_sheets(
    tile: &TileData,
    tile_m: f32,
    max_bytes: Option<usize>,
    paved: &[PavedArea],
    parcels: &[CropParcel],
    atlas_decks: Option<&[AtlasDeck]>,
) -> Result<Vec<SurfaceMesh>, String> {
    let mut meshes = vector_tiles::tessellate_surfaces(tile, tile_m, max_bytes)?;
    // The roads are cut against the decks, so the bridges come first — on a copy
    // of their own: the client keeps its carved decks from its own pass.
    let mut decks = vector_tiles::tessellate_bridges(tile, tile_m).unwrap_or_default();
    let cover = surveyed_cover(paved);
    // Held back rather than appended: the carriageways have to come out of it,
    // and they do not exist yet.
    let mut paving = crate::paving::build_paving_meshes(paved, max_bytes);
    // **Nor under a bridge laid flat.** The survey maps the road surface across
    // a canal bridge as it does the street, and stood on the ground under the
    // deck that paving lay on the canal bed: pale slopes falling from each quay
    // into the water under the Oosterbrug (2026-10-19). The deck is that surface.
    // Only where atlas measured the decks and this one is flat: under a viaduct
    // the paving is the street that runs beneath it.
    let flat = flat_decks(&decks, tile_m, atlas_decks);
    if !flat.is_empty() {
        crate::road_stitch::cut_under_roads(&mut paving, &flat);
    }
    // The registered crops go down over the generic green and under everything
    // else on the ground stack, partitioned against the whole finished ground in
    // a pass of their own once the roads and the paving are in.
    let mut crops = crate::crops::build_crop_meshes(parcels, max_bytes);
    // **The ground's sheets as laid, before they are cut into each other.** A cut
    // only takes out where two sheets overlap, so it never changes the ground they
    // cover between them — and that union is all the fills' cut and the gap fill
    // need. Asked of the cut sheets instead, both paid for the slivers every
    // earlier cut leaves behind: over half of a street-level cell's blocker
    // triangles were under 0.05 m². Taken after the paving's cut under the flat
    // decks, which does change what is covered.
    let paving_whole = paving.clone();
    let mut ground_whole: Vec<SurfaceMesh> = meshes
        .iter()
        .chain(&paving)
        .chain(&crops)
        .filter(|m| m.group.layer().is_sheet())
        .cloned()
        .collect();
    if let Ok(roads) =
        vector_tiles::tessellate_roads_surveyed(tile, tile_m, &mut decks, max_bytes, cover)
    {
        // **The carriageways come out of the paving before either is drawn.** The
        // two sources disagree by centimetres about where a kerb is, and the
        // ground stack held that overlap apart with 1.0 mm of height — which
        // works until each sheet is bent onto the measured ground against its own
        // triangles. Removing the overlap is the only thing that has ever held.
        crate::paving::cut_paving_under_roads(&mut paving, &roads.ground);
        // **And the landuse fills come out from under them too.** On the
        // Groningen fixture 67 % of the road sheet had built land drawn under it,
        // 7 % grass and 2 % woodland: the pale wedges through the tarmac in the
        // phone captures. And from under the paving as laid, in the same pass.
        crate::paving::cut_ground_under_roads_and_paving(&mut meshes, &roads.ground, &paving_whole);
        ground_whole.extend(roads.ground.iter().cloned());
        meshes.extend(std::mem::take(&mut paving));
        meshes.extend(roads.ground);
    }
    // A cell whose roads failed still has its pavements, cut against nothing
    // else; on the ordinary path `paving` is empty by now.
    crate::paving::cut_ground_under_paving(&mut meshes, &paving);
    meshes.extend(paving);
    // The crop parcels rejoin with the ground made a partition around them: the
    // grass under a field comes out, and the field under a canal, a wood or a
    // road does. Without it every field was two sheets over one piece of ground
    // and drew in blotches once relief bent them apart.
    crate::crops::cut_into_ground(&mut meshes, &mut crops);
    meshes.extend(crops);
    // **And the ground nothing covered.** What is left of the cell is a hole
    // that used to show the basemap, and the basemap is no longer drawn under a
    // built cell (`crate::gap_fill`). Asked of the sheets as laid, which cover
    // exactly what the cut ones do.
    if let Some(fill) = crate::gap_fill::gap_fill(
        &ground_whole,
        [0.0, 0.0],
        [tile_m, tile_m],
        vector_tiles::palette::RESIDENTIAL,
    ) {
        meshes.push(fill);
    }
    meshes.retain(|m| m.group.is_flat_ground());
    Ok(meshes)
}

// ---------------------------------------------------------------------------
// What a file was built from
// ---------------------------------------------------------------------------

/// FNV-1a, 64-bit: deterministic on every platform and in every build, which a
/// `std` hasher is not promised to be.
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    fn f32(&mut self, v: f32) {
        self.bytes(&v.to_bits().to_le_bytes());
    }
    fn text(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.bytes(s.as_bytes());
    }
    fn rings(&mut self, rings: &[Vec<(f64, f64)>]) {
        self.u64(rings.len() as u64);
        for r in rings {
            self.u64(r.len() as u64);
            for &(lat, lng) in r {
                self.f64(lat);
                self.f64(lng);
            }
        }
    }
}

/// Everything [`build_sheets`] reads that the client and atlas could disagree
/// on, as one number: the vector tile's bytes, and the paving, the photo's
/// measurements of it, the parcels and atlas's decks **as parsed, in lat/lng** —
/// before either side's `tile_m` has touched them, so the two agree bit for bit
/// when their inputs do. A tile re-imported upstream, a paving file re-extracted,
/// a cached copy older than the host's: each is a different number, and the
/// client builds that cell itself.
#[must_use]
pub fn fingerprint(
    tile_body: &[u8],
    paving: &SurveyedPaving,
    measured: &roofs::Cell,
    crops: &CropCell,
    decks: Option<&[AtlasDeck]>,
) -> u64 {
    let mut h = Fnv::new();
    h.u64(tile_body.len() as u64);
    h.bytes(tile_body);
    h.u64(paving.areas.len() as u64);
    for (kind, material, rings) in &paving.areas {
        h.text(&format!("{kind:?}/{material:?}"));
        h.rings(rings);
    }
    // Ordered maps, so their walk is the same everywhere.
    h.u64(measured.paving.len() as u64);
    for (key, c) in &measured.paving {
        h.text(key);
        h.bytes(&c.c);
    }
    h.u64(measured.gardens.len() as u64);
    for (key, g) in &measured.gardens {
        h.text(key);
        for side in [&g.planted, &g.paved] {
            h.u64(side.len() as u64);
            for poly in side {
                h.rings(poly);
            }
        }
    }
    h.u64(crops.parcels.len() as u64);
    for (category, code, rings) in &crops.parcels {
        h.text(&format!("{category:?}"));
        h.u64(u64::from(*code));
        h.rings(rings);
    }
    match decks {
        None => h.u64(u64::MAX),
        Some(decks) => {
            h.u64(decks.len() as u64);
            for d in decks {
                h.f32(d.at[0]);
                h.f32(d.at[1]);
                h.f32(d.height);
                h.f32(d.top.unwrap_or(f32::NAN));
                let profile = d.profile.as_deref().unwrap_or(&[]);
                h.u64(profile.len() as u64);
                for &p in profile {
                    h.f64(p);
                }
            }
        }
    }
    h.0
}

// ---------------------------------------------------------------------------
// The file
// ---------------------------------------------------------------------------

/// A cell's sheets as atlas wrote them.
#[derive(Debug, Clone, PartialEq)]
pub struct PreCut {
    /// [`GROUND_REVISION`] of the build that wrote it.
    pub revision: u64,
    /// [`fingerprint`] of the inputs it was built from.
    pub fingerprint: u64,
    /// The `tile_m` its positions are in.
    pub tile_m: f32,
    pub meshes: Vec<SurfaceMesh>,
}

impl PreCut {
    /// The sheets, in a frame of `tile_m` metres to the cell — or `None` when the
    /// file is from another revision of [`build_sheets`] or other inputs than
    /// `fingerprint` names, and the caller must build its own.
    #[must_use]
    pub fn sheets(self, fingerprint: u64, tile_m: f32) -> Option<Vec<SurfaceMesh>> {
        if self.revision != GROUND_REVISION
            || self.fingerprint != fingerprint
            || !(self.tile_m > 0.0)
        {
            return None;
        }
        let s = tile_m / self.tile_m;
        let mut meshes = self.meshes;
        if s != 1.0 {
            for m in &mut meshes {
                for p in &mut m.positions {
                    p[0] *= s;
                    p[2] *= s;
                }
            }
        }
        Some(meshes)
    }
}

const MAGIC: &[u8; 4] = b"CGS2";
const VERSION: u32 = 2;

/// Positions are written in steps of `1 / QUANT` metres: a quarter of a
/// millimetre. A street cell was 35 MB of raw `f32` (7.5 MB gzipped) on the
/// Groningen run of 2026-10-21, far too much to send a phone for what it saves.
/// Snapping is deterministic, so a vertex two sheets share snaps to the same
/// place in both and the partition stays closed; a sliver that snaps to nothing
/// draws nothing.
const QUANT: f32 = 4000.0;

const HAS_UVS: u8 = 1;
const HAS_NORMALS: u8 = 2;
const HAS_COLORS: u8 = 4;
const HAS_RUNGS: u8 = 8;
/// Every normal is straight up, so none are stored.
const UP_NORMALS: u8 = 32;
/// Every position is at y = 0, so only x and z are stored.
const FLAT: u8 = 64;

/// The indices are `0, 1, 2, …` — a triangle soup — and are not written.
const SOUP: u8 = 128;

/// One vertex with everything it carries, as compared for sharing: the
/// quantised position and the bits of each attribute present.
type VertexKey = ([i64; 3], Vec<u32>);

/// Write a cell's sheets.
///
/// **The cut writes triangle soups** — every triangle its own three vertices,
/// measured at exactly 3.00 vertices a triangle over a Groningen street cell —
/// and the client must get exactly those meshes back, so the file stores each
/// *distinct* vertex once and then, for every vertex of the mesh, which distinct
/// one it is. That reference is coded against the high-water mark of the
/// distinct table (0 for the next new vertex, otherwise how far back), so a
/// vertex shared with the triangle just written costs a byte.
///
/// Per mesh: its group's [`SurfaceGroup::code`], a flag byte, its vertex and
/// index counts and its distinct-vertex count; the distinct positions,
/// quantised to [`QUANT`], as zigzag varint deltas; their UVs and any non-up
/// normals as raw `f32`; their colours and road rungs as a palette with runs
/// over it; the references; and the indices as deltas, unless they are a soup's
/// `0, 1, 2, …` ([`SOUP`]).
#[must_use]
pub fn encode(revision: u64, fingerprint: u64, tile_m: f32, meshes: &[SurfaceMesh]) -> Vec<u8> {
    let mut w = Writer(Vec::new());
    w.0.extend_from_slice(MAGIC);
    w.0.extend_from_slice(&VERSION.to_le_bytes());
    w.0.extend_from_slice(&revision.to_le_bytes());
    w.0.extend_from_slice(&fingerprint.to_le_bytes());
    w.f32(tile_m);
    w.varint(meshes.len() as u64);
    for m in meshes {
        let n = m.positions.len();
        let mut flags = 0u8;
        if m.uvs.len() == n && n > 0 {
            flags |= HAS_UVS;
        }
        if m.normals.len() == n && n > 0 {
            flags |= HAS_NORMALS;
            if m.normals.iter().all(|v| *v == [0.0, 1.0, 0.0]) {
                flags |= UP_NORMALS;
            }
        }
        if m.colors.len() == n && n > 0 {
            flags |= HAS_COLORS;
        }
        if m.rungs.len() == n && n > 0 {
            flags |= HAS_RUNGS;
        }
        if m.positions.iter().all(|p| p[1] == 0.0) {
            flags |= FLAT;
        }
        if m.indices.iter().enumerate().all(|(i, &v)| v as usize == i) {
            flags |= SOUP;
        }
        // The distinct vertices, in order of first use, and each vertex's.
        let key = |i: usize| -> VertexKey {
            let p = m.positions[i].map(|c| (c * QUANT).round() as i64);
            let mut bits = Vec::new();
            if flags & HAS_UVS != 0 {
                bits.extend(m.uvs[i].map(f32::to_bits));
            }
            if flags & HAS_NORMALS != 0 && flags & UP_NORMALS == 0 {
                bits.extend(m.normals[i].map(f32::to_bits));
            }
            if flags & HAS_COLORS != 0 {
                bits.extend(m.colors[i].map(f32::to_bits));
            }
            if flags & HAS_RUNGS != 0 {
                bits.push(m.rungs[i].to_bits());
            }
            (p, bits)
        };
        let mut table: std::collections::HashMap<VertexKey, u32> = std::collections::HashMap::new();
        let mut distinct: Vec<usize> = Vec::new();
        let refs: Vec<u32> = (0..n)
            .map(|i| {
                *table.entry(key(i)).or_insert_with(|| {
                    distinct.push(i);
                    (distinct.len() - 1) as u32
                })
            })
            .collect();
        w.varint(u64::from(m.group.code()));
        w.0.push(flags);
        w.varint(n as u64);
        w.varint(m.indices.len() as u64);
        w.varint(distinct.len() as u64);
        let mut prev = [0i64; 3];
        for &i in &distinct {
            for k in [0, 1, 2] {
                if k == 1 && flags & FLAT != 0 {
                    continue;
                }
                let q = (m.positions[i][k] * QUANT).round() as i64;
                w.zigzag(q - prev[k]);
                prev[k] = q;
            }
        }
        if flags & HAS_UVS != 0 {
            for &i in &distinct {
                w.f32(m.uvs[i][0]);
                w.f32(m.uvs[i][1]);
            }
        }
        if flags & HAS_NORMALS != 0 && flags & UP_NORMALS == 0 {
            for &i in &distinct {
                m.normals[i].iter().for_each(|&c| w.f32(c));
            }
        }
        if flags & HAS_COLORS != 0 {
            w.runs(
                distinct
                    .iter()
                    .map(|&i| m.colors[i].map(f32::to_bits).to_vec()),
            );
        }
        if flags & HAS_RUNGS != 0 {
            w.runs(distinct.iter().map(|&i| vec![m.rungs[i].to_bits()]));
        }
        let mut high = 0u32;
        for r in refs {
            if r == high {
                w.varint(0);
                high += 1;
            } else {
                w.varint(u64::from(high - r));
            }
        }
        if flags & SOUP == 0 {
            let mut prev = 0i64;
            for &i in &m.indices {
                w.zigzag(i64::from(i) - prev);
                prev = i64::from(i);
            }
        }
    }
    w.0
}

struct Writer(Vec<u8>);

impl Writer {
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn zigzag(&mut self, v: i64) {
        self.varint(((v << 1) ^ (v >> 63)) as u64);
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// A palette of the distinct values in first-seen order, then
    /// `(palette index, run length)` pairs covering every vertex.
    fn runs(&mut self, values: impl Iterator<Item = Vec<u32>>) {
        let values: Vec<Vec<u32>> = values.collect();
        let mut palette: Vec<&Vec<u32>> = Vec::new();
        let mut index = std::collections::HashMap::new();
        let mut runs: Vec<(usize, u64)> = Vec::new();
        for v in &values {
            let i = *index.entry(v).or_insert_with(|| {
                palette.push(v);
                palette.len() - 1
            });
            match runs.last_mut() {
                Some((last, n)) if *last == i => *n += 1,
                _ => runs.push((i, 1)),
            }
        }
        self.varint(palette.len() as u64);
        for v in palette {
            v.iter()
                .for_each(|&b| self.0.extend_from_slice(&b.to_le_bytes()));
        }
        self.varint(runs.len() as u64);
        for (i, n) in runs {
            self.varint(i as u64);
            self.varint(n);
        }
    }
}

/// Read what [`encode`] wrote.
///
/// # Errors
///
/// A wrong magic or version, a group this build has no code for, an index out of
/// range, runs that do not cover the vertices, or a body that ends early or
/// carries bytes after its last mesh.
pub fn decode(bytes: &[u8]) -> Result<PreCut, String> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return Err("not a ground sheets file".into());
    }
    let version = r.u32()?;
    if version != VERSION {
        return Err(format!(
            "ground sheets version {version}, this build reads {VERSION}"
        ));
    }
    let revision = r.u64()?;
    let fingerprint = r.u64()?;
    let tile_m = r.f32()?;
    let count = r.len()?;
    let mut meshes = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let code = u16::try_from(r.varint()?).map_err(|_| "group code out of range")?;
        let group = SurfaceGroup::from_code(code).ok_or_else(|| format!("unknown group {code}"))?;
        let flags = r.u8()?;
        let n = r.len()?;
        let ni = r.len()?;
        let d = r.len()?;
        // The distinct vertices first ([`encode`]).
        let mut dpos = Vec::with_capacity(d.min(1 << 24));
        let mut prev = [0i64; 3];
        for _ in 0..d {
            let mut p = [0.0f32; 3];
            for k in [0, 1, 2] {
                if k == 1 && flags & FLAT != 0 {
                    continue;
                }
                prev[k] += r.zigzag()?;
                p[k] = prev[k] as f32 / QUANT;
            }
            dpos.push(p);
        }
        let mut duvs = Vec::new();
        if flags & HAS_UVS != 0 {
            for _ in 0..d {
                duvs.push([r.f32()?, r.f32()?]);
            }
        }
        let mut dnormals = Vec::new();
        if flags & HAS_NORMALS != 0 && flags & UP_NORMALS == 0 {
            for _ in 0..d {
                dnormals.push([r.f32()?, r.f32()?, r.f32()?]);
            }
        }
        let dcolors: Vec<[f32; 4]> = if flags & HAS_COLORS != 0 {
            r.runs(d, 4)?
                .into_iter()
                .map(|v| [v[0], v[1], v[2], v[3]].map(f32::from_bits))
                .collect()
        } else {
            Vec::new()
        };
        let drungs: Vec<f32> = if flags & HAS_RUNGS != 0 {
            r.runs(d, 1)?
                .into_iter()
                .map(|v| f32::from_bits(v[0]))
                .collect()
        } else {
            Vec::new()
        };
        // …then which distinct vertex each of the mesh's is.
        let mut refs = Vec::with_capacity(n.min(1 << 24));
        let mut high = 0usize;
        for _ in 0..n {
            let back = r.len()?;
            let at = if back == 0 {
                high += 1;
                high - 1
            } else {
                high.checked_sub(back)
                    .ok_or("vertex reference before the first")?
            };
            if at >= d {
                return Err(format!("vertex reference {at} past {d} distinct"));
            }
            refs.push(at);
        }
        let positions: Vec<[f32; 3]> = refs.iter().map(|&i| dpos[i]).collect();
        let uvs: Vec<[f32; 2]> = refs.iter().filter_map(|&i| duvs.get(i).copied()).collect();
        let normals: Vec<[f32; 3]> = if flags & HAS_NORMALS == 0 {
            Vec::new()
        } else if flags & UP_NORMALS != 0 {
            vec![[0.0, 1.0, 0.0]; n]
        } else {
            refs.iter().map(|&i| dnormals[i]).collect()
        };
        let colors: Vec<[f32; 4]> = refs
            .iter()
            .filter_map(|&i| dcolors.get(i).copied())
            .collect();
        let rungs: Vec<f32> = refs
            .iter()
            .filter_map(|&i| drungs.get(i).copied())
            .collect();
        let indices: Vec<u32> = if flags & SOUP != 0 {
            if ni != n {
                return Err(format!("a soup of {n} vertices with {ni} indices"));
            }
            (0..n as u32).collect()
        } else {
            let mut indices = Vec::with_capacity(ni.min(1 << 26));
            let mut prev = 0i64;
            for _ in 0..ni {
                prev += r.zigzag()?;
                if prev < 0 || prev as usize >= n {
                    return Err(format!("index {prev} past {n} vertices"));
                }
                indices.push(prev as u32);
            }
            indices
        };
        meshes.push(SurfaceMesh {
            group,
            positions,
            indices,
            uvs,
            normals,
            colors,
            rungs,
        });
    }
    if r.at != bytes.len() {
        return Err(format!("{} bytes after the last mesh", bytes.len() - r.at));
    }
    Ok(PreCut {
        revision,
        fingerprint,
        tile_m,
        meshes,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let end = end.ok_or("ground sheets file ends early")?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| "short")?,
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "short")?,
        ))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }
    /// A count, bounded by what is left: a count no body could hold is a
    /// corrupt file, not an allocation to attempt.
    fn len(&mut self) -> Result<usize, String> {
        let v = self.varint()?;
        usize::try_from(v)
            .ok()
            .filter(|&v| v <= self.bytes.len().saturating_mul(8))
            .ok_or_else(|| format!("count {v} past the file"))
    }
    fn zigzag(&mut self) -> Result<i64, String> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    /// [`Writer::runs`] back to one value of `width` words per vertex.
    fn runs(&mut self, n: usize, width: usize) -> Result<Vec<Vec<u32>>, String> {
        let pn = self.len()?;
        let mut palette = Vec::with_capacity(pn.min(1 << 16));
        for _ in 0..pn {
            let mut v = Vec::with_capacity(width);
            for _ in 0..width {
                v.push(self.u32()?);
            }
            palette.push(v);
        }
        let rn = self.len()?;
        let mut out = Vec::with_capacity(n.min(1 << 24));
        for _ in 0..rn {
            let i = self.len()?;
            let run = self.len()?;
            let v = palette.get(i).ok_or("run past the palette")?;
            if out.len() + run > n {
                return Err("runs past the vertices".into());
            }
            out.extend(std::iter::repeat_n(v, run).cloned());
        }
        if out.len() != n {
            return Err("runs short of the vertices".into());
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// The revision
// ---------------------------------------------------------------------------

/// Which [`build_sheets`] a file came from: a hash of what it builds on the
/// Groningen fixture with surveyed paving, parcels and a flat deck laid over it
/// ([`measure`]). A change to anything the sheets are built with moves it, a
/// file from before the change no longer matches, and the client builds those
/// cells itself until atlas runs again. Pinned by
/// `the_revision_names_what_build_sheets_builds`, which prints the new value.
pub const GROUND_REVISION: u64 = 0x6871_dd92_058c_b0d5;

/// Build the fixture's sheets and hash them — positions, indices and every
/// attribute, through [`encode`], with the vertex colours rounded to 1/1024
/// first. Paving and crop colours go through `powf` (the sRGB curve), and
/// macOS's libm and glibc's disagree in its last bit: the colours of groups
/// 10 and 11 differed between an ARM Mac and an x86 node, every position and
/// index matched (measured 2026-10-31), and so this failed on every x86 node
/// while passing on the Mac that set the constant. A 1/1024 step is below what
/// an 8-bit channel can show and far above a last-bit difference.
#[cfg(test)]
fn measure() -> u64 {
    let (paved, parcels) = fixture_inputs();
    let tile = crate::testdata::groningen();
    let mut meshes =
        build_sheets(&tile, 1460.0, None, &paved, &parcels, None).expect("fixture builds");
    for m in &mut meshes {
        for c in &mut m.colors {
            *c = c.map(|v| (v * 1024.0).round() / 1024.0);
        }
    }
    let mut h = Fnv::new();
    h.bytes(&encode(0, 0, 1460.0, &meshes));
    h.0
}

/// Paving and parcels laid over the fixture: a square of each paved form that
/// changes how the cut runs, and two crop parcels.
#[cfg(test)]
fn fixture_inputs() -> (Vec<PavedArea>, Vec<CropParcel>) {
    let square =
        |x: f32, z: f32, w: f32| vec![vec![[x, z], [x + w, z], [x + w, z + w], [x, z + w]]];
    let paved = [
        (PavingKind::Footway, 300.0),
        (PavingKind::Street, 520.0),
        (PavingKind::Parking, 740.0),
        (PavingKind::Garden, 960.0),
    ]
    .into_iter()
    .map(|(kind, x)| PavedArea {
        kind,
        material: None,
        rings: square(x, 700.0, 60.0),
        measured: None,
    })
    .collect();
    let parcels = [
        (crate::crops::CropCategory::Arable, 100.0),
        (crate::crops::CropCategory::Grassland, 1200.0),
    ]
    .into_iter()
    .map(|(category, x)| CropParcel {
        category,
        code: 233,
        seed: 7,
        rings: square(x, 100.0, 120.0),
    })
    .collect();
    (paved, parcels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_group_code_round_trips() {
        use crate::road_texture::RoadClass;
        let classes = [
            RoadClass::Motorway,
            RoadClass::Carriageway,
            RoadClass::Major,
            RoadClass::Minor,
            RoadClass::Path,
            RoadClass::Rail,
        ];
        let mut all = vec![
            SurfaceGroup::Water,
            SurfaceGroup::Forest,
            SurfaceGroup::Grass,
            SurfaceGroup::Sand,
            SurfaceGroup::Built,
            SurfaceGroup::BridgeDeck,
            SurfaceGroup::Canopy,
            SurfaceGroup::Trunk,
            SurfaceGroup::Furniture,
            SurfaceGroup::Kerb,
            SurfaceGroup::Paving,
            SurfaceGroup::Crop,
            SurfaceGroup::TunnelStructure,
            SurfaceGroup::QuayWall,
        ];
        for c in classes {
            all.extend([
                SurfaceGroup::Road(c),
                SurfaceGroup::RoadCasing(c),
                SurfaceGroup::TunnelRoad(c),
            ]);
        }
        let mut seen = std::collections::HashSet::new();
        for g in all {
            assert!(seen.insert(g.code()), "{g:?} shares a code");
            assert_eq!(SurfaceGroup::from_code(g.code()), Some(g));
        }
        assert_eq!(SurfaceGroup::from_code(14), None);
    }

    /// The fixture's sheets, written and read back, are the sheets — and a file
    /// from another revision, other inputs or a truncated body is refused.
    #[test]
    fn a_cell_survives_the_file() {
        let (paved, parcels) = fixture_inputs();
        let tile = crate::testdata::groningen();
        let meshes = build_sheets(&tile, 1460.0, None, &paved, &parcels, None).unwrap();
        assert!(meshes.iter().any(|m| m.group == SurfaceGroup::Paving));
        assert!(meshes
            .iter()
            .any(|m| matches!(m.group, SurfaceGroup::Road(_))));
        assert!(meshes.iter().all(|m| m.group.is_flat_ground()));
        let bytes = encode(GROUND_REVISION, 42, 1460.0, &meshes);
        let back = decode(&bytes).unwrap();
        // Everything but the positions exactly, and the positions to the step
        // they are snapped to — and snapping what was snapped changes nothing.
        assert_eq!(back.meshes.len(), meshes.len());
        for (b, m) in back.meshes.iter().zip(&meshes) {
            assert_eq!(
                (b.group, &b.indices, &b.uvs, &b.normals, &b.colors, &b.rungs),
                (m.group, &m.indices, &m.uvs, &m.normals, &m.colors, &m.rungs)
            );
            for (p, q) in b.positions.iter().zip(&m.positions) {
                assert!((0..3).all(|k| (p[k] - q[k]).abs() <= 0.6 / QUANT + 1e-4));
            }
        }
        assert_eq!(encode(GROUND_REVISION, 42, 1460.0, &back.meshes), bytes);
        let snapped = back.meshes.clone();
        assert_eq!(back.clone().sheets(42, 1460.0), Some(snapped.clone()));
        assert_eq!(back.clone().sheets(43, 1460.0), None);
        let scaled = back.sheets(42, 1461.46).unwrap();
        let (a, b) = (snapped[0].positions[1], scaled[0].positions[1]);
        assert!((b[0] - a[0] * 1.001).abs() < 1e-3 && b[1] == a[1]);
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        let mut stale = decode(&bytes).unwrap();
        stale.revision ^= 1;
        assert_eq!(stale.sheets(42, 1460.0), None);
        eprintln!(
            "fixture: {} meshes, {} triangles, {} bytes",
            meshes.len(),
            meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>(),
            bytes.len()
        );
    }

    /// The fingerprint moves with each input, and only with the inputs.
    #[test]
    fn the_fingerprint_names_its_inputs() {
        let paving = crate::surveyed_ground::parse_surveyed_paving(
            r#"{"surfaces":[[0,[[[53.2,6.5],[53.2,6.51],[53.21,6.51]]]]]}"#,
        )
        .unwrap();
        let measured = roofs::Cell::default();
        let crops = CropCell::default();
        let base = fingerprint(b"tile", &paving, &measured, &crops, None);
        assert_eq!(base, fingerprint(b"tile", &paving, &measured, &crops, None));
        assert_ne!(base, fingerprint(b"tilf", &paving, &measured, &crops, None));
        assert_ne!(
            base,
            fingerprint(b"tile", &SurveyedPaving::default(), &measured, &crops, None)
        );
        assert_ne!(
            base,
            fingerprint(b"tile", &paving, &measured, &crops, Some(&[]))
        );
    }

    #[test]
    fn the_revision_names_what_build_sheets_builds() {
        let now = measure();
        assert_eq!(
            GROUND_REVISION, now,
            "build_sheets builds something else now: set GROUND_REVISION to {now:#018x}, \
             and run atlas's `ground` dataset again so the hosts serve sheets this build \
             will use"
        );
    }
}
