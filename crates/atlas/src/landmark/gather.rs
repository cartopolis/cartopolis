//! Everything a builder reads before writing a kit, in one folder.
//!
//! ```text
//! facts.md       the outline in local metres, 3DBAG's heights, the laser's
//!                height map and the heights it clusters at (stage tops)
//! aerial.jpg     PDOK's 8 cm photograph of the building and its surroundings
//! photos.md      openly licensed photographs taken nearby, with where each was
//!                taken from and which way it looks — which face it shows
//! photos/*.jpg   those photographs, 1280 px
//! kit.json       a starting kit: the register outline at 3DBAG's height
//! ```

use std::fmt::Write as _;
use std::path::Path;

use crate::pipeline::models::{self, Plane, Surface};
use crate::terrain::{HeightSource, Pdok};

const UA: &str = "cartopolis-atlas (+https://cartopolis.org)";

/// The local frame a kit is written in: origin at the outline's centre, `+y`
/// along the outline's longer side of its tightest rectangle — so a tower
/// square to its street has square faces in the kit.
pub struct Frame {
    pub plane: Plane,
    pub rotation_deg: f64,
    /// The outline, local metres.
    pub outline: Vec<[f64; 2]>,
    /// The tightest rectangle's sides, metres (along x, along y).
    pub size: [f64; 2],
}

/// The frame of an outline, `(lat, lng)`.
#[must_use]
pub fn frame(outline: &[(f64, f64)]) -> Frame {
    let n = outline.len() as f64;
    let lat = outline.iter().map(|p| p.0).sum::<f64>() / n;
    let lng = outline.iter().map(|p| p.1).sum::<f64>() / n;
    let plane = Plane::at(lat, lng);
    let en: Vec<[f64; 2]> = outline.iter().map(|p| plane.to_en(p.0, p.1)).collect();
    // The tightest rectangle has a side on one of the outline's edges.
    let mut best = (f64::INFINITY, 0.0, [0.0, 0.0]);
    for i in 0..en.len() {
        let (a, b) = (en[i], en[(i + 1) % en.len()]);
        let ang = (b[1] - a[1]).atan2(b[0] - a[0]);
        if (b[0] - a[0]).hypot(b[1] - a[1]) < 0.5 {
            continue;
        }
        let (s, c) = ang.sin_cos();
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for q in &en {
            let u = [q[0] * c + q[1] * s, -q[0] * s + q[1] * c];
            for k in 0..2 {
                lo[k] = lo[k].min(u[k]);
                hi[k] = hi[k].max(u[k]);
            }
        }
        let size = [hi[0] - lo[0], hi[1] - lo[1]];
        if size[0] * size[1] < best.0 {
            best = (size[0] * size[1], ang, size);
        }
    }
    // `ang` is that edge's direction counter-clockwise from east. Making it
    // local +x turns the plan clockwise by `ang`, which is `Kit::to_en`'s
    // rotation of −ang; any quarter turn of that frames the rectangle as well.
    let rotation_deg = (-best.1.to_degrees()).rem_euclid(90.0);
    let r = rotation_deg.to_radians();
    let (s, c) = r.sin_cos();
    // Inverse of `Kit::to_en`.
    let outline = en
        .iter()
        .map(|q| [q[0] * c - q[1] * s, q[0] * s + q[1] * c])
        .collect::<Vec<_>>();
    let lo = outline
        .iter()
        .fold([f64::INFINITY; 2], |m, q| [m[0].min(q[0]), m[1].min(q[1])]);
    let hi = outline.iter().fold([f64::NEG_INFINITY; 2], |m, q| {
        [m[0].max(q[0]), m[1].max(q[1])]
    });
    Frame {
        plane,
        rotation_deg,
        outline,
        size: [hi[0] - lo[0], hi[1] - lo[1]],
    }
}

/// The ground at a building: the median of the bare-earth model in a ring
/// around it (inside, the building is cut out).
pub fn ground(dtm: &Surface, outline_en: &[[f64; 2]]) -> Option<f64> {
    let lo = outline_en
        .iter()
        .fold([f64::INFINITY; 2], |m, q| [m[0].min(q[0]), m[1].min(q[1])]);
    let hi = outline_en.iter().fold([f64::NEG_INFINITY; 2], |m, q| {
        [m[0].max(q[0]), m[1].max(q[1])]
    });
    let mut v = Vec::new();
    let mut y = lo[1] - 10.0;
    while y <= hi[1] + 10.0 {
        let mut x = lo[0] - 10.0;
        while x <= hi[0] + 10.0 {
            if let Some(h) = dtm.at([x, y]) {
                v.push(f64::from(h));
            }
            x += 1.0;
        }
        y += 1.0;
    }
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied()
}

/// Fetch both of AHN's models round a plane's origin.
pub fn surfaces(plane: Plane, half: f64) -> Result<(Surface, Surface), String> {
    let dsm = Surface::fetch(&Pdok::surface() as &dyn HeightSource, plane, half)?;
    let dtm = Surface::fetch(&Pdok::new() as &dyn HeightSource, plane, half)?;
    Ok((dsm, dtm))
}

pub(crate) fn get(url: &str) -> Result<Vec<u8>, String> {
    let r = ureq::get(url)
        .set("User-Agent", UA)
        .timeout(std::time::Duration::from_secs(60))
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    let mut b = Vec::new();
    std::io::Read::read_to_end(&mut r.into_reader(), &mut b).map_err(|e| e.to_string())?;
    Ok(b)
}

/// Compass bearing from one `(lat, lng)` to another, degrees.
fn bearing(from: (f64, f64), to: (f64, f64)) -> f64 {
    let p = Plane::at(from.0, from.1);
    let d = p.to_en(to.0, to.1);
    d[0].atan2(d[1]).to_degrees().rem_euclid(360.0)
}

/// Write the folder. `keywords` pick the photographs whose titles name the
/// building, out of everything Commons has within 300 m.
pub fn gather(pand: &str, keywords: &[String], out: &Path) -> Result<String, String> {
    std::fs::create_dir_all(out.join("photos")).map_err(|e| e.to_string())?;
    let outline = models::bag_outline(pand)?;
    let f = frame(&outline);
    let half = (f.size[0].max(f.size[1]) / 2.0 + 30.0).ceil();
    let (dsm, dtm) = surfaces(f.plane, half)?;
    let to_en = |q: [f64; 2]| {
        let (s, c) = f.rotation_deg.to_radians().sin_cos();
        [q[0] * c + q[1] * s, -q[0] * s + q[1] * c]
    };
    let outline_en: Vec<[f64; 2]> = f.outline.iter().map(|&q| to_en(q)).collect();
    let ground = ground(&dtm, &outline_en).ok_or("no bare-earth heights round the building")?;

    let mut md = String::new();
    let _ = writeln!(md, "# Building {pand}\n");
    let _ = writeln!(
        md,
        "Local frame: origin ({:.7}, {:.7}), +y at bearing {:.1}°. Tightest rectangle \
         {:.1} × {:.1} m. Ground {:.2} m NAP (AHN bare earth, median round the outline).\n",
        f.plane.lat, f.plane.lng, f.rotation_deg, f.size[0], f.size[1], ground
    );
    let _ = writeln!(md, "## Register outline, local metres\n\n```");
    for q in &f.outline {
        let _ = writeln!(md, "{:.2}, {:.2}", q[0], q[1]);
    }
    let _ = writeln!(md, "```\n");
    if let Ok(b) = get(&format!(
        "https://api.3dbag.nl/collections/pand/items/NL.IMBAG.Pand.{pand}"
    )) {
        if let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&b) {
            let _ = writeln!(md, "## 3DBAG\n");
            if let Some(objs) = doc["feature"]["CityObjects"].as_object() {
                for (_, o) in objs.iter().filter(|(_, o)| o["type"] == "Building") {
                    if let Some(a) = o["attributes"].as_object() {
                        for (k, v) in a.iter().filter(|(k, _)| {
                            k.starts_with("b3_h_") || k.as_str() == "b3_val3dity_lod22"
                        }) {
                            let _ = writeln!(md, "- {k}: {v}");
                        }
                    }
                }
            }
            let _ = writeln!(md);
        }
    }

    // The laser's map in the local frame, a metre a cell, heights above ground.
    let _ = writeln!(
        md,
        "## Laser surface (AHN), metres above ground, 1 m cells, local frame\n\n\
         Rows north to south in local y (top row is +y), columns west to east in local x; \
         the register outline above says which cells are the building. Plateaus are stage tops.\n\n```"
    );
    let m = 6.0;
    let (x0, x1) = (-f.size[0] / 2.0 - m, f.size[0] / 2.0 + m);
    let (y0, y1) = (-f.size[1] / 2.0 - m, f.size[1] / 2.0 + m);
    let lo = f
        .outline
        .iter()
        .fold([f64::INFINITY; 2], |a, q| [a[0].min(q[0]), a[1].min(q[1])]);
    let hi = f.outline.iter().fold([f64::NEG_INFINITY; 2], |a, q| {
        [a[0].max(q[0]), a[1].max(q[1])]
    });
    let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
    let mut hist = std::collections::BTreeMap::<i64, usize>::new();
    let mut y = y1;
    let _ = write!(md, "      ");
    let mut x = x0;
    while x <= x1 {
        let _ = write!(md, "{:>4}", (x + mid[0]).round() as i64);
        x += 1.0;
    }
    let _ = writeln!(md);
    while y >= y0 {
        let _ = write!(md, "{:>5} ", (y + mid[1]).round() as i64);
        let mut x = x0;
        while x <= x1 {
            let q = [x + mid[0], y + mid[1]];
            match dsm.at(to_en(q)) {
                Some(h) => {
                    let above = f64::from(h) - ground;
                    let _ = write!(md, "{:>4}", above.round() as i64);
                    if super::score::inside(&f.outline, q) {
                        *hist.entry(above.round() as i64).or_default() += 1;
                    }
                }
                None => {
                    let _ = write!(md, "   .");
                }
            }
            x += 1.0;
        }
        let _ = writeln!(md);
        y -= 1.0;
    }
    let _ = writeln!(
        md,
        "```\n\n## Heights inside the outline, m above ground: cells per metre\n\n\
         A tall bar is a flat roof or a ledge at that height.\n\n```"
    );
    let most = hist.values().copied().max().unwrap_or(1);
    for (h, n) in &hist {
        let _ = writeln!(md, "{h:>4} {n:>4} {}", "#".repeat(n * 50 / most));
    }
    let _ = writeln!(md, "```");

    // How wide the building is at each height: the extent, in the local frame,
    // of the half-metre cells inside the outline that reach it. A stage's width
    // is the extent just under the ledge it stops at.
    let _ = writeln!(
        md,
        "\n## Extent by height, local metres (half-metre cells inside the outline)\n\n\
         | ≥ m | cells | x from | x to | width x | y from | y to | width y |\n\
         |---|---|---|---|---|---|---|---|"
    );
    let mut cells: Vec<([f64; 2], f64)> = Vec::new();
    let mut y = lo[1];
    while y <= hi[1] {
        let mut x = lo[0];
        while x <= hi[0] {
            if super::score::inside(&f.outline, [x, y]) {
                if let Some(h) = dsm.at(to_en([x, y])) {
                    cells.push(([x, y], f64::from(h) - ground));
                }
            }
            x += 0.5;
        }
        y += 0.5;
    }
    let top = cells.iter().map(|c| c.1).fold(0.0, f64::max);
    let mut h = 2.0;
    while h <= top {
        let at: Vec<[f64; 2]> = cells.iter().filter(|c| c.1 >= h).map(|c| c.0).collect();
        let x0 = at.iter().map(|q| q[0]).fold(f64::INFINITY, f64::min);
        let x1 = at.iter().map(|q| q[0]).fold(f64::NEG_INFINITY, f64::max);
        let y0 = at.iter().map(|q| q[1]).fold(f64::INFINITY, f64::min);
        let y1 = at.iter().map(|q| q[1]).fold(f64::NEG_INFINITY, f64::max);
        let _ = writeln!(
            md,
            "| {h:.0} | {} | {x0:.1} | {x1:.1} | {:.1} | {y0:.1} | {y1:.1} | {:.1} |",
            at.len(),
            x1 - x0 + 0.5,
            y1 - y0 + 0.5
        );
        h += 2.0;
    }

    // The plan at a few heights, as a kit's `layers` takes it: the convex hull
    // of the cells reaching each height, with corners under 0.75 m dropped.
    // A hull bridges a courtyard or a notch; the map above shows those.
    let _ = writeln!(
        md,
        "\n## Plan by height, local metres (convex hull, for `layers`)\n"
    );
    for frac in [0.05, 0.3, 0.55, 0.8, 0.95] {
        let h = (top * frac).round();
        let at: Vec<[f64; 2]> = cells.iter().filter(|c| c.1 >= h).map(|c| c.0).collect();
        let ring = simplify(&hull(&at), 0.75);
        let pts: Vec<String> = ring
            .iter()
            .map(|q| format!("[{:.1}, {:.1}]", q[0], q[1]))
            .collect();
        let _ = writeln!(md, "- ≥ {h:.0} m: [{}]", pts.join(", "));
    }
    std::fs::write(out.join("facts.md"), &md).map_err(|e| e.to_string())?;

    // The aerial photograph, 8 cm, square on the building plus 30 m.
    let c = cartopolis_geo::ahn::wgs84_to_rd(f.plane.lat, f.plane.lng);
    let bbox = format!(
        "{},{},{},{}",
        c[0] - half,
        c[1] - half,
        c[0] + half,
        c[1] + half
    );
    let url = format!(
        "https://service.pdok.nl/hwh/luchtfotorgb/wms/v1_0?{}",
        crate::pipeline::http::query(&[
            ("service", "WMS"),
            ("version", "1.3.0"),
            ("request", "GetMap"),
            ("layers", "Actueel_orthoHR"),
            ("styles", ""),
            ("crs", "EPSG:28992"),
            ("bbox", &bbox),
            ("width", "1600"),
            ("height", "1600"),
            ("format", "image/jpeg"),
        ])
    );
    let aerial = get(&url)
        .and_then(|b| std::fs::write(out.join("aerial.jpg"), b).map_err(|e| e.to_string()));

    let photos = photos(f.plane, keywords, out).unwrap_or_else(|e| format!("Commons: {e}\n"));
    std::fs::write(out.join("photos.md"), &photos).map_err(|e| e.to_string())?;

    // A first kit: the register's outline as one block at 3DBAG's 70th percentile.
    let kit = serde_json::json!({
        "name": keywords.first().cloned().unwrap_or_else(|| format!("Building {pand}")),
        "pand": pand,
        "origin": [f.plane.lat, f.plane.lng],
        "rotation_deg": (f.rotation_deg * 10.0).round() / 10.0,
        "sources": ["outline: BAG", "heights: AHN surface model"],
        "materials": {"brick": {"rgb": [140, 80, 60]}, "dark": {"rgb": [30, 30, 32]}},
        "parts": [{
            "name": "block", "material": "brick",
            "shape": {"polygon": {"points": f.outline.iter()
                .map(|q| [(q[0] * 100.0).round() / 100.0, (q[1] * 100.0).round() / 100.0])
                .collect::<Vec<_>>()}},
            "rings": [[0, 1], [10, 1]]
        }]
    });
    std::fs::write(
        out.join("kit.json"),
        serde_json::to_string_pretty(&kit).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "wrote {}: facts.md, photos.md, kit.json{}",
        out.display(),
        if aerial.is_ok() {
            ", aerial.jpg"
        } else {
            " (no aerial photograph)"
        }
    ))
}

/// Commons photographs within 300 m whose title names the building.
fn photos(plane: Plane, keywords: &[String], out: &Path) -> Result<String, String> {
    let api = |params: &[(&str, &str)]| -> Result<serde_json::Value, String> {
        let url = format!(
            "https://commons.wikimedia.org/w/api.php?{}",
            crate::pipeline::http::query(params)
        );
        serde_json::from_slice(&get(&url)?).map_err(|e| e.to_string())
    };
    // Where every file near the building was taken; the list is one answer.
    let near = api(&[
        ("action", "query"),
        ("format", "json"),
        ("list", "geosearch"),
        ("gscoord", &format!("{}|{}", plane.lat, plane.lng)),
        ("gsradius", "300"),
        ("gslimit", "500"),
        ("gsnamespace", "6"),
    ])?;
    let wanted: Vec<(String, f64, f64)> = near["query"]["geosearch"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|g| {
            let title = g["title"].as_str()?;
            let low = title.to_lowercase();
            if !keywords.iter().any(|k| low.contains(&k.to_lowercase())) {
                return None;
            }
            Some((title.to_string(), g["lat"].as_f64()?, g["lon"].as_f64()?))
        })
        .collect();
    // Their licences and thumbnails, fifty titles a request (the API's limit).
    let mut info = std::collections::HashMap::new();
    for batch in wanted.chunks(50) {
        let titles = batch
            .iter()
            .map(|w| w.0.as_str())
            .collect::<Vec<_>>()
            .join("|");
        let doc = api(&[
            ("action", "query"),
            ("format", "json"),
            ("titles", &titles),
            ("prop", "imageinfo"),
            ("iiprop", "url|extmetadata"),
            ("iiurlwidth", "1280"),
            (
                "iiextmetadatafilter",
                "LicenseShortName|Artist|DateTimeOriginal",
            ),
        ])?;
        for p in doc["query"]["pages"].as_object().into_iter().flatten() {
            if let Some(t) = p.1["title"].as_str() {
                info.insert(t.to_string(), p.1["imageinfo"][0].clone());
            }
        }
    }
    let mut rows = Vec::new();
    for (title, lat, lng) in &wanted {
        let (title, lat, lng) = (title.as_str(), *lat, *lng);
        let Some(ii) = info.get(title) else { continue };
        let meta = |k: &str| {
            ii["extmetadata"][k]["value"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        let licence = meta("LicenseShortName");
        if !(licence.starts_with("CC") || licence.contains("Public domain")) {
            continue;
        }
        let d = plane.to_en(lat, lng);
        let dist = d[0].hypot(d[1]);
        // The direction from the photographer to the building: the face seen
        // is the one turned toward the photographer.
        let look = bearing((lat, lng), (plane.lat, plane.lng));
        rows.push((
            dist,
            look,
            (lat, lng),
            title.to_string(),
            licence,
            meta("Artist"),
            meta("DateTimeOriginal"),
            ii["thumburl"].as_str().unwrap_or_default().to_string(),
            ii["descriptionurl"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    rows.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut md = String::from(
        "# Photographs (Wikimedia Commons, within 300 m)\n\n\
         `looks` is the compass bearing from where it was taken to the building: \
         a photo looking north (≈0°) shows the south face. Positions are the \
         uploader's, often a few metres off. Credit every photograph a kit uses \
         in its `sources`. People in a photograph are never what it is used for.\n\n\
         | file | looks | from m | taken at (`--from`) | taken | licence | author |\n|---|---|---|---|---|---|---|\n",
    );
    for (k, (dist, look, at, title, lic, artist, date, thumb, page)) in
        rows.iter().enumerate().take(40)
    {
        let name = format!("{k:02}.jpg");
        let ok = !thumb.is_empty()
            && get(thumb)
                .and_then(|b| {
                    std::fs::write(out.join("photos").join(&name), b).map_err(|e| e.to_string())
                })
                .is_ok();
        let artist = strip_tags(artist);
        let _ = writeln!(
            md,
            "| {} [{}]({page}) | {look:.0}° | {dist:.0} | {:.6},{:.6} | {} | {lic} | {} |",
            if ok {
                format!("`photos/{name}`")
            } else {
                "—".into()
            },
            title.trim_start_matches("File:"),
            at.0,
            at.1,
            date.chars().take(10).collect::<String>(),
            artist.chars().take(40).collect::<String>()
        );
    }
    Ok(md)
}

/// The convex hull of points, counter-clockwise (Andrew's monotone chain).
fn hull(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut p = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut lower: Vec<[f64; 2]> = Vec::new();
    for &q in &p {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], q) <= 0.0 {
            lower.pop();
        }
        lower.push(q);
    }
    let mut upper: Vec<[f64; 2]> = Vec::new();
    for &q in p.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], q) <= 0.0 {
            upper.pop();
        }
        upper.push(q);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// Drop the corners of a ring that stand less than `tol` off the line
/// between their neighbours, the least first, until none does.
fn simplify(ring: &[[f64; 2]], tol: f64) -> Vec<[f64; 2]> {
    let mut r = ring.to_vec();
    while r.len() > 3 {
        let n = r.len();
        let off = |i: usize| {
            let (a, p, b) = (r[(i + n - 1) % n], r[i], r[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = dx.hypot(dy).max(1e-9);
            ((p[0] - a[0]) * dy - (p[1] - a[1]) * dx).abs() / l
        };
        let (i, d) = (0..n)
            .map(|i| (i, off(i)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((0, f64::INFINITY));
        if d >= tol {
            break;
        }
        r.remove(i);
    }
    r
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A square turned 30° comes back square, at a rotation the kit's own
    /// `to_en` undoes.
    #[test]
    fn a_turned_square_is_framed_square() {
        let p = Plane::at(53.2, 6.5);
        let r = 30f64.to_radians();
        let (s, c) = r.sin_cos();
        let local = [[-5.0, -5.0], [5.0, -5.0], [5.0, 5.0], [-5.0, 5.0]];
        // Kit::to_en with rotation 30°.
        let en: Vec<[f64; 2]> = local
            .iter()
            .map(|q| [q[0] * c + q[1] * s, -q[0] * s + q[1] * c])
            .collect();
        let ll: Vec<(f64, f64)> = en.iter().map(|q| p.to_lat_lng(*q)).collect();
        let f = frame(&ll);
        assert!(
            (f.size[0] - 10.0).abs() < 1e-3 && (f.size[1] - 10.0).abs() < 1e-3,
            "{:?}",
            f.size
        );
        // A square's frame is good at 30° or at 30° ± 90°: either way its sides
        // land on the axes.
        let turned = (f.rotation_deg - 30.0).rem_euclid(90.0);
        assert!(
            turned < 1e-3 || (90.0 - turned) < 1e-3,
            "{}",
            f.rotation_deg
        );
        for q in &f.outline {
            assert!(
                (q[0].abs() - 5.0).abs() < 1e-3 && (q[1].abs() - 5.0).abs() < 1e-3,
                "{q:?}"
            );
        }
    }
}
