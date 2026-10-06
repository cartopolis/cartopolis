//! Building overrides: a building's outside replaced by a model
//! (`docs/notes/building-models.md`).
//!
//! An override is set per building — keyed by the BAG identificatie the shell
//! carries — and deleting it restores the building the survey drew, because
//! nothing about that building was ever changed: the client draws the 3DBAG
//! shell unless the `lod3` cell drawing it lists the building as replaced.
//!
//! # Where an override lives
//!
//! A folder per building in atlas's data directory, the source of truth:
//!
//! ```text
//! overrides/<pand>/override.json   what the operator set (`Override`)
//! overrides/<pand>/model.glb       the uploaded model, when the source is one
//! overrides/<pand>/compiled.json   the elements it compiles to, cached
//! overrides/<pand>/history/<rev>/  every earlier override.json (and model.glb)
//! ```
//!
//! and in the served tree, what clients stream: `lod3/14/{x}/{y}.json` for every
//! cell holding an override, with a stamped `lod3/coverage.json` (`crate::stamped`)
//! that also lists the buildings that get no interior. A save or a delete
//! rewrites only the cells it touches, and the client polls the manifest, so an
//! edit is on screen in about thirty seconds.
//!
//! # What a model may be
//!
//! A single self-contained **GLB**: the bytes, triangle and node ceilings are
//! the simulator's model store's (`cartopolis_core::model_limits`). Its axes are
//! glTF's own — +Y up, −Z the front — and the front faces north at heading 0.
//! Textures are not carried: the served format is triangles with one colour per
//! part, which is each primitive's base colour.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cartopolis_geo::formats::lod22::{Element, File};
use serde::{Deserialize, Serialize};

use crate::pipeline::models::{self, Landmark};
use crate::pipeline::tile::{cell_path, Cell, CACHE_ZOOM};
use crate::stamped::{self, now_ms, write_atomic};

/// The credit line for what is served: the generated models' sources, and the
/// people who uploaded the rest — a model belongs to whoever made it.
fn attribution(all: &[Override]) -> String {
    let mut lines = Vec::new();
    if all
        .iter()
        .any(|o| matches!(o.source, Source::Stadium { .. }))
    {
        lines.push(models::ATTRIBUTION.to_string());
    }
    let mut authors: Vec<&str> = all
        .iter()
        .filter(|o| matches!(o.source, Source::Model { .. }))
        .map(|o| o.author.as_str())
        .filter(|a| !a.is_empty())
        .collect();
    authors.sort_unstable();
    authors.dedup();
    if !authors.is_empty() {
        lines.push(format!("Building models by {}", authors.join(", ")));
    }
    lines.join("; ")
}

/// Where a model comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// A stadium generated over a pitch (`pipeline::models`).
    Stadium {
        /// The pitch's corners, `(lat, lng)`, in order round it.
        pitch: [(f64, f64); 4],
    },
    /// An uploaded GLB, kept beside the record as `model.glb`.
    Model {
        /// SHA-256 of the file, hex — which upload this record was saved with.
        sha256: String,
        triangles: u64,
    },
}

/// Which parts of a model stop an avatar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collision {
    /// Every part is solid (its convex hull). Right for a model made of
    /// separate blocks; a single-mesh building becomes one sealed hull.
    #[default]
    Parts,
    /// Only parts named like a wall or a column (`wall`, `column`, `pillar`)
    /// are solid, so a model with an inside keeps its doorways open.
    Walls,
    /// Nothing is solid.
    None,
}

/// What happens inside.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interior {
    /// No generated rooms: the model is the whole building (a stadium).
    #[default]
    None,
    /// Keep the building's design (generated or drawn) inside the model.
    Design,
}

/// Where a model goes. Every field has a default, and the defaults put the
/// model's footprint centre on the register outline's centre, standing on the
/// ground, facing north, at its own scale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    /// Where the model's footprint centre goes; `None` is the outline's centre.
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    /// Degrees clockwise from north that the model's front faces.
    pub heading_deg: f64,
    pub scale: f64,
    /// Metres the model's lowest point stands above the ground.
    pub base_m: f64,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            lat: None,
            lng: None,
            heading_deg: 0.0,
            scale: 1.0,
            base_m: 0.0,
        }
    }
}

/// One building's override, as stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Override {
    pub pand: String,
    pub name: String,
    pub source: Source,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub collision: Collision,
    #[serde(default)]
    pub interior: Interior,
    /// Who supplied the model (or created the override): the credit line.
    #[serde(default)]
    pub author: String,
    /// When it was saved, ms — also its history folder's name.
    #[serde(default)]
    pub revision: u64,
    /// The cell it was last published in, so a delete knows what to rewrite.
    #[serde(default)]
    pub cell: Option<Cell>,
}

/// What a compile came to, for the log and the web UI.
#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub elements: usize,
    pub triangles: usize,
    pub cell: Cell,
    /// Found in a published LoD2.2 cell rather than filed by its outline.
    pub cell_found: bool,
}

/// What an edit from the web UI may change. Absent fields keep what is stored.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Edit {
    pub name: Option<String>,
    pub placement: Option<Placement>,
    pub collision: Option<Collision>,
    pub interior: Option<Interior>,
    /// Switch to (or re-aim) a generated stadium.
    pub stadium_pitch: Option<[(f64, f64); 4]>,
}

pub struct Overrides {
    dir: PathBuf,
    site: PathBuf,
    /// One change at a time: a save rewrites a shared cell file and the shared
    /// manifest, and two interleaved would lose one of them.
    lock: Mutex<()>,
}

impl Overrides {
    #[must_use]
    pub fn new(data_dir: &Path, site_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("overrides"),
            site: site_dir.to_path_buf(),
            lock: Mutex::new(()),
        }
    }

    fn folder(&self, pand: &str) -> PathBuf {
        self.dir.join(pand)
    }

    /// The stored override for a building.
    #[must_use]
    pub fn get(&self, pand: &str) -> Option<Override> {
        if !crate::designs::valid_pand(pand) {
            return None;
        }
        let text = std::fs::read(self.folder(pand).join("override.json")).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Every stored override, by building number.
    #[must_use]
    pub fn list(&self) -> Vec<Override> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut all: Vec<Override> = dir
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter_map(|pand| self.get(&pand))
            .collect();
        all.sort_by(|a, b| a.pand.cmp(&b.pand));
        all
    }

    /// The cells overrides are published in.
    #[must_use]
    pub fn cells(&self) -> BTreeSet<Cell> {
        self.list().into_iter().filter_map(|o| o.cell).collect()
    }

    /// Create the seed record the first time this store is used — and never
    /// again, so a deleted seed stays deleted. `Ok(true)` when it seeded.
    pub fn seed_once(&self, landmark: &Landmark) -> Result<bool, String> {
        let marker = self.dir.join(".seeded");
        if marker.exists() {
            return Ok(false);
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        if self.get(landmark.pand).is_none() {
            let record = Override {
                pand: landmark.pand.into(),
                name: landmark.name.into(),
                source: Source::Stadium {
                    pitch: landmark.pitch,
                },
                placement: Placement::default(),
                collision: Collision::Parts,
                interior: Interior::None,
                author: "atlas".into(),
                revision: 0,
                cell: None,
            };
            self.store(&record, None)?;
        }
        std::fs::write(&marker, b"").map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Write a record (and its model) and keep the old one in history.
    fn store(&self, record: &Override, model: Option<&[u8]>) -> Result<(), String> {
        let folder = self.folder(&record.pand);
        if let Some(previous) = self.get(&record.pand) {
            let hist = folder.join("history").join(previous.revision.to_string());
            let _ = std::fs::create_dir_all(&hist);
            let _ = std::fs::copy(folder.join("override.json"), hist.join("override.json"));
            if model.is_some() {
                let _ = std::fs::copy(folder.join("model.glb"), hist.join("model.glb"));
            }
        }
        if let Some(bytes) = model {
            write_atomic(&folder.join("model.glb"), bytes).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_vec_pretty(record).map_err(|e| e.to_string())?;
        write_atomic(&folder.join("override.json"), &text).map_err(|e| e.to_string())
    }

    /// Apply an edit (and, with `model`, a new upload), compile, and publish.
    ///
    /// A building with no override yet needs a source: a model, or a stadium
    /// pitch. The compile runs before anything is published, so a model that
    /// will not build leaves the building as it was.
    pub fn save(
        &self,
        pand: &str,
        edit: Edit,
        model: Option<Vec<u8>>,
        author: &str,
    ) -> Result<(Override, Summary), String> {
        if !crate::designs::valid_pand(pand) {
            return Err("that is not a BAG building number".into());
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let previous = self.get(pand);
        let source = match (&model, edit.stadium_pitch, &previous) {
            (Some(bytes), _, _) => {
                let stats = gltf_model::inspect(bytes)?;
                Source::Model {
                    sha256: stats.sha256,
                    triangles: stats.triangles,
                }
            }
            (None, Some(pitch), _) => Source::Stadium { pitch },
            (None, None, Some(p)) => p.source.clone(),
            (None, None, None) => {
                return Err("a new override needs a model to upload, or a pitch".into())
            }
        };
        let mut record = previous.clone().unwrap_or(Override {
            pand: pand.into(),
            name: format!("Building {pand}"),
            source: source.clone(),
            placement: Placement::default(),
            collision: Collision::default(),
            interior: Interior::default(),
            author: String::new(),
            revision: 0,
            cell: None,
        });
        record.source = source;
        if let Some(n) = edit.name {
            let n = n.trim();
            if n.is_empty() || n.chars().count() > 80 {
                return Err("a name is 1–80 characters".into());
            }
            record.name = n.into();
        }
        if let Some(p) = edit.placement {
            check_placement(&p)?;
            record.placement = p;
        }
        if let Some(c) = edit.collision {
            record.collision = c;
        }
        if let Some(i) = edit.interior {
            record.interior = i;
        }
        // The author is whoever supplied the model (it is the credit line), so a
        // rename or a turn by somebody else does not take it over.
        if model.is_some() || previous.is_none() {
            record.author = author.into();
        }
        record.revision = now_ms().max(previous.as_ref().map_or(0, |p| p.revision + 1));

        // Compile against the new model before anything is written.
        let model_bytes = match (&model, &record.source) {
            (Some(b), _) => Some(b.clone()),
            (None, Source::Model { .. }) => Some(
                std::fs::read(self.folder(pand).join("model.glb"))
                    .map_err(|_| "this override's model file is missing — upload it again")?,
            ),
            _ => None,
        };
        let (elements, summary) = self.compile(&record, model_bytes.as_deref())?;
        record.cell = Some(summary.cell);
        self.store(&record, model.as_deref())?;
        let compiled = serde_json::to_vec(&elements).map_err(|e| e.to_string())?;
        write_atomic(&self.folder(pand).join("compiled.json"), &compiled)
            .map_err(|e| e.to_string())?;
        let mut cells = vec![summary.cell];
        if let Some(old) = previous.and_then(|p| p.cell) {
            cells.push(old);
        }
        cells.dedup();
        for cell in cells {
            self.publish_cell(cell)?;
        }
        Ok((record, summary))
    }

    /// Recompile a stored override as it stands (the dataset's republish).
    ///
    /// Not a save: nobody edited it, so it keeps its author and revision and
    /// writes no history. Only the cell is written back, if it moved (a lod22
    /// run can change which cell draws the shell).
    pub fn recompile(&self, pand: &str) -> Result<Summary, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut record = self
            .get(pand)
            .ok_or_else(|| format!("{pand} has no override"))?;
        let model = match record.source {
            Source::Model { .. } => Some(
                std::fs::read(self.folder(pand).join("model.glb"))
                    .map_err(|_| "this override's model file is missing — upload it again")?,
            ),
            Source::Stadium { .. } => None,
        };
        let (elements, summary) = self.compile(&record, model.as_deref())?;
        let old = record.cell.replace(summary.cell);
        if old != record.cell {
            let text = serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?;
            write_atomic(&self.folder(pand).join("override.json"), &text)
                .map_err(|e| e.to_string())?;
        }
        let compiled = serde_json::to_vec(&elements).map_err(|e| e.to_string())?;
        write_atomic(&self.folder(pand).join("compiled.json"), &compiled)
            .map_err(|e| e.to_string())?;
        for cell in std::iter::once(summary.cell).chain(old.filter(|c| *c != summary.cell)) {
            self.publish_cell(cell)?;
        }
        Ok(summary)
    }

    /// Delete a building's override: the building goes back to what the survey
    /// drew. The folder's history is kept, under `overrides/.deleted/`.
    pub fn remove(&self, pand: &str) -> Result<bool, String> {
        if !crate::designs::valid_pand(pand) {
            return Ok(false);
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let Some(previous) = self.get(pand) else {
            return Ok(false);
        };
        let gone = self
            .dir
            .join(".deleted")
            .join(format!("{pand}.{}", now_ms()));
        std::fs::create_dir_all(gone.parent().unwrap_or(&self.dir)).map_err(|e| e.to_string())?;
        std::fs::rename(self.folder(pand), &gone).map_err(|e| e.to_string())?;
        if let Some(cell) = previous.cell {
            self.publish_cell(cell)?;
        } else {
            self.write_manifest_only()?;
        }
        Ok(true)
    }

    /// Compile a record to elements and find the cell they belong in.
    fn compile(
        &self,
        record: &Override,
        model: Option<&[u8]>,
    ) -> Result<(Vec<Element>, Summary), String> {
        let outline = models::bag_outline(&record.pand)?;
        let elements = match &record.source {
            Source::Stadium { pitch } => models::stadium(&record.pand, *pitch, &outline)?,
            Source::Model { .. } => gltf_model::elements(
                model.ok_or("the model file is missing")?,
                &record.pand,
                &record.placement,
                record.collision,
                &outline,
            )?,
        };
        if elements.is_empty() {
            return Err("the model has no triangles to draw".into());
        }
        let (cell, cell_found) = models::owner_cell(&self.site, &record.pand, &outline);
        let summary = Summary {
            elements: elements.len(),
            triangles: elements.iter().map(|e| e.t.len()).sum(),
            cell,
            cell_found,
        };
        Ok((elements, summary))
    }

    /// The manifest fields besides the cells: who to credit, and which
    /// buildings get no generated interior.
    fn manifest_extra(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut extra = serde_json::Map::new();
        extra.insert("lod".into(), "3".into());
        let all = self.list();
        let credit = attribution(&all);
        if !credit.is_empty() {
            extra.insert("attribution".into(), credit.into());
        }
        let no_interior: Vec<&str> = all
            .iter()
            .filter(|o| o.interior == Interior::None)
            .map(|o| o.pand.as_str())
            .collect();
        let pands: Vec<&str> = all.iter().map(|o| o.pand.as_str()).collect();
        extra.insert("no_interior".into(), no_interior.into());
        extra.insert("pands".into(), pands.into());
        extra
    }

    /// Rewrite one served cell from every override filed in it.
    fn publish_cell(&self, cell: Cell) -> Result<(), String> {
        let here: Vec<Override> = self
            .list()
            .into_iter()
            .filter(|o| o.cell == Some(cell))
            .collect();
        let mut file = File::default();
        for o in &here {
            let compiled = std::fs::read(self.folder(&o.pand).join("compiled.json"))
                .map_err(|e| format!("{}: compiled model missing: {e}", o.pand))?;
            let elements: Vec<Element> =
                serde_json::from_slice(&compiled).map_err(|e| e.to_string())?;
            file.elements.extend(elements);
            file.pands.push(o.pand.clone());
        }
        let body = if here.is_empty() {
            None
        } else {
            Some(cartopolis_geo::formats::to_vec(&file).map_err(|e| e.to_string())?)
        };
        let newest = here.iter().map(|o| o.revision).max().unwrap_or(0);
        stamped::publish_cell(
            &self.site.join("lod3"),
            CACHE_ZOOM,
            cell,
            body.as_deref(),
            newest,
            self.manifest_extra(),
        )
        .map_err(|e| format!("could not publish {}: {e}", cell_path("lod3", cell)))
    }

    fn write_manifest_only(&self) -> Result<(), String> {
        let path = self.site.join("lod3").join("coverage.json");
        let cells = stamped::read_cells(&path);
        stamped::write_manifest(&path, CACHE_ZOOM, &cells, self.manifest_extra())
            .map_err(|e| e.to_string())
    }
}

fn check_placement(p: &Placement) -> Result<(), String> {
    if !(0.01..=100.0).contains(&p.scale) {
        return Err("scale is between 0.01 and 100".into());
    }
    if !p.heading_deg.is_finite() || !(-1000.0..=1000.0).contains(&p.base_m) {
        return Err("heading and base must be ordinary numbers".into());
    }
    match (p.lat, p.lng) {
        (None, None) => Ok(()),
        (Some(la), Some(lo)) if la.abs() <= 90.0 && lo.abs() <= 180.0 => Ok(()),
        _ => Err("give both a latitude and a longitude, or neither".into()),
    }
}

/// Reading an uploaded GLB and turning it into placed elements.
pub mod gltf_model {
    use cartopolis_core::model_limits::{MAX_ASSET_BYTES, MAX_NODES, MAX_TRIANGLES};
    use cartopolis_geo::formats::lod22::{Element, Id, Tri};
    use glam::{DMat4, DVec3, DVec4};

    use super::{Collision, Placement};

    /// What an upload measured.
    #[derive(Debug)]
    pub struct Stats {
        pub sha256: String,
        pub triangles: u64,
    }

    fn parse(bytes: &[u8]) -> Result<gltf::Gltf, String> {
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(format!(
                "the model is {} MB; the most a model may be is {} MB",
                bytes.len() / (1024 * 1024),
                MAX_ASSET_BYTES / (1024 * 1024)
            ));
        }
        if !bytes.starts_with(b"glTF") {
            return Err(
                "upload a .glb file (binary glTF); export one from Blender with \
                        File ▸ Export ▸ glTF 2.0, format glTF Binary"
                    .into(),
            );
        }
        let g = gltf::Gltf::from_slice(bytes).map_err(|e| format!("not a readable GLB: {e}"))?;
        if g.blob.is_none() {
            return Err("the GLB carries no binary buffer".into());
        }
        if g.nodes().count() > MAX_NODES {
            return Err(format!("the model has more than {MAX_NODES} nodes"));
        }
        for b in g.buffers() {
            if !matches!(b.source(), gltf::buffer::Source::Bin) {
                return Err("the model points at files outside itself; export a single GLB".into());
            }
        }
        Ok(g)
    }

    /// Check an upload against the model store's ceilings.
    pub fn inspect(bytes: &[u8]) -> Result<Stats, String> {
        let g = parse(bytes)?;
        let mut triangles = 0u64;
        for mesh in g.meshes() {
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let n = prim
                    .indices()
                    .map(|a| a.count())
                    .or_else(|| prim.get(&gltf::Semantic::Positions).map(|a| a.count()))
                    .unwrap_or(0);
                triangles += (n / 3) as u64;
            }
        }
        if triangles == 0 {
            return Err("the model has no triangles".into());
        }
        if triangles > MAX_TRIANGLES {
            return Err(format!(
                "the model has {triangles} triangles; the most a model may have is {MAX_TRIANGLES}"
            ));
        }
        Ok(Stats {
            sha256: crate::overrides::sha256_hex(bytes),
            triangles,
        })
    }

    /// One mesh primitive in model space (metres, glTF axes), with the node
    /// name it came from and its base colour.
    struct Part {
        name: String,
        rgb: [u8; 3],
        p: Vec<DVec3>,
        t: Vec<[u32; 3]>,
    }

    fn linear_to_srgb(c: f32) -> u8 {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    }

    fn parts(g: &gltf::Gltf) -> Vec<Part> {
        let blob = g.blob.as_deref().unwrap_or_default();
        let mut out = Vec::new();
        let mut stack: Vec<(gltf::Node, DMat4)> = g
            .default_scene()
            .or_else(|| g.scenes().next())
            .map(|s| s.nodes().map(|n| (n, DMat4::IDENTITY)).collect())
            .unwrap_or_default();
        while let Some((node, parent)) = stack.pop() {
            let local =
                DMat4::from_cols_array_2d(&node.transform().matrix().map(|c| c.map(f64::from)));
            let world = parent * local;
            for child in node.children() {
                stack.push((child, world));
            }
            let Some(mesh) = node.mesh() else { continue };
            let name = node
                .name()
                .or(mesh.name())
                .unwrap_or_default()
                .to_ascii_lowercase();
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let reader = prim
                    .reader(|b| matches!(b.source(), gltf::buffer::Source::Bin).then_some(blob));
                let Some(positions) = reader.read_positions() else {
                    continue;
                };
                let p: Vec<DVec3> = positions
                    .map(|q| {
                        let v = world
                            * DVec4::new(f64::from(q[0]), f64::from(q[1]), f64::from(q[2]), 1.0);
                        v.truncate()
                    })
                    .collect();
                let idx: Vec<u32> = match reader.read_indices() {
                    Some(i) => i.into_u32().collect(),
                    None => (0..p.len() as u32).collect(),
                };
                let t: Vec<[u32; 3]> = idx
                    .chunks_exact(3)
                    .filter(|c| c.iter().all(|&i| (i as usize) < p.len()))
                    .map(|c| [c[0], c[1], c[2]])
                    .collect();
                let base = prim.material().pbr_metallic_roughness().base_color_factor();
                let rgb = [
                    linear_to_srgb(base[0]),
                    linear_to_srgb(base[1]),
                    linear_to_srgb(base[2]),
                ];
                if !t.is_empty() {
                    out.push(Part {
                        name: name.clone(),
                        rgb,
                        p,
                        t,
                    });
                }
            }
        }
        out
    }

    fn is_wall_name(name: &str) -> bool {
        ["wall", "column", "pillar", "muur", "kolom"]
            .iter()
            .any(|w| name.contains(w))
    }

    /// Place a GLB on a building: scale, turn to `heading_deg`, stand its lowest
    /// point `base_m` above the ground and its footprint centre on the outline's
    /// centre (or on `lat`/`lng`).
    pub fn elements(
        bytes: &[u8],
        pand: &str,
        placement: &Placement,
        collision: Collision,
        outline: &[(f64, f64)],
    ) -> Result<Vec<Element>, String> {
        let g = parse(bytes)?;
        let parts = parts(&g);
        if parts.is_empty() {
            return Err("the model has no triangles".into());
        }
        // Model space → east/north/up metres about the model's footprint centre.
        // glTF's front is −Z; at heading 0 it faces north, so −Z is north and +X
        // east. Heading turns it clockwise seen from above.
        let h = placement.heading_deg.to_radians();
        let (s, c) = h.sin_cos();
        let to_enu = |v: DVec3| {
            let (e, n, u) = (
                v.x * placement.scale,
                -v.z * placement.scale,
                v.y * placement.scale,
            );
            DVec3::new(e * c + n * s, -e * s + n * c, u)
        };
        let all: Vec<DVec3> = parts
            .iter()
            .flat_map(|p| p.p.iter().map(|&v| to_enu(v)))
            .collect();
        let min = all
            .iter()
            .fold(DVec3::splat(f64::INFINITY), |m, v| m.min(*v));
        let max = all
            .iter()
            .fold(DVec3::splat(f64::NEG_INFINITY), |m, v| m.max(*v));
        let centre = DVec3::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0, min.z);
        let (lat0, lng0) = match (placement.lat, placement.lng) {
            (Some(la), Some(lo)) => (la, lo),
            _ => {
                let n = outline.len() as f64;
                (
                    outline.iter().map(|p| p.0).sum::<f64>() / n,
                    outline.iter().map(|p| p.1).sum::<f64>() / n,
                )
            }
        };
        let (m_lat, m_lng) = cartopolis_core::degree_scale_m(lat0);
        let round = |v: f64, dp: i32| {
            let m = 10f64.powi(dp);
            (v * m).round() / m
        };
        let mut out = Vec::with_capacity(parts.len());
        for (i, part) in parts.iter().enumerate() {
            let p: Vec<(f64, f64, f32)> = part
                .p
                .iter()
                .map(|&v| {
                    let q = to_enu(v) - centre;
                    (
                        round(lat0 + q.y / m_lat, 7),
                        round(lng0 + q.x / m_lng, 7),
                        round(q.z + placement.base_m, 2) as f32,
                    )
                })
                .collect();
            let world: Vec<DVec3> = part.p.iter().map(|&v| to_enu(v)).collect();
            let t = part
                .t
                .iter()
                .map(|&[a, b, c]| {
                    let n = (world[b as usize] - world[a as usize])
                        .cross(world[c as usize] - world[a as usize])
                        .normalize_or_zero();
                    Tri(a, b, c, u8::from(n.z.abs() > 0.7))
                })
                .collect();
            let solid = match collision {
                Collision::Parts => true,
                Collision::Walls => is_wall_name(&part.name),
                Collision::None => false,
            };
            out.push(Element {
                class: if solid {
                    "IfcWall"
                } else {
                    "IfcBuildingElementProxy"
                }
                .into(),
                id: Id(pand.into()),
                p,
                part: i as u32,
                rgb: Some(part.rgb),
                t,
            });
        }
        Ok(out)
    }
}

/// SHA-256 of some bytes, lowercase hex — which upload a record was saved with.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-triangle GLB, built by hand: 2 m wide and 3 m deep in glTF space.
    pub(crate) fn tiny_glb() -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 4.0, -3.0];
        let bin: Vec<u8> = positions.iter().flat_map(|f| f.to_le_bytes()).collect();
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0,"name":"Wall north"}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1,0,0,1]}}}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
                "min":[0,0,-3],"max":[2,4,0]}}],
            "bufferViews":[{{"buffer":0,"byteLength":{n}}}],
            "buffers":[{{"byteLength":{n}}}]}}"#,
            n = bin.len()
        );
        let mut json = json.into_bytes();
        while json.len() % 4 != 0 {
            json.push(b' ');
        }
        let total = 12 + 8 + json.len() + 8 + bin.len();
        let mut out = Vec::new();
        out.extend(b"glTF");
        out.extend(2u32.to_le_bytes());
        out.extend((total as u32).to_le_bytes());
        out.extend((json.len() as u32).to_le_bytes());
        out.extend(b"JSON");
        out.extend(&json);
        out.extend((bin.len() as u32).to_le_bytes());
        out.extend(b"BIN\0");
        out.extend(&bin);
        out
    }

    fn square() -> Vec<(f64, f64)> {
        vec![
            (53.2194, 6.5665),
            (53.2195, 6.5665),
            (53.2195, 6.5667),
            (53.2194, 6.5667),
        ]
    }

    #[test]
    fn an_upload_is_measured_and_placed_on_the_outline() {
        let glb = tiny_glb();
        let stats = gltf_model::inspect(&glb).unwrap();
        assert_eq!(stats.triangles, 1);
        assert_eq!(stats.sha256.len(), 64);
        let els = gltf_model::elements(
            &glb,
            "0014100010000001",
            &Placement::default(),
            Collision::Walls,
            &square(),
        )
        .unwrap();
        assert_eq!(els.len(), 1);
        let e = &els[0];
        assert_eq!(e.rgb, Some([255, 0, 0]));
        // Named "Wall …", so solid under the walls rule.
        assert_eq!(e.class, "IfcWall");
        // Lowest point on the ground, highest 4 m up.
        let hs: Vec<f32> = e.p.iter().map(|p| p.2).collect();
        assert_eq!(hs.iter().copied().fold(f32::INFINITY, f32::min), 0.0);
        assert_eq!(hs.iter().copied().fold(0.0, f32::max), 4.0);
        // The vertex at −Z (glTF front) is north of the others.
        assert!(e.p[2].0 > e.p[0].0);
        // The footprint is centred on the outline's centre.
        let c_lat = (53.2194 + 53.2195) / 2.0;
        let mid_lat = (e.p.iter().map(|p| p.0).fold(f64::MIN, f64::max)
            + e.p.iter().map(|p| p.0).fold(f64::MAX, f64::min))
            / 2.0;
        assert!((mid_lat - c_lat).abs() < 1e-6, "{mid_lat} vs {c_lat}");
    }

    #[test]
    fn a_heading_turns_the_front() {
        let glb = tiny_glb();
        let east = Placement {
            heading_deg: 90.0,
            ..Placement::default()
        };
        let els = gltf_model::elements(&glb, "1234", &east, Collision::Parts, &square()).unwrap();
        // Facing east, the front vertex is the easternmost.
        let e = &els[0];
        assert!(e.p[2].1 > e.p[0].1, "{:?}", e.p);
        assert_eq!(e.class, "IfcWall");
    }

    #[test]
    fn what_is_not_a_glb_is_refused_with_a_reason() {
        assert!(gltf_model::inspect(b"{\"asset\":{}}")
            .unwrap_err()
            .contains(".glb"));
        let big = vec![0u8; cartopolis_core::model_limits::MAX_ASSET_BYTES + 1];
        assert!(gltf_model::inspect(&big).unwrap_err().contains("MB"));
    }

    #[test]
    fn a_seed_is_planted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Overrides::new(&dir.path().join("data"), &dir.path().join("site"));
        assert!(store.seed_once(&crate::pipeline::models::EUROBORG).unwrap());
        assert!(store.get("0014100010953806").is_some());
        // Deleted by an operator, and not resurrected by the next run.
        std::fs::remove_dir_all(dir.path().join("data/overrides/0014100010953806")).unwrap();
        assert!(!store.seed_once(&crate::pipeline::models::EUROBORG).unwrap());
        assert!(store.get("0014100010953806").is_none());
    }

    #[test]
    fn a_placement_out_of_bounds_is_refused() {
        assert!(check_placement(&Placement {
            scale: 0.0,
            ..Placement::default()
        })
        .is_err());
        assert!(check_placement(&Placement {
            lat: Some(53.0),
            ..Placement::default()
        })
        .is_err());
        assert!(check_placement(&Placement::default()).is_ok());
    }

    #[test]
    fn the_credit_names_who_made_what_is_served() {
        let record = |source, author: &str| Override {
            pand: "0014100010953806".into(),
            name: String::new(),
            source,
            placement: Placement::default(),
            collision: Collision::default(),
            interior: Interior::default(),
            author: author.into(),
            revision: 0,
            cell: None,
        };
        let model = || Source::Model {
            sha256: String::new(),
            triangles: 0,
        };
        let stadium = Source::Stadium {
            pitch: [(0.0, 0.0); 4],
        };
        assert_eq!(attribution(&[]), "");
        // An uploaded model is not "generated from BAG and AHN".
        assert_eq!(
            attribution(&[
                record(model(), "bob"),
                record(model(), "ann"),
                record(model(), "bob")
            ]),
            "Building models by ann, bob"
        );
        assert_eq!(
            attribution(&[record(stadium, "atlas")]),
            models::ATTRIBUTION
        );
    }
}
