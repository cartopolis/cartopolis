//! Write the client's human pack from the MakeHuman and motion-capture sources.
//!
//! ```text
//! cargo run -p cartopolis_human --features pack --bin human-pack -- \
//!     --mpfb <mpfb2>/src/mpfb/data --assets <makehuman_system_assets> \
//!     --style <100STYLE> --cmu <cmu-mocap BVH dir> --out assets/models/avatar/human \
//!     --face-units <faceunits01>/targets/faceunits \
//!     [--community <community items' folders>]
//! ```
//!
//! The selection — the wardrobe, the skins, the eye colours, which clips are cut
//! from which takes — is fixed here; `docs/notes/makehuman-avatars.md` says why
//! each was chosen. `--assets` is the unzipped MakeHuman system assets pack.

use std::path::{Path, PathBuf};

use cartopolis_human::mocap::{Cut, CutKind};
use cartopolis_human::pack::{build_pack, ProxySource, SkinSource, Sources, TargetSet};
use cartopolis_human::{ProxyKind, SkinAge};

/// Standing, from 100STYLE. The frame ranges are the
/// dataset's own `Frame_Cuts.csv`, which trims the calibration T-poses at
/// either end of each take.
fn style_cuts(style: &Path) -> Vec<Cut> {
    let take = |f: &str| style.join("Neutral").join(f);
    vec![Cut {
        name: "idle".into(),
        file: take("Neutral_ID.bvh"),
        start: Some(655),
        end: Some(1555),
        kind: CutKind::Loop {
            min_s: 3.0,
            max_s: 7.0,
        },
        fps: None,
    }]
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
        // 08_08 "walk": an ordinary walk at 1.76 m/s on a 0.92 s cycle, the
        // game's 1.8 near enough to play at its own pace; trunk 3° forward,
        // arms -26° to +19° (`--try-walk`, 2026-10-19, of 23 CMU walks). It
        // replaced 100STYLE's `Rushed` walk (1.65 m/s, but the trunk 20°
        // forward and the arms held behind the body: a funny walk); every
        // 100STYLE walk at the game's pace is a hurried style.
        walk_cut(&cmu.join("08_08.bvh"), "walk", 1, None),
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

/// The wardrobe, from MakeHuman's CC0 system assets. Left out: the outfits
/// printed with MakeHuman's own logo (female_casualsuit01/02,
/// male_casualsuit02/04/06) — the assets are free, the trademark is not ours to
/// put on everyone's shirt — and shoes04, shoes01's mesh again.
const OUTFITS: &[&str] = &[
    "male_casualsuit01",
    "male_casualsuit03",
    "male_casualsuit05",
    "male_elegantsuit01",
    "male_worksuit01",
    "female_elegantsuit01",
    "female_sportsuit01",
];
const SHOES: &[&str] = &["shoes01", "shoes02", "shoes03", "shoes05", "shoes06"];
const HATS: &[&str] = &["fedora01", "fedora_cocked"];
const HAIR: &[&str] = &[
    "short01",
    "short02",
    "short03",
    "short04",
    "bob01",
    "bob02",
    "long01",
    "ponytail01",
    "braid01",
    "afro01",
];
const EYEBROWS: &[&str] = &[
    "eyebrow001",
    "eyebrow002",
    "eyebrow003",
    "eyebrow004",
    "eyebrow005",
    "eyebrow006",
    "eyebrow007",
    "eyebrow008",
    "eyebrow009",
    "eyebrow010",
    "eyebrow011",
    "eyebrow012",
];
const EYE_COLOURS: &[&str] = &[
    "brown",
    "brownlight",
    "blue",
    "bluegreen",
    "green",
    "lightblue",
];

/// From the MakeHuman community's asset packs (`files2.makehumancommunity.org/
/// asset_packs/`) — CC0: `bodyparts05`, `glasses01`, `suits01`, `dress01`,
/// `shirts01`, `pants01`, `hair01`; CC-BY (credited in
/// `LICENSE-THIRD-PARTY.md`): `hair02`, `bodyparts06`, `glasses02` — and
/// Joel Palmius's CC0 "crude kit" (`github.com/Makehuman-2/
/// MH2_Asset_Repository_CCO`, its folders' `.mhclo` renamed to the folder's
/// name). **An item is taken only when its own `.mhclo` names the licence its
/// pack is published under**: about one in five in those packs says AGPL3 in
/// its file header, some say nothing, and with the community's asset pages
/// offline nothing settles which is meant (checked 2026-10-27). Also left
/// out: costume pieces (3-D glasses, goggles, `hats01`'s Christmas cap),
/// `toigo_maga_hat`, whose texture is a slogan, the cloche hat, a camouflage
/// print with the hair through it (seen on mac-1), and models over ~8 000
/// vertices, whose fitting references would each add half a megabyte to the
/// pack every client loads.
/// `--community` is a directory of the items' folders, as unzipped.
const COMMUNITY: &[(ProxyKind, &str)] = &[
    // CC0
    (ProxyKind::Outfit, "toigo_halter_dress_knee_length"),
    (ProxyKind::Outfit, "toigo_halter_dress_midi"),
    (ProxyKind::Outfit, "toigo_shift_dress"),
    (ProxyKind::Outfit, "toigo_camisole_dress_with_full_skirt"),
    (ProxyKind::Outfit, "toigo_cut_out_dress"),
    (ProxyKind::Outfit, "toigo_strapless_ruffle_top_dress"),
    (ProxyKind::Top, "elvs_crude_t-shirt_male"),
    (ProxyKind::Top, "joepal_crude_t-shirt_female"),
    (ProxyKind::Top, "namuhekam_male_polo_shirt"),
    (ProxyKind::Top, "toigo_basic_tucked_t-shirt"),
    (ProxyKind::Top, "toigo_fisherman_sweater"),
    (ProxyKind::Top, "toigo_keyhole_tank_top"),
    (ProxyKind::Top, "toigo_camisole_top"),
    (ProxyKind::Bottom, "toigo_wool_pants"),
    (ProxyKind::Bottom, "toigo_harem_pants"),
    (ProxyKind::Bottom, "crude_loose_pants"),
    (ProxyKind::Hair, "culturalibre_hair_05"),
    (ProxyKind::Hair, "toigo_blunt_bob_with_bangs"),
    (ProxyKind::Hair, "toigo_curled_under_bob"),
    (ProxyKind::Hair, "toigo_inverted_bob"),
    // CC-BY
    (ProxyKind::Hair, "elvs_50s_updo"),
    (ProxyKind::Hair, "elvs_micky_afro"),
    (ProxyKind::Hair, "elvs_short_daisy_hair"),
    (ProxyKind::Hair, "elvs_ashley_may_hair"),
    (ProxyKind::Hair, "elvs_katherine_hair"),
    (ProxyKind::Hair, "elvs_maxwell_hair"),
    (ProxyKind::Beard, "elvs_scruffy_beard1"),
    (ProxyKind::Beard, "culturalibre_dal_moustache"),
    (ProxyKind::Glasses, "punkduck_glasses_modern_form"),
    (ProxyKind::Glasses, "punkduck_sport-sunglasses"),
    (ProxyKind::Beard, "rehmanpolanski_beard_viking"),
    (ProxyKind::Beard, "rehmanpolanski_moustache_viking"),
    (ProxyKind::Beard, "culturalibre_faun_beard"),
    (ProxyKind::Glasses, "spamrakuen_sagerfrogs_glasses_01"),
    (ProxyKind::Glasses, "spamrakuen_sagerfrogs_glasses_02"),
    (ProxyKind::Glasses, "spamrakuen_sagerfrogs_glasses_03"),
    (ProxyKind::Glasses, "spamrakuen_sagerfrogs_glasses_04"),
    (ProxyKind::Glasses, "spamrakuen_tbm_glasses_frames_01"),
    (ProxyKind::Glasses, "toigo_round_glasses_leopard"),
    (ProxyKind::Outfit, "toigo_female_suit"),
    (ProxyKind::Outfit, "toigo_female_double-breasted_suit"),
    (ProxyKind::Outfit, "toigo_male_double-breasted_suit"),
    (ProxyKind::Outfit, "toigo_male_suit_tie_and_jacket"),
    (ProxyKind::Outfit, "toigo_suit_with_dinner_jacket"),
];

fn community(dir: &Path) -> Vec<ProxySource> {
    COMMUNITY
        .iter()
        .map(|(kind, name)| ProxySource {
            kind: *kind,
            mhclo: dir.join(format!("{name}/{name}.mhclo")),
            material: None,
        })
        .collect()
}

fn catalogue(assets: &Path) -> Vec<ProxySource> {
    let one = |kind, dir: &str, name: &str| ProxySource {
        kind,
        mhclo: assets.join(format!("{dir}/{name}/{name}.mhclo")),
        material: None,
    };
    let mut out = vec![
        ProxySource {
            material: Some(assets.join("eyes/materials/brown.mhmat")),
            ..one(ProxyKind::Eyes, "eyes", "high-poly")
        },
        one(ProxyKind::Eyelashes, "eyelashes", "eyelashes01"),
    ];
    out.extend(
        EYEBROWS
            .iter()
            .map(|n| one(ProxyKind::Eyebrows, "eyebrows", n)),
    );
    out.extend(HAIR.iter().map(|n| one(ProxyKind::Hair, "hair", n)));
    out.extend(OUTFITS.iter().map(|n| one(ProxyKind::Outfit, "clothes", n)));
    out.extend(SHOES.iter().map(|n| one(ProxyKind::Shoes, "clothes", n)));
    out.extend(HATS.iter().map(|n| one(ProxyKind::Hat, "clothes", n)));
    out
}

/// MakeHuman's eighteen skins: three families, young, middle-aged and old,
/// female and male (`young_caucasian_male2` and the like are variants left out).
fn skins(assets: &Path) -> Vec<SkinSource> {
    let mut out = Vec::new();
    for (age, a) in [
        ("young", SkinAge::Young),
        ("middleage", SkinAge::Middle),
        ("old", SkinAge::Old),
    ] {
        for family in ["african", "asian", "caucasian"] {
            for (sex, female) in [("female", true), ("male", false)] {
                let name = format!("{age}_{family}_{sex}");
                out.push(SkinSource {
                    mhmat: assets.join(format!("skins/{name}/{name}.mhmat")),
                    name,
                    family: family.into(),
                    female,
                    age: a,
                });
            }
        }
    }
    out
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

/// The walk's cycle bounds when it is looked for in a take: one stride pair.
const WALK_LOOP: CutKind = CutKind::Loop {
    min_s: 0.9,
    max_s: 1.5,
};

/// A walk cut from a whole CMU take (frame 0 is the conversion's T-pose).
fn walk_cut(file: &Path, name: &str, start: usize, end: Option<usize>) -> Cut {
    Cut {
        name: name.into(),
        file: file.to_path_buf(),
        start: Some(start),
        end,
        kind: WALK_LOOP,
        fps: None,
    }
}

/// `--try-walk PACK FILE…`: cut a walk loop from each take, play it on the
/// default man exactly as captured (none of `rig`'s corrections to the walk),
/// and print what makes a walk look like one: its pace and cycle, how far the
/// trunk leans and rocks, how far the upper arm swings and the elbow bends.
/// Ordinary walking, for comparison: a trunk 2–5° forward rocking a few
/// degrees, the upper arm about ±15–20°, the elbow 10–45°.
fn try_walks(pack_path: &Path, files: &[String]) {
    use cartopolis_human::mocap::{clip_from_motion, parse_bvh};
    use cartopolis_human::{build, Look, Phenotype};
    let pack = std::fs::read(pack_path)
        .map_err(|e| e.to_string())
        .and_then(|b| cartopolis_human::HumanPack::decode(&b))
        .unwrap_or_else(|e| {
            eprintln!("{}: {e}", pack_path.display());
            std::process::exit(1);
        });
    let look = Look {
        phenotype: Phenotype::male(),
        skin: String::new(),
        proxies: vec![],
        eyes: None,
        modifiers: vec![],
    };
    for file in files {
        // Named so that no correction meant for the walk applies.
        let cut = walk_cut(Path::new(file), "candidate", 1, None);
        let clip = match std::fs::read_to_string(file)
            .map_err(|e| e.to_string())
            .and_then(|t| parse_bvh(&t))
            .and_then(|m| clip_from_motion(&m, &cut, &pack.bones, 30.0))
        {
            Ok(c) => c,
            Err(e) => {
                println!("{file}: {e}");
                continue;
            }
        };
        let mut trial = pack.clone();
        trial.clips = vec![clip];
        let m = build(&trial, &look);
        let Some(c) = m.clips.first() else { continue };
        let j = |n: &str| m.joint(n).unwrap_or(0);
        let frames = c.rotations.first().map_or(0, Vec::len);
        let (mut lean, mut arm, mut elbow) = (Vec::new(), Vec::new(), Vec::new());
        for f in 0..frames {
            let p = m.joint_positions(Some((0, f)));
            let trunk = p[j("neck_01")] - p[j("pelvis")];
            lean.push(trunk.z.atan2(trunk.y).to_degrees());
            let up = p[j("lowerarm_l")] - p[j("upperarm_l")];
            let fore = p[j("hand_l")] - p[j("lowerarm_l")];
            arm.push(up.z.atan2(-up.y).to_degrees());
            elbow.push(up.angle_between(fore).to_degrees());
        }
        let range = |v: &[f32]| {
            v.iter()
                .fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)))
        };
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
        let (l, a, e) = (range(&lean), range(&arm), range(&elbow));
        println!(
            "{file}: {:.2} m/s, {:.2} s cycle | trunk {:.0}° ({:.0}..{:.0}) | arm {:.0}..{:.0}° | elbow {:.0}..{:.0}°",
            c.speed,
            c.duration(),
            mean(&lean),
            l.0,
            l.1,
            a.0,
            a.1,
            e.0,
            e.1
        );
    }
}

/// `--replace-walk PACK FILE START [END]`: put a walk cut from a take in place
/// of the pack's, leaving everything else in the pack as it is — so a better
/// walk needs only its own take, not every source the pack was built from.
fn replace_walk(pack_path: &Path, file: &Path, start: usize, end: Option<usize>) {
    use cartopolis_human::mocap::{clip_from_motion, parse_bvh};
    let result = std::fs::read(pack_path)
        .map_err(|e| e.to_string())
        .and_then(|b| cartopolis_human::HumanPack::decode(&b))
        .and_then(|mut pack| {
            let text = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
            let clip = clip_from_motion(
                &parse_bvh(&text)?,
                &walk_cut(file, "walk", start, end),
                &pack.bones,
                30.0,
            )?;
            let slot = pack
                .clips
                .iter_mut()
                .find(|c| c.name == "walk")
                .ok_or("the pack has no walk")?;
            *slot = clip;
            let bytes = pack.encode()?;
            std::fs::write(pack_path, &bytes).map_err(|e| e.to_string())?;
            Ok(bytes.len())
        });
    match result {
        Ok(n) => println!("{}: walk replaced, {} KB", pack_path.display(), n / 1024),
        Err(e) => {
            eprintln!("human-pack: {e}");
            std::process::exit(1);
        }
    }
}

/// `--add-face-units PACK DIR`: put the face's units from the `faceunits01`
/// asset pack's `targets/faceunits` directory into a pack, leaving the rest
/// of it as it is (a pack of this version only: `pack::decode_upgrading`).
fn add_face_units(pack_path: &Path, dir: &Path) {
    use cartopolis_human::pack::{decode_upgrading, read_face_units};
    let result = std::fs::read(pack_path)
        .map_err(|e| e.to_string())
        .and_then(|b| decode_upgrading(&b))
        .and_then(|mut pack| {
            pack.face_units = read_face_units(dir)?;
            let bytes = pack.encode()?;
            std::fs::write(pack_path, &bytes).map_err(|e| e.to_string())?;
            Ok((
                bytes.len(),
                pack.face_units.iter().map(|t| t.len()).sum::<usize>(),
            ))
        });
    match result {
        Ok((n, offsets)) => println!(
            "{}: face units added ({offsets} offsets), {} KB",
            pack_path.display(),
            n / 1024
        ),
        Err(e) => {
            eprintln!("human-pack: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let all: Vec<String> = std::env::args().collect();
    if let [_, flag, pack, dir] = all.as_slice() {
        if flag == "--add-face-units" {
            add_face_units(Path::new(pack), Path::new(dir));
            return;
        }
    }
    if all.get(1).is_some_and(|f| f == "--try-walk") && all.len() > 3 {
        try_walks(Path::new(&all[2]), &all[3..]);
        return;
    }
    if all.get(1).is_some_and(|f| f == "--replace-walk") && all.len() > 4 {
        let start = all[4].parse().unwrap_or(1);
        let end = all.get(5).and_then(|s| s.parse().ok());
        replace_walk(Path::new(&all[2]), Path::new(&all[3]), start, end);
        return;
    }
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
    let mut face_units = None;
    let mut community_dir = None;
    while let Some(a) = args.next() {
        let v = args.next().map(PathBuf::from);
        match a.as_str() {
            "--face-units" => face_units = v,
            "--mpfb" => mpfb = v,
            "--assets" => assets = v,
            "--style" => style = v,
            "--cmu" => cmu = v,
            "--out" => out = v,
            "--community" => community_dir = v,
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
    let sources = Sources {
        mpfb_data: mpfb,
        skins: skins(&assets),
        eyes: EYE_COLOURS
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    assets.join(format!("eyes/materials/{c}.mhmat")),
                )
            })
            .collect(),
        proxies: catalogue(&assets)
            .into_iter()
            .chain(community_dir.as_deref().map(community).unwrap_or_default())
            .collect(),
        cuts: style_cuts(&style)
            .into_iter()
            .chain(cmu_cuts(&cmu))
            .collect(),
        // Every body the sliders can make from 18 up: MakeHuman blends its
        // "child" targets in below 25, so only the baby ones are left out.
        targets: TargetSet::Macros {
            skip: vec!["baby".into()],
        },
        // Every face control MakeHuman has, and the body's below the head;
        // the designer offers a choice of them.
        modifier_groups: [
            "head", "forehead", "eyebrows", "eyes", "nose", "mouth", "cheek", "chin", "ears",
            "neck", "torso", "hip", "stomach", "buttocks", "arms", "legs",
        ]
        .map(String::from)
        .to_vec(),
        textures_out: out.join("textures"),
        face_units,
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
