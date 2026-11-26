//! People's discs — a picture or a round initial — and the round button, which
//! the world draws as well as the messenger: on the dock, over people on the
//! map, on the plans card. Moved out of `systems::chats` so a build without
//! `comms` (the web build, whose messenger is the page) keeps them; `chats`
//! names them back, so its callers are unchanged.

use crate::systems::chat::ChatState;
use crate::systems::{icons, theme};

/// The round initials, picked by name so a person keeps theirs (`.c1`–`.c6`).
///
/// Lifted from the mockup's muted set on 2026-10-04: on the ink screen those
/// read as six shades of the same mud, and a person's colour is the one
/// thing in a list of names that is theirs. Each still carries white initials
/// at better than 3:1.
const AVATAR_TONES: [egui::Color32; 6] = [
    egui::Color32::from_rgb(0x86, 0x5C, 0xC4),
    egui::Color32::from_rgb(0x2C, 0x91, 0x7C),
    egui::Color32::from_rgb(0xC0, 0x66, 0x30),
    egui::Color32::from_rgb(0x43, 0x74, 0xC4),
    egui::Color32::from_rgb(0xB8, 0x48, 0x68),
    egui::Color32::from_rgb(0x5F, 0x8A, 0x2E),
];

/// Blend `base` toward `over` by `k`.
pub(crate) fn blend(base: egui::Color32, over: egui::Color32, k: f32) -> egui::Color32 {
    let ch = |b: u8, o: u8| (f32::from(b) + (f32::from(o) - f32::from(b)) * k).round() as u8;
    egui::Color32::from_rgb(
        ch(base.r(), over.r()),
        ch(base.g(), over.g()),
        ch(base.b(), over.b()),
    )
}

/// A disc lit from above: `tone` in the middle, lighter at the top rim and
/// deeper at the bottom, with a faint light edge. What makes an initial read
/// as a token rather than a flat sticker.
fn lit_disc(p: &egui::Painter, c: egui::Pos2, r: f32, tone: egui::Color32) {
    const SEGMENTS: u32 = 40;
    let light = blend(tone, egui::Color32::WHITE, 0.30);
    let deep = blend(tone, egui::Color32::BLACK, 0.22);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, tone);
    for i in 0..=SEGMENTS {
        let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let d = egui::vec2(a.cos(), a.sin());
        // `d.y` is -1 at the top of the disc and +1 at the bottom.
        let k = (d.y + 1.0) / 2.0;
        let rim = if k < 0.5 {
            blend(light, tone, k * 2.0)
        } else {
            blend(tone, deep, (k - 0.5) * 2.0)
        };
        mesh.colored_vertex(c + r * d, rim);
        if i > 0 {
            mesh.add_triangle(0, i, i + 1);
        }
    }
    p.add(mesh);
    p.circle_stroke(
        c,
        r - 0.5,
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 40),
        ),
    );
}

/// The "Here" disc: the v1 inbox's dark slate, with a location pin on it.
const HERE_TONE: egui::Color32 = egui::Color32::from_rgb(0x3F, 0x3B, 0x38);

/// What kind of chat a channel is, which decides what it says about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Here,
    Everyone,
    /// A topic of a community: a private chat, drawn as a place people gather.
    Topic,
    /// A community itself (its row in the list and its screens).
    Community,
}

#[must_use]
pub fn kind_of(key: &str) -> ChatKind {
    if ChatState::is_private(key) {
        ChatKind::Private
    } else if key == "local" {
        ChatKind::Here
    } else {
        // "geo:", and anything else public: rooms are gone (protocol 39).
        ChatKind::Everyone
    }
}

pub(crate) fn tone_for(name: &str) -> egui::Color32 {
    let h = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    AVATAR_TONES[h as usize % AVATAR_TONES.len()]
}

/// A round initial, the stand-in for a picture of the person or the group.
pub(crate) fn avatar(ui: &mut egui::Ui, name: &str, kind: ChatKind, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(size), egui::Sense::hover());
    paint_avatar(ui.painter(), rect.center(), name, kind, size);
}

pub(crate) fn paint_avatar(
    p: &egui::Painter,
    c: egui::Pos2,
    name: &str,
    kind: ChatKind,
    size: f32,
) {
    // A person with a picture is their picture (`systems::faces`).
    if matches!(kind, ChatKind::Private) {
        if let Some(texture) = crate::systems::faces::texture_for(name) {
            crate::systems::faces::paint_face(p, c, size / 2.0, texture);
            return;
        }
    }
    let DiscLook {
        tone,
        text,
        scale,
        square,
        ..
    } = disc_look(name, kind);
    if square {
        let rect = egui::Rect::from_center_size(c, egui::Vec2::splat(size));
        p.rect_filled(rect, egui::CornerRadius::same((size * 0.29) as u8), tone);
    } else {
        lit_disc(p, c, size / 2.0, tone);
    }
    p.text(
        c,
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(size * scale),
        egui::Color32::WHITE,
    );
}

/// How a chat's disc looks when it has no picture: its tone, what is on it
/// (white), how large that is against the disc, and its shape — for egui's
/// [`paint_avatar`] and the component library's discs (`lib_chats`).
#[derive(Clone, Debug, PartialEq)]
pub struct DiscLook {
    pub tone: egui::Color32,
    /// The initial, `#`, or an icon glyph.
    pub text: String,
    /// `text` is a glyph of the icon font (egui's fonts fall back to it).
    #[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
    pub icon: bool,
    /// The text's size as a share of the disc's.
    pub scale: f32,
    /// A topic or a community is a place people gather rather than a
    /// person, so it is the design's rounded square (`.room`), not a circle.
    pub square: bool,
}

/// The look of `name`'s disc for a chat of `kind`.
#[must_use]
pub fn disc_look(name: &str, kind: ChatKind) -> DiscLook {
    let initial = || -> String { name.chars().next().unwrap_or('?').to_uppercase().collect() };
    let (tone, text, icon, scale) = match kind {
        ChatKind::Here => (HERE_TONE, icons::PIN.to_string(), true, 0.45),
        ChatKind::Everyone => (AVATAR_TONES[3], icons::CAM_EARTH.to_string(), true, 0.45),
        ChatKind::Topic => (AVATAR_TONES[5], "#".to_string(), false, 0.42),
        ChatKind::Community => (tone_for(name), initial(), false, 0.4),
        ChatKind::Private => (tone_for(name), initial(), false, 0.38),
    };
    DiscLook {
        tone,
        text,
        icon,
        scale,
        square: matches!(kind, ChatKind::Topic | ChatKind::Community),
    }
}

/// A round button of the design's composer (`.circ`): raised, or the pale
/// accent when it is the one that acts (`.circ.hot`).
pub(crate) fn circle_button(
    ui: &mut egui::Ui,
    icon: &str,
    hot: bool,
    enabled: bool,
    hint: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(38.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, hint));
    let (fill, ink) = match (hot, enabled) {
        (true, true) => (theme::p32().accent, theme::p32().on_accent),
        (true, false) => (theme::p32().accent_muted, theme::p32().text_dim),
        (false, _) if resp.hovered() => (theme::p32().surface_hover, theme::p32().text),
        (false, _) => (theme::p32().surface_raised, theme::p32().text),
    };
    let p = ui.painter();
    p.circle_filled(rect.center(), 19.0, fill);
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(20.0),
        ink,
    );
    resp.on_hover_text(hint)
}

/// A Polaroid's paper: warm white rather than pure white, which would glare
/// against the ink of the panel, and the edge it leaves round the picture.
pub(crate) const POLAROID_WHITE: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xF2, 0xEC);

/// What is written on it, and the time in the corner.
pub(crate) const POLAROID_INK: egui::Color32 = egui::Color32::from_rgb(0x2A, 0x2E, 0x33);
