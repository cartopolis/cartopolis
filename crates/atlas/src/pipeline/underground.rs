//! What the underground datasets share: binning shapes into the per-cell
//! files of `cartopolis_geo::formats::underground`, writing them with a
//! coverage manifest, and reading the small XML documents the BRO answers with.

use std::collections::BTreeMap;

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::underground::{self as wire, Kind};

use super::geom::round;
use super::tile::{cell_path, lat_lng_to_cell, sorted_cells, Cell, CACHE_ZOOM};
use super::{Count, Ctx, DatasetResult, Outcome};

/// The cells one dataset writes, filled shape by shape.
#[derive(Default)]
pub struct Cells {
    cells: BTreeMap<Cell, wire::Cell>,
    counts: BTreeMap<&'static str, usize>,
}

fn flat(points: &[(f64, f64)]) -> Vec<f64> {
    points
        .iter()
        .flat_map(|&(la, ln)| [round(la, 7), round(ln, 7)])
        .collect()
}

fn mid(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len().max(1) as f64;
    let (la, ln) = points
        .iter()
        .fold((0.0, 0.0), |(a, b), &(la, ln)| (a + la, b + ln));
    (la / n, ln / n)
}

impl Cells {
    fn at(&mut self, (lat, lng): (f64, f64)) -> &mut wire::Cell {
        self.cells.entry(lat_lng_to_cell(lat, lng)).or_default()
    }

    /// Count one shape under a label, for the run's summary.
    pub fn count(&mut self, label: &'static str) {
        *self.counts.entry(label).or_default() += 1;
    }

    /// A vertical stack of `(top_m, bottom_m, kind)` segments at a point.
    pub fn column(&mut self, lat: f64, lng: f64, radius_m: f32, segments: Vec<(f32, f32, Kind)>) {
        if segments.is_empty() {
            return;
        }
        let segments = segments
            .into_iter()
            .filter(|s| s.1 > s.0)
            .map(|(a, b, k)| {
                (
                    round(f64::from(a), 2) as f32,
                    round(f64::from(b), 2) as f32,
                    k.index(),
                )
            })
            .collect();
        self.at((lat, lng))
            .columns
            .push((round(lat, 7), round(lng, 7), radius_m, segments));
    }

    /// A tube with a depth at every point.
    pub fn line(&mut self, kind: Kind, radius_m: f32, points: &[(f64, f64)], depths: Vec<f32>) {
        if points.len() < 2 || depths.len() != points.len() {
            return;
        }
        let m = mid(points);
        self.at(m)
            .lines
            .push((kind.index(), radius_m, depths, flat(points)));
    }

    /// One thing at a depth, `size_m` its radius.
    pub fn point(&mut self, kind: Kind, lat: f64, lng: f64, depth_m: f32, size_m: f32) {
        self.at((lat, lng)).points.push((
            kind.index(),
            round(lat, 7),
            round(lng, 7),
            round(f64::from(depth_m), 2) as f32,
            size_m,
        ));
    }

    /// An outline at a depth.
    pub fn ring(&mut self, kind: Kind, depth_m: f32, points: &[(f64, f64)]) {
        if points.len() < 3 {
            return;
        }
        let m = mid(points);
        self.at(m).rings.push((kind.index(), depth_m, flat(points)));
    }

    /// Write every non-empty cell and the manifest, and summarise the run.
    pub fn write(
        self,
        ctx: &Ctx,
        tree: &str,
        attribution: &str,
        source: &str,
    ) -> Outcome<DatasetResult> {
        let cells: Vec<(Cell, wire::Cell)> = self
            .cells
            .into_iter()
            .filter(|(_, c)| !c.is_empty())
            .collect();
        let covered = sorted_cells(cells.iter().map(|(c, _)| *c));
        for (cell, doc) in &cells {
            ctx.check()?;
            ctx.write_doc(&cell_path(tree, *cell), doc)?;
        }
        let mut manifest = Manifest::new(CACHE_ZOOM, covered).described(
            ctx.name.as_str(),
            attribution,
            source,
            ctx.bbox,
        );
        for (label, n) in &self.counts {
            manifest = manifest.with(&label.replace(' ', "_"), *n);
        }
        ctx.write_coverage(&format!("{tree}/coverage.json"), &manifest)?;
        let summary: Vec<String> = self
            .counts
            .iter()
            .map(|(l, n)| format!("{n} {l}"))
            .collect();
        ctx.log(format!("{} over {} cells", summary.join(", "), cells.len()));
        Ok(DatasetResult {
            cells: cells.len(),
            counts: self.counts.iter().map(|(l, n)| Count::new(l, *n)).collect(),
        })
    }
}

/// A soil name (NEN 5104 / BRO, camel case: `sterkSiltigeKlei`,
/// `zwakKleiigVeen`, `siltigZand`) as a [`Kind`]. The **last** word is the
/// soil; the word before it qualifies it, and a clay qualified as sandy or a
/// sand qualified as clayey is the mixed class GeoTOP calls `ClayeySand`.
/// `median` is a sand's grain class where the record has one (`fijn`,
/// `matigGrof`, …).
#[must_use]
pub fn soil_kind(name: &str, median: Option<&str>) -> Kind {
    let lower = name.to_ascii_lowercase();
    let main = name
        .rfind(|c: char| c.is_ascii_uppercase())
        .map_or(lower.as_str(), |i| &lower[i..]);
    let mixed_clay = lower.contains("zandig") || lower.contains("zandige");
    let mixed_sand = lower.contains("kleiig") || lower.contains("leemig");
    match main {
        m if m.starts_with("veen") => Kind::Peat,
        m if m.starts_with("klei") => {
            if mixed_clay {
                Kind::ClayeySand
            } else {
                Kind::Clay
            }
        }
        m if m.starts_with("leem") || m.starts_with("silt") => Kind::ClayeySand,
        m if m.starts_with("zand") => {
            if mixed_sand {
                return Kind::ClayeySand;
            }
            match median.map(str::to_ascii_lowercase).as_deref() {
                Some(m) if m.contains("grof") && !m.contains("matig") => Kind::CoarseSand,
                Some(m) if m.contains("matiggrof") || m.contains("middel") => Kind::MediumSand,
                _ => Kind::FineSand,
            }
        }
        m if m.starts_with("grind") => Kind::Gravel,
        m if m.starts_with("schelp") => Kind::Shells,
        m if m.starts_with("puin") || m.starts_with("antropogeen") => Kind::Anthropogenic,
        _ => Kind::Unknown,
    }
}

/// Merge consecutive segments of one kind, so a column is as few prisms as
/// its soil allows.
#[must_use]
pub fn merge_runs(segments: Vec<(f32, f32, Kind)>) -> Vec<(f32, f32, Kind)> {
    let mut out: Vec<(f32, f32, Kind)> = Vec::new();
    for s in segments {
        match out.last_mut() {
            Some(last) if last.2 == s.2 && (last.1 - s.0).abs() < 1e-3 => last.1 = s.1,
            _ => out.push(s),
        }
    }
    out
}

/// Minimal reading of the BRO's XML: the documents are small and regular, so
/// elements are found by local name, ignoring their namespace prefix.
pub mod xml {
    /// The inner text of every element with this local name, in order.
    #[must_use]
    pub fn all<'a>(text: &'a str, local: &str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut at = 0;
        while let Some((start, after_open)) = open_tag(text, local, at) {
            let close = text[after_open..]
                .find("</")
                .and_then(|_| close_of(&text[after_open..], local));
            match close {
                Some((inner_end, tag_end)) => {
                    out.push(&text[after_open..after_open + inner_end]);
                    at = after_open + tag_end;
                }
                None => at = start + 1,
            }
        }
        out
    }

    /// The inner text of the first element with this local name.
    #[must_use]
    pub fn first<'a>(text: &'a str, local: &str) -> Option<&'a str> {
        all(text, local).into_iter().next()
    }

    /// The first element's text as a number.
    #[must_use]
    pub fn number(text: &str, local: &str) -> Option<f64> {
        first(text, local).and_then(|t| t.trim().parse().ok())
    }

    /// The first `<[prefix:]local …>` at or after `from`: its start and the
    /// index just past its `>`. A self-closing tag is skipped.
    fn open_tag(text: &str, local: &str, from: usize) -> Option<(usize, usize)> {
        let mut at = from;
        while let Some(i) = text[at..].find('<').map(|i| i + at) {
            let rest = &text[i + 1..];
            let name_end =
                rest.find(|c: char| c == '>' || c == ' ' || c == '/' || c.is_whitespace());
            if let Some(end) = name_end {
                let name = &rest[..end];
                let bare = name.rsplit(':').next().unwrap_or(name);
                if bare == local && !name.starts_with('/') {
                    let gt = rest.find('>')?;
                    if rest[..gt].ends_with('/') {
                        at = i + 1;
                        continue;
                    }
                    return Some((i, i + 1 + gt + 1));
                }
            }
            at = i + 1;
        }
        None
    }

    /// In `inner`, the matching `</[prefix:]local>`, counting nested opens of
    /// the same name: where the inner text ends, and where the tag ends.
    fn close_of(inner: &str, local: &str) -> Option<(usize, usize)> {
        let mut depth = 0usize;
        let mut at = 0;
        while let Some(i) = inner[at..].find('<').map(|i| i + at) {
            let rest = &inner[i + 1..];
            let end = rest.find(|c: char| c == '>' || c.is_whitespace())?;
            let name = &rest[..end];
            let closing = name.starts_with('/');
            let bare = name.trim_start_matches('/');
            let bare = bare.rsplit(':').next().unwrap_or(bare);
            if bare == local {
                let gt = rest.find('>')?;
                if closing {
                    if depth == 0 {
                        return Some((i, i + 1 + gt + 1));
                    }
                    depth -= 1;
                } else if !rest[..gt].ends_with('/') {
                    depth += 1;
                }
            }
            at = i + 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soil_names_read_by_their_last_word() {
        assert_eq!(soil_kind("sterkSiltigeKlei", None), Kind::Clay);
        assert_eq!(soil_kind("zwakKleiigVeen", None), Kind::Peat);
        assert_eq!(soil_kind("siltigZand", Some("fijn")), Kind::FineSand);
        assert_eq!(soil_kind("zand", Some("matigGrof")), Kind::MediumSand);
        assert_eq!(soil_kind("zand", Some("zeerGrof")), Kind::CoarseSand);
        assert_eq!(soil_kind("kleiigZand", None), Kind::ClayeySand);
        assert_eq!(soil_kind("zandigeKlei", None), Kind::ClayeySand);
        assert_eq!(soil_kind("grind", None), Kind::Gravel);
        assert_eq!(soil_kind("onbekend", None), Kind::Unknown);
    }

    #[test]
    fn runs_of_one_kind_merge() {
        let merged = merge_runs(vec![
            (0.0, 0.5, Kind::Clay),
            (0.5, 1.0, Kind::Clay),
            (1.0, 2.0, Kind::Peat),
        ]);
        assert_eq!(merged, vec![(0.0, 1.0, Kind::Clay), (1.0, 2.0, Kind::Peat)]);
    }

    #[test]
    fn xml_elements_are_found_by_local_name() {
        let doc = r#"<a:root><b:layer><b:upperBoundary uom="m">0.00</b:upperBoundary><b:x/></b:layer><layer><upperBoundary>1.5</upperBoundary></layer></a:root>"#;
        let layers = xml::all(doc, "layer");
        assert_eq!(layers.len(), 2);
        assert_eq!(xml::number(layers[0], "upperBoundary"), Some(0.0));
        assert_eq!(xml::number(layers[1], "upperBoundary"), Some(1.5));
        assert_eq!(xml::first(doc, "missing"), None);
    }
}
