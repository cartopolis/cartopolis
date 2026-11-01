//! **Galaxies** — the Local Group, the Local Volume and the 2MASS Redshift
//! Survey as one file, `cosmos/galaxies.bin`
//! (`cartopolis_geo::formats::galaxies`), for the zoom ladder past the Milky Way
//! (`systems::cosmos`).
//!
//! Three published catalogues, each where it is the best there is:
//!
//! * **The Local Group**, McConnachie (2012) as its author keeps it up to date
//!   at the CADC (the January 2021 public table, 144 galaxies to ~3 Mpc): the
//!   Magellanic Clouds, Triangulum and the dwarfs, at distances from their
//!   distance moduli. It has no row for the Milky Way or Andromeda.
//! * **The Local Volume**, Karachentsev, Makarov & Kaisina (2013), the Updated
//!   Nearby Galaxy Catalogue (869 galaxies to ~11 Mpc, VizieR `J/AJ/145/101`),
//!   at its own distances (tip of the red giant branch, Cepheids, Tully–Fisher).
//!   Andromeda comes from here. A galaxy McConnachie also lists is taken from
//!   McConnachie; the Milky Way row is left out, as the map draws it.
//! * **The cosmic web**, Huchra et al. (2012), the 2MASS Redshift Survey
//!   (44,599 galaxies brighter than K = 11.75 over 91 % of the sky, VizieR
//!   `J/ApJS/199/26`), at `cz / H₀` with H₀ = 70 km/s/Mpc — **only beyond
//!   11 Mpc**: nearer than that a galaxy's own motion is a large part of its
//!   redshift (Andromeda's is −300 km/s), and the Local Volume has real
//!   distances for those. The empty band across the sky is the survey's: the
//!   Milky Way's own disc hides what lies behind it.
//!
//! A few clusters are named on their brightest galaxy, found in the survey by
//! position ([`LANDMARKS`]); everything else in the survey is unnamed but its
//! Messier objects.
//!
//! **A short answer is never written.** A catalogue that comes back with fewer
//! rows than it has is an error page or a cut download, and a file built from it
//! would replace a whole one on the host.

use std::time::Duration;

use cartopolis_geo::formats::coverage::Manifest;
use cartopolis_geo::formats::galaxies::{Catalogue, Galaxies, Galaxy};

use super::http;
use super::{Count, Ctx, DatasetResult, Outcome, PipelineError, Progress};

/// Where the file goes in the site tree.
pub const OUT: &str = "cosmos/galaxies.bin";
/// And its manifest, for the web UI's coverage view.
pub const COVERAGE: &str = "cosmos/coverage.json";

/// McConnachie's table, as published on his CADC page.
pub const MCCONNACHIE_URL: &str = "https://www.cadc-ccda.hia-iha.nrc-cnrc.gc.ca/files/vault/PANDAS/NearbyGals/NearbyGalaxies_Jan2021_PUBLIC.fits";

/// VizieR, the Harvard mirror first: Strasbourg's own did not answer the VPS
/// on 2026-11-01 while the mirror answered in half a second.
pub const VIZIER: [&str; 2] = [
    "https://vizier.cfa.harvard.edu",
    "https://vizier.cds.unistra.fr",
];

/// The Hubble constant the survey's redshifts are turned into distances with,
/// km/s/Mpc.
pub const H0: f64 = 70.0;

/// The Local Volume's reach, Mpc: the survey is taken only beyond it.
pub const LOCAL_VOLUME_MPC: f64 = 11.0;

/// Fewer rows than these is not the catalogue (they hold 144, 869 and 44,599).
const MIN_LOCAL_GROUP: usize = 100;
const MIN_LOCAL_VOLUME: usize = 700;
const MIN_REDSHIFT: usize = 40_000;

const TIMEOUT: Duration = Duration::from_secs(300);

const ATTRIBUTION: &str = "Galaxies: McConnachie 2012 (CADC, 2021 edition); Karachentsev, \
                           Makarov & Kaisina 2013 (UNGC); Huchra et al. 2012 (2MRS) — via VizieR";

/// Clusters named on their brightest member: name, RA and Dec (degrees, J2000)
/// of that galaxy, as the survey lists it.
pub const LANDMARKS: &[(&str, f64, f64)] = &[
    ("Virgo Cluster", 187.7059, 12.3911),
    ("Fornax Cluster", 54.6212, -35.4507),
    ("Centaurus Cluster", 192.2053, -41.3111),
    ("Perseus Cluster", 49.9510, 41.5117),
    ("Coma Cluster", 195.0338, 27.9770),
    ("Norma Cluster · Great Attractor", 243.7661, -60.9073),
    ("Shapley Supercluster", 201.9870, -31.4955),
];

/// Names the catalogues give in a form nobody says, and the one they do.
const COMMON_NAMES: &[(&str, &str)] = &[
    ("LMC", "Large Magellanic Cloud"),
    ("SMC", "Small Magellanic Cloud"),
    ("MESSIER031", "Andromeda"),
    ("DESJ0225+0304", "DES J0225+0304"),
    ("MESSIER033", "Triangulum"),
    ("NGC5128", "Centaurus A"),
    ("MESSIER081", "Bode's Galaxy"),
    ("MESSIER082", "Cigar Galaxy"),
    ("MESSIER101", "Pinwheel Galaxy"),
    ("NGC0253", "Sculptor Galaxy"),
    ("MESSIER083", "Southern Pinwheel"),
    ("MESSIER051a", "Whirlpool Galaxy"),
    ("MESSIER064", "Black Eye Galaxy"),
    ("MESSIER094", "Cat's Eye Galaxy"),
];

pub fn run(ctx: &Ctx) -> Outcome<DatasetResult> {
    ctx.progress(Progress {
        phase: Some("catalogi".into()),
        done: Some(0),
        total: Some(Some(3)),
    });
    let fits = http::get_parsed_with(ctx, MCCONNACHIE_URL, "McConnachie", TIMEOUT, 4, |b| {
        local_group(b)
    })?;
    ctx.log(format!(
        "Lokale Groep: {} stelsels (McConnachie)",
        fits.len()
    ));
    ctx.progress(Progress {
        done: Some(1),
        ..Progress::default()
    });
    let ungc = vizier(
        ctx,
        "J/AJ/145/101/catalog",
        "Name,Dist,KLum,BMag",
        "UNGC",
        MIN_LOCAL_VOLUME,
    )?;
    ctx.progress(Progress {
        done: Some(2),
        ..Progress::default()
    });
    let mrs = vizier(
        ctx,
        "J/ApJS/199/26/table3",
        "Ktmag,cz,CAT",
        "2MRS",
        MIN_REDSHIFT,
    )?;
    ctx.progress(Progress {
        done: Some(3),
        ..Progress::default()
    });
    let doc = assemble(fits, &ungc, &mrs)?;
    let by = |c: Catalogue| doc.galaxies.iter().filter(|g| g.catalogue == c).count();
    let (lg, lv, rs) = (
        by(Catalogue::LocalGroup),
        by(Catalogue::LocalVolume),
        by(Catalogue::Redshift),
    );
    let named = doc.galaxies.iter().filter(|g| !g.name.is_empty()).count();
    ctx.log(format!(
        "{lg} Lokale Groep, {lv} Lokaal Volume, {rs} 2MRS voorbij {LOCAL_VOLUME_MPC} Mpc; {named} met naam"
    ));
    ctx.write_bytes_gzipped(OUT, &doc.encode())?;
    // The web UI's coverage view reads a manifest; the counts are what it shows.
    let mut manifest = Manifest::default();
    manifest.attribution = Some(ATTRIBUTION.into());
    manifest.name = Some("whole sky".into());
    for (key, n) in [
        ("local_group_count", lg),
        ("local_volume_count", lv),
        ("redshift_survey_count", rs),
    ] {
        manifest.extra.insert(key.into(), n.into());
    }
    ctx.write_coverage(COVERAGE, &manifest)?;
    Ok(DatasetResult {
        cells: 1,
        counts: vec![
            Count::new("Local Group galaxies", lg),
            Count::new("Local Volume galaxies", lv),
            Count::new("redshift-survey galaxies", rs),
        ],
    })
}

/// One VizieR table as TSV, from the first mirror that answers in full.
fn vizier(ctx: &Ctx, table: &str, cols: &str, label: &str, min: usize) -> Outcome<Table> {
    let mut last = PipelineError::Failed(format!("{label}: no mirror"));
    for mirror in VIZIER {
        let url = format!(
            "{mirror}/viz-bin/asu-tsv?-source={table}&-out={cols}&-out.add=_RAJ,_DEJ\
             &-out.max=unlimited&-oc.form=d"
        );
        match http::get_parsed_with(ctx, &url, label, TIMEOUT, 2, |b| {
            let t = Table::parse(&String::from_utf8_lossy(b))?;
            if t.rows.len() < min {
                return Err(format!(
                    "{} rows, the catalogue has more than {min}",
                    t.rows.len()
                ));
            }
            Ok(t)
        }) {
            Ok(t) => {
                ctx.log(format!("{label}: {} rijen van {mirror}", t.rows.len()));
                return Ok(t);
            }
            Err(PipelineError::Cancelled) => return Err(PipelineError::Cancelled),
            Err(e) => {
                ctx.log(format!("{label}: {mirror}: {e}"));
                last = e;
            }
        }
    }
    Err(last)
}

/// A galaxy before it is placed: where it is on the sky and how far.
#[derive(Clone, Debug)]
struct Entry {
    ra: f64,
    dec: f64,
    mpc: f64,
    log_lum: f32,
    name: String,
    /// The catalogue's own name, for [`COMMON_NAMES`].
    key: String,
}

fn place(e: &Entry, catalogue: Catalogue, name: String) -> Galaxy {
    let (ra, dec) = (e.ra.to_radians(), e.dec.to_radians());
    let xyz_mpc = [
        (dec.cos() * ra.cos() * e.mpc) as f32,
        (dec.cos() * ra.sin() * e.mpc) as f32,
        (dec.sin() * e.mpc) as f32,
    ];
    Galaxy {
        xyz_mpc,
        log_lum: e.log_lum,
        catalogue,
        name,
    }
}

/// The angle between two places on the sky, degrees.
fn separation(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (r1, d1, r2, d2) = (
        a.0.to_radians(),
        a.1.to_radians(),
        b.0.to_radians(),
        b.1.to_radians(),
    );
    let c = d1.sin() * d2.sin() + d1.cos() * d2.cos() * (r1 - r2).cos();
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// The three catalogues as one file, in its order (the format's module note).
fn assemble(lg: Vec<Entry>, ungc: &Table, mrs: &Table) -> Outcome<Galaxies> {
    let mut galaxies = Vec::new();
    let mut lg = lg;
    lg.sort_by(|a, b| a.mpc.total_cmp(&b.mpc));
    for e in &lg {
        galaxies.push(place(e, Catalogue::LocalGroup, pretty(&e.key)));
    }

    let col = |t: &Table, n: &str| {
        t.col(n)
            .ok_or_else(|| PipelineError::Failed(format!("no column {n}")))
    };
    let (ra, de, name, dist, klum, bmag) = (
        col(ungc, "_RAJ2000")?,
        col(ungc, "_DEJ2000")?,
        col(ungc, "Name")?,
        col(ungc, "Dist")?,
        col(ungc, "KLum")?,
        col(ungc, "BMag")?,
    );
    let mut lv: Vec<Entry> = ungc
        .rows
        .iter()
        .filter_map(|r| {
            let key = r[name].trim().to_string();
            let mpc = num(&r[dist])?;
            // The map draws the Milky Way itself.
            if key == "Milky Way" || mpc <= 0.0 {
                return None;
            }
            // K luminosity where measured, else from the absolute B magnitude.
            let log_lum = num(&r[klum])
                .or_else(|| num(&r[bmag]).map(|m| (5.48 - m) / 2.5))
                .unwrap_or(7.0) as f32;
            Some(Entry {
                ra: num(&r[ra])?,
                dec: num(&r[de])?,
                mpc,
                log_lum,
                name: String::new(),
                key,
            })
        })
        // Within 0.1° and a factor 1.5 in distance of a Local Group dwarf is
        // the same galaxy, which McConnachie has better.
        .filter(|e| {
            !lg.iter().any(|g| {
                separation((e.ra, e.dec), (g.ra, g.dec)) < 0.1
                    && (e.mpc / g.mpc).max(g.mpc / e.mpc) < 1.5
            })
        })
        .collect();
    lv.sort_by(|a, b| a.mpc.total_cmp(&b.mpc));
    for e in &lv {
        galaxies.push(place(e, Catalogue::LocalVolume, pretty(&e.key)));
    }

    let (ra, de, k, cz, cat) = (
        col(mrs, "_RAJ2000")?,
        col(mrs, "_DEJ2000")?,
        col(mrs, "Ktmag")?,
        col(mrs, "cz")?,
        col(mrs, "CAT")?,
    );
    let mut far: Vec<(f64, Entry)> = mrs
        .rows
        .iter()
        .filter_map(|r| {
            let mpc = num(&r[cz])? / H0;
            if mpc <= LOCAL_VOLUME_MPC {
                return None;
            }
            let kmag = num(&r[k])?;
            // M_K, then L_K with the Sun's M_K = 3.28.
            let abs = kmag - 5.0 * (mpc * 1e6).log10() + 5.0;
            let key = r[cat].trim().to_string();
            Some((
                kmag,
                Entry {
                    ra: num(&r[ra])?,
                    dec: num(&r[de])?,
                    mpc,
                    log_lum: ((3.28 - abs) / 2.5) as f32,
                    name: String::new(),
                    key,
                },
            ))
        })
        .collect();
    // Brightest first as seen from here: the order a phone thins by.
    far.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, e) in &mut far {
        if let Some((n, ..)) = LANDMARKS
            .iter()
            .find(|(_, r, d)| separation((e.ra, e.dec), (*r, *d)) < 0.02)
        {
            e.name = (*n).to_string();
        } else if e.key.starts_with("MESSIER_") {
            e.name = pretty(&e.key.replace('_', ""));
        }
    }
    for (_, e) in &far {
        let name = e.name.clone();
        galaxies.push(place(e, Catalogue::Redshift, name));
    }
    Ok(Galaxies {
        attribution: ATTRIBUTION.into(),
        galaxies,
    })
}

fn num(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A catalogue name as it is said: "AndromedaXIX" → "Andromeda XIX",
/// "NGC0147" → "NGC 147", "MESSIER087" → "M87", "SagittariusdSph" →
/// "Sagittarius dSph", "*Tucana3" → "Tucana 3", "Bootes(1)" → "Bootes 1".
fn pretty(key: &str) -> String {
    let key = key.trim_start_matches('*');
    if let Some((_, n)) = COMMON_NAMES.iter().find(|(k, _)| *k == key) {
        return (*n).to_string();
    }
    let key = key.replace(['(', ')'], " ");
    if let Some(n) = key.strip_prefix("MESSIER") {
        return format!("M{}", n.trim_start_matches('0'));
    }
    // A catalogue prefix and its number: "NGC0147", "UGC12894", "DDO113".
    let letters: String = key.chars().take_while(char::is_ascii_uppercase).collect();
    if (2..=5).contains(&letters.len())
        && key[letters.len()..].starts_with(|c: char| c.is_ascii_digit())
    {
        let rest = key[letters.len()..].trim_start_matches('0');
        return format!("{letters} {rest}").trim().to_string();
    }
    // A word, then a word, a numeral or a number: "UrsaMajor2", "LeoA".
    let chars: Vec<char> = key.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let after_lower = i > 0 && chars[i - 1].is_ascii_lowercase();
        let rest: String = chars[i..].iter().collect();
        if after_lower && (rest == "dSph" || rest == "dIrr") {
            out.push(' ');
            out.push_str(&rest);
            break;
        }
        if after_lower && (c.is_ascii_uppercase() || c.is_ascii_digit()) {
            out.push(' ');
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A VizieR TSV answer: comments, a header, a units line, a rule, rows.
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    fn parse(text: &str) -> Result<Table, String> {
        let mut lines = text
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty());
        let header: Vec<String> = lines
            .next()
            .ok_or("no header")?
            .split('\t')
            .map(|s| s.trim().to_string())
            .collect();
        if header.len() < 2 {
            return Err("not a VizieR table".into());
        }
        let rows = lines
            // The units line and the rule under the header.
            .skip(2)
            .map(|l| l.split('\t').map(str::to_string).collect::<Vec<_>>())
            .filter(|r| r.len() == header.len())
            .collect();
        Ok(Table { header, rows })
    }

    fn col(&self, name: &str) -> Option<usize> {
        self.header.iter().position(|h| h == name)
    }
}

/// McConnachie's FITS table, read as far as this needs: a primary header (and
/// its VOTable bytes), then one binary table of big-endian columns.
fn local_group(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let (primary, mut at) = fits_header(bytes, 0)?;
    at += padded(
        primary.int("NAXIS1").unwrap_or(0) as usize
            * usize::from(primary.int("NAXIS").unwrap_or(0) > 0),
    );
    let (table, data) = fits_header(bytes, at)?;
    if table.text("XTENSION").as_deref() != Some("BINTABLE") {
        return Err("no binary table".into());
    }
    let width = table.int("NAXIS1").ok_or("no NAXIS1")? as usize;
    let rows = table.int("NAXIS2").ok_or("no NAXIS2")? as usize;
    let fields = table.int("TFIELDS").ok_or("no TFIELDS")? as usize;
    // Each column's name, offset and kind.
    let mut cols = Vec::new();
    let mut offset = 0usize;
    for i in 1..=fields {
        let name = table.text(&format!("TTYPE{i}")).unwrap_or_default();
        let form = table.text(&format!("TFORM{i}")).ok_or("no TFORM")?;
        let digits: String = form.chars().take_while(char::is_ascii_digit).collect();
        let repeat = digits.parse::<usize>().unwrap_or(1);
        let kind = form[digits.len()..].chars().next().ok_or("empty TFORM")?;
        let size = match kind {
            'A' | 'L' | 'B' => 1,
            'I' => 2,
            'J' | 'E' => 4,
            'K' | 'D' => 8,
            other => return Err(format!("TFORM {other} not read")),
        } * repeat;
        cols.push((name, offset, kind, size));
        offset += size;
    }
    if offset != width || bytes.len() < data + width * rows {
        return Err("table shorter than its header says".into());
    }
    let find = |n: &str| {
        cols.iter()
            .find(|c| c.0 == n)
            .cloned()
            .ok_or(format!("no column {n}"))
    };
    let (name, ra, dec, ebv, dmod, vmag) = (
        find("GalaxyName")?,
        find("RA")?,
        find("Dec")?,
        find("EB-V")?,
        find("dmod")?,
        find("Vmag")?,
    );
    let text = |row: &[u8], c: &(String, usize, char, usize)| {
        String::from_utf8_lossy(&row[c.1..c.1 + c.3])
            .trim()
            .to_string()
    };
    let float = |row: &[u8], c: &(String, usize, char, usize)| {
        let b = &row[c.1..c.1 + 4];
        f64::from(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let mut out = Vec::new();
    for r in 0..rows {
        let row = &bytes[data + r * width..data + (r + 1) * width];
        let m = float(row, &dmod);
        let (Some(ra_deg), Some(dec_deg)) = (
            sexagesimal(&text(row, &ra)).map(|h| h * 15.0),
            sexagesimal(&text(row, &dec)),
        ) else {
            continue;
        };
        if !m.is_finite() || m <= 0.0 {
            continue;
        }
        let v = float(row, &vmag);
        let e = float(row, &ebv);
        // M_V with the extinction taken off, then L_V with the Sun's 4.83.
        let abs = v - m - 3.1 * if e.is_finite() { e } else { 0.0 };
        out.push(Entry {
            ra: ra_deg,
            dec: dec_deg,
            mpc: 10f64.powf((m + 5.0) / 5.0) / 1e6,
            log_lum: if abs.is_finite() {
                ((4.83 - abs) / 2.5) as f32
            } else {
                5.0
            },
            name: String::new(),
            key: text(row, &name),
        });
    }
    if out.len() < MIN_LOCAL_GROUP {
        return Err(format!(
            "{} galaxies, the table has more than {MIN_LOCAL_GROUP}",
            out.len()
        ));
    }
    Ok(out)
}

/// "13:57:12.0" or "-27:40:0.0" as a number of hours or degrees.
fn sexagesimal(s: &str) -> Option<f64> {
    let s = s.trim();
    let negative = s.starts_with('-');
    let mut parts = s
        .trim_start_matches(['-', '+'])
        .split([':', ' '])
        .filter(|p| !p.is_empty());
    let a: f64 = parts.next()?.parse().ok()?;
    let b: f64 = parts.next().map_or(Some(0.0), |p| p.parse().ok())?;
    let c: f64 = parts.next().map_or(Some(0.0), |p| p.parse().ok())?;
    let v = a + b / 60.0 + c / 3600.0;
    Some(if negative { -v } else { v })
}

/// A FITS header's cards.
struct Header(Vec<(String, String)>);

impl Header {
    fn raw(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
    fn int(&self, key: &str) -> Option<i64> {
        self.raw(key)?.trim().parse().ok()
    }
    fn text(&self, key: &str) -> Option<String> {
        let v = self.raw(key)?.trim();
        Some(v.trim_matches('\'').trim().to_string())
    }
}

fn padded(n: usize) -> usize {
    n.div_ceil(2880) * 2880
}

/// The header starting at `at`, and where its data starts.
fn fits_header(bytes: &[u8], at: usize) -> Result<(Header, usize), String> {
    let mut cards = Vec::new();
    let mut i = at;
    loop {
        let card = bytes.get(i..i + 80).ok_or("header runs past the file")?;
        i += 80;
        let card = String::from_utf8_lossy(card);
        if card.starts_with("END") && card[3..].trim().is_empty() {
            break;
        }
        if card.get(8..10) == Some("= ") {
            let key = card[..8].trim().to_string();
            let value = &card[10..];
            // The value ends at a comment's slash, unless the slash is inside
            // a quoted string.
            let value = if value.trim_start().starts_with('\'') {
                let start = value.find('\'').unwrap_or(0);
                let end = value[start + 1..]
                    .find('\'')
                    .map_or(value.len(), |e| start + e + 2);
                &value[..end]
            } else {
                value.split('/').next().unwrap_or("")
            };
            cards.push((key, value.trim().to_string()));
        }
    }
    Ok((Header(cards), at + padded(i - at)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_said_as_people_say_them() {
        assert_eq!(pretty("AndromedaXIX"), "Andromeda XIX");
        assert_eq!(pretty("NGC0147"), "NGC 147");
        assert_eq!(pretty("UGC12894"), "UGC 12894");
        assert_eq!(pretty("MESSIER087"), "M87");
        assert_eq!(pretty("MESSIER031"), "Andromeda");
        assert_eq!(pretty("LMC"), "Large Magellanic Cloud");
        assert_eq!(pretty("SagittariusdSph"), "Sagittarius dSph");
        assert_eq!(pretty("*Tucana3"), "Tucana 3");
        assert_eq!(pretty("UrsaMajor2"), "Ursa Major 2");
        assert_eq!(pretty("Bootes(1)"), "Bootes 1");
        assert_eq!(pretty("LeoA"), "Leo A");
        assert_eq!(pretty("WLM"), "WLM");
        assert_eq!(pretty("And XVIII"), "And XVIII");
    }

    #[test]
    fn sexagesimal_reads_both_signs() {
        assert!((sexagesimal("13:57:12.0").unwrap() - 13.953_333).abs() < 1e-5);
        assert!((sexagesimal("-27:40:0.0").unwrap() + 27.666_667).abs() < 1e-5);
        assert!((sexagesimal("0:1:58.2").unwrap() - 0.032_833).abs() < 1e-5);
    }

    #[test]
    fn a_vizier_table_skips_its_units_and_rule() {
        let t = Table::parse(
            "#comment\n\n_RAJ2000\t_DEJ2000\tName\tDist\ndeg\tdeg\t \tMpc\n---\t---\t---\t---\n\
             010.68\t+41.27\tMESSIER031\t 0.77\n",
        )
        .unwrap();
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0][t.col("Name").unwrap()], "MESSIER031");
    }

    /// A FITS file of one binary table, as McConnachie's is laid out.
    fn fits(rows: &[(&str, &str, &str, f32, f32, f32)]) -> Vec<u8> {
        let card = |s: String| format!("{s:<80}");
        let mut h = String::new();
        for c in [
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    0",
            "END",
        ] {
            h += &card(c.into());
        }
        let mut out = h.into_bytes();
        out.resize(2880, b' ');
        let cols = [
            ("GalaxyName", "16A"),
            ("RA", "10A"),
            ("Dec", "11A"),
            ("EB-V", "E"),
            ("dmod", "E"),
            ("Vmag", "E"),
        ];
        let mut h = card("XTENSION= 'BINTABLE'           / binary table extension".into());
        h += &card(format!("NAXIS1  = {:>20}", 16 + 10 + 11 + 12));
        h += &card(format!("NAXIS2  = {:>20}", rows.len()));
        h += &card(format!("TFIELDS = {:>20}", cols.len()));
        for (i, (n, f)) in cols.iter().enumerate() {
            h += &card(format!("TTYPE{:<3}= '{n:<8}'", i + 1));
            h += &card(format!("TFORM{:<3}= '{f:<8}'", i + 1));
        }
        h += &card("END".into());
        let mut hb = h.into_bytes();
        hb.resize(padded(hb.len()), b' ');
        out.extend(hb);
        for (n, ra, de, ebv, dmod, v) in rows {
            out.extend(format!("{n:<16}{ra:<10}{de:<11}").bytes());
            for f in [ebv, dmod, v] {
                out.extend(f.to_be_bytes());
            }
        }
        let len = out.len();
        out.resize(padded(len), 0);
        out
    }

    #[test]
    fn the_local_group_table_is_read_at_its_distances() {
        let rows: Vec<_> = (0..MIN_LOCAL_GROUP)
            .map(|_| ("LMC", "5:23:34.5", "-69:45:22.0", 0.075, 18.52, 0.4))
            .collect();
        let lg = local_group(&fits(&rows)).unwrap();
        assert_eq!(lg.len(), MIN_LOCAL_GROUP);
        // (m − M) = 18.52 is 50.6 kpc.
        assert!((lg[0].mpc - 0.0506).abs() < 0.0005, "{}", lg[0].mpc);
        assert!((lg[0].ra - 80.894).abs() < 0.01 && (lg[0].dec + 69.756).abs() < 0.01);
        assert_eq!(lg[0].key, "LMC");
        // A short table is not the catalogue.
        assert!(local_group(&fits(&rows[..3])).is_err());
    }

    #[test]
    fn the_survey_starts_where_the_local_volume_ends() {
        let ungc = Table::parse(
            "_RAJ2000\t_DEJ2000\tName\tDist\tKLum\tBMag\nu\tu\tu\tu\tu\tu\n-\t-\t-\t-\t-\t-\n\
             010.685\t+41.269\tMESSIER031\t0.77\t10.73\t-21.4\n\
             080.894\t-69.756\tLMC\t0.05\t9.42\t-17.9\n\
             266.417\t-29.008\tMilky Way\t0.01\t\t-19.1\n",
        )
        .unwrap();
        let mrs = Table::parse(
            "_RAJ2000\t_DEJ2000\tKtmag\tcz\tCAT\nu\tu\tu\tu\tu\n-\t-\t-\t-\t-\n\
             010.6847\t+41.2688\t0.743\t-300\tMESSIER_031\n\
             187.7059\t+12.3911\t5.804\t1307\tMESSIER_087\n\
             195.0338\t+27.9770\t8.404\t6446\t13000812+2758370\n\
             194.8988\t+27.9594\t8.857\t7167\t12593570+2757333\n",
        )
        .unwrap();
        let lg = vec![Entry {
            ra: 80.894,
            dec: -69.756,
            mpc: 0.0506,
            log_lum: 9.2,
            name: String::new(),
            key: "LMC".into(),
        }];
        let g = assemble(lg, &ungc, &mrs).unwrap().galaxies;
        let names: Vec<_> = g.iter().map(|g| (g.catalogue, g.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (Catalogue::LocalGroup, "Large Magellanic Cloud"),
                (Catalogue::LocalVolume, "Andromeda"),
                (Catalogue::Redshift, "Virgo Cluster"),
                (Catalogue::Redshift, "Coma Cluster"),
                (Catalogue::Redshift, ""),
            ]
        );
        // M87 at 1307 / 70 = 18.7 Mpc.
        assert!((g[2].mpc() - 18.67).abs() < 0.01, "{}", g[2].mpc());
    }
}
