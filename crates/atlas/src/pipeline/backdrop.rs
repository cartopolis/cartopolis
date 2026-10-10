//! **Backdrops**: a city's streets as one-colour line work, for the web UI to
//! put behind its glass (`crates/atlas/web/src/css/depth.css`).
//!
//! A pane of glass over an even colour is just a slightly different even
//! colour; what a blur needs is structure, and for a map product the obvious
//! structure is a map. So the page behind every pane is a city's centre —
//! Groningen's, with its canal ring, by default — drawn from OpenStreetMap
//! geometry through our own Overpass (`tall::overpass_url`).
//!
//! **Two layers, both masks.** Everything is drawn in plain black because the
//! page uses each file as a `mask` and paints the colour itself from a theme
//! token, which is what lets one drawing serve the light and the dark theme.
//!
//! - [`Layer::Streets`]: streets, paths, rail and water, the line weight by the
//!   road's class.
//! - [`Layer::Pulses`]: the main roads only, dashed into short streaks that
//!   travel along them — traffic, as faint light. Animated by CSS *inside* the
//!   SVG, which browsers run for an SVG used as an image (scripts they do not).
//!   The page hides the layer for anyone who asks for reduced motion.
//!
//! The web UI serves a committed copy rather than calling this on every page
//! load: a page must never wait on a live Overpass query. This route is how that
//! copy is made and remade — `GET /api/pipeline/backdrop.svg?bbox=s,w,n,e
//! &layer=streets` — and how one for another city would be.

use std::fmt::Write as _;
use std::time::Duration;

use serde_json::Value;

use super::{http, tall};

/// Groningen's centre: the canal ring and a kilometre or so around it. What the
/// committed backdrop (`crates/atlas/web/src/img/groningen*.svg`) was made from.
pub const GRONINGEN: [f64; 4] = [53.2045, 6.5420, 53.2340, 6.5920];

/// The drawing's width in SVG units; the height follows the bbox's aspect.
const WIDTH: f64 = 2000.0;

/// Douglas-Peucker tolerance in SVG units (~1.3 m at the default bbox): below
/// what a full-screen background can show, and most of a street's vertices.
const SIMPLIFY: f64 = 0.8;

/// The largest area this will ask Overpass for, in square kilometres. A
/// backdrop is a city centre; a province would be megabytes of line work
/// nobody can see and a long query on a server other people share.
pub const MAX_KM2: f64 = 25.0;

const TIMEOUT: Duration = Duration::from_secs(120);

/// Which drawing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Streets,
    Pulses,
}

impl Layer {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "streets" => Some(Self::Streets),
            "pulses" => Some(Self::Pulses),
            _ => None,
        }
    }
}

/// A `[south, west, north, east]` box in WGS84, or why it is not one.
pub fn validate(bbox: &[f64]) -> Result<[f64; 4], String> {
    let [s, w, n, e] = match bbox {
        [a, b, c, d] if bbox.iter().all(|v| v.is_finite()) => [*a, *b, *c, *d],
        _ => return Err("A bbox is four numbers: south,west,north,east in degrees.".into()),
    };
    if n <= s || e <= w {
        return Err("The bbox is inside out — north must exceed south, and east west.".into());
    }
    if !(-85.0..=85.0).contains(&s) || !(-85.0..=85.0).contains(&n) {
        return Err("That bbox is off the map.".into());
    }
    let km2 = km2([s, w, n, e]);
    if km2 > MAX_KM2 {
        return Err(format!(
            "That bbox is {km2:.0} km²; a backdrop is a city centre, at most {MAX_KM2:.0} km²."
        ));
    }
    Ok([s, w, n, e])
}

fn km2([s, w, n, e]: [f64; 4]) -> f64 {
    let k = ((s + n) / 2.0).to_radians().cos();
    (n - s) * 111.32 * (e - w) * 111.32 * k
}

/// The Overpass query for a box: every way the drawing can use, with geometry.
pub fn query([s, w, n, e]: [f64; 4]) -> String {
    let b = format!("({s},{w},{n},{e})");
    format!(
        "[out:json][timeout:90];(\
         way[\"highway\"~\"^(motorway|trunk|primary|secondary|tertiary|unclassified|\
         residential|living_street|pedestrian|cycleway|service|footway|path)$\"]{b};\
         way[\"waterway\"~\"canal|river\"]{b};\
         way[\"natural\"=\"water\"]{b};\
         way[\"railway\"=\"rail\"]{b};);out geom tags;"
    )
}

/// Ask our Overpass for a box and draw it.
pub fn fetch(bbox: [f64; 4], layer: Layer) -> Result<String, String> {
    draw(&fetch_doc(bbox)?, bbox, layer)
}

/// Our Overpass's answer for a box. Retried, because the two layers are the
/// same query and Overpass refuses one it is still answering for somebody else
/// (`duplicate_query`, an HTML page) — asking for both layers back to back is
/// exactly that.
pub fn fetch_doc(bbox: [f64; 4]) -> Result<Value, String> {
    const ATTEMPTS: u32 = 4;
    let url = format!(
        "{}?{}",
        tall::overpass_url(),
        http::query(&[("data", &query(bbox))])
    );
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        match http::fetch(&url, TIMEOUT).and_then(|body| tall::validate(&body)) {
            Ok(doc) => return Ok(doc),
            Err(e) => last = e,
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(5 << attempt));
        }
    }
    Err(last)
}

/// Stroke width of a way in SVG units, by what it is; `None` for a way the
/// layer leaves out. Water areas are filled, not stroked, and are handled apart.
fn stroke(tags: &Value, layer: Layer) -> Option<f64> {
    let tag = |k: &str| tags.get(k).and_then(Value::as_str);
    if let Some(class) = tag("highway") {
        return match (layer, class) {
            (_, "motorway" | "trunk") => Some(9.0),
            (_, "primary") => Some(8.0),
            (_, "secondary") => Some(7.0),
            (_, "tertiary") => Some(6.0),
            (Layer::Pulses, _) => None,
            (_, "unclassified" | "residential") => Some(4.5),
            (_, "living_street" | "pedestrian") => Some(4.0),
            (_, "cycleway") => Some(2.6),
            (_, "service") => Some(2.2),
            (_, "footway") => Some(1.6),
            (_, "path") => Some(1.4),
            _ => None,
        };
    }
    if layer == Layer::Pulses {
        return None;
    }
    if tag("waterway").is_some() {
        return Some(16.0);
    }
    if tag("railway") == Some("rail") {
        return Some(2.4);
    }
    None
}

/// Douglas-Peucker on a polyline.
fn simplify(pts: &[[f64; 2]], eps: f64) -> Vec<[f64; 2]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx.hypot(dy).max(f64::EPSILON);
    let (mut far, mut at) = (0.0, 0);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = (dy * p[0] - dx * p[1] + b[0] * a[1] - b[1] * a[0]).abs() / len;
        if d > far {
            far = d;
            at = i;
        }
    }
    if far <= eps {
        return vec![a, b];
    }
    let mut left = simplify(&pts[..=at], eps);
    left.pop();
    left.extend(simplify(&pts[at..], eps));
    left
}

/// The whole SVG for one layer of an Overpass answer.
pub fn draw(doc: &Value, bbox: [f64; 4], layer: Layer) -> Result<String, String> {
    let [s, w, n, e] = bbox;
    let k = ((s + n) / 2.0).to_radians().cos();
    let height = (WIDTH * (n - s) / ((e - w) * k)).round();
    let project = |lon: f64, lat: f64| [(lon - w) / (e - w) * WIDTH, (n - lat) / (n - s) * height];

    let elements = doc
        .get("elements")
        .and_then(Value::as_array)
        .ok_or("no elements")?;
    // Paths grouped by stroke width (as integer hundredths, to key a map), so
    // the file carries one `<path>` per weight rather than one per street.
    let mut strokes: std::collections::BTreeMap<u32, String> = Default::default();
    let mut water = String::new();
    for el in elements {
        let Some(geometry) = el.get("geometry").and_then(Value::as_array) else {
            continue;
        };
        let pts: Vec<[f64; 2]> = geometry
            .iter()
            .filter_map(|g| Some(project(g.get("lon")?.as_f64()?, g.get("lat")?.as_f64()?)))
            .collect();
        if pts.len() < 2 {
            continue;
        }
        let tags = el.get("tags").cloned().unwrap_or(Value::Null);
        let filled = tags.get("natural").and_then(Value::as_str) == Some("water");
        let target = if filled {
            if layer == Layer::Pulses {
                continue;
            }
            &mut water
        } else {
            match stroke(&tags, layer) {
                Some(width) => strokes.entry((width * 100.0) as u32).or_default(),
                None => continue,
            }
        };
        for (i, p) in simplify(&pts, SIMPLIFY).iter().enumerate() {
            let _ = write!(
                target,
                "{}{:.0} {:.0}",
                if i == 0 { 'M' } else { 'L' },
                p[0],
                p[1]
            );
        }
        if filled {
            target.push('Z');
        }
    }
    if strokes.is_empty() && water.is_empty() {
        return Err("Nothing to draw: that box has no streets in OpenStreetMap.".into());
    }

    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {WIDTH} {height}\">\n\
         <!-- {what} of {s},{w} to {n},{e}, from OpenStreetMap (© OpenStreetMap \
         contributors, ODbL) through atlas's backdrop route \
         (crates/atlas/src/pipeline/backdrop.rs). One colour: the page uses it as \
         a mask and paints it from a theme token. -->\n",
        what = match layer {
            Layer::Streets => "Streets, paths, rail and water",
            Layer::Pulses => "Traffic on the main roads",
        },
    );
    if layer == Layer::Pulses {
        svg.push_str(PULSE_STYLE);
    }
    if !water.is_empty() {
        let _ = writeln!(svg, "<path fill=\"#000\" d=\"{water}\"/>");
    }
    for (i, (width, d)) in strokes.iter().enumerate() {
        let width = f64::from(*width) / 100.0;
        match layer {
            Layer::Streets => {
                let _ = writeln!(
                    svg,
                    "<path fill=\"none\" stroke=\"#000\" stroke-width=\"{width}\" \
                     stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"{d}\"/>"
                );
            }
            // Each road class takes a different speed and direction, so the
            // streaks on neighbouring roads never move in step.
            Layer::Pulses => {
                let _ = writeln!(svg, "<path class=\"p{}\" d=\"{d}\"/>", i % 3);
            }
        }
    }
    svg.push_str("</svg>\n");
    Ok(svg)
}

/// The traffic. A streak is an 18-unit dash with a 300-unit gap; the offset
/// runs through a whole number of periods (318 × 10) so each loop joins the next
/// without a jump. A dash pattern starts at its own path's first vertex, so the
/// streaks on different roads are out of phase without any per-path state.
const PULSE_STYLE: &str = "<style>\
path{fill:none;stroke:#000;stroke-width:6;stroke-linecap:round;\
stroke-dasharray:18 300;animation:drive 92s linear infinite}\
.p1{animation-duration:122s;animation-direction:reverse}\
.p2{animation-duration:76s}\
@keyframes drive{to{stroke-dashoffset:-3180}}\
@media (prefers-reduced-motion:reduce){path{animation:none}}\
</style>\n";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn way(tags: Value, coords: &[(f64, f64)]) -> Value {
        json!({
            "type": "way",
            "tags": tags,
            "geometry": coords.iter().map(|(lat, lon)| json!({"lat": lat, "lon": lon})).collect::<Vec<_>>(),
        })
    }

    fn doc() -> Value {
        json!({"elements": [
            way(json!({"highway": "primary"}), &[(53.21, 6.55), (53.22, 6.56)]),
            way(json!({"highway": "footway"}), &[(53.215, 6.55), (53.215, 6.58)]),
            way(json!({"natural": "water"}), &[(53.21, 6.57), (53.212, 6.57), (53.212, 6.575), (53.21, 6.57)]),
            way(json!({"amenity": "bench"}), &[(53.21, 6.55), (53.211, 6.551)]),
        ]})
    }

    #[test]
    fn streets_draw_every_class_by_weight_and_fill_the_water() {
        let svg = draw(&doc(), GRONINGEN, Layer::Streets).unwrap();
        assert!(svg.contains("stroke-width=\"8\""), "primary");
        assert!(svg.contains("stroke-width=\"1.6\""), "footway");
        assert!(svg.contains("<path fill=\"#000\" d=\"M"), "water filled");
        assert!(svg.contains('Z'));
        assert!(!svg.contains("<style>"), "the streets do not move");
    }

    #[test]
    fn pulses_keep_the_main_roads_only_and_animate() {
        let svg = draw(&doc(), GRONINGEN, Layer::Pulses).unwrap();
        assert_eq!(
            svg.matches("<path class=").count(),
            1,
            "the primary road alone"
        );
        assert!(svg.contains("@keyframes drive"));
        assert!(svg.contains("prefers-reduced-motion"));
        assert!(
            !svg.contains("fill=\"#000\""),
            "no water in the traffic layer"
        );
    }

    /// The loop must join itself: the offset a cycle travels is a whole number
    /// of dash periods, or every streak jumps back once per cycle.
    #[test]
    fn the_pulse_loop_is_seamless() {
        let period: f64 = 18.0 + 300.0;
        assert!(PULSE_STYLE.contains("stroke-dasharray:18 300"));
        assert!(PULSE_STYLE.contains(&format!("stroke-dashoffset:-{}", period * 10.0)));
    }

    #[test]
    fn a_box_with_no_streets_is_refused() {
        let empty = json!({"elements": []});
        assert!(draw(&empty, GRONINGEN, Layer::Streets).is_err());
    }

    #[test]
    fn the_drawing_keeps_the_box_aspect() {
        let svg = draw(&doc(), GRONINGEN, Layer::Streets).unwrap();
        // 3.3 km wide by 3.3 km tall at Groningen's latitude: near square.
        assert!(svg.contains("viewBox=\"0 0 2000 19"), "{}", &svg[..120]);
    }

    #[test]
    fn simplify_drops_collinear_points_and_keeps_corners() {
        let line = [[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [2.0, 5.0]];
        assert_eq!(
            simplify(&line, 0.5),
            vec![[0.0, 0.0], [2.0, 0.0], [2.0, 5.0]]
        );
    }

    #[test]
    fn boxes_are_checked_before_overpass_is_asked() {
        assert!(validate(&GRONINGEN).is_ok());
        assert!(validate(&[53.2, 6.5, 53.1, 6.6]).is_err(), "inside out");
        assert!(validate(&[52.0, 4.0, 53.5, 7.0]).is_err(), "a province");
        assert!(validate(&[1.0, 2.0, 3.0]).is_err(), "three numbers");
        assert!(validate(&[f64::NAN, 6.5, 53.3, 6.6]).is_err());
    }

    /// Remakes the web UI's committed backdrop from Overpass. Off by default —
    /// it is a network fetch, not a test — and run by hand:
    ///
    /// ```text
    /// cargo test -p cartopolis_atlas writes_the_web_backdrop -- --ignored
    /// ```
    #[test]
    #[ignore = "fetches from Overpass and rewrites the web UI's committed files"]
    fn writes_the_web_backdrop() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/img");
        // One query for both layers: they are drawn from the same answer.
        let doc = fetch_doc(GRONINGEN).unwrap();
        for (layer, name) in [
            (Layer::Streets, "groningen.svg"),
            (Layer::Pulses, "groningen-pulses.svg"),
        ] {
            let svg = draw(&doc, GRONINGEN, layer).unwrap();
            std::fs::write(dir.join(name), &svg).unwrap();
            println!("wrote {name} ({} bytes)", svg.len());
        }
    }

    #[test]
    fn layers_parse_by_name() {
        assert_eq!(Layer::parse("streets"), Some(Layer::Streets));
        assert_eq!(Layer::parse("pulses"), Some(Layer::Pulses));
        assert_eq!(Layer::parse("roads"), None);
    }
}
