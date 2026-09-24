//! The map data pipelines: extractors that turn the Dutch open registers into the
//! per-cell files the client streams (`<dataset>/14/{x}/{y}.json` plus a
//! `coverage.json` per dataset, served as `3dbag.cartopolis.org`).
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
//! console drive it: the log is a stream somebody watches, the progress is a bar,
//! and the cancel is a button checked inside every paging loop.

pub mod bag;
pub mod bgt;
pub mod buildings;
pub mod bv3d;
pub mod dikes;
pub mod geom;
pub mod http;
pub mod lod22;
pub mod rd;
pub mod terrain;
pub mod terrain_grade;
pub mod tile;
pub mod water;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

/// How far along a dataset is, for the console's progress bar.
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
    /// Fold an update in, field by field, as the console's `{...old, ...new}` did.
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
    /// Cells written, for the console's coverage view.
    pub cells: usize,
    pub counts: Vec<Count>,
}

#[derive(Debug, PartialEq)]
pub enum PipelineError {
    /// Stopped from the console. Reported as *cancelled*, not as a failure.
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
            cancel,
            sink,
        }
    }

    #[must_use]
    pub fn with_data_dir(mut self, data_dir: &Path) -> Self {
        self.data_dir = Some(data_dir.to_path_buf());
        self
    }

    /// Atlas's own store, or a failure naming the dataset that needed it.
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
        let body = serde_json::to_vec(value).map_err(|e| PipelineError::Failed(e.to_string()))?;
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

/// One extractor, as the console lists it.
pub struct Dataset {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Where its `coverage.json` lives under the output root.
    pub coverage: &'static str,
    /// Roughly how long a city takes, for the console to set expectations.
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
        typical_minutes: 2,
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
        typical_minutes: 4,
        run: lod22::run,
    },
    Dataset {
        id: "bag",
        label: "Building facts (BAG)",
        description: "Year built, use, floor areas, addresses, entrance points, houseboat \
                      berths, and the national monuments and protected districts joined onto them.",
        coverage: "bag/coverage.json",
        typical_minutes: 10,
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
        typical_minutes: 12,
        run: bgt::run,
    },
    Dataset {
        id: "terrain",
        label: "Terrain tiles (AHN)",
        description: "Cuts every z14 height tile over the area into atlas's terrain store and \
                      grades it — water carved, holes filled, roads flattened, bridge approaches \
                      ramped — so the client only samples it, and publishes the bridge deck \
                      heights it measured. Reads the water levels, so it runs after them.",
        coverage: "terrain/coverage.json",
        typical_minutes: 4,
        run: terrain::run,
    },
];

#[must_use]
pub fn dataset(id: &str) -> Option<&'static Dataset> {
    DATASETS.iter().find(|d| d.id == id)
}
