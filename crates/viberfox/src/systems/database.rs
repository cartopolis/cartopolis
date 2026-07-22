use bevy::prelude::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::TextureFormat;
use std::sync::Mutex;
use crate::components::Region;
use crate::db::schema::RegionRow;
use crate::resources::{Database, GameState, PrimTextureCache, TextureLibrary};
use crate::systems::egui_manager::EguiManager;
use vibe_core::TextureEntry;

pub fn init_database(mut commands: Commands) {
    let db_path = "data/regions.db";

    // Ensure data directory exists (SQLite does not create parent dirs)
    if let Some(parent) = std::path::Path::new(db_path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("❌ Failed to create data directory: {}", e);
            return;
        }
    }

    match crate::db::schema::init_database(db_path) {
        Ok(conn) => {
            println!("✅ Database initialized");
            commands.insert_resource(Database { conn: Mutex::new(conn) });
        }
        Err(e) => {
            eprintln!("❌ Failed to initialize database: {}", e);
        }
    }
}

pub fn load_regions(
    db: Res<Database>,
    mut commands: Commands,
    mut game_state: ResMut<GameState>,
) {
    if game_state.regions_loaded {
        return;
    }

    let regions_result: Result<Vec<RegionRow>, rusqlite::Error> = {
        let conn = db.conn.lock().unwrap();
        let mut stmt = match conn.prepare("SELECT * FROM regions ORDER BY created_at DESC") {
            Ok(stmt) => stmt,
            Err(e) => {
                eprintln!("Error preparing regions query: {}", e);
                return;
            }
        };

        let rows_result = stmt.query_map([], |row| {
            Ok(RegionRow {
                id: row.get(0)?,
                name: row.get(1)?,
                latitude: row.get(2)?,
                longitude: row.get(3)?,
                tile_x: row.get(4)?,
                tile_y: row.get(5)?,
                tile_z: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        });

        match rows_result {
            Ok(rows) => rows.collect(),
            Err(e) => {
                eprintln!("Error querying regions: {}", e);
                return;
            }
        }
    };

    match regions_result {
        Ok(regions) => {
            let count = regions.len();
            for region in regions {
                // Create region entity (rendering will be handled separately)
                // Only add Region component, no Transform or other components
                commands.spawn(Region {
                    id: region.id,
                    name: region.name.clone(),
                    latitude: region.latitude,
                    longitude: region.longitude,
                    tile_x: region.tile_x,
                    tile_y: region.tile_y,
                    tile_z: region.tile_z,
                    sim_origin: None,
                });
                tracing::debug!("spawned region entity for '{}'", region.name);
            }
            tracing::info!("loaded {} regions", count);
            game_state.regions_loaded = true;
        }
        Err(e) => {
            eprintln!("Error querying regions: {}", e);
        }
    }
}

/// Offline mode: scan `data/textures/*.{png,jpg,jpeg}` and pre-load them.
pub fn load_local_textures(
    egui_manager: Res<EguiManager>,
    mut texture_lib: ResMut<TextureLibrary>,
    mut texture_cache: ResMut<PrimTextureCache>,
    mut images: ResMut<Assets<Image>>,
) {
    let dir = "data/textures";
    let path = std::path::Path::new(dir);
    if !path.exists() {
        return;
    }

    let mut entries: Vec<TextureEntry> = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let p = entry.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !matches!(ext, "png" | "jpg" | "jpeg") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                let id = stem.to_string();
                if texture_cache.handles.contains_key(&id) {
                    continue; // already loaded (e.g. both .png and .jpg with same stem)
                }
                let name = title_case(stem);
                if let Ok(bytes) = std::fs::read(&p) {
                    if let Some(rgba) = decode_to_rgba(&bytes) {
                        let (w, h) = rgba.dimensions();
                        let raw = rgba.into_raw();

                        // Egui ColorImage for the picker thumbnail.
                        let pixels: Vec<egui::Color32> = raw.chunks_exact(4)
                            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                            .collect();
                        let color_image = egui::ColorImage::new([w as usize, h as usize], pixels);
                        let egui_handle = egui_manager.ctx.load_texture(
                            &id,
                            color_image,
                            egui::TextureOptions::LINEAR,
                        );
                        texture_lib.egui_handles.insert(id.clone(), egui_handle);

                        // Bevy Image for material texturing (mipmapped, repeat
                        // addressing so repeat_u/v > 1 tiles).
                        let bevy_img = crate::systems::rendering::mipmapped_image(
                            w,
                            h,
                            raw,
                            TextureFormat::Rgba8UnormSrgb,
                            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
                            crate::systems::rendering::repeat_linear_sampler(),
                        );
                        let handle = images.add(bevy_img);
                        texture_cache.handles.insert(id.clone(), handle);
                        entries.push(TextureEntry { id, name });
                    }
                }
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let count = entries.len();
    texture_lib.entries = entries;
    if count > 0 {
        tracing::info!(count, "loaded local textures from {dir}");
    }
}

fn decode_to_rgba(bytes: &[u8]) -> Option<image::RgbaImage> {
    Some(image::load_from_memory(bytes).ok()?.to_rgba8())
}

fn title_case(s: &str) -> String {
    let spaced = s.replace('_', " ").replace('-', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
