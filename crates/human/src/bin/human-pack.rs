//! Write the client's human pack from the MakeHuman and Quaternius sources.
//!
//! ```text
//! cargo run -p cartopolis_human --features pack --bin human-pack -- \
//!     --mpfb <mpfb2>/src/mpfb/data --assets <makehuman_system_assets> \
//!     --library <UAL1 Standard>/Godot/AnimationLibrary_Godot_Standard.glb \
//!     --out assets/models/avatar/human
//! ```
//!
//! The selection — two skins, one outfit each, the clips the avatar plays, and
//! the two bodies whose targets are carried — is fixed here while the avatar
//! has no chooser; `docs/notes/makehuman-avatars.md` says where it goes next.

use std::path::PathBuf;

use cartopolis_human::pack::{build_pack, ProxySource, Sources};
use cartopolis_human::{Phenotype, ProxyKind};

/// The clips the avatar plays, by the library's names.
const CLIPS: &[&str] = &[
    "Idle_Loop",
    "Walk_Loop",
    "Jog_Fwd_Loop",
    "Sprint_Loop",
    "Idle_Talking_Loop",
    "Interact",
    "Roll",
    "Punch_Jab",
    "Punch_Cross",
    "Death01",
    "Sitting_Idle_Loop",
    "Jump_Loop",
    "Swim_Fwd_Loop",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut mpfb, mut assets, mut library, mut out) = (None, None, None, None);
    while let Some(a) = args.next() {
        let v = args.next().map(PathBuf::from);
        match a.as_str() {
            "--mpfb" => mpfb = v,
            "--assets" => assets = v,
            "--library" => library = v,
            "--out" => out = v,
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let (Some(mpfb), Some(assets), Some(library), Some(out)) = (mpfb, assets, library, out) else {
        eprintln!("usage: human-pack --mpfb DIR --assets DIR --library GLB --out DIR");
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
        library,
        clips: CLIPS.iter().map(|s| s.to_string()).collect(),
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
    let missing: Vec<&str> = CLIPS
        .iter()
        .copied()
        .filter(|c| !pack.clips.iter().any(|k| k.name == *c))
        .collect();
    if !missing.is_empty() {
        eprintln!("human-pack: the library has no clip called {missing:?}");
        std::process::exit(1);
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
