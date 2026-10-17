//! The avatar designer: MakeHuman's body sliders, a skin tone, hair and an
//! outfit, edited on a draft the avatar is rebuilt from as it changes
//! (`docs/mockups/avatar-designer`, `systems::human_avatar`).
//!
//! Drawn inside the Appearance panel when `CARTO_HUMAN_AVATAR` is on, in place
//! of the slot-by-slot chooser; the panel keeps its Apply, which saves the
//! draft. There is no Face tab yet: the face targets are not in the pack.

use crate::systems::theme;
use crate::systems::user_store::HumanLook;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum DesignerTab {
    #[default]
    Body,
    SkinAndHair,
    Clothes,
}

/// Skin tones, multiplied over the pack's one light skin: lightest first.
const SKIN_TONES: [[u8; 3]; 8] = [
    [255, 255, 255],
    [242, 222, 206],
    [222, 190, 160],
    [196, 152, 118],
    [164, 116, 84],
    [128, 86, 60],
    [96, 64, 46],
    [70, 48, 36],
];

/// Hair tints over the hair's own texture; white keeps it as authored.
const HAIR_COLOURS: [[u8; 3]; 7] = [
    [255, 255, 255],
    [150, 110, 80],
    [96, 70, 50],
    [52, 40, 32],
    [200, 120, 70],
    [235, 210, 150],
    [170, 170, 170],
];

const OUTFIT_COLOURS: [[u8; 3]; 7] = [
    [255, 255, 255],
    [70, 74, 84],
    [96, 130, 190],
    [170, 70, 70],
    [110, 150, 90],
    [230, 190, 90],
    [220, 140, 180],
];

/// The outfits in the pack, by proxy name and label.
const OUTFITS: [(&str, &str); 2] = [
    ("male_casualsuit06", "T-shirt and jeans"),
    ("female_casualsuit01", "Fitted T-shirt and jeans"),
];

/// The designer's body. Returns whether the look changed this frame.
pub fn designer(ui: &mut egui::Ui, tab: &mut DesignerTab, look: &mut HumanLook) -> bool {
    let before = look.clone();
    theme::segmented(
        ui,
        tab,
        &[
            (DesignerTab::Body, "Body"),
            (DesignerTab::SkinAndHair, "Skin & hair"),
            (DesignerTab::Clothes, "Clothes"),
        ],
    );
    ui.add_space(8.0);
    match tab {
        DesignerTab::Body => body(ui, look),
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

fn skin_and_hair(ui: &mut egui::Ui, look: &mut HumanLook) {
    theme::section(ui, "Skin");
    swatches(ui, "skin", &SKIN_TONES, &mut look.skin_tone);
    theme::section(ui, "Hair");
    ui.horizontal_wrapped(|ui| {
        let short = look.hair.as_deref() == Some("short02");
        if theme::chip(ui, "Short", short).clicked() {
            look.hair = Some("short02".into());
        }
        if theme::chip(ui, "None", look.hair.is_none()).clicked() {
            look.hair = None;
        }
    });
    ui.add_space(4.0);
    swatches(ui, "hair", &HAIR_COLOURS, &mut look.hair_colour);
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
    theme::section(ui, "Colour");
    swatches(ui, "outfit", &OUTFIT_COLOURS, &mut look.outfit_colour);
    theme::note(
        ui,
        "Each outfit stretches with the body, so it still fits when the sliders move.",
    );
}

/// A word for where a 0–100 slider sits, in five bands.
fn word(v: u8, words: [&str; 5]) -> String {
    words[(usize::from(v.min(99)) * 5 / 100).min(4)].to_string()
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
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(says).color(theme::TEXT_DIM));
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(egui::Slider::new(value, range).show_value(false));
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

/// A row of colour discs; the chosen one ringed. The colours are the look's,
/// not the interface's, so they are data here rather than theme colours.
fn swatches(ui: &mut egui::Ui, id: &str, colours: &[[u8; 3]], value: &mut [u8; 3]) {
    let d = theme::row_height().max(32.0);
    ui.horizontal_wrapped(|ui| {
        for (i, c) in colours.iter().enumerate() {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(d, d), egui::Sense::click());
            let response = response.on_hover_text(format!("{id} {}", i + 1));
            let p = ui.painter();
            let centre = rect.center();
            let r = d * 0.5 - 4.0;
            p.circle_filled(centre, r, egui::Color32::from_rgb(c[0], c[1], c[2]));
            if value == c {
                p.circle_stroke(centre, r + 2.5, egui::Stroke::new(2.0, theme::ACCENT));
            } else {
                p.circle_stroke(centre, r, egui::Stroke::new(1.0, theme::LINE));
            }
            if response.clicked() {
                *value = *c;
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
}
