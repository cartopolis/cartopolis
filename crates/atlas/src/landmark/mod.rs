//! Landmarks rebuilt from measurements and photographs
//! (`docs/notes/landmark-kit.md`).
//!
//! Three steps, each a subcommand of `atlas landmark`:
//!
//! - **gather** a building's outline, laser heights, aerial photograph and
//!   nearby photographs into a folder ([`gather`]);
//! - write a **kit** — the building as stacked parts with openings — from what
//!   was gathered ([`kit`]);
//! - **build** the kit into the GLB the override store takes, and **score** it
//!   against the laser ([`glb`], [`score`]).
//!
//! The GLB goes on the building through the override store's upload, like any
//! other model, so a kit-built landmark is restored by deleting it.

pub mod gather;
pub mod glb;
pub mod kit;
pub mod score;

use std::path::Path;

/// Build a kit file into `<dir>/model.glb`, `<dir>/placement.json` and
/// `<dir>/score.md`. Answers the score's headline.
pub fn build(kit_path: &Path, dir: &Path) -> Result<String, String> {
    let text =
        std::fs::read_to_string(kit_path).map_err(|e| format!("{}: {e}", kit_path.display()))?;
    let kit = kit::Kit::parse(&text)?;
    let pieces = kit.mesh()?;
    let (bytes, at) = glb::write(&kit, &pieces)?;
    let stats = crate::overrides::gltf_model::inspect(&bytes)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("model.glb"), &bytes).map_err(|e| e.to_string())?;
    // What the override record's placement must say for the model to stand
    // where the kit put it: its footprint centre, as built, on the ground.
    let placement = serde_json::json!({"lat": at.0, "lng": at.1, "heading_deg": 0.0,
                                       "scale": 1.0, "base_m": 0.0});
    std::fs::write(dir.join("placement.json"), placement.to_string()).map_err(|e| e.to_string())?;

    let plane = crate::pipeline::models::Plane::at(kit.origin.0, kit.origin.1);
    let outline = crate::pipeline::models::bag_outline(&kit.pand)?;

    // What the override store would serve for it, through the store's own
    // compile path: a lod3 cell file to preview the model behind a stand-in
    // host before anything is published. Filed by the outline's first vertex;
    // the store asks the published shells instead (`models::owner_cell`).
    let record = crate::overrides::Placement {
        lat: Some(at.0),
        lng: Some(at.1),
        ..crate::overrides::Placement::default()
    };
    let elements = crate::overrides::gltf_model::elements(
        &bytes,
        &kit.pand,
        &record,
        crate::overrides::Collision::Walls,
        &outline,
    )?;
    let cell = crate::pipeline::tile::lat_lng_to_cell(outline[0].0, outline[0].1);
    let file = cartopolis_geo::formats::lod22::File {
        elements,
        pands: vec![kit.pand.clone()],
    };
    std::fs::write(
        dir.join(format!("lod3-{}-{}.json", cell.0, cell.1)),
        cartopolis_geo::formats::to_vec(&file).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let outline_en: Vec<[f64; 2]> = outline.iter().map(|p| plane.to_en(p.0, p.1)).collect();
    let reach = outline_en
        .iter()
        .map(|q| q[0].abs().max(q[1].abs()))
        .fold(0.0, f64::max);
    let (dsm, dtm) = gather::surfaces(plane, (reach + 15.0).ceil())?;
    let ground =
        gather::ground(&dtm, &outline_en).ok_or("no bare-earth heights round the building")?;
    let s = score::score(&pieces, &outline_en, &dsm, ground);
    let report = format!(
        "# {} — {} triangles in {} pieces\n\nground {ground:.2} m NAP\n\n{}",
        kit.name,
        stats.triangles,
        pieces.len(),
        s.report()
    );
    std::fs::write(dir.join("score.md"), &report).map_err(|e| e.to_string())?;
    Ok(report.lines().take(5).collect::<Vec<_>>().join("\n"))
}

#[cfg(test)]
mod tests {
    /// Every landmark checked in (`crates/atlas/landmarks/*.json`) still
    /// parses, meshes and makes a GLB the override store takes.
    #[test]
    fn every_checked_in_kit_builds() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("landmarks");
        let mut n = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let kit = super::kit::Kit::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let pieces = kit.mesh().unwrap();
            let (glb, _) = super::glb::write(&kit, &pieces).unwrap();
            crate::overrides::gltf_model::inspect(&glb)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            n += 1;
        }
        assert!(n > 0, "no kits in {}", dir.display());
    }
}
