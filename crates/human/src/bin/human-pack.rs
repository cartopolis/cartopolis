//! Write the client's human pack from the MakeHuman and motion-capture sources.
//!
//! ```text
//! cargo run -p cartopolis_human --features pack --bin human-pack -- \
//!     --mpfb <mpfb2>/src/mpfb/data --assets <makehuman_system_assets> \
//!     --style <100STYLE> --cmu <cmu-mocap BVH dir> --out assets/models/avatar/human
//! ```
//!
//! The selection — two skins, one outfit each, the clips the avatar plays and
//! where in which take each is cut, and the two bodies whose targets are
//! carried — is fixed here while the avatar has no chooser;
//! `docs/notes/makehuman-avatars.md` says where it goes next.

use std::path::{Path, PathBuf};

use cartopolis_human::mocap::{Cut, CutKind};
use cartopolis_human::pack::{build_pack, ProxySource, Sources};
use cartopolis_human::{Phenotype, ProxyKind};

/// The locomotion, from 100STYLE's Neutral style. The frame ranges are the
/// dataset's own `Frame_Cuts.csv`, which trims the calibration T-poses at
/// either end of each take; the loop lengths bracket one stride pair (left and
/// right foot), the unit a walk or run repeats in.
fn style_cuts(style: &Path) -> Vec<Cut> {
    let take = |f: &str| style.join("Neutral").join(f);
    vec![
        Cut {
            name: "idle".into(),
            file: take("Neutral_ID.bvh"),
            start: Some(655),
            end: Some(1555),
            kind: CutKind::Loop {
                min_s: 3.0,
                max_s: 7.0,
            },
            fps: None,
        },
        Cut {
            name: "walk".into(),
            file: take("Neutral_FW.bvh"),
            start: Some(327),
            end: Some(7445),
            kind: CutKind::Loop {
                min_s: 0.9,
                max_s: 1.5,
            },
            fps: None,
        },
        Cut {
            name: "run".into(),
            file: take("Neutral_FR.bvh"),
            start: Some(357),
            end: Some(4227),
            kind: CutKind::Loop {
                min_s: 0.5,
                max_s: 0.95,
            },
            fps: None,
        },
    ]
}

/// The emotes and the seat, from the CMU database (the cgspeed BVH conversion,
/// mirrored at github.com/una-dinosauria/cmu-mocap). Frame numbers are the
/// file's; frame 0 of every file is a T-pose the conversion added. Subjects 141
/// and 143 were captured at 60 fps though their files say 120 — a jump in
/// them falls at four times gravity.
fn cmu_cuts(cmu: &Path) -> Vec<Cut> {
    let once = |name: &str, file: &str, start, end, fps: Option<f32>| Cut {
        name: name.into(),
        file: cmu.join(format!("{file}.bvh")),
        start: Some(start),
        end: Some(end),
        kind: CutKind::Once,
        fps,
    };
    vec![
        // 141_16 "Wave Hello": the right hand above the head, about 1.6 s.
        once("wave", "141_16", 54, 151, Some(60.0)),
        // 144_24 "Reach_Right": a hand straight ahead to a button's height.
        once("interact", "144_24", 781, 931, None),
        // 127_23 "Run Dive Over Roll Run": the roll, its four metres taken out.
        once("roll", "127_23", 25, 245, None),
        // 143_23 "Punching": a jab and a right cross from a guard.
        once("punch", "143_23", 50, 180, Some(60.0)),
        // 141_14: a right kick from a relaxed stance, in place.
        once("kick", "141_14", 225, 330, Some(60.0)),
        // 90_18 "RugPullFall": there is no faint in the database; the feet
        // slip and he lands on his back.
        once("faint", "90_18", 60, 300, None),
        // 13_04 "sit on stepstool": the still, seated stretch, hands on thighs.
        Cut {
            kind: CutKind::Loop {
                min_s: 2.5,
                max_s: 3.8,
            },
            ..once("sit", "13_04", 4083, 4533, None)
        },
    ]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut mpfb, mut assets, mut style, mut cmu, mut out) = (None, None, None, None, None);
    while let Some(a) = args.next() {
        let v = args.next().map(PathBuf::from);
        match a.as_str() {
            "--mpfb" => mpfb = v,
            "--assets" => assets = v,
            "--style" => style = v,
            "--cmu" => cmu = v,
            "--out" => out = v,
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let (Some(mpfb), Some(assets), Some(style), Some(cmu), Some(out)) =
        (mpfb, assets, style, cmu, out)
    else {
        eprintln!("usage: human-pack --mpfb DIR --assets DIR --style DIR --cmu DIR --out DIR");
        std::process::exit(2);
    };
    let proxy = |kind, rel: &str| ProxySource {
        kind,
        mhclo: assets.join(rel),
        material: None,
    };
    let sources = Sources {
        mpfb_data: mpfb,
        skins: vec![
            (
                "male".into(),
                assets.join("skins/young_caucasian_male/young_caucasian_male.mhmat"),
            ),
            (
                "female".into(),
                assets.join("skins/young_caucasian_female/young_caucasian_female.mhmat"),
            ),
        ],
        proxies: vec![
            ProxySource {
                material: Some(assets.join("eyes/materials/brown.mhmat")),
                ..proxy(ProxyKind::Eyes, "eyes/high-poly/high-poly.mhclo")
            },
            proxy(ProxyKind::Eyebrows, "eyebrows/eyebrow001/eyebrow001.mhclo"),
            proxy(
                ProxyKind::Eyelashes,
                "eyelashes/eyelashes01/eyelashes01.mhclo",
            ),
            proxy(ProxyKind::Hair, "hair/short02/short02.mhclo"),
            proxy(
                ProxyKind::Clothes,
                "clothes/male_casualsuit06/male_casualsuit06.mhclo",
            ),
            proxy(
                ProxyKind::Clothes,
                "clothes/female_casualsuit01/female_casualsuit01.mhclo",
            ),
            proxy(ProxyKind::Clothes, "clothes/shoes06/shoes06.mhclo"),
        ],
        cuts: style_cuts(&style)
            .into_iter()
            .chain(cmu_cuts(&cmu))
            .collect(),
        bodies: vec![Phenotype::male(), Phenotype::female()],
        textures_out: out.join("textures"),
    };
    let pack = match build_pack(&sources) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("human-pack: {e}");
            std::process::exit(1);
        }
    };
    for c in &pack.clips {
        println!(
            "  {:<8} {:>5.2} s  {:>3} frames  {}",
            c.name,
            (c.frames - 1) as f32 / c.fps,
            c.frames,
            if c.speed > 0.0 {
                format!("{:.2} hip heights/s", c.speed)
            } else {
                "on the spot".to_string()
            }
        );
    }
    let bytes = pack.encode().expect("a pack encodes");
    let path = out.join("default.humanpack");
    if let Err(e) = std::fs::write(&path, &bytes) {
        eprintln!("human-pack: {}: {e}", path.display());
        std::process::exit(1);
    }
    println!(
        "{}: {} KB — {} vertices, {} targets, {} bones, {} proxies, {} clips",
        path.display(),
        bytes.len() / 1024,
        pack.vertices.len(),
        pack.targets.len(),
        pack.bones.len(),
        pack.proxies.len(),
        pack.clips.len()
    );
}
