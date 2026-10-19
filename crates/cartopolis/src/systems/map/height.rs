//! The **height layer**: measured elevation, streamed on demand.
//!
//! Everything else in the client stands on one flat plane at `y = 0` — buildings,
//! water, the coplanar surface stack, the avatar. That is a deliberate
//! simplification and it holds up well in the Netherlands, but it has no answer at
//! all for the one place where a road is genuinely *not* at ground level: a bridge.
//! OSM says `bridge=yes` and nothing else. There is no `layer` in the Shortbread
//! schema, no clearance, no deck height — so a viaduct and the rail yard it crosses
//! arrive as two flat ribbons fighting over the same plane, and no amount of
//! draw-order cleverness can make one *pass over* the other.
//!
//! This layer supplies the missing number by measuring it. [AHN] — the national
//! height model of the Netherlands, 0.5 m grid, ~5 cm vertical accuracy, CC0 — is
//! served by PDOK as two rasters:
//!
//! * **DTM** ([`HeightModel::Terrain`]), the bare ground. Bridges and buildings are
//!   cut out of it, so a deck footprint reads as no-data and the yard *underneath*
//!   reads as ground.
//! * **DSM** ([`HeightModel::Surface`]), everything the laser hit. The deck is in
//!   it, at its real height.
//!
//! The difference between the two, over a bridge outline, is the deck's clearance.
//! That is the whole reason this exists, and it is a measurement rather than the
//! plausible-looking constant a renderer would otherwise have to invent.
//!
//! **The request is the unit, not the tile.** A consumer asks for a box of ground
//! at a resolution it chooses ([`HeightRequest`]) and the answer is resampled
//! server-side, so a bridge deck costs a 48×48 grid (a few KB) rather than a cell's
//! worth of 0.5 m raster it would throw away. Patches are cached by a quantised key
//! and shared between consumers. A future terrain-relief pass asks the same
//! question with a bigger box and a coarser grid.
//!
//! **Coverage is a fact, not an assumption.** [`HeightLayer::covers`] is AHN's own
//! envelope out of its `GetCapabilities`; outside it — which is most of the world —
//! the layer answers `None` and its consumers fall back to whatever they did
//! before. Same when the Layers panel switches it off.
//!
//! [AHN]: https://www.ahn.nl

use crate::platform::sync::{Arc, Mutex};
use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;

use crate::platform::http;

use crate::platform::work::Dispatcher;
/// The TIFF decode and the RD projection live in `cartopolis_geo::ahn` so the
/// harness can use them without depending on this crate. Re-exported here because
/// this module is where the rest of the tree looks for them, and because two
/// copies of a projection is the drift this tree spent 2026-09-19 removing
/// everywhere else.
use cartopolis_geo::ahn::decode_float_tiff;

/// PDOK's public service host. Reached direct on native and through the app's own
/// origin on the web (`/up/ahn/…`, see [`http::upstream_url`]) — PDOK is a third
/// party like NOAA, and the browser's CORS rules are not ours to negotiate.
pub const AHN_ORIGIN: &str = "https://service.pdok.nl";

/// The WCS entry point. Version 1.0.0 is what this service speaks.
const AHN_WCS_PATH: &str = "/rws/ahn/wcs/v1_0";

/// AHN's coverage envelope, lifted from its own `GetCapabilities`
/// (`lonLatEnvelope`, CRS84). Asking outside it costs a round trip to be told no.
const AHN_LON: (f64, f64) = (3.20, 7.28);
const AHN_LAT: (f64, f64) = (50.72, 53.56);

/// Concurrent patch fetches. PDOK is a shared public service and we are a guest on
/// it: a small cap, and a minimum spacing between dispatches, are the citizenship
/// this layer owes. It is also plenty — a patch is a few KB and consumers ask for
/// one per bridge, not one per frame.
const HEIGHT_CONCURRENCY: usize = 3;
const HEIGHT_MIN_INTERVAL_S: f64 = 0.15;

/// Queue ranks for [`HeightLayer::patch_ranked`]: the terrain field first, then
/// the bridge decks — the two a map waits on before it settles.
pub const RANK_FIELD: crate::platform::fetch_gate::Rank = 0;
pub const RANK_DECKS: crate::platform::fetch_gate::Rank = 1;

/// Give up on a request after this many failures, so a box that AHN genuinely has
/// nothing for is not re-asked every frame for the rest of the session.
const MAX_ATTEMPTS: u32 = 2;

/// Largest grid a single request may ask for. The stop on a caller that computes a
/// silly number, and nothing more principled than that.
///
/// It was 256 on the reasoning that a patch is a handful of samples over a specific
/// thing. The fine terrain tier (`terrain::FINE_GRID`) is the counter-example: it wants
/// AHN's own half-metre grid over a 512 m box, which is one 1024² request or sixteen
/// 256² ones, and sixteen round trips to a shared public service to assemble one answer
/// is worse citizenship than one. Measured against the live service 2026-09-11 — 256²
/// is 99 kB in 0.6 s, 1024² is 1.5 MB in 0.5 s, 2048² is 6.4 MB in 1.5 s — so the
/// ceiling is what PDOK will actually serve, not a guess.
///
/// Note what this is *not*: a memory bound. A 1024² patch is 4 MB decoded, and the
/// caller that asks for one owes [`HeightLayer::drop_patch`] when it has baked it.
///
/// 4096 rather than 2048 because the terrain field asks for 2049² — one more than a
/// power of two, so that the *intervals* between its samples halve exactly (see
/// `terrain::TERRAIN_GRID`). Measured against the live service 2026-09-13: 2049² is
/// 12.3 MB in 4.5 s.
const MAX_GRID: u32 = 4096;

/// How far from the map anchor, in RD metres, a cached patch is kept.
///
/// The cache had no removal path at all: a patch key is an *absolute* RD box, so
/// nothing ever invalidated one, and a session that walked across a canal city
/// accumulated two decks' worth of patches per bridge plus a 256×256 terrain box
/// (~256 KB) per re-anchor, for the rest of the run. Absolute keys are also what
/// makes eviction safe — a dropped patch is not stale, only absent, and asking
/// again re-fetches it.
///
/// 8 km is comfortably outside the terrain field's own 6.4 km box and the geometry
/// ring around it, so everything a consumer can currently ask about survives a
/// prune; what goes is the town you flew away from.
const CACHE_KEEP_M: f64 = 8_000.0;

/// Which AHN raster to sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeightModel {
    /// Bare ground (`dtm_05m`): buildings and bridge decks are cut out of it, so a
    /// deck reads as no-data and what is under the deck reads as ground.
    Terrain,
    /// Everything the laser hit (`dsm_05m`): roofs, trees, and — the point of it —
    /// bridge decks at their real height.
    Surface,
}

impl HeightModel {
    fn coverage(self) -> &'static str {
        match self {
            Self::Terrain => "dtm_05m",
            Self::Surface => "dsm_05m",
        }
    }
}

/// A box of ground to measure, in **RD New** (EPSG:28992) metres — the only CRS
/// this service speaks, which is why [`wgs84_to_rd`] exists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightRequest {
    pub model: HeightModel,
    /// South-west corner (easting, northing).
    pub min: [f64; 2],
    /// North-east corner.
    pub max: [f64; 2],
    /// Samples across and down. The service resamples, so this is free to be small:
    /// 48 over a 60 m deck is 1.25 m, finer than the deck's own edge.
    pub grid: u32,
}

/// A request's cache identity: metres rounded to whole units, so two consumers
/// asking about the same bridge from geometry that differs in the last float bit
/// share one fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HeightKey {
    model: HeightModel,
    min: [i32; 2],
    max: [i32; 2],
    grid: u32,
}

impl HeightKey {
    /// The box's centre in RD metres — what the cache prunes against, since the key
    /// is the only thing a failure or an in-flight request still carries.
    fn centre(self) -> [f64; 2] {
        [
            (f64::from(self.min[0]) + f64::from(self.max[0])) * 0.5,
            (f64::from(self.min[1]) + f64::from(self.max[1])) * 0.5,
        ]
    }
}

impl HeightRequest {
    /// A box centred on `centre` (RD metres) with `half` metres to each side.
    #[must_use]
    pub fn around(model: HeightModel, centre: [f64; 2], half: f64, grid: u32) -> Self {
        Self {
            model,
            min: [centre[0] - half, centre[1] - half],
            max: [centre[0] + half, centre[1] + half],
            grid,
        }
    }

    /// Grow the box by `m` metres on every side — how a consumer asks about the
    /// ground *around* a thing rather than the thing itself.
    #[must_use]
    pub fn padded(mut self, m: f64) -> Self {
        self.min = [self.min[0] - m, self.min[1] - m];
        self.max = [self.max[0] + m, self.max[1] + m];
        self
    }

    fn key(&self) -> HeightKey {
        HeightKey {
            model: self.model,
            min: [self.min[0].round() as i32, self.min[1].round() as i32],
            max: [self.max[0].round() as i32, self.max[1].round() as i32],
            grid: self.grid.clamp(2, MAX_GRID),
        }
    }

    /// The box its key names: corners on whole metres, the grid clamped.
    #[allow(clippy::cast_possible_truncation)]
    fn quantised(self) -> Self {
        let key = self.key();
        Self {
            model: self.model,
            min: [f64::from(key.min[0]), f64::from(key.min[1])],
            max: [f64::from(key.max[0]), f64::from(key.max[1])],
            grid: key.grid,
        }
    }

    fn valid(&self) -> bool {
        self.max[0] > self.min[0] && self.max[1] > self.min[1] && self.grid >= 2
    }
}

/// One measured box: a regular grid of metres above NAP, row 0 at the **north**
/// edge (the order the GeoTIFF arrives in), with `f32::NAN` for no-data.
#[derive(Debug, Clone)]
pub struct HeightPatch {
    pub min: [f64; 2],
    pub max: [f64; 2],
    pub w: usize,
    pub h: usize,
    pub samples: Vec<f32>,
}

impl HeightPatch {
    /// Height at an RD point, or `None` outside the patch or on no-data. Nearest
    /// sample, not interpolated: these are measurements, and interpolating across
    /// the edge of a bridge deck would invent a slope that is not there.
    #[must_use]
    pub fn at(&self, rd: [f64; 2]) -> Option<f32> {
        let (dx, dy) = (self.max[0] - self.min[0], self.max[1] - self.min[1]);
        if dx <= 0.0 || dy <= 0.0 {
            return None;
        }
        let fx = (rd[0] - self.min[0]) / dx;
        // North is row 0.
        let fy = (self.max[1] - rd[1]) / dy;
        if !(0.0..1.0).contains(&fx) || !(0.0..1.0).contains(&fy) {
            return None;
        }
        let col = (fx * self.w as f64) as usize;
        let row = (fy * self.h as f64) as usize;
        let v = *self.samples.get(row * self.w + col)?;
        v.is_finite().then_some(v)
    }

    /// The median of every valid sample the predicate accepts, by RD position.
    ///
    /// Median rather than mean, and it is not a detail: a deck's samples include
    /// its parapets, the lamp posts on it and the occasional lorry, and a mean is
    /// dragged upward by all three. Half the samples being junk still leaves the
    /// median on the deck.
    #[must_use]
    pub fn median_where(&self, keep: impl Fn([f64; 2]) -> bool) -> Option<f32> {
        let mut vals: Vec<f32> = Vec::new();
        let (dx, dy) = (self.max[0] - self.min[0], self.max[1] - self.min[1]);
        for row in 0..self.h {
            for col in 0..self.w {
                let v = self.samples[row * self.w + col];
                if !v.is_finite() {
                    continue;
                }
                // Sample centres, so a point never lands on a cell boundary.
                let rd = [
                    self.min[0] + (col as f64 + 0.5) / self.w as f64 * dx,
                    self.max[1] - (row as f64 + 0.5) / self.h as f64 * dy,
                ];
                if keep(rd) {
                    vals.push(v);
                }
            }
        }
        if vals.is_empty() {
            return None;
        }
        vals.sort_by(f32::total_cmp);
        Some(vals[vals.len() / 2])
    }

    /// The highest valid sample inside an RD box — a crest, measured from a patch
    /// that covers several (`dikes::measure_dike_crests`).
    #[must_use]
    pub fn max_in(&self, lo: [f64; 2], hi: [f64; 2]) -> Option<f32> {
        let (dx, dy) = (self.max[0] - self.min[0], self.max[1] - self.min[1]);
        if dx <= 0.0 || dy <= 0.0 {
            return None;
        }
        // Only the rows and columns the box covers, not the whole patch.
        let span = |a: f64, b: f64, lo: f64, n: usize, d: f64| {
            let f = |v: f64| (((v - lo) / d) * n as f64).floor().clamp(0.0, n as f64) as usize;
            (f(a.min(b)), f(a.max(b)).min(n.saturating_sub(1)) + 1)
        };
        let (c0, c1) = span(lo[0], hi[0], self.min[0], self.w, dx);
        // Rows run north to south.
        let (r0, r1) = span(self.max[1] - hi[1], self.max[1] - lo[1], 0.0, self.h, dy);
        let mut best = f32::NEG_INFINITY;
        for row in r0..r1.min(self.h) {
            for col in c0..c1.min(self.w) {
                let v = self.samples[row * self.w + col];
                if v.is_finite() && v > best {
                    best = v;
                }
            }
        }
        best.is_finite().then_some(best)
    }

    /// The median of every valid sample in the patch.
    #[must_use]
    pub fn median(&self) -> Option<f32> {
        self.median_where(|_| true)
    }

    /// How much of the patch carries a measurement. A DTM patch over a bridge deck
    /// is mostly holes, and a consumer wants to know that before trusting a median
    /// taken from the handful of samples that survived.
    #[must_use]
    pub fn coverage(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        self.samples.iter().filter(|v| v.is_finite()).count() as f32 / self.samples.len() as f32
    }
}

/// The layer: a cache of measured patches, and the throttled queue that fills it.
#[derive(Resource)]
pub struct HeightLayer {
    dispatch: Dispatcher<HeightRequest>,
    #[allow(clippy::type_complexity)]
    results: Arc<Mutex<Vec<(HeightKey, Result<HeightPatch, String>)>>>,
    /// Device-cache probes that have come back: the patch when the device had
    /// it, otherwise the request and its rank, to go on to the network queue.
    #[allow(clippy::type_complexity)]
    probed: Arc<
        Mutex<
            Vec<(
                HeightRequest,
                crate::platform::fetch_gate::Rank,
                Option<HeightPatch>,
            )>,
        >,
    >,
    patches: HashMap<HeightKey, HeightPatch>,
    requested: HashSet<HeightKey>,
    failed: HashMap<HeightKey, u32>,
    /// RD position the cache was last pruned around, so a stationary anchor costs a
    /// comparison rather than a scan of every patch every frame.
    pruned_at: Option<[f64; 2]>,
    /// Patches fetched this session, for the HUD/metrics readout.
    pub fetched: usize,
    /// Measured ground level at the map anchor, metres above NAP — `None` outside
    /// AHN, with the layer off, or before the first patch lands. The layer's own
    /// end-to-end proof: it appears in `--dump-state`, so "is the height layer
    /// working here?" is one headless capture rather than a debugging session.
    pub ground_m: Option<f32>,
}

impl Default for HeightLayer {
    fn default() -> Self {
        Self {
            dispatch: Dispatcher::new(HEIGHT_CONCURRENCY, HEIGHT_MIN_INTERVAL_S),
            results: Arc::default(),
            probed: Arc::default(),
            patches: HashMap::new(),
            requested: HashSet::new(),
            failed: HashMap::new(),
            pruned_at: None,
            fetched: 0,
            ground_m: None,
        }
    }
}

impl HeightLayer {
    /// Could there be height data for this place? AHN's own envelope, which is a
    /// **bounding box** and therefore also claims slices of Belgium and Germany
    /// that the flights never covered. Tightening it would mean inventing an
    /// outline of the Netherlands; the real answer for those slices arrives as a
    /// patch of pure no-data, which the consumers already have to handle — a
    /// bridge over the Waddenzee has holes in the DTM for the same reason. This is
    /// the cheap filter that keeps the layer from asking about Sydney, not a
    /// promise that an answer exists.
    #[must_use]
    pub fn covers(lat: f64, lng: f64) -> bool {
        (AHN_LAT.0..=AHN_LAT.1).contains(&lat) && (AHN_LON.0..=AHN_LON.1).contains(&lng)
    }

    /// Ask for a patch.
    ///
    /// Returns it when it is already here, and otherwise queues the fetch and
    /// answers `None` — the caller is expected to ask again on a later frame and to
    /// have a fallback for "never arrives". Re-asking is free; the key deduplicates
    /// against both the cache and the in-flight set.
    pub fn patch(&mut self, req: HeightRequest) -> Option<&HeightPatch> {
        self.patch_ranked(req, crate::platform::fetch_gate::ANY)
    }

    /// [`Self::patch`], queued ahead of everything with a higher rank.
    ///
    /// **One queue serves every caller**, three requests in flight, so the order
    /// is the load time: the terrain field and the bridge decks gate a map
    /// settling, while the dike crests only feed a label. Queued first-come, the
    /// field's patch waited behind a couple of hundred crest boxes (measured on the
    /// Deck, 2026-09-24). [`RANK_FIELD`] and [`RANK_DECKS`] go first; everything
    /// else keeps its arrival order behind them.
    pub fn patch_ranked(
        &mut self,
        req: HeightRequest,
        rank: crate::platform::fetch_gate::Rank,
    ) -> Option<&HeightPatch> {
        if !req.valid() {
            return None;
        }
        let key = req.key();
        if self.patches.contains_key(&key) {
            return self.patches.get(&key);
        }
        if !self.requested.contains(&key) && *self.failed.get(&key).unwrap_or(&0) < MAX_ATTEMPTS {
            self.requested.insert(key);
            // **The device first, and outside the queue.** The queue is spaced for
            // PDOK's sake (`HEIGHT_MIN_INTERVAL_S`), and a warm start asks for the
            // same ~170 boxes the last one did: behind the spacing, reading them
            // off the disk would still take half a minute. A miss joins the queue
            // at the rank it was asked with, from `poll_height_layer`.
            match IoTaskPool::try_get() {
                Some(pool) => {
                    let probed = self.probed.clone();
                    pool.spawn(async move {
                        let hit = cached_patch(req).await;
                        if let Ok(mut g) = probed.lock() {
                            g.push((req, rank, hit));
                        }
                    })
                    .detach();
                }
                None => self.dispatch.queue_ranked(req, rank),
            }
        }
        None
    }

    /// Drop one cached patch, keeping the record that it was asked for.
    ///
    /// For the consumer that reads a patch **once** and keeps its own baked copy —
    /// `terrain`'s fine tier, whose 1024² box is 4 MB and whose next ask is a different
    /// box anyway. Without this the distance prune ([`CACHE_KEEP_M`], 8 km) is the only
    /// removal there is, and walking a kilometre would leave ten of them resident for a
    /// consumer that will never look at one again.
    ///
    /// `requested` and `failed` are deliberately untouched: this says "I am done with
    /// the answer", not "ask again". A caller that wants a re-fetch has moved its box,
    /// which is a different key.
    pub fn drop_patch(&mut self, req: &HeightRequest) {
        self.patches.remove(&req.key());
    }

    /// A patch that has already landed, without asking for it — for a caller that
    /// has queued several with [`Self::patch`] and now reads them together.
    #[must_use]
    pub fn get(&self, req: &HeightRequest) -> Option<&HeightPatch> {
        self.patches.get(&req.key())
    }

    /// Whether a request has been answered — by a patch or by giving up on it. Lets
    /// a consumer tell "not yet" from "never", so it can settle for its fallback
    /// instead of rebuilding every frame in hope.
    #[must_use]
    pub fn settled(&self, req: &HeightRequest) -> bool {
        let key = req.key();
        self.patches.contains_key(&key) || *self.failed.get(&key).unwrap_or(&0) >= MAX_ATTEMPTS
    }

    /// Drop everything the layer remembers — the patches, the give-up counts and the
    /// record of what is in flight.
    /// Keep only what is within [`CACHE_KEEP_M`] of `centre` (RD metres), and do
    /// nothing at all while the anchor sits still.
    ///
    /// This is the eviction the cache never had. It is keyed on distance rather than
    /// on the re-anchor generation because a patch key is an absolute RD box: the
    /// bridge you are standing on is the same measurement before and after a
    /// re-anchor, and clearing wholesale would re-fetch it from PDOK — a shared
    /// public service we are a guest on — for no gain.
    pub fn prune_around(&mut self, centre: [f64; 2]) {
        // A re-anchor moves the world origin by hundreds of metres at least; a metre
        // of slop keeps float noise from re-scanning the cache every frame.
        if self
            .pruned_at
            .is_some_and(|p| (p[0] - centre[0]).hypot(p[1] - centre[1]) < 1.0)
        {
            return;
        }
        self.pruned_at = Some(centre);
        let near = |k: &HeightKey| {
            let c = k.centre();
            (c[0] - centre[0]).hypot(c[1] - centre[1]) <= CACHE_KEEP_M
        };
        let before = self.patches.len();
        self.patches.retain(|k, _| near(k));
        self.failed.retain(|k, _| near(k));
        // `requested` is left alone: those fetches are already out, and forgetting a
        // key here would let the next ask queue a second request for the same box.
        if before != self.patches.len() {
            tracing::debug!(
                dropped = before - self.patches.len(),
                kept = self.patches.len(),
                "height patches pruned"
            );
        }
    }
}

/// Dispatch queued patch requests and drain the answers. One `IoTaskPool` task per
/// request: fetch the GeoTIFF, decode it, push the result — the same seam the tile
/// and 3DBAG streamers use, so it works unchanged in the browser.
pub fn poll_height_layer(mut layer: ResMut<HeightLayer>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    let probed: Vec<_> = layer
        .probed
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (req, rank, hit) in probed {
        let key = req.key();
        match hit {
            Some(patch) => {
                layer.requested.remove(&key);
                layer.patches.insert(key, patch);
            }
            None => layer.dispatch.queue_ranked(req, rank),
        }
    }
    let results = layer.results.clone();
    while let Some(req) = layer.dispatch.next(now) {
        let results = results.clone();
        let key = req.key();
        IoTaskPool::get()
            .spawn(async move {
                let out = fetch_patch(req).await;
                if let Ok(mut g) = results.lock() {
                    g.push((key, out));
                }
            })
            .detach();
    }

    let done: Vec<(HeightKey, Result<HeightPatch, String>)> = layer
        .results
        .lock()
        .map(|mut g| g.drain(..).collect())
        .unwrap_or_default();
    for (key, out) in done {
        layer.dispatch.completed();
        layer.requested.remove(&key);
        match out {
            Ok(patch) => {
                layer.fetched += 1;
                tracing::debug!(
                    ?key,
                    coverage = patch.coverage(),
                    median = patch.median(),
                    "height patch"
                );
                layer.patches.insert(key, patch);
            }
            Err(e) => {
                let n = layer.failed.entry(key).or_insert(0);
                *n += 1;
                tracing::warn!(?key, attempt = *n, "height patch failed: {e}");
            }
        }
    }
}

/// How far around the map anchor the ground probe measures, and how finely. 100 m
/// of ground at ~6 m per sample: enough to be a *level* rather than one lucky
/// laser return, and small enough to be one small request per re-anchor.
const GROUND_PROBE_HALF_M: f64 = 50.0;
const GROUND_PROBE_GRID: u32 = 16;

/// Keep [`HeightLayer::ground_m`] — the measured ground level under the map anchor
/// — current.
///
/// This is the layer's first consumer and its self-test: one small request per
/// re-anchor, whose answer says whether the whole path (URL, proxy, projection,
/// GeoTIFF decode) works *here*. It costs one patch per place you fly to.
///
/// It also owns the cache's **removal path**, because it is the one system that
/// runs every frame holding both the anchor and the layer toggle (it is ordered
/// after `apply_reanchor` and before `poll_height_layer` for the probe's own sake,
/// which is exactly where an eviction wants to be): the anchor moving prunes what
/// is now far away, which is also what empties the cache on leaving the Netherlands
/// — the only way out of coverage now that the layer has no switch.
pub fn probe_anchor_ground(
    mut layer: ResMut<HeightLayer>,
    map_anchor: Res<crate::resources::MapAnchor>,
) {
    let Some((lat, lng, _)) = map_anchor.geo else {
        return;
    };
    // Prune before the coverage test, not after: flying from Groningen to Berlin is
    // precisely the case where the whole Dutch cache should go, and outside AHN the
    // early return below would otherwise skip the only eviction there is. RD is
    // meaningless outside the Netherlands, but it is only ever used here to say
    // "further away than anything we could still ask about", which it does fine.
    layer.prune_around(wgs84_to_rd(lat, lng));
    if !HeightLayer::covers(lat, lng) {
        layer.ground_m = None;
        return;
    }
    let req = HeightRequest::around(
        HeightModel::Terrain,
        wgs84_to_rd(lat, lng),
        GROUND_PROBE_HALF_M,
        GROUND_PROBE_GRID,
    );
    let measured = layer.patch(req).and_then(HeightPatch::median);
    if measured.is_some() {
        layer.ground_m = measured;
    }
}

/// Where a patch is kept on the device: the request's own identity, so the same
/// box asked for on the next launch is the same file.
///
/// **The box is fetched as the key says, in whole metres** ([`HeightRequest::quantised`]).
/// Two launches measure a deck from geometry that can differ in the last float
/// bit, and a cached answer must describe exactly the box it is handed back for.
/// The in-memory cache has always shared a patch between requests that round
/// alike, so no consumer depends on the sub-metre part.
///
/// `source` separates the flights: `v1` is PDOK's AHN4, `atlas-v1` atlas's
/// answer — AHN6 where it is published — so a patch measured before atlas served
/// heights is never handed back as one it measured.
fn device_key(key: HeightKey, source: &str) -> String {
    format!(
        "height/{source}/{}/{}_{}_{}_{}_{}.tif",
        key.model.coverage(),
        key.min[0],
        key.min[1],
        key.max[0],
        key.max[1],
        key.grid
    )
}

/// A patch this device measured before, if it has it.
async fn cached_patch(req: HeightRequest) -> Option<HeightPatch> {
    let req = req.quantised();
    let bytes = crate::platform::storage::get(
        crate::platform::storage::Namespace::Cache,
        &device_key(req.key(), device_source()),
    )
    .await?;
    patch_from(req, &bytes).ok()
}

fn patch_from(req: HeightRequest, bytes: &[u8]) -> Result<HeightPatch, String> {
    let grid = req.grid.clamp(2, MAX_GRID);
    let samples = decode_float_tiff(bytes, grid as usize)?;
    Ok(HeightPatch {
        min: req.min,
        max: req.max,
        w: grid as usize,
        h: grid as usize,
        samples,
    })
}

/// Atlas's `/height`: the same question as PDOK's `GetCoverage`, answered from
/// **AHN6** wherever that flight has been published and from AHN4 elsewhere
/// (`crates/atlas/src/ahn6.rs`). PDOK serves AHN4 only, and AHN4 was flown while
/// Groningen's southern ring was an open trench — over the roofed tunnel it reads
/// −7.4 m NAP where people now walk on a park at +2.4.
///
/// `CARTO_HEIGHT_URL` overrides it; an empty value asks PDOK directly.
fn atlas_height_url() -> Option<String> {
    let url = std::env::var("CARTO_HEIGHT_URL").unwrap_or_else(|_| {
        if cfg!(target_arch = "wasm32") {
            "/height".to_string()
        } else {
            "https://cartopolis.org/height".to_string()
        }
    });
    (!url.is_empty()).then_some(url)
}

/// Whether atlas answers `/height`: unknown until the first patch asks, then
/// latched. A host that has not been given the route answers 404 — or, on the
/// web, the viewer's own page — and every later patch goes to PDOK, so a session
/// measures everything against one flight rather than alternating.
static ATLAS_HEIGHTS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
const ATLAS_UNKNOWN: u8 = 0;
const ATLAS_SERVES: u8 = 1;
const ATLAS_ABSENT: u8 = 2;

/// Which flight a patch from this session is stored under ([`device_key`]).
fn device_source() -> &'static str {
    use std::sync::atomic::Ordering;
    if atlas_height_url().is_none() || ATLAS_HEIGHTS.load(Ordering::Relaxed) == ATLAS_ABSENT {
        "v1"
    } else {
        "atlas-v1"
    }
}

/// Fetch and decode one patch — from atlas, else PDOK — and keep it on the device.
async fn fetch_patch(req: HeightRequest) -> Result<HeightPatch, String> {
    use std::sync::atomic::Ordering;
    let req = req.quantised();
    let grid = req.grid.clamp(2, MAX_GRID);
    if let Some(base) = atlas_height_url() {
        let state = ATLAS_HEIGHTS.load(Ordering::Relaxed);
        if state != ATLAS_ABSENT {
            let url = format!(
                "{base}?coverage={coverage}&bbox={x0:.2},{y0:.2},{x1:.2},{y1:.2}&grid={grid}",
                coverage = req.model.coverage(),
                x0 = req.min[0],
                y0 = req.min[1],
                x1 = req.max[0],
                y1 = req.max[1],
            );
            // Longer than the default: the first request over ground nobody has
            // visited makes atlas download the sheets under it — ~8 MB a square
            // kilometre, and a terrain field spans 6.4 km.
            let request = http::Request::get(url).timeout(std::time::Duration::from_secs(90));
            match http::fetch_optional(request).await {
                Ok(Some(bytes)) => match patch_from(req, &bytes) {
                    Ok(patch) => {
                        ATLAS_HEIGHTS.store(ATLAS_SERVES, Ordering::Relaxed);
                        crate::platform::storage::put(
                            crate::platform::storage::Namespace::Cache,
                            &device_key(req.key(), "atlas-v1"),
                            &bytes,
                        )
                        .await;
                        return Ok(patch);
                    }
                    // Not a height raster at all: a host without the route that
                    // answers every path with a page.
                    Err(e) if state == ATLAS_UNKNOWN => {
                        tracing::info!(error = %e, "atlas serves no heights; asking PDOK");
                        ATLAS_HEIGHTS.store(ATLAS_ABSENT, Ordering::Relaxed);
                    }
                    Err(e) => return Err(e),
                },
                Ok(None) => {
                    tracing::info!("atlas serves no heights; asking PDOK");
                    ATLAS_HEIGHTS.store(ATLAS_ABSENT, Ordering::Relaxed);
                }
                // Once atlas has answered, a failure is a failure to retry — not
                // a reason to measure this one patch against another flight.
                Err(e) if state == ATLAS_SERVES => return Err(e),
                Err(e) => {
                    tracing::info!(error = %e, "atlas heights unreachable; asking PDOK");
                }
            }
        }
    }
    let path = format!(
        "{AHN_WCS_PATH}?SERVICE=WCS&VERSION=1.0.0&REQUEST=GetCoverage\
         &COVERAGE={coverage}&CRS=EPSG:28992&FORMAT=GEOTIFF\
         &BBOX={x0:.2},{y0:.2},{x1:.2},{y1:.2}&WIDTH={grid}&HEIGHT={grid}",
        coverage = req.model.coverage(),
        x0 = req.min[0],
        y0 = req.min[1],
        x1 = req.max[0],
        y1 = req.max[1],
    );
    let url = http::upstream_url("ahn", AHN_ORIGIN, &path);
    let bytes = http::fetch(http::Request::get(url)).await?;
    let patch = patch_from(req, &bytes)?;
    // Only what decoded: a service exception arrives as a 200, and keeping it
    // would answer every later launch with the same refusal.
    crate::platform::storage::put(
        crate::platform::storage::Namespace::Cache,
        &device_key(req.key(), "v1"),
        &bytes,
    )
    .await;
    Ok(patch)
}

/// Decode a single-band float32 GeoTIFF into metres, with no-data as `NaN`.
///
/// The georeferencing tags are ignored on purpose: we asked for an exact box at an
/// exact grid size and the service honours both, so the request *is* the transform.
/// Reading it back out of the file would only add a way for the two to disagree.
/// WGS84 → **RD New** (EPSG:28992), the Dutch national grid, in metres.
///
/// The published polynomial approximation about the Amersfoort datum point, good to
/// a few centimetres inside the Netherlands and meaningless outside it — which is
/// exactly the domain AHN covers, so [`HeightLayer::covers`] is what keeps callers
/// inside it. A real projection library would be several megabytes of dependency to
/// answer one question a dozen coefficients answer here.
#[must_use]
pub fn wgs84_to_rd(lat: f64, lng: f64) -> [f64; 2] {
    cartopolis_geo::ahn::wgs84_to_rd(lat, lng)
}

/// **RD New** → WGS84, the same approximation run the other way.
///
/// Test-only since the city registers moved to atlas (2026-10-19), which converts
/// them itself. It earns its place in the round-trip test, whose two coefficient
/// sets are transcribed independently, so it catches a mistyped forward
/// coefficient that a one-way check would happily accept.
#[cfg(test)]
#[must_use]
pub fn rd_to_wgs84(x: f64, y: f64) -> (f64, f64) {
    let dx = (x - 155_000.0) / 100_000.0;
    let dy = (y - 463_000.0) / 100_000.0;
    let lat = 52.155_174_40
        + (3_235.65389 * dy
            - 32.58297 * dx * dx
            - 0.24750 * dy * dy
            - 0.84978 * dx * dx * dy
            - 0.06550 * dy * dy * dy
            - 0.01709 * dx * dx * dy * dy
            - 0.00738 * dx
            + 0.00530 * dx * dx * dx * dx
            - 0.00039 * dx * dx * dy * dy * dy
            + 0.00033 * dx * dx * dx * dx * dy
            - 0.00012 * dx * dy)
            / 3600.0;
    let lng = 5.387_206_21
        + (5_260.52916 * dx + 105.94684 * dx * dy + 2.45656 * dx * dy * dy
            - 0.81885 * dx * dx * dx
            + 0.05594 * dx * dy * dy * dy
            - 0.05607 * dx * dx * dx * dy
            + 0.01199 * dy
            + 0.00256 * dx * dx * dx * dy * dy
            + 0.00128 * dx * dy * dy * dy * dy
            + 0.00022 * dy * dy
            + 0.00026 * dx * dx * dx * dx * dx)
            / 3600.0;
    (lat, lng)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The datum point the whole approximation is expanded about: the Onze Lieve
    /// Vrouwetoren in Amersfoort is (155000, 463000) by construction.
    #[test]
    fn the_amersfoort_datum_point_is_the_grid_origin() {
        let rd = wgs84_to_rd(52.155_174_40, 5.387_206_21);
        assert!((rd[0] - 155_000.0).abs() < 0.01, "x {}", rd[0]);
        assert!((rd[1] - 463_000.0).abs() < 0.01, "y {}", rd[1]);
    }

    /// Both directions of the approximation, against each other, across the country
    /// — Groningen to Maastricht is the whole diagonal of the domain.
    #[test]
    fn the_rd_conversion_round_trips_across_the_country() {
        for (lat, lng) in [
            (53.2194, 6.5665), // Groningen
            (52.3676, 4.9041), // Amsterdam
            (51.9244, 4.4777), // Rotterdam
            (50.8514, 5.6910), // Maastricht
            (53.4000, 5.3000), // Vlieland, up in the Wadden
        ] {
            let rd = wgs84_to_rd(lat, lng);
            let (back_lat, back_lng) = rd_to_wgs84(rd[0], rd[1]);
            // A metre is ~9e-6° of latitude; hold both to well inside that.
            assert!(
                (back_lat - lat).abs() < 2e-6 && (back_lng - lng).abs() < 4e-6,
                "{lat},{lng} → {rd:?} → {back_lat},{back_lng}"
            );
            // And the grid values themselves have to be in the country: RD is
            // ~0–280 km east, ~300–630 km north over the Netherlands.
            assert!(
                (0.0..300_000.0).contains(&rd[0]) && (300_000.0..640_000.0).contains(&rd[1]),
                "{lat},{lng} → {rd:?} is off the Dutch grid"
            );
        }
    }

    /// Groningen station, where the bridge work is being looked at, lands where the
    /// Dutch grid says it does — a check against the coefficients being transcribed
    /// plausibly but wrongly, which a round trip alone would never catch.
    #[test]
    fn a_known_place_lands_on_its_known_grid_square() {
        let rd = wgs84_to_rd(53.2105, 6.5624);
        assert!(
            (rd[0] - 233_512.0).abs() < 50.0 && (rd[1] - 581_070.0).abs() < 50.0,
            "Groningen station came out at {rd:?}"
        );
    }

    /// The filter is AHN's own bounding box, so it says yes to the whole country
    /// and no to the rest of the world — but it is a *box*, and Brussels sits
    /// inside it. That is deliberate (see [`HeightLayer::covers`]): the honest
    /// answer there is a patch of no-data, not a coastline we made up.
    #[test]
    fn coverage_is_the_netherlands_give_or_take_a_bounding_box() {
        assert!(HeightLayer::covers(53.2194, 6.5665)); // Groningen
        assert!(HeightLayer::covers(50.8514, 5.6910)); // Maastricht
        assert!(HeightLayer::covers(50.8467, 4.3499)); // Brussels — inside the box
        assert!(!HeightLayer::covers(52.5200, 13.4050)); // Berlin
        assert!(!HeightLayer::covers(48.8566, 2.3522)); // Paris
        assert!(!HeightLayer::covers(-33.8688, 151.2093)); // Sydney
    }

    fn patch(samples: Vec<f32>, w: usize) -> HeightPatch {
        HeightPatch {
            min: [0.0, 0.0],
            max: [100.0, 100.0],
            w,
            h: samples.len() / w,
            samples,
        }
    }

    /// A box reads only its own samples, north row first like the rest.
    #[test]
    fn the_highest_sample_in_a_box_is_that_boxes_own() {
        // 4×4 over 100 m: row 0 (north) holds the peak at its west end, and one
        // more high sample sits in the south-east corner.
        let mut s = vec![1.0; 16];
        s[0] = 9.0;
        s[15] = 5.0;
        s[5] = f32::NAN;
        let p = patch(s, 4);
        // The north-west quarter: RD x 0..50, y 50..100.
        assert_eq!(p.max_in([0.0, 50.0], [49.0, 100.0]), Some(9.0));
        // The south-east quarter.
        assert_eq!(p.max_in([50.0, 0.0], [100.0, 49.0]), Some(5.0));
        // Outside the patch entirely.
        assert_eq!(p.max_in([200.0, 200.0], [300.0, 300.0]), None);
    }

    /// Row 0 is the *north* edge, which is the order a GeoTIFF arrives in and the
    /// easiest thing in this module to get upside down.
    #[test]
    fn the_first_row_is_the_north_edge() {
        let p = patch(vec![1.0, 2.0, 3.0, 4.0], 2);
        assert_eq!(p.at([25.0, 75.0]), Some(1.0)); // north-west
        assert_eq!(p.at([75.0, 75.0]), Some(2.0)); // north-east
        assert_eq!(p.at([25.0, 25.0]), Some(3.0)); // south-west
        assert_eq!(p.at([75.0, 25.0]), Some(4.0)); // south-east
        assert_eq!(p.at([-1.0, 50.0]), None, "outside the box");
    }

    /// The deck-height case in miniature: a handful of parapet and lamp-post
    /// samples must not drag the answer up off the road surface.
    #[test]
    fn the_median_ignores_what_is_standing_on_the_deck() {
        let p = patch(vec![6.0, 6.1, 9.5, 6.0, 6.2, 12.0, 6.1, 6.0, 6.1], 3);
        let m = p.median().unwrap();
        assert!((m - 6.1).abs() < 1e-5, "median {m}");
    }

    /// No-data is what a DTM says over a bridge deck, and it must neither count as
    /// a height nor poison the median.
    #[test]
    fn no_data_is_absent_rather_than_zero() {
        let p = patch(vec![f32::NAN, 2.0, f32::NAN, 4.0], 2);
        assert_eq!(p.at([25.0, 75.0]), None);
        assert_eq!(p.median(), Some(4.0));
        assert!((p.coverage() - 0.5).abs() < 1e-6);
        let empty = patch(vec![f32::NAN; 4], 2);
        assert_eq!(empty.median(), None);
        assert_eq!(empty.coverage(), 0.0);
    }

    /// The ring-around-a-thing query the bridge code is built on: keep the ground
    /// outside the deck, drop everything on it.
    #[test]
    fn a_predicate_selects_the_ground_around_a_thing() {
        let p = patch(vec![9.0, 9.0, 9.0, 9.0, 9.0, 2.0, 9.0, 9.0, 9.0], 3);
        // The middle sample is the deck; everything else is ground at 9.
        let outside = p
            .median_where(|rd| (rd[0] - 50.0).abs() > 20.0 || (rd[1] - 50.0).abs() > 20.0)
            .unwrap();
        assert!((outside - 9.0).abs() < 1e-5, "ground came out at {outside}");
    }

    /// Two consumers asking about the same thing from geometry that differs in the
    /// last bit must share one fetch.
    #[test]
    fn requests_that_mean_the_same_box_share_a_key() {
        let a = HeightRequest::around(HeightModel::Surface, [233_512.0, 581_070.0], 60.0, 48);
        let b = HeightRequest::around(
            HeightModel::Surface,
            [233_512.000_3, 581_069.999_7],
            60.0,
            48,
        );
        assert_eq!(a.key(), b.key());
        // …but a different model, box or grid is a different question.
        let c = HeightRequest::around(HeightModel::Terrain, [233_512.0, 581_070.0], 60.0, 48);
        let d = HeightRequest::around(HeightModel::Surface, [233_512.0, 581_070.0], 60.0, 32);
        assert_ne!(a.key(), c.key());
        assert_ne!(a.key(), d.key());
        assert_ne!(a.key(), a.padded(25.0).key());
    }

    /// The cache used to have no removal path at all. Flying away must drop the
    /// measurements you flew away from and keep the ones under you — and switching
    /// the layer off must leave nothing behind.
    #[test]
    fn the_cache_forgets_what_you_flew_away_from() {
        let groningen = [233_512.0, 581_070.0];
        let maastricht = [176_000.0, 318_000.0];
        let mut layer = HeightLayer::default();
        for centre in [groningen, maastricht] {
            let req = HeightRequest::around(HeightModel::Terrain, centre, 60.0, 8);
            layer.patches.insert(
                req.key(),
                HeightPatch {
                    min: req.min,
                    max: req.max,
                    w: 1,
                    h: 1,
                    samples: vec![1.0],
                },
            );
            layer.failed.insert(req.padded(25.0).key(), MAX_ATTEMPTS);
        }
        assert_eq!(layer.patches.len(), 2);

        layer.prune_around(groningen);
        assert_eq!(layer.patches.len(), 1, "the far patch should be gone");
        assert_eq!(layer.failed.len(), 1, "and so should its give-up count");
        assert!(layer
            .patch(HeightRequest::around(
                HeightModel::Terrain,
                groningen,
                60.0,
                8
            ))
            .is_some());

        // Standing still is not a reason to re-scan, and must not lose anything.
        layer.prune_around(groningen);
        assert_eq!(layer.patches.len(), 1);
    }

    /// An OGC service reports a refused request as XML with a 200, so the decoder
    /// has to surface what it said instead of a magic-number complaint.
    #[test]
    fn a_service_exception_comes_back_as_its_own_text() {
        let xml = b"<ServiceExceptionReport><ServiceException>msWCSGetCoverage(): \
                    Requested BBOX is outside the coverage</ServiceException>";
        let err = decode_float_tiff(xml, 48).unwrap_err();
        assert!(err.contains("outside the coverage"), "{err}");
    }
}
