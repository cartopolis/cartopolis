//! The world's countries as the globe names them, from an embedded table
//! (`assets/globe/countries.tsv`, written by `atlas countries` from Natural
//! Earth's admin-0): where each name goes, how many live there, and from how
//! far out it is written. The vector tiles carry the borders but neither a
//! country's name at a sensible point nor its population.

use std::sync::OnceLock;

static TSV: &str = include_str!("../../../assets/globe/countries.tsv");

/// One country.
#[derive(Debug, Clone, PartialEq)]
pub struct Country {
    /// Natural Earth's `ADM0_A3`.
    pub a3: String,
    pub name: String,
    /// Where the name is written: inside the main land, not the centroid.
    pub lat: f64,
    pub lng: f64,
    /// Natural Earth's estimate; 0 where it has none.
    pub population: u64,
    /// The web-map zoom a cartographer first writes the name at: 1.7 for
    /// Russia or France, 4 for the Netherlands, 5–6 for the smallest.
    pub min_label: f32,
}

fn parse(tsv: &str) -> Vec<Country> {
    tsv.lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split('\t');
            Some(Country {
                a3: f.next()?.to_owned(),
                name: f.next()?.to_owned(),
                lng: f.next()?.parse().ok()?,
                lat: f.next()?.parse().ok()?,
                population: f.next()?.parse().ok()?,
                min_label: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Every country, biggest names first.
pub fn all() -> &'static [Country] {
    static ALL: OnceLock<Vec<Country>> = OnceLock::new();
    ALL.get_or_init(|| parse(TSV))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_holds_the_world() {
        let all = all();
        assert!(all.len() > 200, "{} countries", all.len());
        let nl = all.iter().find(|c| c.a3 == "NLD").expect("the Netherlands");
        assert!(nl.population > 15_000_000);
        assert!((50.0..54.0).contains(&nl.lat) && (3.0..8.0).contains(&nl.lng));
        assert!(all.windows(2).all(|w| w[0].min_label <= w[1].min_label));
    }
}
