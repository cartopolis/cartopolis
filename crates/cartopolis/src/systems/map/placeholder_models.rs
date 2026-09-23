//! What a lamppost looks like on this server: the operator's model standing in
//! for the shape built into this client.
//!
//! # Why these objects need a mechanism of their own
//!
//! A prim wears a model because somebody selected it and said so. A lamppost
//! cannot: nobody placed it. It is drawn from a register — the BGT's columns, the
//! city's own, Shortbread's `pois` — through `cartopolis_geo::furniture`, which
//! builds six boxes per object and merges a whole cell of them into one mesh.
//! That is what makes a city of 651 lampposts per page affordable, and it is also
//! why there is nothing to click on to improve one.
//!
//! So the statement is made once, about the world, by an operator:
//! `PUT /api/admin/library/placeholders/lamp` with a model out of the shared
//! library. Every client reads the table at sign-in and draws that model at every
//! lamppost instead of the boxes.
//!
//! # Three properties, each chosen against the obvious alternative
//!
//! **It is world-wide, not per person.** A per-user override would be a
//! preference, and two people standing on the same street disagreeing about
//! whether there is a bench there is not a preference — it is two different
//! worlds. It also could not be made to work offline-first without the client
//! holding models the server has never heard of.
//!
//! **The procedural form is removed on the worker, not hidden here.** The mask
//! rides in `map_geometry::DetailWants`, so an overridden form is never meshed at
//! all — hiding it afterwards would pay the extraction, the merge and the upload
//! for geometry nobody sees, which is exactly the mistake `DetailWants` was
//! written to stop. Changing the table therefore retears the live cells, which is
//! free here because `DetailWants` already does that when a layer is toggled.
//!
//! **Spawning is bounded per frame.** A dense cell is hundreds of instances and a
//! teleport lands several cells at once; standing them all up in one frame is the
//! documented upload burst that loses the device (`docs/notes/upload-bursts.md`).
//! They queue and drain at [`MAX_SPAWNS_PER_FRAME`].
//!
//! # What is deliberately not here
//!
//! No per-instance variation. The forms carry a `luma` and a `paint` so a car
//! park is not one identical grey, and a model has its own materials that this
//! has no business tinting. A street of identical lampposts is what a street of
//! lampposts looks like.

use bevy::prelude::*;
use bevy::tasks::IoTaskPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::systems::furniture::{FurnitureKind, FurnitureMask};
use crate::systems::model_assets;

/// How many instances may be stood up in one frame.
///
/// Low on purpose. Each one is a glTF scene instance — entities, not bytes — and
/// the meshes behind them are uploaded once for the first, so the cost of the
/// rest is spawn work on the frame loop. Sixty-four a frame clears a dense cell
/// in a couple of seconds of walking and never in one hitch.
pub const MAX_SPAWNS_PER_FRAME: usize = 64;

/// One form's replacement.
#[derive(Debug, Clone)]
pub struct Placeholder {
    /// The asset hash, loaded through `sim://` like any other model.
    pub hash: String,
    pub scale: f32,
    /// Radians, added to the angle the extractor gave the object.
    pub yaw_offset: f32,
}

/// The table this server published, and the mask derived from it.
#[derive(Resource, Default)]
pub struct PlaceholderModels {
    by_kind: HashMap<FurnitureKind, Placeholder>,
    /// The same set as a bitmask, for `DetailWants`. Derived once on every change
    /// rather than rebuilt per frame by the comparison that reads it.
    mask: FurnitureMask,
    /// Cleared when a session comes up, so entering a world fetches once.
    fetched: bool,
    inbox: Arc<Mutex<Vec<Vec<(String, Placeholder)>>>>,
    /// Instances waiting to be stood up: `(cell, hash, transform)`.
    queue: Vec<(Cell, String, Transform)>,
}

/// The cell an instance belongs to, so it is retired with the cell.
pub type Cell = (i64, i64);

impl PlaceholderModels {
    /// Which forms this server draws with a model. Read by `map_geometry` on
    /// every pass, so it is a `Copy` bitmask rather than a set to walk.
    #[must_use]
    pub fn mask(&self) -> FurnitureMask {
        self.mask
    }

    #[must_use]
    pub fn get(&self, kind: FurnitureKind) -> Option<&Placeholder> {
        self.by_kind.get(&kind)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_kind.is_empty()
    }

    /// Queue one object to be stood up in `cell`.
    ///
    /// The translation is **absolute anchored world XZ**, terrain lift included —
    /// the frame the extractor's positions become once the cell's origin is added
    /// — and is turned into a `big_space` cell plus a local offset when the object
    /// is actually spawned. Queuing the local form instead would mean deciding the
    /// grid cell now and spawning frames later, across a re-anchor that moves what
    /// "local" means.
    pub fn enqueue(&mut self, cell: Cell, hash: &str, world: Transform) {
        self.queue.push((cell, hash.to_string(), world));
    }

    /// Forget everything queued, full stop — a re-anchor, or a retear because the
    /// table itself changed. The cells those objects belonged to are gone.
    pub fn clear_queue(&mut self) {
        self.queue.clear();
    }

    /// Forget everything queued for a cell that has been retired.
    ///
    /// Without this a cell that arrives and leaves while the queue is long stands
    /// its objects up afterwards, parented to an entity that is gone — which is
    /// the `try_insert` case in `entity-commands-outlive-entities.md`, except the
    /// parent is missing rather than the child.
    pub fn forget_cell(&mut self, cell: Cell) {
        self.queue.retain(|(c, _, _)| *c != cell);
    }

    /// Replace the table, and say whether the mask changed — which is what
    /// decides a retear.
    fn adopt(&mut self, rows: Vec<(String, Placeholder)>) -> bool {
        let before = self.mask;
        self.by_kind.clear();
        self.mask = FurnitureMask::NONE;
        for (slug, placeholder) in rows {
            // A slug this build does not know is dropped rather than guessed at:
            // a newer server may have a form this client has never drawn.
            let Some(kind) = FurnitureKind::from_slug(&slug) else {
                debug!("this server replaces {slug}, which this client does not draw");
                continue;
            };
            self.mask = self.mask.with(kind);
            self.by_kind.insert(kind, placeholder);
        }
        self.mask != before
    }
}

/// Fetch the table once per session, and adopt whatever comes back.
///
/// Runs whenever there is an endpoint and nothing has been fetched against it —
/// `model_assets::have_endpoint` goes false on teardown, which is what makes
/// signing out and into another server re-ask. A server with no table answers an
/// empty list, which is the same as no placeholders and costs one request.
pub fn poll_placeholders(mut models: ResMut<PlaceholderModels>) {
    let replies: Vec<Vec<(String, Placeholder)>> = match models.inbox.lock() {
        Ok(mut queue) => queue.drain(..).collect(),
        Err(_) => Vec::new(),
    };
    for rows in replies {
        if models.adopt(rows) {
            // Nothing to do here: `map_geometry` compares the mask into
            // `DetailWants` and retears on the difference, which is the one place
            // that knows how to rebuild a cell.
            info!(
                "this server draws {} street forms with models",
                models.by_kind.len()
            );
        }
    }

    if !model_assets::have_endpoint() {
        // Signed out. Drop the table with the session it belonged to — the hashes
        // are that server's, and the next one's will not be the same.
        if !models.by_kind.is_empty() || models.fetched {
            models.adopt(Vec::new());
            models.queue.clear();
            models.fetched = false;
        }
        return;
    }
    if models.fetched {
        return;
    }
    models.fetched = true;
    let inbox = models.inbox.clone();
    IoTaskPool::get()
        .spawn(async move {
            match model_assets::api_get("/api/library/placeholders").await {
                Ok(body) => {
                    if let Ok(mut queue) = inbox.lock() {
                        queue.push(parse_placeholders(&body));
                    }
                }
                // Not a warning: a simulator older than this client answers 404,
                // and that means "no placeholders", not a fault.
                Err(e) => debug!("no placeholder table from this server: {e}"),
            }
        })
        .detach();
}

/// `[{kind, asset, scale, yaw_offset}]` → the rows this module holds.
///
/// The angle arrives in degrees, which is what a person types into the atlas console, and
/// is kept in radians here, which is what a `Transform` wants. Converting at the
/// boundary is the only place both units are visible at once.
fn parse_placeholders(body: &str) -> Vec<(String, Placeholder)> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(rows) = json.as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let kind = row["kind"].as_str()?.to_string();
            let hash = row["asset"].as_str()?.to_string();
            if hash.is_empty() {
                return None;
            }
            let scale = row["scale"].as_f64().unwrap_or(1.0) as f32;
            let yaw = row["yaw_offset"].as_f64().unwrap_or(0.0) as f32;
            Some((
                kind,
                Placeholder {
                    hash,
                    scale: if scale.is_finite() && scale > 0.0 {
                        scale
                    } else {
                        1.0
                    },
                    yaw_offset: yaw.to_radians(),
                },
            ))
        })
        .collect()
}

/// Stand queued objects up, a bounded number per frame.
///
/// Each one hangs off the cell's own surface list, so retiring the cell retires
/// them — the same contract every other thing a cell holds has.
pub fn spawn_placeholder_models(
    mut commands: Commands,
    mut models: ResMut<PlaceholderModels>,
    mut surfaces: ResMut<crate::systems::map_geometry::MapSurfaces>,
    asset_server: Res<AssetServer>,
    lod: Res<crate::systems::street_furniture::FurnitureLod>,
    bigspace: Query<Entity, With<big_space::prelude::BigSpace>>,
) {
    if models.queue.is_empty() {
        return;
    }
    let Ok(root) = bigspace.single() else {
        return;
    };
    let take = models.queue.len().min(MAX_SPAWNS_PER_FRAME);
    let batch: Vec<(Cell, String, Transform)> = models.queue.drain(..take).collect();
    for (cell, hash, world) in batch {
        // The cell may have been retired between the enqueue and now — walking out
        // of a cell with a full queue behind it does exactly that.
        if !surfaces.holds(cell) {
            continue;
        }
        let (grid_cell, local) = big_space::prelude::Grid::new(
            crate::grid::WORLD_CELL_EDGE,
            crate::grid::WORLD_SWITCH_THRESHOLD,
        )
        .translation_to_grid(world.translation.as_dvec3());
        let scene = asset_server
            .load(bevy::gltf::GltfAssetLabel::Scene(0).from_asset(model_assets::asset_path(&hash)));
        // `WorldAssetRoot`, not `SceneRoot`: this tree loads glTF through
        // `bevy_world_serialization` everywhere else it loads one
        // (`model_assets::spawn_model`, `avatar::spawn_holder`), which is what the
        // `reflect_auto_register` feature is enabled for.
        let id = commands
            .spawn((
                bevy::world_serialization::WorldAssetRoot(scene),
                Transform {
                    translation: local,
                    rotation: world.rotation,
                    scale: world.scale,
                },
                grid_cell,
                ChildOf(root),
                // Fades with the band its procedural form would have faded with,
                // so an overridden street LODs like the one beside it.
                crate::components::FurnitureSurface,
                lod.visibility(),
                crate::components::HideInSpace,
            ))
            .id();
        surfaces.attach(cell, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_servers_table() {
        let rows = parse_placeholders(
            r#"[{"kind":"lamp","asset":"aa","scale":2.0,"yaw_offset":90.0,"set_by":"jeroen"}]"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "lamp");
        assert_eq!(rows[0].1.hash, "aa");
        assert!((rows[0].1.scale - 2.0).abs() < 1e-6);
        // Degrees on the wire, radians in the transform.
        assert!((rows[0].1.yaw_offset - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
    }

    /// A nonsense scale must not put a kilometre-high bench in the street: the
    /// server bounds it, and a client reading a value it cannot use falls back to
    /// the honest 1.0 rather than trusting it.
    #[test]
    fn an_unusable_scale_falls_back() {
        let rows = parse_placeholders(r#"[{"kind":"bin","asset":"bb","scale":0.0}]"#);
        assert!((rows[0].1.scale - 1.0).abs() < 1e-6);
    }

    /// A row with no model is not a row.
    #[test]
    fn drops_a_row_with_no_asset() {
        assert!(parse_placeholders(r#"[{"kind":"bin"}]"#).is_empty());
        assert!(parse_placeholders("not json").is_empty());
    }

    /// A form this build has never heard of is dropped, and the mask says so —
    /// the alternative is a mask bit with nothing behind it, which retears every
    /// cell in the world to remove geometry and then draws nothing in its place.
    #[test]
    fn an_unknown_form_is_dropped() {
        let mut models = PlaceholderModels::default();
        let changed = models.adopt(vec![(
            "lamppost".into(),
            Placeholder {
                hash: "aa".into(),
                scale: 1.0,
                yaw_offset: 0.0,
            },
        )]);
        assert!(!changed);
        assert!(models.is_empty());
        assert!(models.mask().is_empty());
    }

    #[test]
    fn adopting_a_form_moves_the_mask() {
        let mut models = PlaceholderModels::default();
        let changed = models.adopt(vec![(
            "lamp".into(),
            Placeholder {
                hash: "aa".into(),
                scale: 1.0,
                yaw_offset: 0.0,
            },
        )]);
        assert!(changed);
        assert!(models.mask().contains(FurnitureKind::Lamp));
        assert!(!models.mask().contains(FurnitureKind::Bench));
        assert!(models.get(FurnitureKind::Lamp).is_some());
        // Adopting the same table again is not a change, or every fetch would
        // retear every cell.
        let again = models.adopt(vec![(
            "lamp".into(),
            Placeholder {
                hash: "aa".into(),
                scale: 1.0,
                yaw_offset: 0.0,
            },
        )]);
        assert!(!again);
    }

    /// A queued object whose cell has gone must not be stood up later.
    #[test]
    fn forgetting_a_cell_drops_its_queue() {
        let mut models = PlaceholderModels::default();
        models.enqueue((1, 2), "aa", Transform::IDENTITY);
        models.enqueue((3, 4), "aa", Transform::IDENTITY);
        models.forget_cell((1, 2));
        assert_eq!(models.queue.len(), 1);
        assert_eq!(models.queue[0].0, (3, 4));
    }
}
