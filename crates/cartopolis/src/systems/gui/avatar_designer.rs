//! The avatar designer: MakeHuman's body and face sliders, a skin, eyes, hair
//! and a wardrobe, edited on a draft the avatar is rebuilt from as it changes
//! (`docs/mockups/avatar-designer`, `systems::human_avatar`).
//!
//! **Taps before sliders.** A person is a starter or "Surprise me" first,
//! then a quick pick per row (body shape, age, build, height), "Another face",
//! and the skin, hair and clothes choices; the sliders are under "Fine-tune"
//! and the face parts, for whoever wants them. Shaping a body from the
//! default by sliders alone took a dozen drags.
//!
//! Drawn inside the Appearance panel (`ui::render_avatar_chooser`), whose
//! Apply saves the draft. The names here are the pack's
//! (`crates/human/src/bin/human-pack.rs` lists what it carries); a name the
//! pack lacks is skipped by the builder.

use crate::systems::theme;
use crate::systems::user_store::HumanLook;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DesignerTab {
    #[default]
    Body,
    Face,
    SkinAndHair,
    Clothes,
}

/// The part of the face the Face tab shows.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum FacePart {
    #[default]
    Head,
    Eyes,
    Nose,
    Mouth,
    Jaw,
    Ears,
}

/// One face slider: the MakeHuman modifier, its label, its ends, and whether
/// it is one-sided (0 … 100 rather than -100 … 100).
struct FaceSlider(
    &'static str,
    &'static str,
    (&'static str, &'static str),
    bool,
);

/// About thirty of MakeHuman's hundred-odd face controls, five or six a part —
/// the ones whose effect reads at the distance an avatar is seen. The pack
/// carries them all; adding one here is a row.
const FACE: [(FacePart, &[FaceSlider]); 6] = [
    (
        FacePart::Head,
        &[
            FaceSlider(
                "head/head-scale-horiz-decr-incr",
                "Width",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "head/head-scale-vert-decr-incr",
                "Length",
                ("Short", "Long"),
                false,
            ),
            FaceSlider("head/head-oval", "Oval", ("", "Oval"), true),
            FaceSlider("head/head-round", "Round", ("", "Round"), true),
            FaceSlider("head/head-square", "Square", ("", "Square"), true),
            FaceSlider(
                "forehead/forehead-scale-vert-decr-incr",
                "Forehead",
                ("Low", "High"),
                false,
            ),
        ],
    ),
    (
        FacePart::Eyes,
        &[
            FaceSlider(
                "eyes/eye-scale-decr-incr",
                "Size",
                ("Small", "Large"),
                false,
            ),
            FaceSlider(
                "eyes/eye-trans-in-out",
                "Spacing",
                ("Close", "Apart"),
                false,
            ),
            FaceSlider(
                "eyes/eye-trans-down-up",
                "Height",
                ("Lower", "Higher"),
                false,
            ),
            FaceSlider(
                "eyes/eye-epicanthus-in-out",
                "Eyelid fold",
                ("Less", "More"),
                false,
            ),
            FaceSlider(
                "eyebrows/eyebrows-trans-down-up",
                "Brows",
                ("Lower", "Higher"),
                false,
            ),
            FaceSlider(
                "eyebrows/eyebrows-angle-down-up",
                "Brow angle",
                ("Down", "Up"),
                false,
            ),
        ],
    ),
    (
        FacePart::Nose,
        &[
            FaceSlider(
                "nose/nose-scale-horiz-decr-incr",
                "Width",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "nose/nose-scale-vert-decr-incr",
                "Length",
                ("Short", "Long"),
                false,
            ),
            FaceSlider(
                "nose/nose-scale-depth-decr-incr",
                "Depth",
                ("Flat", "Deep"),
                false,
            ),
            FaceSlider(
                "nose/nose-hump-decr-incr",
                "Bridge",
                ("Straight", "Hump"),
                false,
            ),
            FaceSlider("nose/nose-point-down-up", "Tip", ("Down", "Up"), false),
            FaceSlider(
                "nose/nose-nostrils-width-decr-incr",
                "Nostrils",
                ("Narrow", "Wide"),
                false,
            ),
        ],
    ),
    (
        FacePart::Mouth,
        &[
            FaceSlider(
                "mouth/mouth-scale-horiz-decr-incr",
                "Width",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "mouth/mouth-upperlip-volume-decr-incr",
                "Upper lip",
                ("Thin", "Full"),
                false,
            ),
            FaceSlider(
                "mouth/mouth-lowerlip-volume-decr-incr",
                "Lower lip",
                ("Thin", "Full"),
                false,
            ),
            FaceSlider(
                "mouth/mouth-angles-down-up",
                "Corners",
                ("Down", "Up"),
                false,
            ),
            FaceSlider(
                "mouth/mouth-trans-down-up",
                "Height",
                ("Lower", "Higher"),
                false,
            ),
        ],
    ),
    (
        FacePart::Jaw,
        &[
            FaceSlider(
                "chin/chin-width-decr-incr",
                "Chin width",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "chin/chin-prominent-decr-incr",
                "Chin",
                ("Receding", "Prominent"),
                false,
            ),
            FaceSlider(
                "chin/chin-height-decr-incr",
                "Chin height",
                ("Short", "Tall"),
                false,
            ),
            FaceSlider(
                "chin/chin-bones-decr-incr",
                "Jaw",
                ("Soft", "Strong"),
                false,
            ),
            FaceSlider(
                "cheek/cheek-bones-decr-incr",
                "Cheekbones",
                ("Low", "High"),
                false,
            ),
            FaceSlider(
                "cheek/cheek-volume-decr-incr",
                "Cheeks",
                ("Hollow", "Full"),
                false,
            ),
        ],
    ),
    (
        FacePart::Ears,
        &[
            FaceSlider(
                "ears/ear-scale-decr-incr",
                "Size",
                ("Small", "Large"),
                false,
            ),
            FaceSlider(
                "ears/ear-flap-decr-incr",
                "Stick out",
                ("Flat", "Out"),
                false,
            ),
            FaceSlider(
                "ears/ear-lobe-decr-incr",
                "Lobes",
                ("Small", "Large"),
                false,
            ),
            FaceSlider(
                "ears/ear-shape-pointed-triangle",
                "Shape",
                ("Pointed", "Triangular"),
                false,
            ),
        ],
    ),
];

/// The skin families, each with a colour to show it by.
const SKINS: [(&str, [u8; 3]); 3] = [
    ("caucasian", [236, 200, 178]),
    ("asian", [222, 184, 146]),
    ("african", [112, 72, 52]),
];

/// The pack's eye colours, each with a colour to show it by.
const EYES: [(&str, [u8; 3]); 6] = [
    ("brown", [92, 58, 34]),
    ("brownlight", [150, 108, 60]),
    ("green", [92, 128, 70]),
    ("bluegreen", [80, 128, 130]),
    ("blue", [70, 110, 170]),
    ("lightblue", [130, 170, 210]),
];

const HAIR: [(&str, &str); 10] = [
    ("short01", "Short 1"),
    ("short02", "Short 2"),
    ("short03", "Short 3"),
    ("short04", "Short 4"),
    ("bob01", "Bob"),
    ("bob02", "Long bob"),
    ("long01", "Long"),
    ("ponytail01", "Ponytail"),
    ("braid01", "Braid"),
    ("afro01", "Afro"),
];

const EYEBROWS: [(&str, &str); 5] = [
    ("eyebrow001", "Natural"),
    ("eyebrow002", "Thin"),
    ("eyebrow004", "Arched"),
    ("eyebrow006", "Straight"),
    ("eyebrow009", "Full"),
];

const OUTFITS: [(&str, &str); 7] = [
    ("male_casualsuit01", "Denim shirt, jeans"),
    ("male_casualsuit03", "Striped shirt, jeans"),
    ("male_casualsuit05", "Jacket, jeans"),
    ("male_elegantsuit01", "Suit"),
    ("male_worksuit01", "Overalls"),
    ("female_elegantsuit01", "Blouse, skirt"),
    ("female_sportsuit01", "Sports top, leggings"),
];

const SHOES: [(&str, &str); 5] = [
    ("shoes01", "Brown leather"),
    ("shoes02", "Grey trainers"),
    ("shoes03", "Boots"),
    ("shoes05", "White trainers"),
    ("shoes06", "Blue trainers"),
];

/// Colours to pick for hair, over its neutral texture.
const HAIR_COLOURS: [[u8; 3]; 9] = [
    [30, 24, 20],
    [70, 48, 34],
    [110, 76, 50],
    [150, 104, 64],
    [196, 150, 94],
    [232, 204, 150],
    [176, 84, 46],
    [150, 150, 150],
    [235, 235, 235],
];

/// Colours to pick for a garment half.
const CLOTH_COLOURS: [[u8; 3]; 10] = [
    [245, 245, 245],
    [40, 40, 44],
    [110, 114, 122],
    [42, 62, 110],
    [96, 140, 200],
    [150, 40, 40],
    [80, 120, 70],
    [196, 160, 110],
    [230, 190, 80],
    [214, 140, 176],
];

/// A ready-made person to start from: a whole look in one tap, to adjust
/// rather than build from the default.
struct Starter {
    label: &'static str,
    body_shape: u8,
    age: u8,
    /// Height, weight, muscle.
    build: [u8; 3],
    skin: &'static str,
    hair: &'static str,
    hair_colour: [u8; 3],
    outfit: &'static str,
    top: Option<[u8; 3]>,
    bottom: Option<[u8; 3]>,
    shoes: &'static str,
    hat: Option<&'static str>,
}

/// Eight people across body shape, age, build, skin and dress, so that most
/// looks are a starter and a few taps away. Only names the pack carries
/// (the lists above).
const STARTERS: [Starter; 8] = [
    Starter {
        label: "Student",
        body_shape: 15,
        age: 21,
        build: [45, 40, 45],
        skin: "asian",
        hair: "ponytail01",
        hair_colour: [30, 24, 20],
        outfit: "female_sportsuit01",
        top: None,
        bottom: None,
        shoes: "shoes05",
        hat: None,
    },
    Starter {
        label: "Suit",
        body_shape: 90,
        age: 38,
        build: [60, 50, 55],
        skin: "african",
        hair: "short01",
        hair_colour: [30, 24, 20],
        outfit: "male_elegantsuit01",
        top: None,
        bottom: None,
        shoes: "shoes01",
        hat: None,
    },
    Starter {
        label: "Athlete",
        body_shape: 10,
        age: 27,
        build: [60, 35, 80],
        skin: "african",
        hair: "braid01",
        hair_colour: [30, 24, 20],
        outfit: "female_sportsuit01",
        top: Some([42, 62, 110]),
        bottom: Some([40, 40, 44]),
        shoes: "shoes02",
        hat: None,
    },
    Starter {
        label: "Builder",
        body_shape: 95,
        age: 45,
        build: [55, 70, 75],
        skin: "caucasian",
        hair: "short04",
        hair_colour: [110, 76, 50],
        outfit: "male_worksuit01",
        top: None,
        bottom: None,
        shoes: "shoes03",
        hat: None,
    },
    Starter {
        label: "Artist",
        body_shape: 50,
        age: 32,
        build: [50, 45, 45],
        skin: "african",
        hair: "afro01",
        hair_colour: [30, 24, 20],
        outfit: "male_casualsuit01",
        top: Some([214, 140, 176]),
        bottom: None,
        shoes: "shoes06",
        hat: None,
    },
    Starter {
        label: "Office",
        body_shape: 20,
        age: 45,
        build: [55, 55, 45],
        skin: "caucasian",
        hair: "long01",
        hair_colour: [176, 84, 46],
        outfit: "female_elegantsuit01",
        top: Some([42, 62, 110]),
        bottom: Some([40, 40, 44]),
        shoes: "shoes01",
        hat: None,
    },
    Starter {
        label: "Silver",
        body_shape: 5,
        age: 72,
        build: [35, 60, 30],
        skin: "caucasian",
        hair: "bob02",
        hair_colour: [235, 235, 235],
        outfit: "female_elegantsuit01",
        top: Some([150, 40, 40]),
        bottom: None,
        shoes: "shoes01",
        hat: None,
    },
    Starter {
        label: "Fedora",
        body_shape: 95,
        age: 75,
        build: [45, 55, 35],
        skin: "asian",
        hair: "short02",
        hair_colour: [150, 150, 150],
        outfit: "male_casualsuit03",
        top: None,
        bottom: None,
        shoes: "shoes01",
        hat: Some("fedora01"),
    },
];

impl Starter {
    fn look(&self) -> HumanLook {
        let mut look = HumanLook::default_for(self.body_shape < 50);
        look.body_shape = self.body_shape;
        look.age = self.age;
        [look.height, look.weight, look.muscle] = self.build;
        look.skin = self.skin.into();
        look.hair = Some(self.hair.into());
        look.hair_colour = Some(self.hair_colour);
        look.outfit = self.outfit.into();
        look.top_colour = self.top;
        look.bottom_colour = self.bottom;
        look.shoes = self.shoes.into();
        look.hat = self.hat.map(Into::into);
        look
    }
}

/// The designer's state between frames.
#[derive(Default)]
pub struct DesignerState {
    pub tab: DesignerTab,
    pub face: FacePart,
    /// The dice: "Surprise me" and "Another face" draw from it.
    dice: u64,
}

impl DesignerState {
    /// A number from the dice (splitmix64), stirred with the time so that two
    /// sessions do not roll the same people.
    fn roll(&mut self, time: f64) -> u64 {
        self.dice = self
            .dice
            .wrapping_add(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(time.to_bits());
        let mut z = self.dice;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Draws from the dice: a number in `range`, and one of a list.
struct Dice<'a> {
    state: &'a mut DesignerState,
    time: f64,
}

impl Dice<'_> {
    fn int(&mut self, range: std::ops::RangeInclusive<i32>) -> i32 {
        let span = (range.end() - range.start() + 1).max(1) as u64;
        range.start() + (self.state.roll(self.time) % span) as i32
    }

    /// Mostly near the middle of `range`: the mean of two rolls.
    fn middling(&mut self, range: std::ops::RangeInclusive<i32>) -> u8 {
        ((self.int(range.clone()) + self.int(range)) / 2) as u8
    }

    fn chance(&mut self, percent: i32) -> bool {
        self.int(0..=99) < percent
    }

    fn pick<'t, T>(&mut self, items: &'t [T]) -> &'t T {
        &items[self.int(0..=items.len() as i32 - 1) as usize]
    }
}

/// A whole look at random, kept to people one meets: bodies near the middle
/// of each slider, a face only a little off average.
fn surprise(dice: &mut Dice) -> HumanLook {
    let body_shape = if dice.chance(85) {
        if dice.chance(50) {
            dice.int(0..=25)
        } else {
            dice.int(75..=100)
        }
    } else {
        dice.int(26..=74)
    } as u8;
    let mut look = HumanLook::default_for(body_shape < 50);
    look.body_shape = body_shape;
    look.age = dice.middling(18..=80);
    look.height = dice.middling(10..=90);
    look.weight = dice.middling(10..=90);
    look.muscle = dice.middling(15..=85);
    look.proportions = dice.middling(40..=80);
    look.chest = dice.middling(20..=80);
    look.skin = dice.pick(&SKINS).0.into();
    look.eyes = dice.pick(&EYES).0.into();
    look.hair = (!dice.chance(5)).then(|| dice.pick(&HAIR).0.into());
    // Grey and white hair only with the years for it.
    let greys = if look.age >= 55 { 9 } else { 7 };
    look.hair_colour = Some(*dice.pick(&HAIR_COLOURS[..greys]));
    look.eyebrows = dice.pick(&EYEBROWS).0.into();
    look.outfit = dice.pick(&OUTFITS).0.into();
    look.top_colour = dice.chance(60).then(|| *dice.pick(&CLOTH_COLOURS));
    look.bottom_colour = dice.chance(40).then(|| *dice.pick(&CLOTH_COLOURS));
    look.shoes = dice.pick(&SHOES).0.into();
    look.hat = dice.chance(10).then(|| "fedora01".into());
    another_face(dice, &mut look);
    look
}

/// A different face: a handful of the face controls, each a little off
/// average.
fn another_face(dice: &mut Dice, look: &mut HumanLook) {
    look.face.clear();
    for (_, sliders) in FACE.iter() {
        for FaceSlider(modifier, _, _, one_sided) in sliders.iter() {
            if dice.chance(40) {
                let lo = if *one_sided { 0 } else { -45 };
                look.set_face(modifier, dice.int(lo..=45) as i8);
            }
        }
    }
}

/// The designer's body. Returns whether the look changed this frame.
pub fn designer(ui: &mut egui::Ui, state: &mut DesignerState, look: &mut HumanLook) -> bool {
    let before = look.clone();
    start_from(ui, state, look);
    ui.add_space(8.0);
    theme::segmented(
        ui,
        &mut state.tab,
        &[
            (DesignerTab::Body, "Body"),
            (DesignerTab::Face, "Face"),
            (DesignerTab::SkinAndHair, "Skin & hair"),
            (DesignerTab::Clothes, "Clothes"),
        ],
    );
    ui.add_space(8.0);
    match state.tab {
        DesignerTab::Body => body(ui, look),
        DesignerTab::Face => face(ui, state, look),
        DesignerTab::SkinAndHair => skin_and_hair(ui, look),
        DesignerTab::Clothes => clothes(ui, look),
    }
    *look != before
}

/// The starters and the dice, above the tabs: a whole person in one tap.
fn start_from(ui: &mut egui::Ui, state: &mut DesignerState, look: &mut HumanLook) {
    theme::section(ui, "Start from");
    let time = ui.input(|i| i.time);
    ui.horizontal_wrapped(|ui| {
        for starter in &STARTERS {
            let theirs = starter.look();
            if theme::chip(ui, starter.label, *look == theirs).clicked() {
                *look = theirs;
            }
        }
        if theme::chip(ui, "Surprise me", false).clicked() {
            *look = surprise(&mut Dice { state, time });
        }
    });
    theme::note(ui, "Then change anything below.");
}

/// A row of quick picks for one or more sliders: each pick sets them all,
/// and shows chosen while they hold its values. Most bodies are a pick per
/// row; the sliders under "Fine-tune" are for the rest.
fn picks<const N: usize>(
    ui: &mut egui::Ui,
    title: &str,
    options: &[(&str, [u8; N])],
    mut values: [&mut u8; N],
) {
    ui.label(title);
    ui.horizontal_wrapped(|ui| {
        for (label, set) in options {
            let chosen = values.iter().zip(set).all(|(v, s)| **v == *s);
            if theme::chip(ui, label, chosen).clicked() {
                for (v, s) in values.iter_mut().zip(set) {
                    **v = *s;
                }
            }
        }
    });
    ui.add_space(6.0);
}

fn body(ui: &mut egui::Ui, look: &mut HumanLook) {
    picks(
        ui,
        "Body shape",
        &[
            ("Feminine", [0]),
            ("In between", [50]),
            ("Masculine", [100]),
        ],
        [&mut look.body_shape],
    );
    picks(
        ui,
        "Age",
        &[
            ("20s", [24]),
            ("30s", [34]),
            ("40s", [44]),
            ("50s", [54]),
            ("60s", [64]),
            ("70s", [74]),
            ("80s", [84]),
        ],
        [&mut look.age],
    );
    // Weight and muscle together: a build is both.
    picks(
        ui,
        "Build",
        &[
            ("Slim", [25, 40]),
            ("Average", [50, 50]),
            ("Athletic", [40, 85]),
            ("Sturdy", [70, 70]),
            ("Heavy", [90, 40]),
        ],
        [&mut look.weight, &mut look.muscle],
    );
    picks(
        ui,
        "Height",
        &[("Short", [20]), ("Average", [50]), ("Tall", [80])],
        [&mut look.height],
    );
    egui::CollapsingHeader::new("Fine-tune")
        .id_salt("avatar_body_fine_tune")
        .show(ui, |ui| body_sliders(ui, look));
}

fn body_sliders(ui: &mut egui::Ui, look: &mut HumanLook) {
    let shape = look.body_shape;
    slider(
        ui,
        "Body shape",
        &mut look.body_shape,
        0..=100,
        word(
            shape,
            [
                "feminine",
                "more feminine",
                "in between",
                "more masculine",
                "masculine",
            ],
        ),
        ("Feminine", "Masculine"),
    );
    let age = look.age;
    slider(
        ui,
        "Age",
        &mut look.age,
        18..=90,
        age.to_string(),
        ("18", "90"),
    );
    let v = look.height;
    slider(
        ui,
        "Height",
        &mut look.height,
        0..=100,
        word(v, ["short", "shorter", "average", "taller", "tall"]),
        ("Short", "Tall"),
    );
    let v = look.weight;
    slider(
        ui,
        "Weight",
        &mut look.weight,
        0..=100,
        word(v, ["slim", "slimmer", "average", "heavier", "heavy"]),
        ("Slim", "Heavy"),
    );
    let v = look.muscle;
    slider(
        ui,
        "Muscle",
        &mut look.muscle,
        0..=100,
        word(v, ["soft", "softer", "average", "toned", "muscular"]),
        ("Soft", "Muscular"),
    );
    let v = look.proportions;
    slider(
        ui,
        "Proportions",
        &mut look.proportions,
        0..=100,
        word(
            v,
            ["unusual", "less usual", "average", "classic", "classic"],
        ),
        ("Unusual", "Classic"),
    );
    let v = look.chest;
    slider(
        ui,
        "Chest",
        &mut look.chest,
        0..=100,
        word(v, ["small", "smaller", "average", "fuller", "full"]),
        ("Small", "Full"),
    );
}

fn face(ui: &mut egui::Ui, state: &mut DesignerState, look: &mut HumanLook) {
    // Whole faces first: most people want a different face, not a narrower
    // nose.
    let time = ui.input(|i| i.time);
    ui.horizontal_wrapped(|ui| {
        if theme::action_button(ui, "Another face").clicked() {
            another_face(&mut Dice { state, time }, look);
        }
        if !look.face.is_empty() && theme::action_button(ui, "Average face").clicked() {
            look.face.clear();
        }
    });
    ui.add_space(6.0);
    let part = &mut state.face;
    ui.horizontal_wrapped(|ui| {
        for (p, label) in [
            (FacePart::Head, "Head"),
            (FacePart::Eyes, "Eyes"),
            (FacePart::Nose, "Nose"),
            (FacePart::Mouth, "Mouth"),
            (FacePart::Jaw, "Jaw"),
            (FacePart::Ears, "Ears"),
        ] {
            if theme::chip(ui, label, *part == p).clicked() {
                *part = p;
            }
        }
    });
    ui.add_space(6.0);
    let Some((_, sliders)) = FACE.iter().find(|(p, _)| p == part) else {
        return;
    };
    for FaceSlider(modifier, label, ends, one_sided) in sliders.iter() {
        let mut v = i32::from(look.face_value(modifier));
        let says = match v {
            0 => "average".to_string(),
            v if v > 0 => format!("{} {}", ends.1.to_lowercase(), v),
            v => format!("{} {}", ends.0.to_lowercase(), -v),
        };
        let lo = if *one_sided { 0 } else { -100 };
        header(ui, label, says);
        ui.spacing_mut().slider_width = ui.available_width();
        if ui
            .add(egui::Slider::new(&mut v, lo..=100).show_value(false))
            .changed()
        {
            look.set_face(modifier, v.clamp(-100, 100) as i8);
        }
        ends_row(ui, if *one_sided { ("", ends.1) } else { *ends });
    }
}

fn skin_and_hair(ui: &mut egui::Ui, look: &mut HumanLook) {
    theme::section(ui, "Skin");
    ui.horizontal_wrapped(|ui| {
        for (family, colour) in SKINS {
            if disc(ui, colour, look.skin == family, family).clicked() {
                look.skin = family.into();
            }
        }
    });
    theme::note(
        ui,
        "Your age and body shape choose among the family's skins.",
    );
    theme::section(ui, "Eyes");
    ui.horizontal_wrapped(|ui| {
        for (name, colour) in EYES {
            if disc(ui, colour, look.eyes == name, name).clicked() {
                look.eyes = name.into();
            }
        }
    });
    theme::section(ui, "Hair");
    ui.horizontal_wrapped(|ui| {
        if theme::chip(ui, "None", look.hair.is_none()).clicked() {
            look.hair = None;
        }
        for (name, label) in HAIR {
            if theme::chip(ui, label, look.hair.as_deref() == Some(name)).clicked() {
                look.hair = Some(name.into());
            }
        }
    });
    ui.add_space(4.0);
    colours(ui, "hair", &HAIR_COLOURS, &mut look.hair_colour);
    theme::section(ui, "Brows");
    ui.horizontal_wrapped(|ui| {
        for (name, label) in EYEBROWS {
            if theme::chip(ui, label, look.eyebrows == name).clicked() {
                look.eyebrows = name.into();
            }
        }
    });
    theme::note(ui, "Brows and lashes take the hair's colour.");
}

fn clothes(ui: &mut egui::Ui, look: &mut HumanLook) {
    theme::section(ui, "Outfit");
    ui.horizontal_wrapped(|ui| {
        for (name, label) in OUTFITS {
            if theme::chip(ui, label, look.outfit == name).clicked() {
                look.outfit = name.into();
            }
        }
    });
    theme::section(ui, "Top");
    colours(ui, "top", &CLOTH_COLOURS, &mut look.top_colour);
    theme::section(ui, "Bottom");
    colours(ui, "bottom", &CLOTH_COLOURS, &mut look.bottom_colour);
    theme::section(ui, "Shoes");
    ui.horizontal_wrapped(|ui| {
        for (name, label) in SHOES {
            if theme::chip(ui, label, look.shoes == name).clicked() {
                look.shoes = name.into();
            }
        }
    });
    theme::section(ui, "Hat");
    ui.horizontal_wrapped(|ui| {
        if theme::chip(ui, "None", look.hat.is_none()).clicked() {
            look.hat = None;
        }
        if theme::chip(ui, "Fedora", look.hat.as_deref() == Some("fedora01")).clicked() {
            look.hat = Some("fedora01".into());
        }
    });
    theme::note(
        ui,
        "Everything worn stretches with the body, so it still fits when the sliders move.",
    );
}

/// A word for where a 0–100 slider sits, in five bands.
fn word(v: u8, words: [&str; 5]) -> String {
    words[(usize::from(v.min(99)) * 5 / 100).min(4)].to_string()
}

fn header(ui: &mut egui::Ui, label: &str, says: String) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(says).color(theme::TEXT_DIM));
        });
    });
}

fn ends_row(ui: &mut egui::Ui, ends: (&str, &str)) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(ends.0)
                .size(theme::SIZE_CHIP)
                .color(theme::LINE),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(ends.1)
                    .size(theme::SIZE_CHIP)
                    .color(theme::LINE),
            );
        });
    });
    ui.add_space(4.0);
}

/// One slider: its name and a word for its value on a line, the track, and
/// what its two ends mean.
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut u8,
    range: std::ops::RangeInclusive<u8>,
    says: String,
    ends: (&str, &str),
) {
    header(ui, label, says);
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(egui::Slider::new(value, range).show_value(false));
    ends_row(ui, ends);
}

/// A colour disc, ringed when chosen. The colours are the look's, not the
/// interface's, so they are data here rather than theme colours.
fn disc(ui: &mut egui::Ui, c: [u8; 3], chosen: bool, hover: &str) -> egui::Response {
    let d = theme::row_height().max(32.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::click());
    let p = ui.painter();
    let centre = rect.center();
    let r = d * 0.5 - 4.0;
    p.circle_filled(centre, r, egui::Color32::from_rgb(c[0], c[1], c[2]));
    if chosen {
        p.circle_stroke(centre, r + 2.5, egui::Stroke::new(2.0, theme::ACCENT));
    } else {
        p.circle_stroke(centre, r, egui::Stroke::new(1.0, theme::LINE));
    }
    response.on_hover_text(hover)
}

/// "As it is" and a row of colour discs for an optional colour.
fn colours(ui: &mut egui::Ui, what: &str, palette: &[[u8; 3]], value: &mut Option<[u8; 3]>) {
    ui.horizontal_wrapped(|ui| {
        if theme::chip(ui, "As it is", value.is_none()).clicked() {
            *value = None;
        }
        for c in palette {
            if disc(ui, *c, *value == Some(*c), what).clicked() {
                *value = Some(*c);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_follows_the_slider() {
        let w = ["a", "b", "c", "d", "e"];
        assert_eq!(word(0, w), "a");
        assert_eq!(word(50, w), "c");
        assert_eq!(word(100, w), "e");
    }

    /// Every starter wears only what the designer offers, so picking one
    /// leaves every tab showing a choice, and they are all different people.
    #[test]
    fn the_starters_are_made_of_what_is_on_offer() {
        let mut seen = std::collections::HashSet::new();
        for s in &STARTERS {
            let look = s.look();
            assert!(
                look.is_reasonable(),
                "{} is a look the server keeps",
                s.label
            );
            assert!(SKINS.iter().any(|(n, _)| *n == look.skin), "{}", s.label);
            assert!(
                HAIR.iter().any(|(n, _)| Some(*n) == look.hair.as_deref()),
                "{}",
                s.label
            );
            assert!(
                OUTFITS.iter().any(|(n, _)| *n == look.outfit),
                "{}",
                s.label
            );
            assert!(SHOES.iter().any(|(n, _)| *n == look.shoes), "{}", s.label);
            assert!(HAIR_COLOURS.contains(&s.hair_colour), "{}", s.label);
            assert!(seen.insert(format!("{look:?}")), "{} twice", s.label);
        }
    }

    /// The dice make people the server keeps, and different ones each roll.
    #[test]
    fn a_surprise_is_a_reasonable_new_person() {
        let mut state = DesignerState::default();
        let mut looks = std::collections::HashSet::new();
        for i in 0..50 {
            let look = surprise(&mut Dice {
                state: &mut state,
                time: f64::from(i),
            });
            assert!(look.is_reasonable(), "{look:?}");
            assert!((18..=90).contains(&look.age));
            looks.insert(format!("{look:?}"));
        }
        assert!(looks.len() > 45, "{} different people in 50", looks.len());
    }

    /// Every face part has sliders, and no modifier is offered twice.
    #[test]
    fn the_face_tab_offers_each_control_once() {
        let mut seen = std::collections::HashSet::new();
        for (_, sliders) in FACE.iter() {
            assert!(!sliders.is_empty());
            for s in sliders.iter() {
                assert!(seen.insert(s.0), "{} twice", s.0);
            }
        }
    }
}
