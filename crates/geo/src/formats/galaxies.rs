//! `cosmos/galaxies.bin` — the galaxies around the Milky Way, from the Local
//! Group to the edge of the 2MASS Redshift Survey, for the zoom ladder's top
//! rungs (`systems::cosmos`).
//!
//! Written by atlas's `pipeline/galaxies.rs` from three published catalogues;
//! read by the client when the view climbs past the Milky Way. Binary, not JSON,
//! because it is 44,000 rows a phone fetches whole: little-endian, the magic and
//! a format number, the attribution, then one record per galaxy —
//! `x, y, z` (Mpc, equatorial J2000: x towards RA 0h, z towards the north
//! celestial pole), `log10` of its luminosity in Suns, its catalogue, and its
//! name (empty for most of the redshift survey).
//!
//! **The order is the thinning.** Records come catalogue by catalogue, nearest
//! first ([`Catalogue`]), and within the redshift survey brightest first as seen
//! from here, so a reader that can draw only `n` takes the first `n`: it keeps
//! every galaxy of the Local Group and the Local Volume and drops the survey's
//! faintest, which are mostly its farthest.

/// Which catalogue a galaxy is from, nearest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Catalogue {
    /// McConnachie (2012), "The observed properties of dwarf galaxies in and
    /// around the Local Group", as kept up to date at the CADC: to about 3 Mpc.
    LocalGroup = 0,
    /// Karachentsev, Makarov & Kaisina (2013), the Updated Nearby Galaxy
    /// Catalogue: to about 11 Mpc.
    LocalVolume = 1,
    /// Huchra et al. (2012), the 2MASS Redshift Survey, at `cz / H₀`.
    Redshift = 2,
}

impl Catalogue {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::LocalGroup),
            1 => Some(Self::LocalVolume),
            2 => Some(Self::Redshift),
            _ => None,
        }
    }
}

/// One galaxy.
#[derive(Clone, Debug, PartialEq)]
pub struct Galaxy {
    /// Mpc from the Sun, equatorial J2000.
    pub xyz_mpc: [f32; 3],
    /// `log10(L / L☉)`: in K for the Local Volume and the redshift survey, in V
    /// for the Local Group's dwarfs, which have no K photometry.
    pub log_lum: f32,
    pub catalogue: Catalogue,
    /// Empty when the catalogue gives no name worth showing.
    pub name: String,
}

impl Galaxy {
    /// Its distance from the Sun, Mpc.
    #[must_use]
    pub fn mpc(&self) -> f32 {
        let [x, y, z] = self.xyz_mpc;
        (x * x + y * y + z * z).sqrt()
    }
}

/// The whole file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Galaxies {
    /// Who to credit, one line.
    pub attribution: String,
    /// In file order (see the module note).
    pub galaxies: Vec<Galaxy>,
}

const MAGIC: &[u8; 8] = b"CARTGALX";
/// Bumped whenever the record layout changes; a reader refuses any other.
pub const FORMAT: u32 = 1;

/// Why bytes are not a galaxy file.
#[derive(Debug, PartialEq, Eq)]
pub enum GalaxyFileError {
    NotAGalaxyFile,
    Format(u32),
    Truncated,
}

impl std::fmt::Display for GalaxyFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGalaxyFile => f.write_str("not a galaxy file"),
            Self::Format(v) => write!(f, "galaxy file format {v}, this reads {FORMAT}"),
            Self::Truncated => f.write_str("galaxy file cut short"),
        }
    }
}

impl std::error::Error for GalaxyFileError {}

impl Galaxies {
    /// The stored form.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.galaxies.len() * 20);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT.to_le_bytes());
        let attribution = self.attribution.as_bytes();
        let n = attribution.len().min(usize::from(u16::MAX));
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&attribution[..n]);
        out.extend_from_slice(&(self.galaxies.len() as u32).to_le_bytes());
        for g in &self.galaxies {
            for v in g.xyz_mpc {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&g.log_lum.to_le_bytes());
            out.push(g.catalogue as u8);
            // A name is cut on a character boundary, never mid-character.
            let mut end = g.name.len().min(255);
            while !g.name.is_char_boundary(end) {
                end -= 1;
            }
            out.push(end as u8);
            out.extend_from_slice(&g.name.as_bytes()[..end]);
        }
        out
    }

    /// Read a stored file.
    ///
    /// # Errors
    ///
    /// When the bytes are not a galaxy file, are another format's, or end early.
    pub fn decode(bytes: &[u8]) -> Result<Galaxies, GalaxyFileError> {
        let mut r = bytes
            .strip_prefix(MAGIC.as_slice())
            .ok_or(GalaxyFileError::NotAGalaxyFile)?;
        let mut take = |n: usize| -> Result<&[u8], GalaxyFileError> {
            if r.len() < n {
                return Err(GalaxyFileError::Truncated);
            }
            let (a, b) = r.split_at(n);
            r = b;
            Ok(a)
        };
        let u32_at = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let f32_at = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let format = u32_at(take(4)?);
        if format != FORMAT {
            return Err(GalaxyFileError::Format(format));
        }
        let n = take(2)?;
        let n = usize::from(u16::from_le_bytes([n[0], n[1]]));
        let attribution = String::from_utf8_lossy(take(n)?).into_owned();
        let count = u32_at(take(4)?) as usize;
        // Every record is at least 18 bytes, so a count the bytes cannot hold is
        // a cut file, and is not allowed to reserve memory for it.
        let mut galaxies = Vec::with_capacity(count.min(bytes.len() / 18));
        for _ in 0..count {
            let rec = take(18)?;
            let xyz_mpc = [f32_at(&rec[0..]), f32_at(&rec[4..]), f32_at(&rec[8..])];
            let log_lum = f32_at(&rec[12..]);
            let catalogue = Catalogue::from_u8(rec[16]).ok_or(GalaxyFileError::Truncated)?;
            let name = String::from_utf8_lossy(take(usize::from(rec[17]))?).into_owned();
            galaxies.push(Galaxy {
                xyz_mpc,
                log_lum,
                catalogue,
                name,
            });
        }
        Ok(Galaxies {
            attribution,
            galaxies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Galaxies {
        Galaxies {
            attribution: "McConnachie 2012; Karachentsev+ 2013; Huchra+ 2012".into(),
            galaxies: vec![
                Galaxy {
                    xyz_mpc: [0.58, 0.11, 0.51],
                    log_lum: 10.5,
                    catalogue: Catalogue::LocalGroup,
                    name: "Andromeda".into(),
                },
                Galaxy {
                    xyz_mpc: [-3.0, 1.5, 2.25],
                    log_lum: 9.1,
                    catalogue: Catalogue::LocalVolume,
                    name: "NGC 253 — Sculptor".into(),
                },
                Galaxy {
                    xyz_mpc: [40.0, -12.0, 3.0],
                    log_lum: 11.0,
                    catalogue: Catalogue::Redshift,
                    name: String::new(),
                },
            ],
        }
    }

    #[test]
    fn a_file_reads_back_as_written() {
        let g = sample();
        assert_eq!(Galaxies::decode(&g.encode()), Ok(g));
    }

    #[test]
    fn a_cut_or_foreign_file_is_refused() {
        let bytes = sample().encode();
        assert_eq!(
            Galaxies::decode(&bytes[..bytes.len() - 3]),
            Err(GalaxyFileError::Truncated)
        );
        assert_eq!(
            Galaxies::decode(b"CARTAREA\x01\0\0\0"),
            Err(GalaxyFileError::NotAGalaxyFile)
        );
        let mut other = bytes.clone();
        other[8] = 9;
        assert_eq!(Galaxies::decode(&other), Err(GalaxyFileError::Format(9)));
    }

    #[test]
    fn a_long_name_is_cut_on_a_character() {
        let mut g = sample();
        g.galaxies[0].name = "é".repeat(200);
        let back = Galaxies::decode(&g.encode()).unwrap();
        assert_eq!(back.galaxies[0].name, "é".repeat(127));
    }
}
