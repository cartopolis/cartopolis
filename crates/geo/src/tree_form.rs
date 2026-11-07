//! What a tree's species says about its shape, and the crown each shape is
//! built from.
//!
//! Until 2026-11-07 every tree was one lumpy ball on a post: a lime on a canal,
//! a Lombardy poplar and a spruce differed only in size. Groningen's register
//! names the species of all but 155 of its 126,016 trees (`LATIJNSE_NAAM`), and
//! OSM's `natural=tree` carries `genus` / `species` / `leaf_type` where somebody
//! surveyed it, so the shape is real data rather than a guess. Seven forms are
//! enough to tell a street apart — they are the silhouettes a passer-by knows a
//! tree by, not a botanical classification:
//!
//! | form | silhouette | e.g. |
//! |---|---|---|
//! | [`TreeForm::Round`] | the old ball | no species known — and every scattered tree |
//! | [`TreeForm::Broad`] | a wide crown of a few masses | oak, ash, maple, lime, elm, plane |
//! | [`TreeForm::Upright`] | a narrow oval | birch, alder, rowan, pear, sweetgum |
//! | [`TreeForm::Columnar`] | a slim column | Lombardy poplar, `'Fastigiata'` cultivars |
//! | [`TreeForm::Weeping`] | a dome with a hanging curtain | weeping willow, `'Pendula'` cultivars |
//! | [`TreeForm::Conical`] | a tiered cone from near the ground | spruce, fir, thuja, larch, metasequoia |
//! | [`TreeForm::Pine`] | a flat crown high on a bare stem | pine |
//!
//! **`Round` is exactly the old tree**, vertex for vertex, so a tree whose species
//! nobody recorded looks as it always did. The other forms cost more than its 18
//! vertices (the most, [`TreeForm::Broad`], 54), which is affordable because
//! only surveyed trees get a species: the scatter that fills a wood with 6,000
//! trees a cell has no species to read and stays `Round`.
//!
//! A crown stays inside the cylinder of its radius and the band between the top
//! of its stem and its height, as the ball did — the tunnel and sewer keep-outs
//! clear a tree at its radius and must not be outgrown.

use std::sync::LazyLock;

use crate::scatter::{normalize, unit_canopy};

/// The silhouette a tree is drawn with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TreeForm {
    /// No species known: the ball every tree was before forms existed.
    #[default]
    Round,
    Broad,
    Upright,
    Columnar,
    Weeping,
    Conical,
    Pine,
}

/// Every form, in declaration order.
pub const TREE_FORMS: [TreeForm; 7] = [
    TreeForm::Round,
    TreeForm::Broad,
    TreeForm::Upright,
    TreeForm::Columnar,
    TreeForm::Weeping,
    TreeForm::Conical,
    TreeForm::Pine,
];

/// Cultivar names that mean a column whatever the species: "fastigiate" in its
/// spellings, and the named columnar cultivars that are common in Dutch streets
/// (`'Frans Fontaine'` hornbeam, `'Columella'` elm, `'Obelisk'` ash).
const COLUMNAR_CULTIVARS: &[&str] = &[
    "fastigiat",
    "italica",
    "columna",
    "columella",
    "obelisk",
    "pyramidal",
    "erect",
    "stricta",
    "frans fontaine",
    "lucas",
];

/// Cultivar names that mean a weeping crown. Matched only inside the quoted
/// cultivar: `Betula pendula`, the silver birch, is a species and does not weep.
const WEEPING_CULTIVARS: &[&str] = &["pendula", "tristis", "chrysocoma", "youngii"];

/// Species that weep without a cultivar name.
const WEEPING_SPECIES: &[&str] = &["sepulcralis", "babylonica"];

const CONICAL_GENERA: &[&str] = &[
    "abies",
    "araucaria",
    "cedrus",
    "chamaecyparis",
    "cryptomeria",
    "cupressocyparis",
    "cupressus",
    "juniperus",
    "larix",
    "metasequoia",
    "picea",
    "pseudotsuga",
    "sequoia",
    "sequoiadendron",
    "taxodium",
    "taxus",
    "thuja",
    "tsuga",
];

const UPRIGHT_GENERA: &[&str] = &[
    "alnus",
    "betula",
    "corylus",
    "ginkgo",
    "liquidambar",
    "liriodendron",
    "ostrya",
    "pyrus",
    "sorbus",
];

/// What a register writes where it does not know the species. Anything else
/// that is not in a list above is a broadleaf, which is what most trees are.
const NOT_A_TAXON: &[&str] = &["onbekend", "unknown", "verwijderd", "overig", "diversen"];

impl TreeForm {
    /// The name the register files carry. `Round` is never written — a tree with
    /// no form is written without one — but parses, so the list stays closed.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Round => "round",
            Self::Broad => "broad",
            Self::Upright => "upright",
            Self::Columnar => "columnar",
            Self::Weeping => "weeping",
            Self::Conical => "conical",
            Self::Pine => "pine",
        }
    }

    /// The form a slug names; `None` for one this build does not know, which a
    /// reader draws as `Round` rather than refusing the tree.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        TREE_FORMS.into_iter().find(|f| f.slug() == slug)
    }

    /// The form a scientific name implies: `"Tilia x europaea 'Koningslinde'"`,
    /// `"Populus nigra 'Italica'"`, or a bare genus as OSM's `genus` tag carries
    /// it. `None` where the text names no species.
    ///
    /// The cultivar is read first, because it overrides the genus: a
    /// `'Fastigiata'` oak is a column, not a broad crown.
    #[must_use]
    pub fn from_taxon(name: &str) -> Option<Self> {
        let lower = name.trim().to_lowercase();
        // "Tilia x europaea", "x Cupressocyparis leylandii": the hybrid sign is
        // not the genus.
        let mut words = lower.split_whitespace().filter(|w| *w != "x" && *w != "×");
        let genus = words.next()?;
        let species = words.next().unwrap_or("");
        if NOT_A_TAXON.contains(&genus) || !genus.chars().all(char::is_alphabetic) {
            return None;
        }
        let rest: Vec<&str> = words.collect();
        let cultivar = cultivar(&lower, &rest);
        if COLUMNAR_CULTIVARS.iter().any(|c| cultivar.contains(c)) {
            return Some(Self::Columnar);
        }
        if WEEPING_CULTIVARS.iter().any(|c| cultivar.contains(c))
            || WEEPING_SPECIES.contains(&species)
        {
            return Some(Self::Weeping);
        }
        Some(if genus == "pinus" {
            Self::Pine
        } else if CONICAL_GENERA.contains(&genus) {
            Self::Conical
        } else if UPRIGHT_GENERA.contains(&genus) {
            Self::Upright
        } else {
            Self::Broad
        })
    }

    /// Crown radius as a multiple of what a tree of this height is given when
    /// nobody measured its crown — a column is a third as wide as a lime.
    #[must_use]
    pub fn width(self) -> f32 {
        match self {
            Self::Round => 1.0,
            Self::Broad => 1.1,
            Self::Upright => 0.72,
            Self::Columnar => 0.38,
            Self::Weeping => 1.15,
            Self::Conical => 0.6,
            Self::Pine => 0.85,
        }
    }

    /// Where on the palette's two-tone ramp a tree of this form sits, from a
    /// uniform draw `r`: conifers deep, birch and willow light, the rest spread
    /// across it as every tree was.
    #[must_use]
    pub fn tone(self, r: f32) -> f32 {
        match self {
            Self::Conical | Self::Pine => 0.75 + 0.25 * r,
            Self::Upright | Self::Weeping => 0.25 * r,
            Self::Round | Self::Broad | Self::Columnar => r,
        }
    }

    /// Brightness on top of the per-tree jitter: the light-leaved forms a touch
    /// paler, so a birch next to an oak is not the same green.
    #[must_use]
    pub fn luma(self) -> f32 {
        match self {
            Self::Upright | Self::Weeping => 1.06,
            _ => 1.0,
        }
    }

    pub(crate) fn shape(self) -> &'static Shape {
        &SHAPES[self as usize]
    }
}

/// What the BGT says grows in a wood (`begroeidterreindeel.fysiek_voorkomen`):
/// the kind of wood, not the species, so it decides the scattered trees' forms
/// in proportion rather than one by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WoodKind {
    Broadleaf,
    Conifer,
    Mixed,
}

impl WoodKind {
    /// The kind a `formats::bgt::WOOD_KINDS` slug names.
    #[must_use]
    pub fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "broadleaf" => Self::Broadleaf,
            "conifer" => Self::Conifer,
            "mixed" => Self::Mixed,
            _ => return None,
        })
    }

    /// The form a scattered tree takes in a wood of this kind, from a uniform
    /// draw `r`.
    ///
    /// **The proportions are the one judgement here.** The BGT says conifer, not
    /// which: the Dutch forest inventory has Scots and Corsican pine as about two
    /// thirds of the country's conifer stands, Douglas fir, larch and spruce the
    /// rest, so a conifer wood is 70 % pine and 30 % cone; a mixed wood is half
    /// broadleaf and half that. A broadleaf wood stays the old ball: its species
    /// are unknown, and the ball is the cheapest crown in a cell of 6,000 trees.
    #[must_use]
    pub fn form(self, r: f32) -> TreeForm {
        let conifer = |r: f32| {
            if r < 0.7 {
                TreeForm::Pine
            } else {
                TreeForm::Conical
            }
        };
        match self {
            Self::Broadleaf => TreeForm::Round,
            Self::Conifer => conifer(r),
            Self::Mixed if r < 0.5 => TreeForm::Round,
            Self::Mixed => conifer((r - 0.5) * 2.0),
        }
    }
}

/// The cultivar: the text between its quotes, as Groningen and Amsterdam write
/// it (`Carpinus betulus 'Fastigiata'`), or where there are no quotes every word
/// after the species, as Nijmegen writes it (`Carpinus betulus Fastigiata 0`).
fn cultivar(name: &str, after_species: &[&str]) -> String {
    let quote = |c: char| matches!(c, '\'' | '"' | '‘' | '’' | '`');
    match name.find(quote) {
        Some(i) => {
            let rest = &name[i + name[i..].chars().next().map_or(1, char::len_utf8)..];
            rest.find(quote).map_or(rest, |j| &rest[..j]).to_string()
        }
        None => after_species.join(" "),
    }
}

// ─── Crowns ──────────────────────────────────────────────────────────────────

/// One form's crown in unit space — inside the unit cylinder, `y` from −1 at the
/// crown's foot to 1 at its top — and how it stands on its stem.
pub(crate) struct Shape {
    /// Where the crown starts, as a fraction of the tree's height.
    pub crown_bottom: f32,
    /// Stem half-width as a fraction of the crown radius.
    pub trunk: f32,
    /// Turn each tree by its own angle, so a row of one species does not repeat
    /// one outline. Off for `Round`, which must stay the old tree exactly.
    pub turns: bool,
    pub verts: Vec<[f32; 3]>,
    /// Unit-space normals: the builder divides by the tree's scale, which is the
    /// inverse transpose of stretching the crown to its radius and height.
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// An ellipsoid mass: a centre and three radii, in unit space.
struct Lobe([f32; 3], [f32; 3]);

/// A surface of revolution: `(y, radius)` rings from the top down, between a
/// pole at `top` and one at `bottom`.
struct Lathe {
    top: f32,
    rings: &'static [(f32, f32)],
    bottom: f32,
}

/// Segments round a lathe. Eight keep a column round from the street; six
/// showed corners on a cone seen from above.
const LATHE_SEGMENTS: usize = 8;

static SHAPES: LazyLock<[Shape; 7]> = LazyLock::new(|| TREE_FORMS.map(build));

fn build(form: TreeForm) -> Shape {
    let (crown_bottom, trunk) = match form {
        TreeForm::Round => (0.30, 0.09),
        TreeForm::Broad => (0.28, 0.10),
        TreeForm::Upright => (0.22, 0.08),
        TreeForm::Columnar => (0.10, 0.14),
        TreeForm::Weeping => (0.14, 0.10),
        TreeForm::Conical => (0.06, 0.12),
        TreeForm::Pine => (0.55, 0.07),
    };
    let mut shape = Shape {
        crown_bottom,
        trunk,
        turns: form != TreeForm::Round,
        verts: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
    };
    match form {
        TreeForm::Round => lobes(&mut shape, &[Lobe([0.0; 3], [1.0; 3])]),
        // Three masses rather than one ball: what makes a crown read as leaves
        // on branches rather than as a sphere.
        TreeForm::Broad => lobes(
            &mut shape,
            &[
                Lobe([-0.36, -0.12, 0.12], [0.62, 0.72, 0.62]),
                Lobe([0.36, -0.05, -0.12], [0.62, 0.75, 0.62]),
                Lobe([0.0, 0.32, 0.18], [0.62, 0.66, 0.62]),
            ],
        ),
        TreeForm::Upright => lobes(
            &mut shape,
            &[
                Lobe([0.0, -0.38, 0.0], [0.9, 0.6, 0.9]),
                Lobe([0.1, 0.35, -0.06], [0.72, 0.64, 0.72]),
            ],
        ),
        TreeForm::Pine => lobes(
            &mut shape,
            &[
                Lobe([-0.3, -0.1, 0.05], [0.68, 0.55, 0.68]),
                Lobe([0.32, 0.2, -0.1], [0.62, 0.5, 0.62]),
            ],
        ),
        TreeForm::Columnar => lathe(
            &mut shape,
            &Lathe {
                top: 1.0,
                rings: &[(0.8, 0.45), (0.45, 0.85), (-0.1, 1.0), (-0.65, 0.9)],
                bottom: -1.0,
            },
        ),
        // A dome, and a curtain that hangs nearly straight; the foot is drawn up
        // inside it, so from below the crown is hollow as a willow is.
        TreeForm::Weeping => lathe(
            &mut shape,
            &Lathe {
                top: 1.0,
                rings: &[
                    (0.8, 0.55),
                    (0.4, 0.9),
                    (-0.1, 1.0),
                    (-0.6, 0.97),
                    (-1.0, 0.9),
                ],
                bottom: -0.7,
            },
        ),
        // Tiers: each ring that steps in under a wider one is a whorl of
        // branches, which is what tells a spruce from a cone.
        TreeForm::Conical => lathe(
            &mut shape,
            &Lathe {
                top: 1.0,
                rings: &[
                    (0.45, 0.4),
                    (0.3, 0.3),
                    (-0.2, 0.7),
                    (-0.35, 0.58),
                    (-0.9, 1.0),
                ],
                bottom: -1.0,
            },
        ),
    }
    shape
}

/// Ellipsoid lobes from the shared unit sphere, each with the analytic normal of
/// its own ellipsoid.
fn lobes(shape: &mut Shape, lobes: &[Lobe]) {
    let unit = unit_canopy();
    for Lobe(c, r) in lobes {
        let base = shape.verts.len() as u32;
        for p in &unit.verts {
            shape
                .verts
                .push([c[0] + p[0] * r[0], c[1] + p[1] * r[1], c[2] + p[2] * r[2]]);
            shape
                .normals
                .push(normalize([p[0] / r[0], p[1] / r[1], p[2] / r[2]]));
        }
        shape.indices.extend(unit.indices.iter().map(|i| base + i));
    }
}

/// A surface of revolution, wound outward like the unit sphere, with smooth
/// normals averaged from its faces.
fn lathe(shape: &mut Shape, lathe: &Lathe) {
    let n = LATHE_SEGMENTS;
    let top = shape.verts.len() as u32;
    shape.verts.push([0.0, lathe.top, 0.0]);
    for &(y, r) in lathe.rings {
        for j in 0..n {
            let a = std::f32::consts::TAU * j as f32 / n as f32;
            shape.verts.push([a.cos() * r, y, a.sin() * r]);
        }
    }
    let bottom = shape.verts.len() as u32;
    shape.verts.push([0.0, lathe.bottom, 0.0]);

    let ring = |i: usize, j: usize| top + 1 + (i * n + j % n) as u32;
    for j in 0..n {
        shape.indices.extend([top, ring(0, j + 1), ring(0, j)]);
    }
    for i in 0..lathe.rings.len() - 1 {
        for j in 0..n {
            let (u0, u1, l0, l1) = (
                ring(i, j),
                ring(i, j + 1),
                ring(i + 1, j),
                ring(i + 1, j + 1),
            );
            shape.indices.extend([u0, l1, l0, u0, u1, l1]);
        }
    }
    let last = lathe.rings.len() - 1;
    for j in 0..n {
        shape
            .indices
            .extend([bottom, ring(last, j), ring(last, j + 1)]);
    }

    let first = top as usize;
    let mut normals = vec![[0.0f32; 3]; shape.verts.len() - first];
    for t in shape.indices[shape.indices.len() - 6 * n * lathe.rings.len()..].chunks(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| shape.verts[i as usize]);
        let (e, f) = (sub(b, a), sub(c, a));
        let face = [
            e[1] * f[2] - e[2] * f[1],
            e[2] * f[0] - e[0] * f[2],
            e[0] * f[1] - e[1] * f[0],
        ];
        for &i in t {
            let acc = &mut normals[i as usize - first];
            for k in 0..3 {
                acc[k] += face[k];
            }
        }
    }
    shape.normals.extend(normals.into_iter().map(normalize));
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip_and_are_unique() {
        for f in TREE_FORMS {
            assert_eq!(TreeForm::from_slug(f.slug()), Some(f));
            assert_eq!(TREE_FORMS[f as usize], f, "TREE_FORMS out of order");
        }
        assert_eq!(TreeForm::from_slug("baobab"), None);
    }

    /// The names as Groningen's register spells them — the most common entries
    /// and the traps among them, fetched 2026-11-07.
    #[test]
    fn species_names_give_their_silhouettes() {
        use TreeForm::*;
        for (name, form) in [
            ("Quercus robur", Broad),
            ("Tilia x europaea 'Koningslinde'", Broad),
            ("Fraxinus excelsior 'Westhof's Glorie'", Broad),
            ("Betula pendula", Upright),
            ("Alnus glutinosa", Upright),
            ("Pyrus calleryana 'Chanticleer'", Upright),
            ("Populus nigra 'Italica'", Columnar),
            ("Carpinus betulus 'Fastigiata'", Columnar),
            ("Carpinus betulus 'Frans Fontaine'", Columnar),
            ("Quercus robur 'Fastigiata'", Columnar),
            ("Ulmus 'Columella'", Columnar),
            ("Fraxinus ornus 'Obelisk'", Columnar),
            ("Salix sepulcralis 'Chrysocoma'", Weeping),
            ("Salix alba 'Tristis'", Weeping),
            ("Betula pendula 'Youngii'", Weeping),
            ("Picea abies", Conical),
            ("Metasequoia glyptostroboides", Conical),
            ("x Cupressocyparis leylandii", Conical),
            ("Pinus nigra", Pine),
            ("Tilia", Broad),
            ("Vruchtboom", Broad),
            // Nijmegen writes the cultivar bare, with a suffix of its own.
            ("Carpinus betulus Fastigiata 0", Columnar),
            ("Quercus robur Fastigiata", Columnar),
            ("Betula pendula 0", Upright),
            ("Tilia x europaea -", Broad),
            ("Robinia pseudoacacia (risico biodiversiteit)", Broad),
        ] {
            assert_eq!(TreeForm::from_taxon(name), Some(form), "{name}");
        }
        for name in ["", "  ", "Onbekend", "Verwijderd", "?"] {
            assert_eq!(TreeForm::from_taxon(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_wood_plants_in_its_proportions() {
        let count = |kind: WoodKind, form: TreeForm| {
            (0..1000)
                .filter(|i| kind.form((*i as f32 + 0.5) / 1000.0) == form)
                .count()
        };
        assert_eq!(count(WoodKind::Broadleaf, TreeForm::Round), 1000);
        assert_eq!(count(WoodKind::Conifer, TreeForm::Pine), 700);
        assert_eq!(count(WoodKind::Conifer, TreeForm::Conical), 300);
        assert_eq!(count(WoodKind::Mixed, TreeForm::Round), 500);
        assert_eq!(count(WoodKind::Mixed, TreeForm::Pine), 350);
        assert_eq!(WoodKind::from_slug("mixed"), Some(WoodKind::Mixed));
        assert_eq!(WoodKind::from_slug("jungle"), None);
    }

    /// The keep-outs clear a tree at its radius and between its foot and its
    /// height; no crown may reach past either.
    #[test]
    fn every_crown_stays_inside_its_cylinder() {
        for f in TREE_FORMS {
            let s = f.shape();
            for v in &s.verts {
                let r = (v[0] * v[0] + v[2] * v[2]).sqrt();
                assert!(r <= 1.0 + 1e-3, "{f:?} reaches {r}");
                assert!(v[1].abs() <= 1.0 + 1e-3, "{f:?} at y {}", v[1]);
            }
        }
    }

    #[test]
    fn every_crown_is_well_formed() {
        for f in TREE_FORMS {
            let s = f.shape();
            assert_eq!(s.normals.len(), s.verts.len(), "{f:?}");
            assert_eq!(s.indices.len() % 3, 0, "{f:?}");
            assert!(s.indices.iter().all(|&i| (i as usize) < s.verts.len()));
            for n in &s.normals {
                let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                assert!((len - 1.0).abs() < 1e-3, "{f:?}: normal {n:?}");
            }
        }
    }

    /// Faces point out: a normal agrees with the direction from the crown's
    /// axis, or the material's culling eats the side facing the camera.
    #[test]
    fn every_crown_is_wound_outward() {
        for f in TREE_FORMS {
            let s = f.shape();
            for t in s.indices.chunks(3) {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| s.verts[i as usize]);
                let (e, g) = (sub(b, a), sub(c, a));
                let face = [
                    e[1] * g[2] - e[2] * g[1],
                    e[2] * g[0] - e[0] * g[2],
                    e[0] * g[1] - e[1] * g[0],
                ];
                let n = s.normals[t[0] as usize];
                let dot = face[0] * n[0] + face[1] * n[1] + face[2] * n[2];
                assert!(dot > 0.0, "{f:?}: a face winds inward");
            }
        }
    }

    /// The old tree, exactly — what every unclassified tree must still be.
    #[test]
    fn round_is_the_old_ball() {
        let s = TreeForm::Round.shape();
        let unit = unit_canopy();
        assert_eq!(s.verts, unit.verts);
        assert_eq!(s.indices, unit.indices);
        assert!(!s.turns);
        assert_eq!(s.crown_bottom, 0.30);
    }

    /// The per-tree cost: a form that silently grows multiplies every surveyed
    /// street's triangle count.
    #[test]
    fn crowns_stay_small() {
        for f in TREE_FORMS {
            let s = f.shape();
            assert!(s.verts.len() <= 54, "{f:?}: {} verts", s.verts.len());
            assert!(
                s.indices.len() / 3 <= 96,
                "{f:?}: {} tris",
                s.indices.len() / 3
            );
        }
    }
}
