//! The macro sliders: which shape targets a body is made of, and at what weight.
//!
//! This is MakeHuman's own scheme (MPFB's `TargetService`, `macro.json`), kept
//! to the letter so a body here is the body MakeHuman would make from the same
//! values. Each slider splits into one or two named components by linear
//! interpolation inside a band; a target is a product of components, one per
//! slider it depends on, and a product at or under [`CUTOFF`] is dropped.

use serde::{Deserialize, Serialize};

/// MakeHuman's cut-off: a target weighted at or under this is not applied.
pub const CUTOFF: f32 = 0.01;

/// The macro slider values, each 0–1, as MakeHuman defines them: `gender` 0 is
/// female and 1 male, `age` 0.5 is 25 years, and 0.5 is average for the rest.
/// `race` is African, Asian, Caucasian, and is normalised by the targets'
/// weights rather than required to sum to one.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Phenotype {
    pub gender: f32,
    pub age: f32,
    pub muscle: f32,
    pub weight: f32,
    pub height: f32,
    pub proportions: f32,
    pub cupsize: f32,
    pub firmness: f32,
    pub race: [f32; 3],
}

impl Default for Phenotype {
    fn default() -> Self {
        Self {
            gender: 0.5,
            age: 0.5,
            muscle: 0.5,
            weight: 0.5,
            height: 0.5,
            proportions: 0.5,
            cupsize: 0.5,
            firmness: 0.5,
            race: [1.0 / 3.0; 3],
        }
    }
}

impl Phenotype {
    pub fn male() -> Self {
        Self {
            gender: 1.0,
            ..Self::default()
        }
    }

    pub fn female() -> Self {
        Self {
            gender: 0.0,
            ..Self::default()
        }
    }
}

/// One band of a slider: inside `(lowest, highest)` the value splits into
/// `low` and `high`; an empty name contributes nothing.
struct Part {
    lowest: f32,
    highest: f32,
    low: &'static str,
    high: &'static str,
}

const fn part(lowest: f32, highest: f32, low: &'static str, high: &'static str) -> Part {
    Part {
        lowest,
        highest,
        low,
        high,
    }
}

// `macro.json`'s bands, verbatim. The bounds are exclusive and the gaps between
// them (0.49998–0.49999) are MakeHuman's; at a gap value no component is made.
const GENDER: &[Part] = &[part(-0.01, 1.01, "female", "male")];
const AGE: &[Part] = &[
    part(-0.01, 0.187_499_8, "baby", "child"),
    part(0.187_499_9, 0.499_98, "child", "young"),
    part(0.499_99, 1.01, "young", "old"),
];
const MUSCLE: &[Part] = &[
    part(-0.01, 0.499_98, "minmuscle", "averagemuscle"),
    part(0.499_99, 1.01, "averagemuscle", "maxmuscle"),
];
const WEIGHT: &[Part] = &[
    part(-0.01, 0.499_98, "minweight", "averageweight"),
    part(0.499_99, 1.01, "averageweight", "maxweight"),
];
const PROPORTIONS: &[Part] = &[
    part(-0.01, 0.4999, "uncommonproportions", ""),
    part(0.5, 1.01, "", "idealproportions"),
];
const HEIGHT: &[Part] = &[
    part(-0.01, 0.49, "minheight", ""),
    part(0.51, 1.01, "", "maxheight"),
];
const CUPSIZE: &[Part] = &[
    part(-0.01, 0.499_98, "mincup", "averagecup"),
    part(0.499_99, 1.01, "averagecup", "maxcup"),
];
const FIRMNESS: &[Part] = &[
    part(-0.01, 0.4998, "minfirmness", "averagefirmness"),
    part(0.4999, 1.01, "averagefirmness", "maxfirmness"),
];
const RACES: [&str; 3] = ["african", "asian", "caucasian"];

fn round4(x: f32) -> f32 {
    (x * 10_000.0).round() / 10_000.0
}

fn components(parts: &[Part], value: f32) -> Vec<(&'static str, f32)> {
    let mut out = Vec::new();
    for p in parts {
        if value > p.lowest && value < p.highest {
            let t = (value - p.lowest) / (p.highest - p.lowest);
            if !p.low.is_empty() {
                out.push((p.low, round4(1.0 - t)));
            }
            if !p.high.is_empty() {
                out.push((p.high, round4(t)));
            }
        }
    }
    out
}

/// The targets this phenotype is made of, as `(name, weight)`; names are paths
/// under MakeHuman's `targets/` without the `.target` extension.
#[must_use]
pub fn macro_targets(p: &Phenotype) -> Vec<(String, f32)> {
    let gender = components(GENDER, p.gender);
    let age = components(AGE, p.age);
    let muscle = components(MUSCLE, p.muscle);
    let weight = components(WEIGHT, p.weight);
    let proportions = components(PROPORTIONS, p.proportions);
    let height = components(HEIGHT, p.height);
    let cup = components(CUPSIZE, p.cupsize);
    let firm = components(FIRMNESS, p.firmness);
    let mut out = Vec::new();
    let mut push = |name: String, w: f32| {
        if w > CUTOFF {
            out.push((name, w));
        }
    };

    for (race, &rw) in RACES.iter().zip(&p.race) {
        if rw <= 0.0001 {
            continue;
        }
        for &(a, aw) in &age {
            for &(g, gw) in &gender {
                push(format!("macrodetails/{race}-{g}-{a}"), rw * gw * aw);
            }
        }
    }
    for &(g, gw) in &gender {
        for &(a, aw) in &age {
            for &(m, mw) in &muscle {
                for &(w, ww) in &weight {
                    let stem = format!("{g}-{a}-{m}-{w}");
                    let base = gw * aw * mw * ww;
                    push(format!("macrodetails/universal-{stem}"), base);
                    for &(h, hw) in &height {
                        push(format!("macrodetails/height/{stem}-{h}"), base * hw);
                    }
                    // There are no baby proportions targets on disk.
                    if a != "baby" {
                        for &(pr, pw) in &proportions {
                            push(format!("macrodetails/proportions/{stem}-{pr}"), base * pw);
                        }
                    }
                    // Breasts are female-only targets with no male complement,
                    // so their weight leaves gender out (MakeHuman's comment).
                    if g == "female" && a != "baby" {
                        for &(c, cw) in &cup {
                            for &(f, fw) in &firm {
                                if c == "averagecup" && f == "averagefirmness" {
                                    continue;
                                }
                                push(
                                    format!("breast/female-{a}-{m}-{w}-{c}-{f}"),
                                    aw * mw * ww * cw * fw,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MakeHuman's default human, as MPFB lists it: eight targets.
    #[test]
    fn the_default_human_is_eight_targets() {
        let t = macro_targets(&Phenotype::default());
        assert_eq!(t.len(), 8, "{t:?}");
        for race in RACES {
            assert!(t
                .iter()
                .any(|(n, _)| n == &format!("macrodetails/{race}-male-young")));
        }
        assert!(t
            .iter()
            .any(|(n, _)| n.starts_with("macrodetails/universal-female-young-averagemuscle")));
    }

    /// A man is a man's targets: nothing female survives the cut-off, and no
    /// breast target either, though their weight ignores gender.
    #[test]
    fn a_man_has_no_female_targets() {
        let t = macro_targets(&Phenotype::male());
        assert!(t.iter().all(|(n, _)| !n.contains("female")), "{t:?}");
        let universal: f32 = t
            .iter()
            .filter(|(n, _)| n.starts_with("macrodetails/universal-"))
            .map(|(_, w)| w)
            .sum();
        assert!(
            (universal - 1.0).abs() < 0.02,
            "the universal weights sum to one, got {universal}"
        );
    }

    #[test]
    fn a_tall_woman_takes_the_height_target() {
        let t = macro_targets(&Phenotype {
            height: 1.0,
            ..Phenotype::female()
        });
        assert!(
            t.iter().any(|(n, w)| n.ends_with("-maxheight") && *w > 0.9),
            "{t:?}"
        );
    }
}
