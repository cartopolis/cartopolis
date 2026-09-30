//! Registration for `systems::map`: every streamer, the terrain field, the
//! buildings and detail layers, the globe, the sky and the server's prims as
//! drawn geometry. The phase each system runs in is `crate::phase`'s; the edges
//! beside them are the ones `lib.rs` carried before the split.
//!
//! The material plugins these systems draw with stay in `lib.rs`, added before
//! this plugin: [`WaterSeaState`](systems::water::WaterSeaState) mints its handle
//! out of `Assets<WaterMaterial>` when it is initialised here.

use bevy::prelude::*;

use crate::phase::ClientPhase;
use crate::systems;
use crate::systems::stream_watch::WatchStreamExt;
use crate::systems::{model_assets, network, rendering, sky};

pub struct MapPlugin {
    /// Live met.no weather from the first frame: on for an interactive run, off
    /// for a scripted capture unless `--weather-live` asks for it.
    pub live_weather: bool,
}

impl Plugin for MapPlugin {
    fn build(&self, app: &mut App) {
        // Video on a prim (protocol 28): after the prims' own textures are put
        // back, so a screen keeps its picture through an edit.
        #[cfg(not(target_arch = "wasm32"))]
        app.init_resource::<systems::prim_video::PrimVideos>()
            .init_resource::<systems::prim_video::VideoPicker>()
            .add_systems(
                Update,
                systems::prim_video::drive_prim_videos
                    .after(systems::rendering::refresh_prim_textures)
                    .after(systems::network::apply_network_snapshot),
            )
            .add_systems(Update, systems::prim_video::drive_video_actions);
        app.init_resource::<systems::rendering::MeshCache>()
            .init_resource::<systems::rendering::MaterialCache>()
            // Every streamer is watched on the line after it is initialised:
            // that is the settle test's list (`systems::stream_watch`), and a
            // streamer missing from it lets a capture fire before it lands.
            .init_resource::<systems::osm_buildings::OsmBuildings>()
            .watch_stream::<systems::osm_buildings::OsmBuildings>()
            .init_resource::<systems::lod22::Lod22Buildings>()
            .watch_stream::<systems::lod22::Lod22Buildings>()
            .init_resource::<systems::building_facts::BuildingFacts>()
            .watch_stream::<systems::building_facts::BuildingFacts>()
            .init_resource::<systems::dikes::Dikes>()
            .watch_stream::<systems::dikes::Dikes>()
            .init_resource::<systems::interiors::Interiors>()
            .init_resource::<systems::interiors::InteriorFloors>()
            .init_resource::<systems::tunnels::Tunnels>()
            .init_resource::<systems::terrain::RoadMask>()
            .init_resource::<systems::building_select::BuildingSelection>()
            .init_resource::<systems::street_detail::StreetDetail>()
            .watch_stream::<systems::street_detail::StreetDetail>()
            .init_resource::<systems::tall_structures::TallStructures>()
            .watch_stream::<systems::tall_structures::TallStructures>()
            .init_resource::<systems::height::HeightLayer>()
            .init_resource::<systems::terrain_tiles::TerrainTiles>()
            .init_resource::<systems::water_levels::WaterLevels>()
            .watch_stream::<systems::water_levels::WaterLevels>()
            .init_resource::<systems::terrain::TerrainField>()
            .watch_stream::<systems::terrain::TerrainField>()
            // Default to Auto (live met.no) in interactive runs; keep scripted
            // captures deterministic — no network wait — unless `--weather-live`
            // asks for it.
            .insert_resource(systems::weather::WeatherState::with_live(self.live_weather))
            .watch_stream::<systems::weather::WeatherState>()
            .init_resource::<systems::map_stream::MapStream>()
            .watch_stream::<systems::map_stream::MapStream>()
            .init_resource::<systems::map_stream::ReliefBacklog>()
            .watch_stream::<systems::map_stream::ReliefBacklog>()
            .init_resource::<systems::map_stream::BasemapHoles>()
            .init_resource::<systems::ground_detail::GroundDetail>()
            .init_resource::<systems::ground_detail::GridCellOrigin>()
            // The Settings ▸ Storage button's request, and what came of it.
            .init_resource::<systems::cache_reset::CacheReset>()
            .init_resource::<systems::map_geometry::MapSurfaces>()
            .watch_stream::<systems::map_geometry::MapSurfaces>()
            // Filled by the same streamer, cell for cell (`systems::street_labels`).
            .init_resource::<systems::world_places::WorldPlaces>()
            .init_resource::<systems::street_labels::StreetLabelStore>()
            .init_resource::<systems::map_geometry::DeckPlates>()
            .init_resource::<systems::street_labels::StreetLabelSigns>()
            // Before any ground or water material can prepare: they all bind the
            // cloud deck's buffer (`sky::CLOUD_FIELD_BUFFER`), which this inserts.
            .init_resource::<systems::sky::CloudField>()
            // Must come after the `WaterMaterial` plugin (in `lib.rs`): it mints
            // the shared handle out of `Assets<WaterMaterial>`.
            .init_resource::<systems::water::WaterSeaState>()
            .init_resource::<systems::vegetation::VegetationLod>()
            .init_resource::<systems::wet_surfaces::DrySurfaces>()
            .init_resource::<systems::grass::GrassAreas>()
            .init_resource::<systems::grass::GrassPatches>()
            .watch_stream::<systems::grass::GrassPatches>()
            .init_resource::<systems::street_furniture::FurnitureLod>()
            .init_resource::<systems::globe::GlobeState>()
            .watch_stream::<systems::globe::GlobeState>()
            .init_resource::<systems::globe::GlobeSurfaceAlbedo>()
            .init_resource::<systems::globe_ocean::GlobeOceanGloss>()
            .init_resource::<systems::placeholder_models::PlaceholderModels>();

        app.add_systems(
            Startup,
            (
                // After the camera, which is where this run's `SkyMode` is decided —
                // it can't be known before the render device exists.
                systems::sky_dome::setup_sky_dome.after(systems::free_camera::setup_camera),
                sky::setup_sky,
                sky::setup_stars,
                sky::setup_moon,
                sky::setup_horizon_haze,
                sky::setup_clouds,
                sky::setup_globe_clouds,
                sky::setup_rain,
                // The shared quad pool every particle emitter draws. No ordering: it
                // only builds a mesh, and `sync_particle_emitters` waits for the
                // resource rather than assuming it.
                systems::particles::setup_particle_pool,
            ),
        );

        // The meter's enforcement half: fund the frame's shared finalisation
        // budget before any streamer runs, so what they collectively hand the
        // GPU per frame is bounded once rather than per streamer — see
        // `systems::stream_ledger` for why the per-streamer caps alone still
        // queued 27 MB frames.
        app.add_systems(First, systems::stream_ledger::open_frame);
        // `CARTO_DBG_HIDE`: a bisecting switch that hides whole layers by name.
        app.add_systems(PostUpdate, systems::map_geometry::debug_hide_layers);
        // The upload meter's other consumer: the ring that streamers reach with,
        // steered by the residency the meter measures rather than fixed by the tier
        // alone (`systems::load_control`). After `measure_uploads` in the same
        // schedule, so it reads this frame's total and not last frame's; off under
        // the shot harness, where a ring that changed size between captures would
        // make two runs of `cargo shots` differ for a reason no image could show.
        app.add_systems(
            Last,
            systems::load_control::observe
                .after(systems::upload_meter::measure_uploads)
                .run_if(bevy::prelude::not(systems::shot_harness::harness_active)),
        );

        app.add_systems(
            Update,
            (
                systems::map_stream::init_map_stream,
                // The buildings behind the prims. **After** `prim_picking`, which
                // is what gets to say whether the click was already spent — see
                // `picking::PrimClick`.
                // A tap on another person's avatar opens their card; between the
                // prims (which keep a tap on a prim in front) and the buildings
                // (which must not take a tap the avatar took).
                systems::contacts::tap_avatar
                    .after(systems::picking::prim_picking)
                    .before(systems::building_select::pick_building),
                systems::building_select::pick_building.after(systems::picking::prim_picking),
            )
                .in_set(ClientPhase::Input),
        );

        // Re-anchor requests (from teleports / globe descent) drop the old tiles
        // before streaming resumes at the new origin (ADR-025).
        app.add_systems(
            Update,
            (
                systems::map_stream::apply_reanchor
                    .after(systems::geo_nav::apply_teleport)
                    .before(systems::map_stream::update_map_stream),
                // A basemap source change retears the clipmap the same way, so it
                // has to land before the streamer refills it — and before
                // `apply_stream_textures`, which asks the same resolved answer
                // which source a finished tile came from.
                systems::map_stream::apply_aerial_source
                    .after(systems::map_stream::init_map_stream)
                    .before(systems::map_stream::update_map_stream),
                // …and so does a cache reset, for the same reason and through the
                // same teardown: the tiles it drops must be gone before the
                // streamer decides what to refill.
                systems::cache_reset::apply_cache_reset
                    .after(systems::map_stream::init_map_stream)
                    .before(systems::map_stream::update_map_stream),
            )
                .in_set(ClientPhase::Anchor),
        );

        app.add_systems(
            Update,
            (
                // Buildings and what hangs off them. `update_building_facts` takes
                // no ordering edges: it spawns nothing and is read by panels and
                // readouts, never by a streamer mid-frame.
                (
                    systems::osm_buildings::update_buildings,
                    // After the field's ask, for the hold — see `update_map_surfaces`.
                    systems::lod22::update_lod22.after(systems::terrain::update_terrain_field),
                    systems::building_facts::update_building_facts,
                    systems::surveyed::prefetch_bgt_coverage,
                    systems::crops::prefetch_brp_coverage,
                    systems::terrain_tiles::prefetch_terrain_coverage,
                    systems::interiors::update_interiors,
                    // Which building the avatar is standing in, and the glass that
                    // answers. Chained after the rooms because `track_indoors` reads
                    // the set `update_interiors` has just published — a frame's lag
                    // there is a frame in which the collider still seals a doorway
                    // the avatar has already walked through.
                    (
                        systems::building_select::track_indoors,
                        systems::building_select::clear_selection_on_reanchor,
                        systems::building_select::update_building_shells,
                    )
                        .chain()
                        .after(systems::interiors::update_interiors),
                    // The flood defences: stream, measure the crest, hand the names
                    // to the label placer. Chained because each step reads what the
                    // one before it wrote, and the whole run is a few hundred lines
                    // of polyline — the alternative is a name that appears a frame
                    // after the crest it stands on.
                    (
                        systems::dikes::update_dikes,
                        systems::dikes::measure_dike_crests,
                        systems::dikes::feed_dike_labels,
                    )
                        .chain(),
                ),
                // Weather, the height layer and the terrain field.
                (
                    // Live weather for the current anchor (met.no); folds onto the
                    // cloud/fog/rain knobs after the anchor is (re)established.
                    systems::weather::poll_weather.after(systems::map_stream::apply_reanchor),
                    // Live global cloud cover (GFS/TCDC) for the planet layer
                    // (ADR-032): one background fetch → the coarse coverage texture
                    // the globe cloud shader samples. Independent of the anchor
                    // (it's global).
                    systems::globe_weather::poll_globe_clouds,
                    // The height layer (AHN): the queue that fills its patch cache,
                    // and the anchor ground probe that both proves the path works
                    // and gives `--dump-state` a measured number to show.
                    systems::height::poll_height_layer,
                    // The same field's heights as atlas tiles, where atlas has cut
                    // the area (`terrain_tiles`); asked for by the same system.
                    systems::terrain_tiles::poll_terrain_tiles,
                    systems::height::probe_anchor_ground
                        .after(systems::map_stream::apply_reanchor)
                        .before(systems::height::poll_height_layer),
                    // Terrain relief: one measured box per anchor, baked into the
                    // field every ground-attached system samples. Asks *before* the
                    // queue is polled so a fetch it starts is dispatched the same
                    // frame. The measured water levels the two terrain bakes carve
                    // with. Before them, so a cell that lands is folded into the
                    // same frame's bake rather than costing an extra re-bake of the
                    // whole field.
                    systems::water_levels::update_water_levels
                        .after(systems::map_stream::apply_reanchor),
                    systems::terrain::update_terrain_field
                        .after(systems::map_stream::apply_reanchor)
                        .after(systems::water_levels::update_water_levels)
                        .after(systems::terrain::apply_coarse_terrain)
                        .before(systems::height::poll_height_layer),
                    // Install a field bake that landed. Before the ask, so a box
                    // that finished this frame is the one the ask compares against.
                    systems::terrain::apply_coarse_terrain
                        .after(systems::map_stream::apply_reanchor),
                    // The sub-metre half: the tier's slope into a shared texture,
                    // and the render-space uniform that tells the ground materials
                    // where it is. After `apply_fine_terrain`, which is what hands
                    // the texels over.
                    systems::ground_detail::track_grid_origin,
                    systems::ground_detail::receive_ground_detail
                        .after(systems::terrain::apply_fine_terrain),
                    systems::ground_detail::refresh_ground_detail
                        .after(systems::ground_detail::receive_ground_detail)
                        .after(systems::ground_detail::track_grid_origin),
                    // The half-metre tier under the camera. After the coarse field,
                    // whose datum it borrows and whose answer it fades into at its
                    // rim.
                    systems::terrain::update_fine_terrain
                        .after(systems::terrain::update_terrain_field)
                        .after(systems::terrain::apply_coarse_terrain)
                        .after(systems::water_levels::update_water_levels)
                        .after(systems::terrain::apply_fine_terrain)
                        .before(systems::height::poll_height_layer),
                    // Install a bake that landed. Before the ask, so a box that
                    // finished this frame is the one the ask compares against.
                    systems::terrain::apply_fine_terrain.after(systems::map_stream::apply_reanchor),
                ),
                // The basemap and the vector surfaces over it.
                (
                    systems::map_stream::update_map_stream
                        .after(systems::map_stream::init_map_stream)
                        .after(systems::map_stream::apply_reanchor),
                    systems::map_stream::apply_stream_textures
                        .after(systems::map_stream::update_map_stream),
                    // Which clipmap tiles are buried under finer ones, and can be
                    // left out of the draw list. After the textures land, so a tile
                    // that became ready this frame counts this frame; before the
                    // visibility sweep, which is the only thing allowed to act on it.
                    systems::map_stream::update_tile_occlusion
                        .after(systems::map_stream::apply_stream_textures),
                    // Each tile's own ring, so the shader can soften it into the
                    // level beneath rather than meeting it at a step. After the
                    // textures, for the reason the occlusion pass is: a tile that
                    // gained its material this frame gets its ring in the same frame
                    // instead of being drawn once at full sharpness and corrected
                    // afterwards.
                    systems::map_stream::refresh_tile_lod_blend
                        .after(systems::map_stream::apply_stream_textures)
                        .after(systems::ground_detail::track_grid_origin),
                    // Stand the basemap on the measured ground. After the spawn
                    // pass, so a tile that appeared this frame is lifted in the same
                    // frame it is placed rather than showing flat for one.
                    systems::map_stream::apply_terrain_to_tiles
                        .after(systems::map_stream::update_map_stream)
                        .after(systems::terrain::update_terrain_field),
                    // Vector surfaces as geometry (ADR-035 option B): water and
                    // landuse meshes over the textured tiles. No-op on a raster tile
                    // source. After the field's ask, so a new anchor's hold on the
                    // cells (`TerrainField::holds_builds`) is in place before any
                    // dispatch.
                    systems::map_geometry::update_map_surfaces
                        .after(systems::map_stream::update_map_stream)
                        .after(systems::terrain::update_terrain_field),
                    // Bridge decks stand at a *measured* height, so they are placed
                    // a few frames after their cell — whenever the height layer
                    // answers.
                    systems::map_geometry::resolve_bridge_decks
                        .after(systems::map_geometry::update_map_surfaces)
                        .after(systems::height::poll_height_layer),
                    // Sea state from the live wind (cheap: skips unless the wind
                    // moved).
                    systems::water::update_water_waves,
                    // "Road textures" toggle: retexture/flatten the shared road
                    // materials (cheap: reads unless a material is on the wrong
                    // side).
                    systems::map_geometry::apply_road_texture
                        .after(systems::map_geometry::update_map_surfaces),
                    // Ramp the scattered trees into the flat fill with altitude.
                    // Must follow the surface build, which is what creates the
                    // materials it repaints. Skips unless the ramp actually moved.
                    systems::vegetation::update_vegetation_lod
                        .after(systems::map_geometry::update_map_surfaces),
                    // Same for the street furniture, over its own (much lower) band.
                    systems::street_furniture::update_furniture_lod
                        .after(systems::map_geometry::update_map_surfaces),
                    // Stand up the models this server draws its street objects
                    // with, a bounded number per frame. After the surface build,
                    // which is what queues them and what owns the cell they are
                    // attached to — running first would spawn into cells that do
                    // not exist yet.
                    systems::placeholder_models::spawn_placeholder_models
                        .after(systems::map_geometry::update_map_surfaces),
                    // …and ask the server what those are, once per session.
                    systems::placeholder_models::poll_placeholders,
                    // Near-field grass: slide the patch pool along its lattice, keep
                    // the coverage mask under it and drive the sway from the live
                    // wind. After the surface build, which is what fills the
                    // per-cell grass areas it plants into — a frame's lag there is a
                    // frame of bare ground on a cell that just streamed in.
                    systems::grass::update_grass.after(systems::map_geometry::update_map_surfaces),
                    // Wet the hard ground when it rains. Same ordering and the same
                    // reason as the two ramps above: it repaints materials that the
                    // surface build is what creates, so running it first would ramp
                    // whatever existed last frame and leave a cell that has just
                    // landed dry until the rain next moved.
                    systems::wet_surfaces::apply_wet_surfaces
                        .after(systems::map_geometry::update_map_surfaces),
                ),
                // The globe, the Overpass street detail, and the server's prims.
                (
                    // Globe / space tier (ADR-025): (re)build when the anchor
                    // moves, then bake its OSM skin in the background.
                    systems::globe::setup_globe
                        .after(systems::map_stream::init_map_stream)
                        .after(systems::map_stream::apply_reanchor),
                    // Whole-globe quadtree tile streaming (ADR-025).
                    systems::globe::update_globe_tiles.after(systems::globe::setup_globe),
                    // The Overpass street-detail streamer, and the small systems
                    // riding it: night glow + the point-light pool, the signal phase
                    // cycle and the altitude fade.
                    systems::street_detail::update_street_detail,
                    systems::street_detail::update_lamp_glow
                        .after(systems::street_detail::update_street_detail),
                    systems::street_detail::update_lamp_lights
                        .after(systems::street_detail::update_street_detail),
                    systems::street_detail::update_signal_cycle
                        .after(systems::street_detail::update_street_detail),
                    systems::street_detail::update_prop_fade
                        .after(systems::street_detail::update_street_detail),
                    systems::tall_structures::update_tall_structures,
                    systems::tall_structures::spin_rotors
                        .after(systems::tall_structures::update_tall_structures),
                    rendering::spawn_prims.after(network::apply_network_snapshot),
                    // After `spawn_prims`, for the same reason the particle emitters
                    // are: this hangs a child off the prim, and a `Children` list
                    // written in the same flush as the parent's own components is
                    // how a scene ends up on an entity that is still being
                    // assembled. It also *removes* `PrimMesh` when a model is taken
                    // off, which hands the prim back to `spawn_prims` on the
                    // following frame.
                    model_assets::attach_prim_models
                        .after(network::apply_network_snapshot)
                        .after(rendering::spawn_prims),
                    rendering::rebuild_prim_meshes.after(rendering::spawn_prims),
                    rendering::refresh_prim_textures
                        .after(rendering::spawn_prims)
                        .after(network::apply_network_snapshot),
                    // After the snapshot has written the prims' `particles` field,
                    // and after `spawn_prims`, so the emitter child is parented to
                    // an entity that already has its mesh — a `Children` list
                    // rewritten in the same flush as the parent's own components is
                    // how an emitter ends up on a prim that is still being
                    // assembled.
                    systems::particles::sync_particle_emitters
                        .after(network::apply_network_snapshot)
                        .after(rendering::spawn_prims),
                    // Swap bevy_pbr's atmosphere functions module for the patched
                    // copy as soon as its async load lands (Adreno 840 black-sky
                    // hardening — see `systems::atmosphere_patch`). It acts on an
                    // asset, not on this frame's entities, so it orders against
                    // nothing.
                    systems::atmosphere_patch::patch_atmosphere_shader,
                ),
            )
                .in_set(ClientPhase::Stream),
        );

        app.add_systems(
            Update,
            (
                systems::globe::update_globe_visibility
                    .after(systems::free_camera::camera_controls),
                systems::globe::update_globe_albedo.after(systems::free_camera::camera_controls),
                // Track the atmosphere's planet centre against the floating-origin
                // camera cell (correct sky altitude in every tier — see its doc).
                systems::globe::update_atmosphere_origin
                    .after(systems::free_camera::camera_controls),
                // Every switch in Settings ▸ Graphics — the light shafts, bloom,
                // samples, the sky and its reflections — reconciled with what it
                // currently asks for. After the camera, which is the entity they
                // write and which measures the capability half of the answer.
                (
                    systems::render_features::apply_render_features,
                    systems::render_features::apply_sky,
                    systems::render_features::find_env_map_component,
                )
                    .chain()
                    .after(systems::free_camera::camera_controls),
                (
                    sky::apply_sun_mode
                        .before(sky::advance_day_cycle)
                        .after(systems::transit_live::update_world_clock),
                    sky::advance_day_cycle.before(sky::apply_day_night_cycle),
                ),
                sky::apply_day_night_cycle.after(systems::dev_panel::render_dev_panel),
                sky::manage_lights.after(systems::dev_panel::render_dev_panel),
                sky::apply_distance_fog.after(systems::dev_panel::render_dev_panel),
                sky::update_window_glow,
                // The other lane's windows. `update_window_glow` above only
                // reaches the OSM buildings, which are never drawn over 3DBAG
                // coverage — see `systems::building_windows`.
                systems::building_windows::update_building_windows,
                // Keep the far tier's height ramp centred on the camera. Every
                // frame, affordable for the reason the line above is: five
                // materials, not one per cell.
                systems::building_fade::update_building_fade
                    .after(systems::ground_detail::track_grid_origin),
                // Track the camera after it has been positioned this frame.
                (
                    sky::update_star_sky.after(systems::free_camera::camera_controls),
                    // The fog volume the light shafts are marched through: placed
                    // from the camera's transform, so a frame's lag here is a
                    // frame of shafts centred where the viewer used to be.
                    systems::light_shafts::follow_camera
                        .after(systems::free_camera::camera_controls),
                    // Where the sun is on screen, for the sunbeams: after the camera
                    // has moved, or the beams trail the turn by a frame.
                    systems::sun_rays::update_sun_rays.after(systems::free_camera::camera_controls),
                ),
                sky::update_moon.after(systems::free_camera::camera_controls),
                (
                    sky::update_horizon_haze.after(systems::free_camera::camera_controls),
                    systems::sky_dome::update_sky_dome.after(systems::free_camera::camera_controls),
                    sky::update_clouds.after(systems::free_camera::camera_controls),
                    sky::update_globe_clouds.after(systems::free_camera::camera_controls),
                    sky::update_rain.after(systems::free_camera::camera_controls),
                ),
                // Street names, laid on the streets themselves. World geometry, not
                // an overlay — after the camera so a turn that flips the letters is
                // acted on in the frame it happened.
                systems::street_labels::update_street_labels
                    .after(systems::free_camera::camera_controls),
                systems::render_scale::apply_preferred_render_scale,
            )
                .in_set(ClientPhase::Present),
        );
    }
}
