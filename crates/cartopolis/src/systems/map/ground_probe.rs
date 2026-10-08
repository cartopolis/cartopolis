//! `CARTO_PROBE="lat,lng;lat,lng…"`: at each point, log the measured ground
//! and the height every ground layer is actually drawn at.
//!
//! The ground is half a dozen sheets — the basemap's relief tiles, the landuse
//! fills, the surveyed paving, the roads — each bent onto the terrain by its own
//! code against its own triangles, and the failure that keeps recurring is one
//! of them standing through another. A capture shows *that* it happens and never
//! *which* sheet is wrong or by how much; this answers both, from the producers
//! themselves, at the moment they place their vertices. `ground probe` lines in
//! the log, one per sheet per point.

use crate::platform::sync::Mutex;

use bevy::prelude::*;

static PROBES: Mutex<Vec<Vec2>> = Mutex::new(Vec::new());

/// The probe points in the anchored world-XZ frame; empty when no probe is set.
pub fn probes() -> Vec<Vec2> {
    PROBES.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Resolve `CARTO_PROBE` against the current map anchor, and log the measured
/// ground there whenever the anchor or the terrain field changes.
pub fn update_ground_probes(
    anchor: Res<crate::resources::MapAnchor>,
    field: Res<crate::systems::terrain::TerrainField>,
    mut last: Local<Option<((f64, f64, u32), u32)>>,
) {
    let Some(a) = anchor.geo else { return };
    let Ok(spec) = std::env::var("CARTO_PROBE") else {
        return;
    };
    if *last == Some((a, field.revision)) {
        return;
    }
    *last = Some((a, field.revision));
    let points: Vec<Vec2> = spec
        .split(';')
        .filter_map(|p| {
            let (lat, lng) = p.split_once(',')?;
            let (lat, lng) = (lat.trim().parse().ok()?, lng.trim().parse().ok()?);
            let (x, z) = crate::systems::geo_nav::lat_lng_to_world(a, lat, lng);
            Some(Vec2::new(x as f32, z as f32))
        })
        .collect();
    for (i, p) in points.iter().enumerate() {
        info!(
            probe = i,
            x = p.x,
            z = p.y,
            field = field.at(p.x, p.y),
            floor_3m = field.floor_at(p.x, p.y, 3.125),
            floor_6m = field.floor_at(p.x, p.y, 6.25),
            floor_12m = field.floor_at(p.x, p.y, 12.5),
            revision = field.revision,
            "ground probe: measured"
        );
    }
    if let Ok(mut g) = PROBES.lock() {
        *g = points;
    }
}

/// The height of a triangle mesh at a point in its own XZ frame, from the first
/// upward-facing triangle containing it.
#[must_use]
pub fn height_at(positions: &[[f32; 3]], indices: &[u32], x: f32, z: f32) -> Option<f32> {
    for t in indices.chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions[i as usize]);
        let det = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
        if det.abs() < 1e-9 {
            continue;
        }
        let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / det;
        let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / det;
        let l3 = 1.0 - l1 - l2;
        if l1 >= -1e-5 && l2 >= -1e-5 && l3 >= -1e-5 {
            return Some(l1 * a[1] + l2 * b[1] + l3 * c[1]);
        }
    }
    None
}
