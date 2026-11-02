//! The home country's outline, which the painted far map draws as the strategy
//! map's glowing border (`docs/mockups/macro-view`, v1-macro).
//!
//! The vector tiles' `boundaries` are lines between countries, so they have no
//! coast and no islands; the mockup traced the whole country. The outline is
//! Natural Earth's, as the mockup's was (`atlas areas ne-admin0-europe`, served
//! at `{CARTO_3DBAG_URL}/areas/ne-admin0-europe.bin`), fetched once. Not the
//! Kadaster's country (`cbs-land-2024`): that is the municipalities' territory,
//! which runs a few kilometres out to sea and round the Wadden Sea.
//!
//! Every painted tile waits for it before it is drawn, because the outline is
//! part of the pixels and so of the stored tile's key: a tile drawn before it
//! arrived would be kept without a border. A fetch that fails gives no outline
//! for the rest of the run, the same as a host that serves none.

use crate::platform::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_channel::oneshot;

use cartopolis_core::TileKey;

/// The area file the outline comes from.
const AREA_SET: &str = "ne-admin0-europe";

/// The country bordered, by its ISO alpha-3 code.
const HOME: &str = "NLD";

/// One outer ring in lon/lat degrees, with its `[west, south, east, north]`.
/// Holes are left out: a country's holes are lakes or an enclave, which the
/// mockup does not border.
pub struct Ring {
    points: Vec<[f64; 2]>,
    bbox: [f64; 4],
}

pub type Outline = Arc<Vec<Ring>>;

enum State {
    Idle,
    Loading(Vec<oneshot::Sender<Outline>>),
    Ready(Outline),
}

static STATE: Mutex<State> = Mutex::new(State::Idle);

fn lock() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The outline, fetched by the first caller while the rest wait for it.
pub async fn outline() -> Outline {
    loop {
        let rx = {
            let mut state = lock();
            match &mut *state {
                State::Ready(o) => return o.clone(),
                State::Loading(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                State::Idle => {
                    *state = State::Loading(Vec::new());
                    None
                }
            }
        };
        match rx {
            // A leader dropped mid-fetch (its tile left the view) cancels the
            // waiters, and one of them leads the next round.
            Some(rx) => {
                if let Ok(o) = rx.await {
                    return o;
                }
            }
            None => return lead().await,
        }
    }
}

async fn lead() -> Outline {
    /// Puts the state back to idle if this fetch is dropped before it ends.
    struct Leading(bool);
    impl Drop for Leading {
        fn drop(&mut self) {
            if self.0 {
                *lock() = State::Idle;
            }
        }
    }
    let mut guard = Leading(true);
    let outline: Outline = Arc::new(fetch().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "home country outline: none, the far map draws no border");
        Vec::new()
    }));
    guard.0 = false;
    let previous = std::mem::replace(&mut *lock(), State::Ready(outline.clone()));
    if let State::Loading(waiters) = previous {
        for tx in waiters {
            let _ = tx.send(outline.clone());
        }
    }
    outline
}

async fn fetch() -> Result<Vec<Ring>, String> {
    let base = crate::systems::osm_buildings::tdbag_url();
    let url = format!("{}/areas/{AREA_SET}.bin", base.trim_end_matches('/'));
    let bytes = crate::platform::http::fetch(crate::platform::http::Request::get(url)).await?;
    let set = cartopolis_scenario::board::AreaSet::decode(&bytes)
        .map_err(|e| format!("{AREA_SET}: {e}"))?;
    Ok(set
        .areas
        .iter()
        .filter(|a| a.country == HOME)
        .flat_map(|a| a.polygons.iter())
        .filter_map(|polygon| polygon.first())
        .filter(|outer| outer.len() >= 3)
        .map(|outer| {
            let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for &[lon, lat] in outer {
                bbox = [
                    bbox[0].min(lon),
                    bbox[1].min(lat),
                    bbox[2].max(lon),
                    bbox[3].max(lat),
                ];
            }
            Ring {
                points: outer.clone(),
                bbox,
            }
        })
        .collect())
}

/// The outline's rings for `source`, once the outline is in.
pub async fn rings_for(source: TileKey) -> Vec<Vec<[f32; 2]>> {
    rings_in(&outline().await, source)
}

/// The rings that reach `source`, in its unit square (x east, y south, Web
/// Mercator), for `vector_tiles::FarDetail::country`.
#[must_use]
pub fn rings_in(outline: &[Ring], source: TileKey) -> Vec<Vec<[f32; 2]>> {
    let n = f64::from(1u32 << source.z.min(30));
    let (tx, ty) = (source.x as f64, source.y as f64);
    let lon = |x: f64| x / n * 360.0 - 180.0;
    let lat = |y: f64| {
        (std::f64::consts::PI * (1.0 - 2.0 * y / n))
            .sinh()
            .atan()
            .to_degrees()
    };
    // A twentieth of the tile around it, wider than the glow, so a ring just
    // outside still paints its edge in.
    let m = 0.05;
    let (w, e) = (lon(tx - m), lon(tx + 1.0 + m));
    let (north, south) = (lat(ty - m), lat(ty + 1.0 + m));
    outline
        .iter()
        .filter(|r| r.bbox[0] <= e && r.bbox[2] >= w && r.bbox[1] <= north && r.bbox[3] >= south)
        .map(|r| {
            r.points
                .iter()
                .map(|&[lo, la]| {
                    let x = (lo + 180.0) / 360.0 * n - tx;
                    let y =
                        (1.0 - la.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n - ty;
                    [x as f32, y as f32]
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(w: f64, s: f64, e: f64, n: f64) -> Ring {
        Ring {
            points: vec![[w, s], [e, s], [e, n], [w, n]],
            bbox: [w, s, e, n],
        }
    }

    #[test]
    fn a_ring_lands_in_its_tile_and_not_in_another() {
        // z7 tile 65/42 holds Utrecht (5.12°E, 52.09°N).
        let utrecht = [square(5.0, 52.0, 5.2, 52.2)];
        let rings = rings_in(&utrecht, TileKey::new(65, 42, 7));
        assert_eq!(rings.len(), 1);
        for p in &rings[0] {
            assert!(
                (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1]),
                "{p:?}"
            );
        }
        assert!(rings_in(&utrecht, TileKey::new(70, 42, 7)).is_empty());
    }
}
