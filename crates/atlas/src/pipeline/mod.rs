//! The map data pipelines: extractors that turn the Dutch open registers into the
//! per-cell files the client streams (`<dataset>/14/{x}/{y}.json` plus a
//! `coverage.json` per dataset, served as `3dbag.cartopolis.org`). One is not
//! Dutch and not z14: `tall` pre-cuts the client's z11 Overpass query for the
//! horizon's tall structures into `tall/11/{x}/{y}.json`.
//!
//! Ported from the earlier TypeScript pipeline (in a separate repository), which was itself ported
//! from the Python scripts that used to live in `tools/`. The formats are unchanged
//! byte for byte in meaning — the client's parsers (`lod22.rs`,
//! `building_facts.rs`, `surveyed.rs`, `osm_buildings.rs`, `dikes.rs`,
//! `water_levels.rs`) read them — and each port was diffed against the TypeScript
//! output for the same area before it replaced it (`docs/notes/atlas.md`).
//!
//! **A dataset is a blocking function over a [`Ctx`].** It pages somebody else's
//! open-data service a few thousand times, so it runs on a blocking thread with a
//! blocking HTTP client; the async runtime only owns the queue and the HTTP API.
//! The `Ctx` is the only way a dataset reaches the world, which is what lets a
//! web UI drive it: the log is a stream somebody watches, the progress is a bar,
//! and the cancel is a button checked inside every paging loop.

pub mod bag;
pub mod bgt;
pub mod bgt_extract;
pub mod brp;
pub mod buildings;
pub mod bv3d;
pub mod dikes;
pub mod gardens;
pub mod geom;
pub mod http;
pub mod lod22;
pub mod models;
pub mod rd;
pub mod roofs;
pub mod tall;
pub mod terrain;
pub mod terrain_grade;
pub mod tile;
pub mod water;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How far along a dataset is, for the web UI's progress bar.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Progress {
    /// What it is doing now, in the words the log uses ("panden", "wegdeel").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Units finished, in whatever the phase counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    /// Units expected; `Some(None)` is "the source will not say", which is common
    /// and is sent as `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<Option<u64>>,
}

impl Progress {
    /// Fold an update in, field by field, as the web UI's `{...old, ...new}` did.
    pub fn merge(&mut self, update: Progress) {
        if update.phase.is_some() {
            self.phase = update.phase;
        }
        if update.done.is_some() {
            self.done = update.done;
        }
        if update.total.is_some() {
            self.total = update.total;
        }
    }
}

/// One named total — "94,052 buildings". A list of pairs, in the order worth
/// reading, rather than a map.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Count {
    pub label: String,
    pub n: u64,
}

impl Count {
    pub fn new(label: &str, n: usize) -> Self {
        Self {
            label: label.to_string(),
            n: n as u64,
        }
    }
}

/// What a dataset reports when it finishes.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DatasetResult {
    /// Cells written, for the web UI's coverage view.
    pub cells: usize,
    pub counts: Vec<Count>,
}

#[derive(Debug, PartialEq)]
pub enum PipelineError {
    /// Stopped from the web UI. Reported as *cancelled*, not as a failure.
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for PipelineError {
    fn from(e: String) -> Self {
        Self::Failed(e)
    }
}

pub type Outcome<T> = Result<T, PipelineError>;

/// Where a dataset's log lines and progress go.
pub trait Sink: Send + Sync {
    fn log(&self, line: String);
    fn progress(&self, update: Progress);
}

/// Everything a dataset is given, and the only way it reaches the world.
pub struct Ctx {
    /// RD New `[xmin, ymin, xmax, ymax]`.
    pub bbox: [f64; 4],
    /// The coverage name — "groningen", and what every manifest is stamped with.
    pub name: String,
    out_root: PathBuf,
    /// Atlas's own store (`AtlasConfig::data_dir`), for a dataset that fills
    /// something atlas serves itself rather than the site tree — `terrain`.
    data_dir: Option<PathBuf>,
    /// Ask every source again rather than reusing what was fetched within
    /// [`http::SOURCE_CACHE_TTL`] — what was fetched is still written back.
    fresh: bool,
    /// Requests kept in flight to one upstream ([`UPSTREAM_FETCHES`] unless
    /// the configuration says otherwise).
    upstream: usize,
    cancel: Arc<AtomicBool>,
    sink: Arc<dyn Sink>,
}

impl Ctx {
    pub fn new(
        bbox: [f64; 4],
        name: &str,
        out_root: &Path,
        cancel: Arc<AtomicBool>,
        sink: Arc<dyn Sink>,
    ) -> Self {
        Self {
            bbox,
            name: name.to_string(),
            out_root: out_root.to_path_buf(),
            data_dir: None,
            fresh: false,
            upstream: UPSTREAM_FETCHES,
            cancel,
            sink,
        }
    }

    /// `AtlasConfig::upstream_concurrency`, at least one.
    #[must_use]
    pub fn with_upstream(mut self, n: usize) -> Self {
        self.upstream = n.max(1);
        self
    }

    /// How many requests a dataset may keep in flight to one upstream — the
    /// `threads` every [`in_order`] walk is given.
    #[must_use]
    pub fn upstream(&self) -> usize {
        self.upstream
    }

    /// Fetch every source anew this run (`http::cached`).
    #[must_use]
    pub fn with_fresh(mut self, fresh: bool) -> Self {
        self.fresh = fresh;
        self
    }

    /// Where fetched source data is kept between runs, and whether this run may
    /// read it: `None` without a data directory, where nothing is kept.
    pub(crate) fn source_cache(&self) -> Option<(PathBuf, bool)> {
        self.data_dir
            .as_ref()
            .map(|d| (d.join("source-cache"), !self.fresh))
    }

    #[must_use]
    pub fn with_data_dir(mut self, data_dir: &Path) -> Self {
        self.data_dir = Some(data_dir.to_path_buf());
        self
    }

    /// Atlas's own store, or a failure naming the dataset that needed it.
    /// The served tree this run writes into.
    #[must_use]
    pub fn site_root(&self) -> &Path {
        &self.out_root
    }

    pub fn data_dir(&self) -> Outcome<&Path> {
        self.data_dir.as_deref().ok_or_else(|| {
            PipelineError::Failed(
                "this dataset writes atlas's data directory, and none was given".into(),
            )
        })
    }

    pub fn log(&self, message: impl Into<String>) {
        self.sink.log(message.into());
    }

    pub fn progress(&self, update: Progress) {
        self.sink.progress(update);
    }

    /// Called inside every paging loop, so a stop takes effect within one request.
    pub fn check(&self) -> Outcome<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(PipelineError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// A sleep a cancel interrupts, so a stop during a retry backoff does not wait
    /// the backoff out.
    pub fn sleep(&self, duration: Duration) -> Outcome<()> {
        let until = Instant::now() + duration;
        while Instant::now() < until {
            self.check()?;
            std::thread::sleep((until - Instant::now()).min(Duration::from_millis(250)));
        }
        Ok(())
    }

    /// Write one JSON file under the output root so that nothing ever serves a
    /// half-written one: the tree is served live, so each file is written beside
    /// its destination and renamed into place (a rename is only atomic within one
    /// filesystem, which is why not /tmp). Compact separators, as every earlier
    /// writer of these files used — this is megabytes per cell.
    pub fn write_json(&self, relative: &str, value: &serde_json::Value) -> Outcome<()> {
        let body = serde_json::to_vec(value).map_err(|e| PipelineError::Failed(e.to_string()))?;
        self.write_bytes(relative, &body)
    }

    /// Write one of the per-cell documents in `cartopolis_geo::formats`, the same
    /// atomic way [`Self::write_json`] writes a `Value`.
    ///
    /// Serialised **directly**, never through `serde_json::to_value`: the format
    /// types declare their fields in the sorted order a `Value` used to emit, and
    /// a `Value` would widen an `f32` height to the `f64` nearest it and change
    /// the bytes every installed client reads.
    pub fn write_doc<T: serde::Serialize>(&self, relative: &str, doc: &T) -> Outcome<()> {
        let body = cartopolis_geo::formats::to_vec(doc)
            .map_err(|e| PipelineError::Failed(format!("{relative}: {e}")))?;
        self.write_bytes(relative, &body)
    }

    /// Write a binary file, and a `.gz` of it beside it, the same atomic way
    /// [`Self::write_json`] writes JSON.
    ///
    /// The sidecar is what gets the file served compressed. Caddy's `encode gzip`
    /// compresses only a default list of text content types, and a `.bin` goes
    /// out as `application/octet-stream`, which is not on it — so the host's
    /// `file_server { precompressed gzip }` has to find `x.bin.gz` already there.
    /// Compressed at the best level, because it is done once per run and read by
    /// every client.
    pub fn write_bytes_gzipped(&self, relative: &str, body: &[u8]) -> Outcome<()> {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let compressed = gz
            .write_all(body)
            .and_then(|()| gz.finish())
            .map_err(|e| PipelineError::Failed(format!("{relative}: gzip: {e}")))?;
        // The plain file second, so that a reader who sees it can also find the
        // sidecar — Caddy looks for the `.gz` only once the plain file exists.
        self.write_bytes(&format!("{relative}.gz"), &compressed)?;
        self.write_bytes(relative, body)
    }

    fn write_bytes(&self, relative: &str, body: &[u8]) -> Outcome<()> {
        let file = self.out_root.join(relative);
        let dir = file
            .parent()
            .ok_or_else(|| PipelineError::Failed(format!("{relative}: no parent directory")))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| PipelineError::Failed(format!("{relative}: {e}")))?;
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, body)
            .map_err(|e| PipelineError::Failed(format!("{relative}: {e}")))?;
        std::fs::rename(&tmp, &file)
            .map_err(|e| PipelineError::Failed(format!("{relative}: {e}")))?;
        Ok(())
    }

    /// A file another dataset wrote into the site tree, if it is there — how the
    /// terrain grading reads the water levels (`water/14/x/y.json`).
    #[must_use]
    pub fn read_site(&self, relative: &str) -> Option<Vec<u8>> {
        std::fs::read(self.out_root.join(relative)).ok()
    }
}

/// Requests one dataset keeps in flight to one upstream — atlas's default
/// `upstream_concurrency`. Every source here is somebody else's public service,
/// so this is a courtesy ceiling as much as a speed: three is where a Groningen
/// run stopped being bound by one request's latency (2026-09-24) without
/// hammering anybody.
pub const UPSTREAM_FETCHES: usize = 3;

/// Run `work` over `items` on up to `threads` threads, and hand each answer to
/// `visit` on the calling thread **in item order**.
///
/// Order is the point. Every dataset was written as a serial walk — page after
/// page, block after block — and its output (which duplicate wins, the order
/// features land in a cell file) follows from that order, so a parallel fetch
/// that visited in arrival order would change files nobody meant to change.
/// Here only the waiting and the parsing overlap; `visit` sees exactly the
/// sequence the serial walk gave it.
///
/// A worker runs at most `threads` items ahead of `visit`, which bounds what is
/// held at once: a 3D basisvoorziening block is a few hundred MB parsed. A
/// cancel stops workers taking new items and ends the walk with
/// [`PipelineError::Cancelled`]; an `Err` from `visit` stops it with that error;
/// a panicking `work` is re-raised here once the others have stopped.
pub fn in_order<I, T>(
    ctx: &Ctx,
    items: &[I],
    threads: usize,
    work: impl Fn(&I) -> T + Sync,
    mut visit: impl FnMut(&I, T) -> Outcome<()>,
) -> Outcome<()>
where
    I: Sync,
    T: Send,
{
    use std::collections::BTreeMap;
    use std::sync::{mpsc, Condvar, Mutex, PoisonError};

    struct Gate {
        /// The next item a worker may take.
        next: usize,
        /// Items `visit` has had.
        visited: usize,
        stop: bool,
    }
    let threads = threads.clamp(1, items.len().max(1));
    let gate = Mutex::new(Gate {
        next: 0,
        visited: 0,
        stop: false,
    });
    let turn = Condvar::new();
    let halt = |gate: &Mutex<Gate>| {
        gate.lock().unwrap_or_else(PoisonError::into_inner).stop = true;
        turn.notify_all();
    };

    std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<(usize, T)>();
        for _ in 0..threads {
            let tx = tx.clone();
            let (gate, turn, work, halt) = (&gate, &turn, &work, &halt);
            s.spawn(move || loop {
                let i = {
                    let mut g = gate.lock().unwrap_or_else(PoisonError::into_inner);
                    while !g.stop && g.next < items.len() && g.next >= g.visited + threads {
                        g = turn.wait(g).unwrap_or_else(PoisonError::into_inner);
                    }
                    if g.stop || g.next >= items.len() {
                        return;
                    }
                    g.next += 1;
                    g.next - 1
                };
                if ctx.check().is_err() {
                    halt(gate);
                    return;
                }
                let answer = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(&items[i])
                })) {
                    Ok(answer) => answer,
                    // Stop the others first: they would otherwise wait for
                    // this item to be visited, and never exit.
                    Err(panic) => {
                        halt(gate);
                        std::panic::resume_unwind(panic);
                    }
                };
                if tx.send((i, answer)).is_err() {
                    return;
                }
            });
        }
        drop(tx);

        let mut pending = BTreeMap::new();
        let mut visited = 0usize;
        let mut result = Ok(());
        for (i, answer) in rx.iter() {
            pending.insert(i, answer);
            while let Some(answer) = pending.remove(&visited) {
                result = ctx.check().and_then(|()| visit(&items[visited], answer));
                visited += 1;
                gate.lock().unwrap_or_else(PoisonError::into_inner).visited = visited;
                turn.notify_all();
                if result.is_err() {
                    break;
                }
            }
            if result.is_err() {
                break;
            }
        }
        halt(&gate);
        drop(rx);
        // Every worker has exited when the channel closes; one that stopped
        // early did so because of a cancel (a panic is re-raised by the scope).
        if result.is_ok() && visited < items.len() {
            ctx.check()?;
            return Err(PipelineError::Failed("a fetch worker stopped early".into()));
        }
        result
    })
}

/// One extractor, as the web UI lists it.
pub struct Dataset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Where its `coverage.json` lives under the output root.
    pub coverage: &'static str,
    /// Roughly how long a city takes, for the web UI to set expectations.
    pub typical_minutes: u32,
    pub run: fn(&Ctx) -> Outcome<DatasetResult>,
}

/// The datasets, in the order a fresh area wants them. `buildings` before `lod22`
/// because the LoD1.2 footprints are the fallback tier where a LoD2.2 mesh is
/// missing, and `bag` after both because its records key on the identificatie the
/// meshes carry — nothing enforces that (each run is independent), but a
/// half-covered area looks less broken if the fallback lands first.
pub const DATASETS: &[Dataset] = &[
    Dataset {
        id: "water",
        label: "Water levels",
        description: "The height each canal, river and pond actually stands at, from the \
                      Kadaster 3D basisvoorziening. Without it the client draws water level \
                      with the quay beside it, because AHN has no reading over water.",
        coverage: "water/coverage.json",
        typical_minutes: 1,
        run: water::run,
    },
    Dataset {
        id: "buildings",
        label: "Buildings (LoD1.2)",
        description: "Footprints and heights from the 3DBAG WFS, shaped like Overpass so the \
                      client parses them unchanged. The fallback tier under the LoD2.2 meshes.",
        coverage: "coverage.json",
        typical_minutes: 1,
        run: buildings::run,
    },
    Dataset {
        id: "lod22",
        label: "Roof meshes (LoD2.2)",
        description: "Real roof and wall geometry, triangulated from 3DBAG CityJSON. The \
                      heaviest output by far — Groningen is ~115 MB.",
        coverage: "lod22/coverage.json",
        typical_minutes: 1,
        run: lod22::run,
    },
    Dataset {
        id: "models",
        label: "Building models (landmarks)",
        description: "Generated models for the landmarks the survey gets wrong — the Euroborg \
                      is a 23 m block with its pitch filled in — measured from the AHN \
                      surface model and drawn in place of their LoD2.2 shell. Reads the \
                      LoD2.2 cells to file each model where its shell is, so it runs after \
                      them.",
        coverage: "lod3/coverage.json",
        typical_minutes: 1,
        run: models::run,
    },
    Dataset {
        id: "roofs",
        label: "Roof colours (aerial photo)",
        description: "What each LoD2.2 roof is made of and what colour it is — red tiles, \
                      anthracite, bitumen, gravel, sedum, solar panels — measured from PDOK's \
                      aerial photograph inside the roof's own outline. Reads the LoD2.2 \
                      cells, so it runs after them.",
        coverage: "roofs/coverage.json",
        typical_minutes: 10,
        run: roofs::run,
    },
    Dataset {
        id: "bag",
        label: "Building facts (BAG)",
        description: "Year built, use, floor areas, addresses, entrance points, houseboat \
                      berths, and the national monuments and protected districts joined onto them.",
        coverage: "bag/coverage.json",
        typical_minutes: 1,
        run: bag::run,
    },
    Dataset {
        id: "dikes",
        label: "Flood defences (IMWA)",
        description: "The dikes, dunes and dams the water boards register, as named lines. \
                      The relief is already in the height model; this is what says the ridge \
                      is the Lauwersmeerdijk.",
        coverage: "dikes/coverage.json",
        typical_minutes: 1,
        run: dikes::run,
    },
    Dataset {
        id: "bgt",
        label: "Street objects (BGT)",
        description: "Surveyed trees, lampposts, benches, fences, hedges and the paving beside \
                      the carriageway, from the BGT.",
        coverage: "bgt/coverage.json",
        typical_minutes: 1,
        run: bgt::run,
    },
    Dataset {
        id: "brp",
        label: "Crop parcels (BRP)",
        description: "Every agricultural field with the crop grown on it this year, from the \
                      RVO's Basisregistratie Gewaspercelen. The client paints each field in its \
                      crop's tone instead of one generic green.",
        coverage: "brp/coverage.json",
        typical_minutes: 1,
        run: brp::run,
    },
    Dataset {
        id: "tall",
        label: "Tall structures (OSM)",
        description: "Wind turbines, windmills, masts, towers, chimneys, water towers and \
                      cranes from our Overpass, pre-cut into the z11 cells the client's \
                      horizon lane streams, over the area and 30 km around it — so a cold \
                      start reads 25 small files instead of queueing 25 live queries.",
        coverage: "tall/coverage.json",
        typical_minutes: 1,
        run: tall::run,
    },
    Dataset {
        id: "terrain",
        label: "Terrain tiles (AHN)",
        description: "Cuts every z14 height tile over the area into atlas's terrain store and \
                      grades it — water carved, holes filled, roads flattened, bridge approaches \
                      ramped — so the client only samples it, and publishes the bridge deck \
                      heights it measured. Reads the water levels, so it runs after them.",
        coverage: "terrain/coverage.json",
        typical_minutes: 1,
        run: terrain::run,
    },
];

#[must_use]
pub fn dataset(id: &str) -> Option<&'static Dataset> {
    DATASETS.iter().find(|d| d.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Quiet;
    impl Sink for Quiet {
        fn log(&self, _: String) {}
        fn progress(&self, _: Progress) {}
    }

    fn ctx(cancel: Arc<AtomicBool>) -> Ctx {
        Ctx::new(
            [0.0; 4],
            "t",
            std::path::Path::new("."),
            cancel,
            Arc::new(Quiet),
        )
    }

    /// The later items finish first, and are still visited last.
    #[test]
    fn answers_are_visited_in_item_order() {
        let items: Vec<u64> = (0..40).collect();
        let mut seen = Vec::new();
        in_order(
            &ctx(Arc::new(AtomicBool::new(false))),
            &items,
            4,
            |&i| {
                std::thread::sleep(Duration::from_millis((40 - i) % 7));
                i * 10
            },
            |&i, answer| {
                assert_eq!(answer, i * 10);
                seen.push(i);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(seen, items);
    }

    /// Nothing is started far ahead of what has been visited.
    #[test]
    fn workers_stay_within_the_window() {
        let started = AtomicUsize::new(0);
        let items: Vec<usize> = (0..30).collect();
        in_order(
            &ctx(Arc::new(AtomicBool::new(false))),
            &items,
            3,
            |_| started.fetch_add(1, Ordering::SeqCst),
            |&i, _| {
                assert!(started.load(Ordering::SeqCst) <= i + 1 + 3);
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn a_failed_visit_stops_the_walk() {
        let worked = AtomicUsize::new(0);
        let items: Vec<usize> = (0..100).collect();
        let err = in_order(
            &ctx(Arc::new(AtomicBool::new(false))),
            &items,
            3,
            |_| worked.fetch_add(1, Ordering::SeqCst),
            |&i, _| {
                if i == 5 {
                    Err(PipelineError::Failed("disk full".into()))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(err, PipelineError::Failed("disk full".into()));
        assert!(worked.load(Ordering::SeqCst) < 20);
    }

    #[test]
    fn a_cancel_ends_it_as_cancelled() {
        let cancel = Arc::new(AtomicBool::new(false));
        let items: Vec<usize> = (0..100).collect();
        let err = in_order(
            &ctx(cancel.clone()),
            &items,
            3,
            |&i| {
                if i == 10 {
                    cancel.store(true, Ordering::SeqCst);
                }
            },
            |_, ()| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err, PipelineError::Cancelled);
    }

    /// A panic reaches the caller rather than leaving the others waiting on an
    /// item that will never be visited.
    #[test]
    fn a_panicking_worker_does_not_hang_the_rest() {
        let items: Vec<usize> = (0..50).collect();
        let panicked = std::panic::catch_unwind(|| {
            in_order(
                &ctx(Arc::new(AtomicBool::new(false))),
                &items,
                3,
                |&i| assert!(i != 7, "item 7"),
                |_, ()| Ok(()),
            )
        });
        assert!(panicked.is_err());
    }
}
