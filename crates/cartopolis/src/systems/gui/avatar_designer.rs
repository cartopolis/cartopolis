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
//! The Appearance panel (`ui::drive_avatar_chooser`) holds the draft; the
//! component library draws the designer (`lib_avatar`) from the tables and
//! `DesignerState` methods here, so a change to what it offers goes here,
//! once. The names here are the pack's (`crates/human/src/bin/human-pack.rs`
//! lists what it carries); a name the pack lacks is skipped by the builder.

use crate::systems::user_store::HumanLook;

pub use crate::resources::DesignerTab;

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
pub(crate) struct FaceSlider(
    pub(crate) &'static str,
    pub(crate) &'static str,
    pub(crate) (&'static str, &'static str),
    pub(crate) bool,
);

/// About thirty of MakeHuman's hundred-odd face controls, five or six a part —
/// the ones whose effect reads at the distance an avatar is seen. The pack
/// carries them all; adding one here is a row.
pub(crate) const FACE: [(FacePart, &[FaceSlider]); 6] = [
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

/// The part of the body the Body tab's "Body parts" shows.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum BodyPart {
    #[default]
    Torso,
    Hips,
    Neck,
    Arms,
    Legs,
}

/// MakeHuman's body controls below the head, three or four a part: what the
/// macro sliders cannot say — broad shoulders on a slim body, a belly, long
/// legs. The pack carries every control of these groups (`human-pack`).
pub(crate) const BODY: [(BodyPart, &[FaceSlider]); 5] = [
    (
        BodyPart::Torso,
        &[
            FaceSlider(
                "torso/measure-shoulder-dist-decr-incr",
                "Shoulders",
                ("Narrow", "Broad"),
                false,
            ),
            FaceSlider(
                "torso/torso-scale-horiz-decr-incr",
                "Chest width",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "torso/torso-vshape-decr-incr",
                "Taper",
                ("Straight", "V-shaped"),
                false,
            ),
            FaceSlider(
                "torso/measure-waist-circ-decr-incr",
                "Waist",
                ("Slim", "Full"),
                false,
            ),
        ],
    ),
    (
        BodyPart::Hips,
        &[
            FaceSlider(
                "hip/hip-scale-horiz-decr-incr",
                "Hips",
                ("Narrow", "Wide"),
                false,
            ),
            FaceSlider(
                "stomach/stomach-pregnant-decr-incr",
                "Belly",
                ("Flat", "Round"),
                false,
            ),
            FaceSlider(
                "buttocks/buttocks-volume-decr-incr",
                "Seat",
                ("Small", "Full"),
                false,
            ),
        ],
    ),
    (
        BodyPart::Neck,
        &[
            FaceSlider(
                "neck/measure-neck-circ-decr-incr",
                "Thickness",
                ("Thin", "Thick"),
                false,
            ),
            FaceSlider(
                "neck/measure-neck-height-decr-incr",
                "Length",
                ("Short", "Long"),
                false,
            ),
            FaceSlider(
                "neck/neck-double-decr-incr",
                "Double chin",
                ("None", "Full"),
                false,
            ),
        ],
    ),
    (
        BodyPart::Arms,
        &[
            FaceSlider(
                "arms/upperarm-muscle-decr-incr",
                "Upper arms",
                ("Soft", "Muscular"),
                false,
            ),
            FaceSlider(
                "arms/upperarm-fat-decr-incr",
                "Arm fullness",
                ("Lean", "Full"),
                false,
            ),
            FaceSlider(
                "arms/lowerarm-muscle-decr-incr",
                "Forearms",
                ("Soft", "Muscular"),
                false,
            ),
        ],
    ),
    (
        BodyPart::Legs,
        &[
            FaceSlider(
                "legs/upperlegs-height-decr-incr",
                "Thigh length",
                ("Short", "Long"),
                false,
            ),
            FaceSlider(
                "legs/lowerlegs-height-decr-incr",
                "Shin length",
                ("Short", "Long"),
                false,
            ),
            FaceSlider(
                "legs/upperleg-fat-decr-incr",
                "Thighs",
                ("Lean", "Full"),
                false,
            ),
            FaceSlider(
                "legs/lowerleg-muscle-decr-incr",
                "Calves",
                ("Soft", "Muscular"),
                false,
            ),
        ],
    ),
];

/// The skin families, each with a colour to show it by.
pub(crate) const SKINS: [(&str, [u8; 3]); 3] = [
    ("caucasian", [236, 200, 178]),
    ("asian", [222, 184, 146]),
    ("african", [112, 72, 52]),
];

/// The pack's eye colours, each with a colour to show it by.
pub(crate) const EYES: [(&str, [u8; 3]); 6] = [
    ("brown", [92, 58, 34]),
    ("brownlight", [150, 108, 60]),
    ("green", [92, 128, 70]),
    ("bluegreen", [80, 128, 130]),
    ("blue", [70, 110, 170]),
    ("lightblue", [130, 170, 210]),
];

pub(crate) const HAIR: [(&str, &str); 20] = [
    ("culturalibre_hair_05", "Crop"),
    ("elvs_maxwell_hair", "Swept"),
    ("elvs_micky_afro", "Afro puffs"),
    ("toigo_blunt_bob_with_bangs", "Bob, fringe"),
    ("toigo_curled_under_bob", "Curled bob"),
    ("toigo_inverted_bob", "Inverted bob"),
    ("elvs_short_daisy_hair", "Shoulder"),
    ("elvs_katherine_hair", "Long, fringe"),
    ("elvs_ashley_may_hair", "Long waves"),
    ("elvs_50s_updo", "Updo"),
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

/// Named by their drawn shape (looked at 2026-10-27).
pub(crate) const EYEBROWS: [(&str, &str); 12] = [
    ("eyebrow001", "Natural"),
    ("eyebrow002", "Soft arch"),
    ("eyebrow003", "High arch"),
    ("eyebrow004", "Straight"),
    ("eyebrow005", "Flat"),
    ("eyebrow006", "Thin arch"),
    ("eyebrow007", "Fine"),
    ("eyebrow008", "Bold"),
    ("eyebrow009", "Bushy"),
    ("eyebrow010", "Rounded"),
    ("eyebrow011", "Dark arch"),
    ("eyebrow012", "Thick"),
];

pub(crate) const OUTFITS: [(&str, &str); 12] = [
    ("male_casualsuit01", "Denim shirt, jeans"),
    ("male_casualsuit03", "Striped shirt, jeans"),
    ("male_casualsuit05", "Jacket, jeans"),
    ("male_elegantsuit01", "Suit"),
    ("toigo_male_suit_tie_and_jacket", "Suit and tie"),
    ("toigo_male_double-breasted_suit", "Double-breasted"),
    ("toigo_suit_with_dinner_jacket", "Dinner jacket"),
    ("male_worksuit01", "Overalls"),
    ("female_elegantsuit01", "Blouse, skirt"),
    ("toigo_female_suit", "Skirt suit"),
    ("toigo_female_double-breasted_suit", "Trouser suit"),
    ("female_sportsuit01", "Sports top, leggings"),
];

pub(crate) const DRESSES: [(&str, &str); 6] = [
    ("toigo_shift_dress", "Shift dress"),
    ("toigo_halter_dress_knee_length", "Halter dress"),
    ("toigo_halter_dress_midi", "Long halter"),
    ("toigo_camisole_dress_with_full_skirt", "Sundress"),
    ("toigo_cut_out_dress", "Fitted dress"),
    ("toigo_strapless_ruffle_top_dress", "Ruffle dress"),
];

pub(crate) const HATS: [(&str, &str); 2] =
    [("fedora01", "Fedora"), ("fedora_cocked", "Fedora, tilted")];

/// Worn as `extras`, at most one of each list.
pub(crate) const BEARDS: [(&str, &str); 5] = [
    ("rehmanpolanski_moustache_viking", "Moustache"),
    ("culturalibre_dal_moustache", "Curled moustache"),
    ("culturalibre_faun_beard", "Goatee"),
    ("elvs_scruffy_beard1", "Scruffy"),
    ("rehmanpolanski_beard_viking", "Full beard"),
];

/// Separates: worn instead of an outfit (`outfit` empty), one of each, as
/// extras; tinted with the top and bottom colours.
pub(crate) const TOPS: [(&str, &str); 7] = [
    ("elvs_crude_t-shirt_male", "T-shirt"),
    ("joepal_crude_t-shirt_female", "Fitted tee"),
    ("toigo_basic_tucked_t-shirt", "Tucked tee"),
    ("namuhekam_male_polo_shirt", "Polo"),
    ("toigo_fisherman_sweater", "Sweater"),
    ("toigo_keyhole_tank_top", "Tank top"),
    ("toigo_camisole_top", "Camisole"),
];

pub(crate) const BOTTOMS: [(&str, &str); 3] = [
    ("toigo_wool_pants", "Trousers"),
    ("crude_loose_pants", "Loose trousers"),
    ("toigo_harem_pants", "Harem trousers"),
];

pub(crate) const GLASSES: [(&str, &str); 8] = [
    ("punkduck_glasses_modern_form", "Sunglasses"),
    ("punkduck_sport-sunglasses", "Sport shades"),
    ("spamrakuen_tbm_glasses_frames_01", "Black frames"),
    ("spamrakuen_sagerfrogs_glasses_01", "Angular"),
    ("spamrakuen_sagerfrogs_glasses_02", "Rectangular"),
    ("spamrakuen_sagerfrogs_glasses_03", "Wire"),
    ("spamrakuen_sagerfrogs_glasses_04", "Rimless"),
    ("toigo_round_glasses_leopard", "Round"),
];

pub(crate) const SHOES: [(&str, &str); 5] = [
    ("shoes01", "Brown leather"),
    ("shoes02", "Grey trainers"),
    ("shoes03", "Boots"),
    ("shoes05", "White trainers"),
    ("shoes06", "Blue trainers"),
];

/// Colours to pick for hair, over its neutral texture.
pub(crate) const HAIR_COLOURS: [[u8; 3]; 9] = [
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
pub(crate) const CLOTH_COLOURS: [[u8; 3]; 10] = [
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
pub(crate) struct Starter {
    pub(crate) label: &'static str,
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
    extras: &'static [&'static str],
    skin_tone: i8,
}

/// Fourteen people across body shape, age, build, skin and dress, so that most
/// looks are a starter and a few taps away. Only names the pack carries
/// (the lists above).
pub(crate) const STARTERS: [Starter; 14] = [
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
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
        extras: &[],
        skin_tone: 0,
    },
    Starter {
        label: "Professor",
        body_shape: 90,
        age: 62,
        build: [55, 60, 35],
        skin: "caucasian",
        hair: "short03",
        hair_colour: [150, 150, 150],
        outfit: "toigo_male_suit_tie_and_jacket",
        top: None,
        bottom: None,
        shoes: "shoes01",
        hat: None,
        extras: &[
            "rehmanpolanski_beard_viking",
            "spamrakuen_sagerfrogs_glasses_03",
        ],
        skin_tone: 20,
    },
    Starter {
        label: "Barista",
        body_shape: 80,
        age: 29,
        build: [50, 45, 50],
        skin: "asian",
        hair: "short04",
        hair_colour: [30, 24, 20],
        outfit: "male_casualsuit05",
        top: Some([40, 40, 44]),
        bottom: None,
        shoes: "shoes02",
        hat: None,
        extras: &["rehmanpolanski_moustache_viking"],
        skin_tone: -15,
    },
    Starter {
        label: "Lawyer",
        body_shape: 10,
        age: 41,
        build: [60, 45, 45],
        skin: "african",
        hair: "bob01",
        hair_colour: [30, 24, 20],
        outfit: "toigo_female_double-breasted_suit",
        top: None,
        bottom: None,
        shoes: "shoes01",
        hat: None,
        extras: &["spamrakuen_tbm_glasses_frames_01"],
        skin_tone: 25,
    },
    Starter {
        label: "Architect",
        body_shape: 0,
        age: 34,
        build: [45, 40, 40],
        skin: "caucasian",
        hair: "bob02",
        hair_colour: [196, 150, 94],
        outfit: "toigo_female_suit",
        top: Some([80, 120, 70]),
        bottom: Some([80, 120, 70]),
        shoes: "shoes01",
        hat: None,
        extras: &["toigo_round_glasses_leopard"],
        skin_tone: -20,
    },
    Starter {
        label: "Weekend",
        body_shape: 85,
        age: 36,
        build: [55, 55, 50],
        skin: "caucasian",
        hair: "culturalibre_hair_05",
        hair_colour: [110, 76, 50],
        outfit: "",
        top: Some([42, 62, 110]),
        bottom: Some([196, 160, 110]),
        shoes: "shoes02",
        hat: None,
        extras: &[
            "toigo_fisherman_sweater",
            "toigo_wool_pants",
            "elvs_scruffy_beard1",
        ],
        skin_tone: 0,
    },
    Starter {
        label: "Summer",
        body_shape: 5,
        age: 26,
        build: [50, 40, 40],
        skin: "asian",
        hair: "elvs_ashley_may_hair",
        hair_colour: [70, 48, 34],
        outfit: "toigo_camisole_dress_with_full_skirt",
        top: Some([230, 190, 80]),
        bottom: Some([230, 190, 80]),
        shoes: "shoes05",
        hat: None,
        extras: &["punkduck_glasses_modern_form"],
        skin_tone: 10,
    },
];

impl Starter {
    pub(crate) fn look(&self) -> HumanLook {
        let mut look = HumanLook::default_for(self.body_shape < 50);
        look.body_shape = self.body_shape;
        look.age = self.age;
        [look.height, look.weight, look.muscle] = self.build;
        look.skin = self.skin.into();
        look.skin_tone = self.skin_tone;
        look.hair = Some(self.hair.into());
        look.hair_colour = Some(self.hair_colour);
        look.outfit = self.outfit.into();
        look.top_colour = self.top;
        look.bottom_colour = self.bottom;
        look.shoes = self.shoes.into();
        look.hat = self.hat.map(Into::into);
        look.extras = self.extras.iter().map(|e| (*e).into()).collect();
        look
    }
}

/// Every starter's look: the people the street crowd is made of
/// (`systems::crowd`), which recolours and re-skins them.
pub(crate) fn starter_looks() -> Vec<HumanLook> {
    STARTERS.iter().map(Starter::look).collect()
}

/// What the camera does while the designer is open: which way the model
/// turns from the camera, and whether to come close to the face.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DesignerView {
    #[default]
    Front,
    Side,
    Back,
}

impl DesignerView {
    /// The model's turn away from facing the camera, radians.
    pub fn turn(self) -> f32 {
        match self {
            Self::Front => 0.0,
            Self::Side => std::f32::consts::FRAC_PI_2,
            Self::Back => std::f32::consts::PI,
        }
    }
}

/// The designer's state between frames.
#[derive(Default)]
pub struct DesignerState {
    pub tab: DesignerTab,
    pub face: FacePart,
    pub body: BodyPart,
    pub view: DesignerView,
    /// Undo: the looks before each change, oldest first, and the ones undone.
    history: Vec<HumanLook>,
    future: Vec<HumanLook>,
    /// The look as of the last undo step: a drag is one step, recorded when
    /// the pointer lets go.
    settled: Option<HumanLook>,
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
    // A third in separates, a dress for some feminine bodies, else an outfit.
    if dice.chance(33) {
        look.outfit.clear();
        look.extras.push(dice.pick(&TOPS).0.into());
        look.extras.push(dice.pick(&BOTTOMS).0.into());
    } else if look.body_shape < 40 && dice.chance(40) {
        look.outfit = dice.pick(&DRESSES).0.into();
    } else {
        look.outfit = dice.pick(&OUTFITS).0.into();
    }
    look.top_colour = dice.chance(60).then(|| *dice.pick(&CLOTH_COLOURS));
    look.bottom_colour = dice.chance(40).then(|| *dice.pick(&CLOTH_COLOURS));
    look.shoes = dice.pick(&SHOES).0.into();
    look.hat = dice.chance(10).then(|| dice.pick(&HATS).0.into());
    look.skin_tone = dice.int(-40..=40) as i8;
    if look.body_shape >= 60 && look.age >= 22 && dice.chance(30) {
        look.extras.push(dice.pick(&BEARDS).0.into());
    }
    if dice.chance(25 + i32::from(look.age) / 2) {
        look.extras.push(dice.pick(&GLASSES).0.into());
    }
    another_face(dice, &mut look);
    look
}

/// A face like this one: each control it has moved a little, and one or two
/// it has not, so a few taps walk toward a face rather than jumping between
/// strangers.
fn similar_face(dice: &mut Dice, look: &mut HumanLook) {
    let moved: Vec<(String, i8)> = look.face.clone();
    for (modifier, v) in moved {
        if FACE.iter().any(|(_, s)| s.iter().any(|f| f.0 == modifier)) {
            let one_sided = FACE
                .iter()
                .flat_map(|(_, s)| s.iter())
                .any(|f| f.0 == modifier && f.3);
            let lo = if one_sided { 0 } else { -100 };
            let n = (i32::from(v) + dice.int(-15..=15)).clamp(lo, 100);
            look.set_face(&modifier, n as i8);
        }
    }
    for _ in 0..dice.int(1..=2) {
        let (_, sliders) = dice.pick(&FACE);
        let FaceSlider(modifier, _, _, one_sided) = dice.pick(sliders);
        if look.face_value(modifier) == 0 {
            let lo = if *one_sided { 0 } else { -25 };
            look.set_face(modifier, dice.int(lo..=25) as i8);
        }
    }
}

/// One of `list` worn as an extra, or none: the extras hold at most one from
/// each list (a beard, glasses).
pub(crate) fn extra_of<'a>(look: &HumanLook, list: &[(&'a str, &'a str)]) -> Option<&'a str> {
    list.iter()
        .map(|(n, _)| *n)
        .find(|n| look.extras.iter().any(|e| e == n))
}

pub(crate) fn set_extra(look: &mut HumanLook, list: &[(&str, &str)], name: Option<&str>) {
    look.extras.retain(|e| !list.iter().any(|(n, _)| n == e));
    if let Some(n) = name {
        look.extras.push(n.into());
    }
}

/// Put on a whole outfit (or a dress), taking off any separates.
pub(crate) fn wear_outfit(look: &mut HumanLook, name: &str) {
    look.outfit = name.into();
    set_extra(look, &TOPS, None);
    set_extra(look, &BOTTOMS, None);
}

/// Change into separates: no outfit, and a top and a bottom — the first of
/// each unless some are worn already.
pub(crate) fn wear_separates(look: &mut HumanLook) {
    look.outfit.clear();
    if extra_of(look, &TOPS).is_none() {
        set_extra(look, &TOPS, Some(TOPS[0].0));
    }
    if extra_of(look, &BOTTOMS).is_none() {
        set_extra(look, &BOTTOMS, Some(BOTTOMS[0].0));
    }
}

/// A different face: a handful of the face controls, each a little off
/// average.
fn another_face(dice: &mut Dice, look: &mut HumanLook) {
    clear_face(look);
    for (_, sliders) in FACE.iter() {
        for FaceSlider(modifier, _, _, one_sided) in sliders.iter() {
            if dice.chance(40) {
                let lo = if *one_sided { 0 } else { -45 };
                look.set_face(modifier, dice.int(lo..=45) as i8);
            }
        }
    }
}

/// The width of the library's panel (`lib_avatar`) and of the empty strip
/// egui keeps under it, in points.
pub const LIB_PANEL_WIDTH: f32 = 320.0;

/// Apply: the draft becomes the saved appearance, which
/// `human_avatar::rebuild_on_look_change` watches. Both editions' Apply.
pub fn apply_draft(
    chooser: &crate::resources::AvatarChooserState,
    store: &mut crate::systems::user_store::UserStore,
) {
    store.data.appearance = chooser.draft.clone();
    store.dirty = true;
}

/// Most undo steps kept.
const HISTORY: usize = 50;

/// Keep the undo history: once the pointer is up, a look that differs from
/// the last step is a new step, so a slider drag is one step, not sixty.
fn record(state: &mut DesignerState, look: &HumanLook, dragging: bool) {
    let Some(settled) = &state.settled else {
        state.settled = Some(look.clone());
        return;
    };
    if dragging || settled == look {
        return;
    }
    state.history.push(settled.clone());
    if state.history.len() > HISTORY {
        state.history.remove(0);
    }
    state.future.clear();
    state.settled = Some(look.clone());
}

/// What the designer does, for both its editions: egui's (`designer`) and
/// the component library's (`lib_avatar`), so the two cannot drift apart.
impl DesignerState {
    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn undo(&mut self, look: &mut HumanLook) {
        if let Some(prev) = self.history.pop() {
            self.future.push(std::mem::replace(look, prev));
            self.settled = Some(look.clone());
        }
    }

    pub fn redo(&mut self, look: &mut HumanLook) {
        if let Some(next) = self.future.pop() {
            self.history.push(std::mem::replace(look, next));
            self.settled = Some(look.clone());
        }
    }

    /// Back to the look last applied: a step like any other, so it can
    /// itself be undone.
    pub fn back_to_saved(&mut self, look: &mut HumanLook, saved: &HumanLook) {
        if look != saved {
            self.history.push(std::mem::replace(look, saved.clone()));
            self.future.clear();
            self.settled = Some(look.clone());
        }
    }

    /// "Surprise me": a whole person from the dice. `time` stirs them.
    pub fn surprise(&mut self, look: &mut HumanLook, time: f64) {
        *look = surprise(&mut Dice { state: self, time });
    }

    /// "Another face".
    pub fn another_face(&mut self, look: &mut HumanLook, time: f64) {
        another_face(&mut Dice { state: self, time }, look);
    }

    /// "Similar face".
    pub fn similar_face(&mut self, look: &mut HumanLook, time: f64) {
        similar_face(&mut Dice { state: self, time }, look);
    }

    /// Keep the undo history (`record`): call once a frame with whether a
    /// pointer is down.
    pub fn record(&mut self, look: &HumanLook, dragging: bool) {
        record(self, look, dragging);
    }
}

/// The designer's four tabs, as both editions label them.
pub(crate) const TABS: [(DesignerTab, &str); 4] = [
    (DesignerTab::Body, "Body"),
    (DesignerTab::Face, "Face"),
    (DesignerTab::SkinAndHair, "Skin & hair"),
    (DesignerTab::Clothes, "Clothes"),
];

/// The sides of the avatar to see.
pub(crate) const VIEWS: [(DesignerView, &str); 3] = [
    (DesignerView::Front, "Front"),
    (DesignerView::Side, "Side"),
    (DesignerView::Back, "Back"),
];

/// The face parts, in the Face tab's order.
pub(crate) const FACE_PARTS: [(FacePart, &str); 6] = [
    (FacePart::Head, "Head"),
    (FacePart::Eyes, "Eyes"),
    (FacePart::Nose, "Nose"),
    (FacePart::Mouth, "Mouth"),
    (FacePart::Jaw, "Jaw"),
    (FacePart::Ears, "Ears"),
];

/// The body parts under "Body parts".
pub(crate) const BODY_PARTS: [(BodyPart, &str); 5] = [
    (BodyPart::Torso, "Torso"),
    (BodyPart::Hips, "Hips"),
    (BodyPart::Neck, "Neck"),
    (BodyPart::Arms, "Arms"),
    (BodyPart::Legs, "Legs"),
];

/// The Body tab's quick picks, each setting one or two sliders.
pub(crate) const SHAPE_PICKS: [(&str, [u8; 1]); 3] = [
    ("Feminine", [0]),
    ("In between", [50]),
    ("Masculine", [100]),
];
pub(crate) const AGE_PICKS: [(&str, [u8; 1]); 7] = [
    ("20s", [24]),
    ("30s", [34]),
    ("40s", [44]),
    ("50s", [54]),
    ("60s", [64]),
    ("70s", [74]),
    ("80s", [84]),
];
/// Weight and muscle together: a build is both.
pub(crate) const BUILD_PICKS: [(&str, [u8; 2]); 5] = [
    ("Slim", [25, 40]),
    ("Average", [50, 50]),
    ("Athletic", [40, 85]),
    ("Sturdy", [70, 70]),
    ("Heavy", [90, 40]),
];
pub(crate) const HEIGHT_PICKS: [(&str, [u8; 1]); 3] =
    [("Short", [20]), ("Average", [50]), ("Tall", [80])];

/// One of the body's whole-body sliders under "Fine-tune".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Macro {
    BodyShape,
    Age,
    Height,
    Weight,
    Muscle,
    Proportions,
    Chest,
}

impl Macro {
    pub(crate) fn get(self, look: &HumanLook) -> u8 {
        match self {
            Macro::BodyShape => look.body_shape,
            Macro::Age => look.age,
            Macro::Height => look.height,
            Macro::Weight => look.weight,
            Macro::Muscle => look.muscle,
            Macro::Proportions => look.proportions,
            Macro::Chest => look.chest,
        }
    }

    pub(crate) fn get_mut(self, look: &mut HumanLook) -> &mut u8 {
        match self {
            Macro::BodyShape => &mut look.body_shape,
            Macro::Age => &mut look.age,
            Macro::Height => &mut look.height,
            Macro::Weight => &mut look.weight,
            Macro::Muscle => &mut look.muscle,
            Macro::Proportions => &mut look.proportions,
            Macro::Chest => &mut look.chest,
        }
    }
}

/// A "Fine-tune" slider: which, its label, its range, its five words (or
/// the number, for the age) and what its ends mean.
pub(crate) struct MacroSlider {
    pub(crate) which: Macro,
    pub(crate) label: &'static str,
    pub(crate) range: (u8, u8),
    pub(crate) words: Option<[&'static str; 5]>,
    pub(crate) ends: (&'static str, &'static str),
}

pub(crate) const MACROS: [MacroSlider; 7] = [
    MacroSlider {
        which: Macro::BodyShape,
        label: "Body shape",
        range: (0, 100),
        words: Some([
            "feminine",
            "more feminine",
            "in between",
            "more masculine",
            "masculine",
        ]),
        ends: ("Feminine", "Masculine"),
    },
    MacroSlider {
        which: Macro::Age,
        label: "Age",
        range: (18, 90),
        words: None,
        ends: ("18", "90"),
    },
    MacroSlider {
        which: Macro::Height,
        label: "Height",
        range: (0, 100),
        words: Some(["short", "shorter", "average", "taller", "tall"]),
        ends: ("Short", "Tall"),
    },
    MacroSlider {
        which: Macro::Weight,
        label: "Weight",
        range: (0, 100),
        words: Some(["slim", "slimmer", "average", "heavier", "heavy"]),
        ends: ("Slim", "Heavy"),
    },
    MacroSlider {
        which: Macro::Muscle,
        label: "Muscle",
        range: (0, 100),
        words: Some(["soft", "softer", "average", "toned", "muscular"]),
        ends: ("Soft", "Muscular"),
    },
    MacroSlider {
        which: Macro::Proportions,
        label: "Proportions",
        range: (0, 100),
        words: Some(["unusual", "less usual", "average", "classic", "classic"]),
        ends: ("Unusual", "Classic"),
    },
    MacroSlider {
        which: Macro::Chest,
        label: "Chest",
        range: (0, 100),
        words: Some(["small", "smaller", "average", "fuller", "full"]),
        ends: ("Small", "Full"),
    },
];

/// The word beside a face or body-part slider.
pub(crate) fn shape_says(v: i32, ends: (&str, &str)) -> String {
    match v {
        0 => "average".to_string(),
        v if v > 0 => format!("{} {}", ends.1.to_lowercase(), v),
        v => format!("{} {}", ends.0.to_lowercase(), -v),
    }
}

/// The word beside the skin's tone slider.
pub(crate) fn tone_says(tone: i32) -> String {
    match tone {
        0 => "as painted".to_string(),
        t if t > 0 => format!("lighter {t}"),
        t => format!("darker {}", -t),
    }
}

/// Whether any body-part control has moved ("Average body parts" shows).
pub(crate) fn body_parts_moved(look: &HumanLook) -> bool {
    BODY.iter()
        .flat_map(|(_, s)| s.iter())
        .any(|s| look.face_value(s.0) != 0)
}

/// "Average body parts".
pub(crate) fn average_body_parts(look: &mut HumanLook) {
    for s in BODY.iter().flat_map(|(_, s)| s.iter()) {
        look.set_face(s.0, 0);
    }
}

impl DesignerState {
    /// Whether the camera should come close to the face: the Face tab is a
    /// set of controls for things a few millimetres across.
    pub fn close_up(&self) -> bool {
        self.tab == DesignerTab::Face
    }
}

/// Whether a face control is a body's: the look's shape list holds both.
fn is_body_control(modifier: &str) -> bool {
    modifier
        .split_once('/')
        .is_some_and(|(group, _)| BODY_GROUPS.contains(&group))
}

/// The pack's body groups (`human-pack`'s `modifier_groups` below the head),
/// so a body control the designer does not offer is still not the face's.
const BODY_GROUPS: [&str; 7] = [
    "neck", "torso", "hip", "stomach", "buttocks", "arms", "legs",
];

pub(crate) fn has_face(look: &HumanLook) -> bool {
    look.face.iter().any(|(m, _)| !is_body_control(m))
}

/// Back to the average face, keeping the body's controls.
pub(crate) fn clear_face(look: &mut HumanLook) {
    look.face.retain(|(m, _)| is_body_control(m));
}

/// A word for where a 0–100 slider sits, in five bands.
pub(crate) fn word(v: u8, words: [&str; 5]) -> String {
    words[(usize::from(v.min(99)) * 5 / 100).min(4)].to_string()
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
                OUTFITS
                    .iter()
                    .chain(&DRESSES)
                    .any(|(n, _)| *n == look.outfit)
                    || (look.outfit.is_empty()
                        && extra_of(&look, &TOPS).is_some()
                        && extra_of(&look, &BOTTOMS).is_some()),
                "{}: an outfit, a dress, or a top and a bottom",
                s.label
            );
            assert!(SHOES.iter().any(|(n, _)| *n == look.shoes), "{}", s.label);
            assert!(HAIR_COLOURS.contains(&s.hair_colour), "{}", s.label);
            assert!(
                look.hat
                    .as_deref()
                    .is_none_or(|h| HATS.iter().any(|(n, _)| *n == h)),
                "{}",
                s.label
            );
            for e in &look.extras {
                assert!(
                    BEARDS
                        .iter()
                        .chain(&GLASSES)
                        .chain(&TOPS)
                        .chain(&BOTTOMS)
                        .any(|(n, _)| n == e),
                    "{}: {e}",
                    s.label
                );
            }
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

    /// The face's buttons leave the body's controls alone, though both live
    /// in one list; and a similar face is near the one it started from.
    #[test]
    fn a_new_face_keeps_the_body() {
        let mut look = HumanLook::default_for(false);
        look.set_face("hip/hip-scale-horiz-decr-incr", 40);
        look.set_face("nose/nose-hump-decr-incr", 60);
        let mut state = DesignerState::default();
        another_face(
            &mut Dice {
                state: &mut state,
                time: 1.0,
            },
            &mut look,
        );
        assert_eq!(look.face_value("hip/hip-scale-horiz-decr-incr"), 40);
        clear_face(&mut look);
        assert_eq!(
            look.face,
            vec![("hip/hip-scale-horiz-decr-incr".into(), 40)]
        );
        look.set_face("nose/nose-hump-decr-incr", 60);
        similar_face(
            &mut Dice {
                state: &mut state,
                time: 2.0,
            },
            &mut look,
        );
        let hump = look.face_value("nose/nose-hump-decr-incr");
        assert!((45..=75).contains(&hump), "{hump}");
        assert_eq!(look.face_value("hip/hip-scale-horiz-decr-incr"), 40);
        assert!(look.is_reasonable());
    }

    /// A drag is one undo step, and undo, redo and back again land where
    /// they should.
    #[test]
    fn undo_steps_over_whole_changes() {
        let mut state = DesignerState::default();
        let mut look = HumanLook::default_for(false);
        record(&mut state, &look, false);
        for w in [51, 60, 70] {
            look.weight = w;
            record(&mut state, &look, true);
        }
        record(&mut state, &look, false);
        assert_eq!(state.history.len(), 1, "the drag is one step");
        let undone = state.history.pop().unwrap();
        assert_eq!(undone.weight, 50);
    }

    /// One beard and one pair of glasses at most: choosing another replaces
    /// the one worn.
    #[test]
    fn an_extra_replaces_its_own_kind() {
        let mut look = HumanLook::default_for(false);
        set_extra(&mut look, &BEARDS, Some(BEARDS[0].0));
        set_extra(&mut look, &GLASSES, Some(GLASSES[0].0));
        set_extra(&mut look, &BEARDS, Some(BEARDS[2].0));
        assert_eq!(look.extras.len(), 2);
        assert_eq!(extra_of(&look, &BEARDS), Some(BEARDS[2].0));
        set_extra(&mut look, &GLASSES, None);
        assert_eq!(look.extras, vec![BEARDS[2].0.to_string()]);
    }

    /// Every face and body part has sliders, and no modifier is offered
    /// twice; the body's are all in the groups the face buttons leave alone.
    #[test]
    fn the_face_tab_offers_each_control_once() {
        let mut seen = std::collections::HashSet::new();
        for (_, sliders) in BODY.iter() {
            for s in sliders.iter() {
                assert!(is_body_control(s.0), "{}", s.0);
                assert!(seen.insert(s.0), "{} twice", s.0);
            }
        }
        for (_, sliders) in FACE.iter() {
            assert!(!sliders.is_empty());
            for s in sliders.iter() {
                assert!(seen.insert(s.0), "{} twice", s.0);
            }
        }
    }
}
