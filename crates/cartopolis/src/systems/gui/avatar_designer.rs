//! The avatar designer: MakeHuman's body and face sliders, a skin, eyes, hair
//! and a wardrobe, edited on a draft the avatar is rebuilt from as it changes
//! (`docs/mockups/avatar-designer`, `systems::human_avatar`).
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

/// The designer's state between frames.
#[derive(Default)]
pub struct DesignerState {
    pub tab: DesignerTab,
    pub face: FacePart,
}

/// The designer's body. Returns whether the look changed this frame.
pub fn designer(ui: &mut egui::Ui, state: &mut DesignerState, look: &mut HumanLook) -> bool {
    let before = look.clone();
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
        DesignerTab::Face => face(ui, &mut state.face, look),
        DesignerTab::SkinAndHair => skin_and_hair(ui, look),
        DesignerTab::Clothes => clothes(ui, look),
    }
    *look != before
}

fn body(ui: &mut egui::Ui, look: &mut HumanLook) {
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

fn face(ui: &mut egui::Ui, part: &mut FacePart, look: &mut HumanLook) {
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
    if !look.face.is_empty() && theme::action_button(ui, "Reset the face").clicked() {
        look.face.clear();
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
