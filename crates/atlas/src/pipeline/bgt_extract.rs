//! The BGT as **one extract** from PDOK's download API, rather than as tens of
//! thousands of feature-API pages.
//!
//! Measured 2026-09-24 over Groningen (9 × 9 km, seven object types): paging the
//! OGC API one request at a time took ~13 minutes of a ~15-minute import, at 1–5 s
//! a page and ~12 MB a `wegdeel` page. The extract is built server-side in ~16 s
//! and downloads as a 65 MB zip in ~5 s. Parallel paging was measured too and
//! bought 1.5×, which is not the same order of thing.
//!
//! Three properties of the extract shape this module, and none is obvious from the
//! API description:
//!
//! - **It is cut by tile, not by the geofilter.** A 1.5 km square came back as the
//!   whole 4 km tile around it, five times the features. So every feature is
//!   tested here against the WGS84 envelope of the area — the same test the OGC
//!   API's `bbox` parameter applied, which keeps each cell's contents what they
//!   were.
//! - **It carries every historical version**, as the OGC API does, under
//!   `objectEindTijd` / `eindRegistratie`, two thirds of all features. They are
//!   dropped here ([`is_historical`]), before their geometry is projected,
//!   which with the tile cut is most of what parsing costs.
//! - **Its outlines contain true arcs** (`gml:Arc`), which the OGC API had already
//!   straightened. [`arc`] straightens them **as that API did**, into
//!   [`ARC_POINTS`] points whatever the arc's size: the walls are not simplified
//!   afterwards, and a wall surveyed as an area becomes the principal axis of its
//!   vertices (`bgt::centreline`), which moved by up to 23 m over a curved quay
//!   when the arcs were cut to a 2 cm tolerance instead.
//!
//! Features are handed on as GeoJSON-shaped [`Value`]s with the OGC API's
//! property names, in WGS84 through [`rd_to_lat_lng`] — the same conversion every
//! other atlas dataset uses — so everything downstream of `bgt::items` is the
//! code that was diffed against the TypeScript pipeline, unchanged.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use serde_json::{json, Map, Value};

use super::http::{download_to, get_json_live, post_json};
use super::rd::{rd_to_lat_lng, wgs84_bbox};
use super::{Ctx, Outcome, PipelineError, Progress};

const API_ORIGIN: &str = "https://api.pdok.nl";
const FULL_CUSTOM: &str = "https://api.pdok.nl/lv/bgt/download/v1_0/full/custom";
/// How long PDOK may take to build an extract. Groningen took 16 s; this is the
/// guard against a request that never completes, not an estimate.
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const POLL: Duration = Duration::from_secs(3);
/// Points a straightened arc is cut into, ends included — what the OGC API
/// answered for a 2 m arc of wall (`G0014.404f9259c2680015e053660013ac6945`,
/// 2026-09-24).
const ARC_POINTS: usize = 100;

/// GML-light element → the OGC API's property name, for the properties `bgt.rs`
/// reads. Anything else in the extract is dropped.
const PROPERTIES: [(&str, &str); 9] = [
    ("identificatie.lokaalID", "lokaal_id"),
    ("relatieveHoogteligging", "relatieve_hoogteligging"),
    ("bgt-type", "type"),
    ("plus-type", "plus_type"),
    ("bgt-functie", "functie"),
    ("bgt-fysiekVoorkomen", "fysiek_voorkomen"),
    ("plus-fysiekVoorkomen", "plus_fysiek_voorkomen"),
    ("typeOverbruggingsdeel", "type_overbruggingsdeel"),
    ("overbruggingIsBeweegbaar", "overbrugging_is_beweegbaar"),
];

/// A downloaded extract, deleted when dropped.
pub struct Extract {
    path: PathBuf,
    entries: Vec<ZipEntry>,
    /// WGS84 `[min lng, min lat, max lng, max lat]` of the area.
    envelope: [f64; 4],
    /// The area in RD, grown to cover all of `envelope`: the RD grid is turned up
    /// to ~2° against the meridians, so the envelope's corners reach past the RD
    /// rectangle by up to 3.5 % of its span.
    rd_window: [f64; 4],
}

fn rd_window([x0, y0, x1, y1]: [f64; 4]) -> [f64; 4] {
    let margin = 0.05 * (x1 - x0).max(y1 - y0) + 100.0;
    [x0 - margin, y0 - margin, x1 + margin, y1 + margin]
}

impl Drop for Extract {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Extract {
    /// Ask PDOK for `featuretypes` over the area, wait for it to be built, and
    /// download it — into atlas's data directory where there is one (the served
    /// site tree is no place for it), the system temporary directory otherwise.
    pub fn fetch(ctx: &Ctx, featuretypes: &[&str]) -> Outcome<Self> {
        let [x0, y0, x1, y1] = ctx.bbox;
        let request = json!({
            "featuretypes": featuretypes,
            "format": "gmllight",
            "geofilter": format!("POLYGON(({x0} {y0},{x1} {y0},{x1} {y1},{x0} {y1},{x0} {y0}))"),
        });
        let started = Instant::now();
        let answer = post_json(ctx, FULL_CUSTOM, &request, "bgt extract")?;
        let status_url = link(&answer, "status")
            .ok_or_else(|| fail(format!("bgt extract: no status link in {answer}")))?;
        let download = loop {
            ctx.sleep(POLL)?;
            let status = get_json_live(ctx, &status_url, "bgt extract status")?;
            let state = status["status"].as_str().unwrap_or_default();
            ctx.progress(Progress {
                phase: Some("extract".into()),
                done: status["progress"].as_u64(),
                total: Some(Some(100)),
            });
            if state == "COMPLETED" {
                break link(&status, "download")
                    .ok_or_else(|| fail(format!("bgt extract: no download link in {status}")))?;
            }
            if !matches!(state, "PENDING" | "RUNNING") {
                return Err(fail(format!("bgt extract: PDOK answered {status}")));
            }
            if started.elapsed() > BUILD_TIMEOUT {
                return Err(fail(format!(
                    "bgt extract: not built after {} minutes",
                    BUILD_TIMEOUT.as_secs() / 60
                )));
            }
        };
        ctx.log(format!(
            "extract gebouwd in {}s",
            started.elapsed().as_secs()
        ));
        let dir = ctx
            .data_dir()
            .map_or_else(|_| std::env::temp_dir(), Path::to_path_buf);
        let path = dir.join(format!(
            ".bgt-extract-{}-{}.zip",
            std::process::id(),
            started.elapsed().as_nanos()
        ));
        let mut extract = Self {
            path,
            entries: Vec::new(),
            envelope: wgs84_bbox(ctx.bbox),
            rd_window: rd_window(ctx.bbox),
        };
        let bytes = download_to(ctx, &download, &extract.path, "bgt extract")?;
        extract.entries = zip_entries(&mut File::open(&extract.path).map_err(io_fail)?)
            .map_err(|e| fail(format!("bgt extract: {e}")))?;
        ctx.log(format!(
            "extract gedownload: {} MB in {}s",
            bytes / 1_000_000,
            started.elapsed().as_secs()
        ));
        Ok(extract)
    }

    /// Every **current** feature of one object type whose extent meets the area,
    /// as a GeoJSON-shaped `{properties, geometry}`.
    pub fn features(
        &self,
        ctx: &Ctx,
        featuretype: &str,
        mut visit: impl FnMut(&Value),
    ) -> Outcome<()> {
        let name = format!("bgt_{featuretype}.gml");
        let Some(entry) = self.entries.iter().find(|e| e.name == name) else {
            ctx.log(format!("{featuretype}: not in the extract"));
            return Ok(());
        };
        let reader = entry
            .open(&self.path)
            .map_err(|e| fail(format!("{name}: {e}")))?;
        let mut n = 0usize;
        for_each_feature(reader, |feature| {
            n += 1;
            if n % 2_000 == 0 {
                ctx.check()?;
            }
            // Both before the projection, which is most of the cost: two thirds of
            // the features are old versions, and much of the tile is outside the
            // area.
            if is_historical(&feature) || !near(&feature, self.rd_window) {
                return Ok(());
            }
            if let Some(value) = feature_value(&feature) {
                if meets(&value["geometry"], self.envelope) {
                    visit(&value);
                }
            }
            Ok(())
        })
        .map_err(|e| match e {
            PipelineError::Failed(m) => fail(format!("{name}: {m}")),
            cancelled => cancelled,
        })
    }
}

fn fail(message: String) -> PipelineError {
    PipelineError::Failed(message)
}

fn io_fail(e: std::io::Error) -> PipelineError {
    fail(format!("bgt extract: {e}"))
}

/// `_links.<rel>.href`, made absolute — PDOK answers with a path.
fn link(answer: &Value, rel: &str) -> Option<String> {
    let href = answer["_links"][rel]["href"].as_str()?;
    Some(if href.starts_with("http") {
        href.to_string()
    } else {
        format!("{API_ORIGIN}{href}")
    })
}

// ---------------------------------------------------------------------------
// Zip

/// One member of the archive. The extract's members have **data descriptors**
/// (sizes after the data, not in the local header), so the sizes come from the
/// central directory, and zip64 is read because a province's `wegdeel` alone can
/// pass 4 GB uncompressed.
#[derive(Debug, PartialEq)]
struct ZipEntry {
    name: String,
    method: u16,
    compressed: u64,
    /// Offset of the entry's local header.
    header: u64,
}

fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn le64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn zip_entries(file: &mut (impl Read + Seek)) -> Result<Vec<ZipEntry>, String> {
    let len = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    // The end record is 22 bytes plus a comment of up to 64 KiB.
    let tail_len = len.min(22 + 65_535 + 20);
    let mut tail = vec![0; tail_len as usize];
    file.seek(SeekFrom::Start(len - tail_len))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut tail).map_err(|e| e.to_string())?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| le32(&tail, i) == Some(0x0605_4b50))
        .ok_or("not a zip (no end of central directory)")?;
    let truncated = "truncated zip";
    let mut count = u64::from(le16(&tail, eocd + 10).ok_or(truncated)?);
    let mut cd_size = u64::from(le32(&tail, eocd + 12).ok_or(truncated)?);
    let mut cd_offset = u64::from(le32(&tail, eocd + 16).ok_or(truncated)?);
    // A zip64 archive says so with a locator immediately before the end record.
    if eocd >= 20 && le32(&tail, eocd - 20) == Some(0x0706_4b50) {
        let at = le64(&tail, eocd - 20 + 8).ok_or(truncated)?;
        let mut rec = [0u8; 56];
        file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        file.read_exact(&mut rec).map_err(|e| e.to_string())?;
        if le32(&rec, 0) != Some(0x0606_4b50) {
            return Err("zip64 locator points at no zip64 record".into());
        }
        count = le64(&rec, 32).ok_or(truncated)?;
        cd_size = le64(&rec, 40).ok_or(truncated)?;
        cd_offset = le64(&rec, 48).ok_or(truncated)?;
    }
    let mut cd = vec![0; usize::try_from(cd_size).map_err(|e| e.to_string())?];
    file.seek(SeekFrom::Start(cd_offset))
        .map_err(|e| e.to_string())?;
    file.read_exact(&mut cd).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    let mut o = 0usize;
    for _ in 0..count {
        if le32(&cd, o) != Some(0x0201_4b50) {
            return Err("corrupt zip central directory".into());
        }
        let method = le16(&cd, o + 10).ok_or(truncated)?;
        let mut compressed = u64::from(le32(&cd, o + 20).ok_or(truncated)?);
        let uncompressed = le32(&cd, o + 24).ok_or(truncated)?;
        let name_len = usize::from(le16(&cd, o + 28).ok_or(truncated)?);
        let extra_len = usize::from(le16(&cd, o + 30).ok_or(truncated)?);
        let comment_len = usize::from(le16(&cd, o + 32).ok_or(truncated)?);
        let mut header = u64::from(le32(&cd, o + 42).ok_or(truncated)?);
        let name = cd.get(o + 46..o + 46 + name_len).ok_or(truncated)?;
        let extra = cd
            .get(o + 46 + name_len..o + 46 + name_len + extra_len)
            .ok_or(truncated)?;
        // The zip64 extra field holds, in this order, only the fields whose
        // 32-bit slot is saturated.
        let mut e = 0usize;
        while e + 4 <= extra.len() {
            let (id, size) = (
                le16(extra, e).unwrap_or(0),
                usize::from(le16(extra, e + 2).unwrap_or(0)),
            );
            if id == 0x0001 {
                let mut f = e + 4;
                if uncompressed == u32::MAX {
                    f += 8;
                }
                if compressed == u64::from(u32::MAX) {
                    compressed = le64(extra, f).ok_or(truncated)?;
                    f += 8;
                }
                if header == u64::from(u32::MAX) {
                    header = le64(extra, f).ok_or(truncated)?;
                }
            }
            e += 4 + size;
        }
        entries.push(ZipEntry {
            name: String::from_utf8_lossy(name).into_owned(),
            method,
            compressed,
            header,
        });
        o += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

impl ZipEntry {
    /// The member's contents as a stream, inflated on the fly: `wegdeel` for
    /// Groningen is 350 MB of XML and is never held whole.
    fn open(&self, path: &Path) -> Result<Box<dyn BufRead>, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut local = [0u8; 30];
        file.seek(SeekFrom::Start(self.header))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut local).map_err(|e| e.to_string())?;
        if le32(&local, 0) != Some(0x0403_4b50) {
            return Err("no local file header".into());
        }
        // The local header's own name and extra lengths, which need not match the
        // central directory's.
        let skip =
            i64::from(le16(&local, 26).unwrap_or(0)) + i64::from(le16(&local, 28).unwrap_or(0));
        file.seek(SeekFrom::Current(skip))
            .map_err(|e| e.to_string())?;
        let body = BufReader::with_capacity(1 << 20, file).take(self.compressed);
        match self.method {
            0 => Ok(Box::new(body)),
            8 => Ok(Box::new(BufReader::with_capacity(
                1 << 20,
                flate2::bufread::DeflateDecoder::new(body),
            ))),
            m => Err(format!("unsupported zip compression method {m}")),
        }
    }
}

// ---------------------------------------------------------------------------
// GML light

/// One element of a feature, with its namespace prefix dropped.
#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    /// `srsDimension`, where a coordinate list says it.
    dim: Option<usize>,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
}

fn local(name: &[u8]) -> String {
    let name = name.rsplit(|&b| b == b':').next().unwrap_or(name);
    String::from_utf8_lossy(name).into_owned()
}

/// Each `gml:featureMember`'s feature element, as a small tree. A feature is a
/// few kilobytes, so building one per feature is cheap where holding the whole
/// document would not be.
fn for_each_feature(
    reader: impl BufRead,
    mut visit: impl FnMut(Node) -> Outcome<()>,
) -> Outcome<()> {
    let mut xml = quick_xml::Reader::from_reader(reader);
    let mut buf = Vec::new();
    // Open elements below a featureMember, outermost first.
    let mut stack: Vec<Node> = Vec::new();
    let mut in_member = false;
    let xml_fail = |e: &dyn std::fmt::Display, xml: &quick_xml::Reader<_>| {
        fail(format!("XML at byte {}: {e}", xml.buffer_position()))
    };
    loop {
        let event = xml
            .read_event_into(&mut buf)
            .map_err(|e| xml_fail(&e, &xml))?;
        match event {
            Event::Start(e) => {
                let name = local(e.name().as_ref());
                if !in_member {
                    in_member = name == "featureMember";
                } else {
                    let dim = e
                        .try_get_attribute("srsDimension")
                        .ok()
                        .flatten()
                        .and_then(|a| std::str::from_utf8(&a.value).ok()?.parse().ok());
                    stack.push(Node {
                        name,
                        dim,
                        ..Node::default()
                    });
                }
            }
            Event::Empty(e) if in_member => {
                let node = Node {
                    name: local(e.name().as_ref()),
                    ..Node::default()
                };
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                }
            }
            Event::Text(t) if !stack.is_empty() => {
                let text = t.decode().map_err(|e| xml_fail(&e, &xml))?;
                stack.last_mut().expect("checked").text.push_str(&text);
            }
            Event::CData(t) if !stack.is_empty() => {
                let text = t.decode().map_err(|e| xml_fail(&e, &xml))?;
                stack.last_mut().expect("checked").text.push_str(&text);
            }
            Event::GeneralRef(r) if !stack.is_empty() => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => {
                        let name = r.decode().map_err(|e| xml_fail(&e, &xml))?;
                        quick_xml::escape::resolve_predefined_entity(&name)
                            .unwrap_or_default()
                            .to_string()
                    }
                };
                stack.last_mut().expect("checked").text.push_str(&resolved);
            }
            Event::End(_) if in_member => match stack.pop() {
                None => in_member = false,
                Some(node) => match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => visit(node)?,
                },
            },
            Event::Eof => return Ok(()),
            _ => {}
        }
        buf.clear();
    }
}

/// A feature element as `{properties, geometry}` in the OGC API's shape, or
/// `None` without a geometry it can read.
/// Removed from reality (`objectEindTijd`), or a version a later edit replaced
/// (`eindRegistratie`). A feature is current only when **both** are empty — the
/// first test alone kept seven copies of one footway (`bgt.rs` has the story).
fn is_historical(feature: &Node) -> bool {
    feature.children.iter().any(|c| {
        matches!(c.name.as_str(), "objectEindTijd" | "eindRegistratie") && !c.text.trim().is_empty()
    })
}

/// Whether any coordinate of the feature's geometry falls in an RD window — the
/// cheap, generous test that spares the projection for most of a tile.
fn near(feature: &Node, [x0, y0, x1, y1]: [f64; 4]) -> bool {
    fn walk(node: &Node, ext: &mut [f64; 4]) {
        if matches!(node.name.as_str(), "posList" | "pos") {
            for [x, y] in positions(node) {
                *ext = [ext[0].min(x), ext[1].min(y), ext[2].max(x), ext[3].max(y)];
            }
        }
        node.children.iter().for_each(|c| walk(c, ext));
    }
    let Some(geometry) = feature.child("geometrie2d") else {
        return false;
    };
    let mut ext = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    walk(geometry, &mut ext);
    ext[0] <= x1 && ext[2] >= x0 && ext[1] <= y1 && ext[3] >= y0
}

fn feature_value(feature: &Node) -> Option<Value> {
    let geometry = geometry(feature.child("geometrie2d")?.children.first()?)?;
    let mut properties = Map::new();
    for child in feature.children.iter().filter(|c| c.children.is_empty()) {
        let Some((_, ogc)) = PROPERTIES.iter().find(|(gml, _)| *gml == child.name) else {
            continue;
        };
        let text = child.text.trim();
        let value = if *ogc == "relatieve_hoogteligging" {
            text.parse::<i64>().map_or(Value::Null, Value::from)
        } else {
            Value::from(text)
        };
        properties.insert((*ogc).to_string(), value);
    }
    Some(json!({"properties": properties, "geometry": geometry}))
}

type Rd = [f64; 2];

/// A coordinate list in RD metres.
fn positions(node: &Node) -> Vec<Rd> {
    let dim = node.dim.unwrap_or(2).max(2);
    let numbers: Vec<f64> = node
        .text
        .split_ascii_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    numbers.chunks_exact(dim).map(|c| [c[0], c[1]]).collect()
}

/// Append `pts` to `out`, dropping a first point that repeats the last one — the
/// joint between two segments of one curve.
fn extend_joined(out: &mut Vec<Rd>, pts: &[Rd]) {
    let skip = usize::from(!out.is_empty() && pts.first() == out.last());
    out.extend_from_slice(&pts[skip.min(pts.len())..]);
}

/// The circular arc through `a`, `b`, `c`, as a polyline from `a` to `c` (both
/// included) of [`ARC_POINTS`] points evenly spaced by angle. Three points on
/// a line are a line.
fn arc(a: Rd, b: Rd, c: Rd) -> Vec<Rd> {
    let d = 2.0 * (a[0] * (b[1] - c[1]) + b[0] * (c[1] - a[1]) + c[0] * (a[1] - b[1]));
    let chord = (c[0] - a[0])
        .hypot(c[1] - a[1])
        .max((b[0] - a[0]).hypot(b[1] - a[1]));
    if d.abs() < 1e-9 * chord * chord.max(1.0) {
        return vec![a, b, c];
    }
    let sq = |p: Rd| p[0] * p[0] + p[1] * p[1];
    let ux = (sq(a) * (b[1] - c[1]) + sq(b) * (c[1] - a[1]) + sq(c) * (a[1] - b[1])) / d;
    let uy = (sq(a) * (c[0] - b[0]) + sq(b) * (a[0] - c[0]) + sq(c) * (b[0] - a[0])) / d;
    let r = (a[0] - ux).hypot(a[1] - uy);
    let angle = |p: Rd| (p[1] - uy).atan2(p[0] - ux);
    let tau = std::f64::consts::TAU;
    let (a0, a1, a2) = (angle(a), angle(b), angle(c));
    let ccw_to_c = (a2 - a0).rem_euclid(tau);
    let ccw_to_b = (a1 - a0).rem_euclid(tau);
    // Counter-clockwise if that way passes through `b` first; clockwise otherwise.
    let sweep = if ccw_to_b <= ccw_to_c {
        ccw_to_c
    } else {
        ccw_to_c - tau
    };
    let n = ARC_POINTS - 1;
    let mut out = Vec::with_capacity(n + 1);
    out.push(a);
    for i in 1..n {
        let t = a0 + sweep * i as f64 / n as f64;
        out.push([ux + r * t.cos(), uy + r * t.sin()]);
    }
    out.push(c);
    out
}

/// The vertices of a `LineString`, a `LinearRing` or a `Curve` of line and arc
/// segments, in RD metres.
fn line(node: &Node) -> Vec<Rd> {
    match node.name.as_str() {
        "LineString" | "LinearRing" | "LineStringSegment" => {
            if let Some(list) = node.child("posList") {
                positions(list)
            } else {
                node.children
                    .iter()
                    .filter(|c| c.name == "pos")
                    .flat_map(positions)
                    .collect()
            }
        }
        "Arc" | "ArcString" => {
            let pts = node.child("posList").map(positions).unwrap_or_default();
            let mut out = Vec::new();
            // An ArcString is arcs sharing their end points: 3, 5, 7… positions.
            for w in pts.windows(3).step_by(2) {
                extend_joined(&mut out, &arc(w[0], w[1], w[2]));
            }
            out
        }
        "Curve" => {
            let mut out = Vec::new();
            for segment in node.child("segments").map_or(&[][..], |s| &s.children) {
                extend_joined(&mut out, &line(segment));
            }
            out
        }
        // A `Ring` is a closed chain of curve members.
        "Ring" => {
            let mut out = Vec::new();
            for member in node.children.iter().filter(|c| c.name == "curveMember") {
                for curve in &member.children {
                    extend_joined(&mut out, &line(curve));
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

fn to_lng_lat(pts: &[Rd]) -> Value {
    Value::Array(
        pts.iter()
            .map(|&[x, y]| {
                let (lat, lng) = rd_to_lat_lng(x, y);
                json!([lng, lat])
            })
            .collect(),
    )
}

/// A polygon's rings in GeoJSON order, exterior first.
fn polygon(node: &Node) -> Option<Value> {
    let mut rings = Vec::new();
    for part in &node.children {
        if !matches!(part.name.as_str(), "exterior" | "interior") {
            continue;
        }
        let pts = part.children.first().map(line).unwrap_or_default();
        if pts.len() < 3 {
            if part.name == "exterior" {
                return None;
            }
            continue;
        }
        let ring = to_lng_lat(&pts);
        if part.name == "exterior" {
            rings.insert(0, ring);
        } else {
            rings.push(ring);
        }
    }
    (!rings.is_empty()).then(|| Value::Array(rings))
}

/// A GML geometry as a GeoJSON one, in WGS84.
fn geometry(node: &Node) -> Option<Value> {
    fn members<'a>(node: &'a Node, wrapper: &'static str) -> impl Iterator<Item = &'a Node> {
        node.children
            .iter()
            .filter(move |c| c.name == wrapper)
            .flat_map(|m| m.children.iter())
    }
    match node.name.as_str() {
        "Point" => {
            let &[x, y] = node.child("pos").map(positions)?.first()?;
            let (lat, lng) = rd_to_lat_lng(x, y);
            Some(json!({"type": "Point", "coordinates": [lng, lat]}))
        }
        "LineString" | "Curve" => {
            let pts = line(node);
            (pts.len() >= 2).then(|| json!({"type": "LineString", "coordinates": to_lng_lat(&pts)}))
        }
        "Polygon" => polygon(node).map(|c| json!({"type": "Polygon", "coordinates": c})),
        "MultiSurface" | "MultiPolygon" => {
            let polys: Vec<Value> = members(node, "surfaceMember")
                .chain(members(node, "polygonMember"))
                .filter_map(polygon)
                .collect();
            (!polys.is_empty()).then(|| json!({"type": "MultiPolygon", "coordinates": polys}))
        }
        "MultiCurve" | "MultiLineString" => {
            let parts: Vec<Value> = members(node, "curveMember")
                .chain(members(node, "lineStringMember"))
                .map(line)
                .filter(|p| p.len() >= 2)
                .map(|p| to_lng_lat(&p))
                .collect();
            (!parts.is_empty()).then(|| json!({"type": "MultiLineString", "coordinates": parts}))
        }
        _ => None,
    }
}

/// Whether a GeoJSON geometry's extent meets a `[min lng, min lat, max lng, max
/// lat]` envelope — the OGC API's `bbox` test.
fn meets(geometry: &Value, [x0, y0, x1, y1]: [f64; 4]) -> bool {
    fn walk(v: &Value, ext: &mut [f64; 4]) {
        let Some(a) = v.as_array() else { return };
        if let (Some(x), Some(y)) = (
            a.first().and_then(Value::as_f64),
            a.get(1).and_then(Value::as_f64),
        ) {
            *ext = [ext[0].min(x), ext[1].min(y), ext[2].max(x), ext[3].max(y)];
        } else {
            a.iter().for_each(|c| walk(c, ext));
        }
    }
    let mut ext = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    walk(&geometry["coordinates"], &mut ext);
    ext[0] <= x1 && ext[2] >= x0 && ext[1] <= y1 && ext[3] >= y0
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<gml:FeatureCollection xmlns:imgeo-s="x" xmlns:gml="http://www.opengis.net/gml"><gml:featureMember>
	<imgeo-s:Wegdeel gml:id="a">
	<imgeo-s:identificatie.lokaalID>G1699.one</imgeo-s:identificatie.lokaalID>
<imgeo-s:relatieveHoogteligging>0</imgeo-s:relatieveHoogteligging>
	<imgeo-s:bgt-functie>voetpad</imgeo-s:bgt-functie>
	<imgeo-s:plus-fysiekVoorkomen>tegels &amp; klinkers</imgeo-s:plus-fysiekVoorkomen>
	<imgeo-s:geometrie2d><gml:Polygon xmlns:gml="http://www.opengis.net/gml"><gml:exterior><gml:Ring><gml:curveMember><gml:Curve><gml:segments><gml:LineStringSegment><gml:posList>233000 581000 233010 581000 233010 581010</gml:posList></gml:LineStringSegment><gml:Arc><gml:posList>233010 581010 233005 581015 233000 581010</gml:posList></gml:Arc><gml:LineStringSegment><gml:posList>233000 581010 233000 581000</gml:posList></gml:LineStringSegment></gml:segments></gml:Curve></gml:curveMember></gml:Ring></gml:exterior><gml:interior><gml:LinearRing><gml:posList>233002 581002 233004 581002 233004 581004 233002 581002</gml:posList></gml:LinearRing></gml:interior></gml:Polygon></imgeo-s:geometrie2d>
	</imgeo-s:Wegdeel>
</gml:featureMember><gml:featureMember>
	<imgeo-s:Paal gml:id="b">
	<imgeo-s:eindRegistratie>2025-09-26T09:40:48</imgeo-s:eindRegistratie>
	<imgeo-s:plus-type>lichtmast</imgeo-s:plus-type>
	<imgeo-s:geometrie2d><gml:Point xmlns:gml="http://www.opengis.net/gml"><gml:pos>233000.5 581000.5</gml:pos></gml:Point></imgeo-s:geometrie2d>
	</imgeo-s:Paal>
</gml:featureMember></gml:FeatureCollection>"#;

    fn parse(doc: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for_each_feature(doc.as_bytes(), |f| {
            out.extend(feature_value(&f));
            Ok(())
        })
        .unwrap();
        out
    }

    /// Properties come out under the OGC API's names, so `bgt.rs` reads the
    /// extract as it read the API.
    #[test]
    fn features_carry_the_ogc_property_names() {
        let fs = parse(DOC);
        assert_eq!(fs.len(), 2);
        let p = &fs[0]["properties"];
        assert_eq!(p["lokaal_id"], "G1699.one");
        assert_eq!(p["relatieve_hoogteligging"], 0);
        assert_eq!(p["functie"], "voetpad");
        assert_eq!(p["plus_fysiek_voorkomen"], "tegels & klinkers");
        assert_eq!(fs[1]["properties"]["plus_type"], "lichtmast");
        assert_eq!(fs[1]["geometry"]["type"], "Point");
    }

    /// An end of registration alone makes a feature history, as an end date
    /// alone does; and the RD window test sees every coordinate of the geometry.
    #[test]
    fn old_versions_and_far_features_are_dropped_before_projection() {
        let mut seen = Vec::new();
        for_each_feature(DOC.as_bytes(), |f| {
            seen.push((
                is_historical(&f),
                near(&f, [233_000.0, 581_000.0, 233_001.0, 581_001.0]),
                near(&f, [233_011.0, 581_000.0, 233_020.0, 581_020.0]),
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(false, true, false), (true, true, false)]);
    }

    /// A ring of curve segments is one closed ring with the arc straightened in
    /// between, joints not doubled, and the hole kept after it.
    #[test]
    fn a_curved_ring_is_straightened_and_joined() {
        let g = &parse(DOC)[0]["geometry"];
        assert_eq!(g["type"], "Polygon");
        let rings = g["coordinates"].as_array().unwrap();
        assert_eq!(rings.len(), 2);
        let outer = rings[0].as_array().unwrap();
        assert!(outer.len() > 8, "the arc adds vertices: {}", outer.len());
        assert_eq!(outer.first(), outer.last());
        for w in outer.windows(2) {
            assert_ne!(w[0], w[1], "a doubled joint");
        }
        let (lat, lng) = rd_to_lat_lng(233_000.0, 581_000.0);
        assert_eq!(outer[0], json!([lng, lat]));
    }

    #[test]
    fn an_arc_stays_on_its_circle_and_goes_the_right_way() {
        // A half circle of radius 5 around (5, 0), over the top.
        let pts = arc([0.0, 0.0], [5.0, 5.0], [10.0, 0.0]);
        assert_eq!(pts.first(), Some(&[0.0, 0.0]));
        assert_eq!(pts.last(), Some(&[10.0, 0.0]));
        for p in &pts {
            assert!(((p[0] - 5.0).hypot(p[1]) - 5.0).abs() < 1e-9);
            assert!(p[1] >= -1e-9, "went under: {p:?}");
        }
        assert_eq!(pts.len(), ARC_POINTS);
        // The same ends through the bottom point go under instead.
        assert!(arc([0.0, 0.0], [5.0, -5.0], [10.0, 0.0])
            .iter()
            .all(|p| p[1] <= 1e-9));
        // Three points on a line are left as they are.
        assert_eq!(arc([0.0, 0.0], [1.0, 1.0], [2.0, 2.0]).len(), 3);
    }

    #[test]
    fn the_envelope_test_is_the_apis_bbox_test() {
        let area = [6.0, 53.0, 6.1, 53.1];
        let line = |a: [f64; 2], b: [f64; 2]| json!({"type": "LineString", "coordinates": [a, b]});
        assert!(meets(&line([5.9, 53.05], [6.05, 53.05]), area));
        assert!(!meets(&line([5.8, 53.05], [5.9, 53.05]), area));
        assert!(meets(
            &json!({"type": "Point", "coordinates": [6.1, 53.1]}),
            area
        ));
    }

    /// A zip written the way PDOK writes one — deflated, sizes in a data
    /// descriptor after the data — is found through its central directory and
    /// read back.
    #[test]
    fn a_zip_with_data_descriptors_is_read_through_its_central_directory() {
        use std::io::Write;
        let body = b"<gml:FeatureCollection/>".repeat(50);
        let mut deflated =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        deflated.write_all(&body).unwrap();
        let deflated = deflated.finish().unwrap();
        let name = b"bgt_paal.gml";
        let mut z = Vec::new();
        z.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 8, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // flag 8: descriptor
        z.extend_from_slice(&[0; 8]); // sizes unknown here
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());
        z.extend_from_slice(name);
        z.extend_from_slice(&deflated);
        z.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        z.extend_from_slice(&[0; 4]);
        z.extend_from_slice(&(deflated.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        let cd = z.len();
        z.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        z.extend_from_slice(&[20, 0, 20, 0, 8, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        z.extend_from_slice(&(deflated.len() as u32).to_le_bytes());
        z.extend_from_slice(&(body.len() as u32).to_le_bytes());
        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
        z.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
        z.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        z.extend_from_slice(name);
        let cd_len = z.len() - cd;
        z.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        z.extend_from_slice(&(cd_len as u32).to_le_bytes());
        z.extend_from_slice(&(cd as u32).to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes());

        let entries = zip_entries(&mut std::io::Cursor::new(&z)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "bgt_paal.gml");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.zip");
        std::fs::write(&path, &z).unwrap();
        let mut out = Vec::new();
        entries[0]
            .open(&path)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, body);
    }
}
