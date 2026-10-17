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
use cartopolis_human::pack::{build_pack, ProxySource, Sources, TargetSet};
use cartopolis_human::ProxyKind;

/// Walking and standing, from 100STYLE. The frame ranges are the
/// dataset's own `Frame_Cuts.csv`, which trims the calibration T-poses at
/// either end of each take; the walk loop brackets one stride pair (left and
/// right foot), the unit a walk repeats in.
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
        // Rushed, not Neutral: Neutral's walk is 0.86 m/s on the default man and
        // the game walks at 1.8, which played it at twice its own cadence.
        // Rushed's is 1.65 m/s with a 0.9 s cycle (`--probe`, 2026-10-17).
        Cut {
            name: "walk".into(),
            file: style.join("Rushed").join("Rushed_FW.bvh"),
            start: Some(343),
            end: Some(3265),
            kind: CutKind::Loop {
                min_s: 0.9,
                max_s: 1.5,
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
        // 09_01 "run": a real run, 3.46 m/s with a 0.73 s cycle on the default
        // man. Every 100STYLE "run" is a jog (2.3 m/s at best) and the game runs
        // at 6 m/s.
        Cut {
            kind: CutKind::Loop {
                min_s: 0.45,
                max_s: 0.9,
            },
            ..once("run", "09_01", 1, 149, None)
        },
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

/// `--probe STYLE_DIR`: cut a walk and a run loop from every style in a
/// 100STYLE directory and print how long and how fast each is, to choose the
/// avatar's gait by. The ranges are the dataset's `Frame_Cuts.csv`.
fn probe(style: &Path) {
    use cartopolis_human::mocap::{clip_from_motion, parse_bvh};
    let cuts = std::fs::read_to_string(style.join("Frame_Cuts.csv")).unwrap_or_default();
    let mpfb_bones = probe_bones();
    for line in cuts.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        let Some(name) = f.first() else { continue };
        for (gait, col, min_s, max_s) in [("FW", 7, 0.9, 1.5), ("FR", 5, 0.5, 0.95)] {
            let file = style.join(name).join(format!("{name}_{gait}.bvh"));
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let range = |i: usize| f.get(i).and_then(|s| s.parse::<usize>().ok());
            let cut = Cut {
                name: format!("{name} {gait}"),
                file,
                start: range(col),
                end: range(col + 1),
                kind: CutKind::Loop { min_s, max_s },
                fps: None,
            };
            match parse_bvh(&text).and_then(|m| clip_from_motion(&m, &cut, &mpfb_bones, 30.0)) {
                Ok(c) => println!(
                    "{:<20} {:>5.2} s cycle  {:>4.2} hip heights/s  ≈{:>4.2} m/s on the default man",
                    cut.name,
                    (c.frames - 1) as f32 / c.fps,
                    c.speed,
                    c.speed * DEFAULT_HIP_HEIGHT
                ),
                Err(e) => println!("{:<20} {e}", cut.name),
            }
        }
    }
}

/// The default man's pelvis above his ankles, metres (`build`'s `hip_height`).
const DEFAULT_HIP_HEIGHT: f32 = 0.87;

/// Just enough of the rig to map a capture onto: the bones' names and parents.
fn probe_bones() -> Vec<cartopolis_human::Bone> {
    let rig = [
        ("Root", None),
        ("pelvis", Some(0)),
        ("spine_01", Some(1)),
        ("thigh_l", Some(1)),
        ("calf_l", Some(3)),
        ("foot_l", Some(4)),
        ("ball_l", Some(5)),
        ("thigh_r", Some(1)),
        ("calf_r", Some(7)),
        ("foot_r", Some(8)),
        ("ball_r", Some(9)),
    ];
    rig.iter()
        .map(|(n, p)| cartopolis_human::Bone {
            name: (*n).into(),
            parent: *p,
            head: vec![],
            tail: vec![],
            roll: 0.0,
            tpose: [0.0; 3],
        })
        .collect()
}

fn main() {
    let all: Vec<String> = std::env::args().collect();
    if let [_, flag, dir] = all.as_slice() {
        if flag == "--probe" {
            probe(Path::new(dir));
            return;
        }
    }
    // `--probe-run FILE…`: the same for single takes of running (CMU's), cut
    // from frame 1 (frame 0 is the conversion's T-pose).
    if all.get(1).is_some_and(|f| f == "--probe-run") {
        use cartopolis_human::mocap::{clip_from_motion, parse_bvh};
        let bones = probe_bones();
        for file in &all[2..] {
            let cut = Cut {
                name: file.clone(),
                file: PathBuf::from(file),
                start: Some(1),
                end: None,
                kind: CutKind::Loop {
                    min_s: 0.45,
                    max_s: 0.9,
                },
                fps: None,
            };
            let probed = std::fs::read_to_string(file)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_bvh(&t))
                .and_then(|m| clip_from_motion(&m, &cut, &bones, 30.0));
            match probed {
                Ok(c) => println!(
                    "{file}: {:.2} s cycle, ≈{:.2} m/s on the default man",
                    (c.frames - 1) as f32 / c.fps,
                    c.speed * DEFAULT_HIP_HEIGHT
                ),
                Err(e) => println!("{file}: {e}"),
            }
        }
        return;
    }
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
        // Every body the sliders can make from 18 up: MakeHuman blends its
        // "child" targets in below 25, so only the baby ones are left out.
        targets: TargetSet::Macros {
            skip: vec!["baby".into()],
        },
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
