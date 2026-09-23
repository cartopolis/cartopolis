//! The Kadaster's **3D basisvoorziening**: the same BGT, draped and extruded
//! against AHN by 3dfier and published as CityJSON. Two answers come out of it —
//! measured heights for the BGT's boundaries (`bgt`) and the level each water body
//! stands at (`water`). Ported from the earlier TypeScript pipeline (`bv3d.ts`).
//!
//! Objects carry `gml_id`, the flat BGT API's `lokaal_id` verbatim, so the join is
//! an exact string match — no geometry matching, no tolerance. **Polygons only**:
//! every table ends in `_vlak`, so a boundary surveyed as a *line* has no
//! counterpart here and gets no height. **It is a drape**: an object with no real
//! elevation comes back as a few centimetres of AHN noise, and deciding what is
//! worth a mesh is the client's job, not this one's.
//!
//! One zip per 2 km RD block, ~12.5 MB compressed, ~64 MB of CityJSON inside: a
//! bulk download, because the service publishes sheets, not features.

use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::http::{get_bytes_optional, head_ok};
use super::{Ctx, Outcome, PipelineError, Progress};

/// Edge of one published sheet, RD metres.
const BLOCK_M: f64 = 2000.0;

const DOWNLOAD_ROOT: &str = "https://download.pdok.nl/kadaster/basisvoorziening-3d/v1_0";

/// Years to look back for a published edition, newest first. The path carries
/// the year and there is no index to ask, so the year is *probed*: a constant
/// would work for a while and then silently find nothing, which puts every wall
/// back to a nominal slab with nothing in the log to say why.
const YEARS_BACK: i32 = 4;

/// The table holding the BGT's boundaries.
const SEPARATION_TABLE: &str = "Scheiding_vlak";

#[derive(Deserialize)]
pub struct Transform {
    pub scale: [f64; 3],
    pub translate: [f64; 3],
}

#[derive(Deserialize, Default)]
pub struct Attributes {
    pub tabelnaam: Option<String>,
    pub gml_id: Option<Value>,
}

#[derive(Deserialize)]
pub struct Geometry {
    #[serde(default)]
    pub boundaries: Value,
}

#[derive(Deserialize)]
pub struct CityObject {
    #[serde(default)]
    pub attributes: Option<Attributes>,
    #[serde(default)]
    pub geometry: Vec<Geometry>,
}

/// Only what the two readers need, typed: a block is ~64 MB of JSON and an
/// untyped tree of it would cost several times that in memory.
#[derive(Deserialize)]
pub struct CityJson {
    #[serde(rename = "CityObjects")]
    pub city_objects: HashMap<String, CityObject>,
    pub transform: Transform,
    pub vertices: Vec<[f64; 3]>,
}

/// The single entry of a zip, without a zip library. These archives hold exactly
/// one CityJSON file as their first entry, so the local file header at offset 0 is
/// all that has to be understood — as the TypeScript did it.
pub fn single_zip_entry(raw: &[u8]) -> Result<Vec<u8>, String> {
    let u16_at = |o: usize| raw.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| {
        raw.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    if u32_at(0) != Some(0x0403_4b50) {
        return Err("not a zip (no local file header)".into());
    }
    let method = u16_at(8).ok_or("truncated zip")?;
    let compressed = u32_at(18).ok_or("truncated zip")? as usize;
    let start = 30
        + usize::from(u16_at(26).ok_or("truncated zip")?)
        + usize::from(u16_at(28).ok_or("truncated zip")?);
    let body = raw
        .get(start..start + compressed)
        .ok_or("truncated zip entry")?;
    match method {
        0 => Ok(body.to_vec()),
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(body)
                .read_to_end(&mut out)
                .map_err(|e| format!("inflate: {e}"))?;
            Ok(out)
        }
        m => Err(format!("unsupported zip compression method {m}")),
    }
}

/// The 2 km blocks a bbox touches, as `[x, y]` south-west corners in RD metres.
#[must_use]
pub fn blocks_for(bbox: [f64; 4]) -> Vec<[i64; 2]> {
    let [min_x, min_y, max_x, max_y] = bbox;
    let x0 = (min_x / BLOCK_M).floor() * BLOCK_M;
    let y0 = (min_y / BLOCK_M).floor() * BLOCK_M;
    let mut out = Vec::new();
    let mut y = y0;
    while y <= max_y {
        let mut x = x0;
        while x <= max_x {
            out.push([x as i64, y as i64]);
            x += BLOCK_M;
        }
        y += BLOCK_M;
    }
    out
}

fn block_url(year: i32, x: i64, y: i64) -> String {
    format!("{DOWNLOAD_ROOT}/{year}/volledig/volledig_{year}_{x}_{y}.zip")
}

/// The calendar year now, UTC, from the system clock (Howard Hinnant's
/// days-to-civil, to avoid a date library for one number).
fn this_year() -> i32 {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2)) as i32
}

/// Height of every separation object in one block, keyed by `gml_id`: `max z −
/// min z` over the object's own vertices. A wall is a slab, not a roof, so there
/// is no ridge to discount.
#[must_use]
pub fn separation_heights(cj: &CityJson) -> HashMap<String, f64> {
    let (scale_z, translate_z) = (cj.transform.scale[2], cj.transform.translate[2]);
    let mut out = HashMap::new();
    for object in cj.city_objects.values() {
        let Some(attrs) = &object.attributes else {
            continue;
        };
        if attrs.tabelnaam.as_deref() != Some(SEPARATION_TABLE) {
            continue;
        }
        let id = match &attrs.gml_id {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        };
        if id.is_empty() {
            continue;
        }
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        fn walk(node: &Value, vertices: &[[f64; 3]], f: &mut dyn FnMut(f64)) {
            match node {
                Value::Number(n) => {
                    if let Some(v) = n.as_u64().and_then(|i| vertices.get(i as usize)) {
                        f(v[2]);
                    }
                }
                Value::Array(a) => a.iter().for_each(|c| walk(c, vertices, f)),
                _ => {}
            }
        }
        for g in &object.geometry {
            walk(&g.boundaries, &cj.vertices, &mut |z| {
                let z = z * scale_z + translate_z;
                lo = lo.min(z);
                hi = hi.max(z);
            });
        }
        if lo <= hi {
            out.insert(id, hi - lo);
        }
    }
    out
}

/// Every published 2 km block over `ctx.bbox`, parsed and handed to `visit`.
///
/// **Never fails the caller** for a missing edition, an unreachable host or a
/// block that is all water: for both callers that means yesterday's map —
/// nominal wall heights, water at the quay — and failing a whole dataset over a
/// refinement would be the wrong trade. Only a cancel propagates.
pub fn for_each_block(
    ctx: &Ctx,
    what: &str,
    mut visit: impl FnMut(&CityJson, i64, i64),
) -> Outcome<()> {
    let blocks = blocks_for(ctx.bbox);
    let Some(&[bx, by]) = blocks.first() else {
        return Ok(());
    };
    let now = this_year();
    let mut year = None;
    for candidate in ((now - YEARS_BACK + 1)..=now).rev() {
        ctx.check()?;
        if head_ok(&block_url(candidate, bx, by)) {
            year = Some(candidate);
            break;
        }
    }
    let Some(year) = year else {
        ctx.log(format!("{what}: geen 3D basisvoorziening gevonden"));
        return Ok(());
    };
    ctx.log(format!(
        "{what}: 3D basisvoorziening {year} ({} blokken)…",
        blocks.len()
    ));
    let mut failed = 0;
    for (index, &[x, y]) in blocks.iter().enumerate() {
        ctx.check()?;
        ctx.progress(Progress {
            phase: Some(what.to_string()),
            done: Some(index as u64),
            total: Some(Some(blocks.len() as u64)),
        });
        // A block outside the published area (the sea, over the border) is a 404:
        // a block with nothing in it, not a failure.
        let parsed = get_bytes_optional(ctx, &block_url(year, x, y), Duration::from_secs(300))
            .and_then(|b| match b {
                None => Ok(None),
                Some(raw) => single_zip_entry(&raw)
                    .and_then(|json| {
                        serde_json::from_slice::<CityJson>(&json).map_err(|e| e.to_string())
                    })
                    .map(Some)
                    .map_err(PipelineError::Failed),
            });
        match parsed {
            Ok(Some(cj)) => visit(&cj, x, y),
            Ok(None) => {}
            Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(e) => {
                failed += 1;
                ctx.log(format!("{what}: blok {x},{y} overgeslagen ({e})"));
            }
        }
    }
    if failed > 0 {
        ctx.log(format!("{what}: {failed} blokken mislukt"));
    }
    Ok(())
}

/// Measured heights for every separation object over `ctx.bbox`, keyed by the
/// identificatie the flat BGT API returns as `lokaal_id`.
pub fn separation_heights_for(ctx: &Ctx) -> Outcome<HashMap<String, f64>> {
    let mut heights = HashMap::new();
    for_each_block(ctx, "hoogtes", |cj, _, _| {
        heights.extend(separation_heights(cj))
    })?;
    ctx.log(format!("hoogtes: {} gemeten scheidingen", heights.len()));
    Ok(heights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bbox_touches_the_2_km_sheets_it_overlaps() {
        // An edge that lands exactly on a sheet boundary includes the sheet beyond
        // it (`<=`), as the TypeScript did: a block with nothing of the bbox in it
        // costs a download, one missing a strip of it costs data.
        assert_eq!(
            blocks_for([233_000.0, 581_000.0, 234_000.0, 582_000.0]),
            vec![
                [232_000, 580_000],
                [234_000, 580_000],
                [232_000, 582_000],
                [234_000, 582_000]
            ]
        );
    }

    #[test]
    fn the_year_is_plausible() {
        let y = this_year();
        assert!((2025..2100).contains(&y), "{y}");
    }

    #[test]
    fn a_stored_zip_entry_is_read_back() {
        // A minimal zip with one stored ("method 0") entry holding "hello".
        let name = b"a.json";
        let mut z = Vec::new();
        z.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, method 0, time, date, crc
        z.extend_from_slice(&5u32.to_le_bytes()); // compressed size
        z.extend_from_slice(&5u32.to_le_bytes()); // uncompressed size
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name);
        z.extend_from_slice(b"hello");
        assert_eq!(single_zip_entry(&z).unwrap(), b"hello");
        assert!(single_zip_entry(b"PK").is_err());
    }
}
