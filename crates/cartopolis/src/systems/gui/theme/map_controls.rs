use super::*;

// ─── The map's floating controls ──────────────────────────────────────────────
//
// Everything that floats over the map — the dock, the compass column, the
// on-screen navigation controls, the scale bar — is laid out from these three
// numbers and painted by [`map_disc`]. They used to be per-module constants, and
// what that bought was six edge insets (8, 10, 12, 14, 16 and a bare `-10`),
// four disc diameters (28, 40, 44, 46) and two different answers to what a held
// control looks like, all within one screen. The point of a floating control is
// that it reads as *chrome over a map* rather than as part of the map, and that
// only works if there is one of it.

/// Inset of a floating control from the window edge, in points. One number for
/// all four edges: a top-left column indented 12 and a top margin of 8 is a
/// corner that looks like a mistake.
pub const OSD_MARGIN: f32 = 12.0;
/// Gap between two controls that belong to one cluster (the two halves of a
/// zoom pair, the compass and the locate button under it).
pub const OSD_GAP: f32 = 8.0;
/// Gap between two clusters sharing an edge. Deliberately more than twice
/// [`OSD_GAP`]: with one gap for both, a column of discs reads as a single
/// undifferentiated strip, which is what the layers disc and the compass under
/// it had become.
pub const OSD_GROUP_GAP: f32 = 18.0;

/// Where a [`map_disc`] wants its mark drawn, and in what ink.
pub struct DiscFace {
    pub center: egui::Pos2,
    pub radius: f32,
    pub ink: Color32,
}

/// Allocate and paint one floating map disc, and hand back where to draw its
/// mark. The plate — fill, ring, hover and pressed states, focus ring — is this
/// function's business; the glyph inside it is the caller's, because half of
/// them are painted from line segments rather than typed ([`close_button`] has
/// the reason).
///
/// `on` is the engaged state — a panel open, a lock held, a button pressed — and
/// wears the same [`Palette::accent`] plate a selected dock disc does, so "this is on" is
/// one treatment everywhere on the map: a dark glyph on a teal plate, which
/// is unmistakable against a map at any brightness. `tone` is the rim that engagement carries: [`Palette::accent_ring`]
/// normally, [`Palette::accent_bright`] for the *held* states (north-up lock,
/// follow-and-rotate), which also draw a second, heavier ring — with no second
/// accent left, "held" is said by weight, not by colour.
///
/// `sense` is the caller's, because the tests differ by what the control means:
/// a rate wants `click_and_drag` (a finger drifting a point while held is a
/// drag, and `clicked()` drops the whole push), an edge wants `click()`.
pub fn map_disc(
    ui: &mut egui::Ui,
    size: f32,
    on: bool,
    tone: Color32,
    sense: egui::Sense,
) -> (egui::Response, DiscFace) {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(size), sense);
    let held = resp.is_pointer_button_down_on();
    let (fill, ring, ink) = match (on || held, resp.hovered()) {
        (true, _) => (p32().accent, tone, p32().on_accent),
        (false, true) => (p32().surface_hover, p32().accent, p32().cream),
        (false, false) => (p32().map_fill, p32().map_rim, p32().map_ink),
    };
    let radius = (size * 0.5 - 1.0) * crate::systems::motion::press(&resp);
    let p = ui.painter();
    paint_disc_shadow(p, rect.center(), radius, lift_shadow());
    p.circle_filled(rect.center(), radius, fill);
    p.circle_stroke(rect.center(), radius, Stroke::new(1.5, ring));
    paint_contact_edge(p, rect.center(), radius + 0.75);
    paint_focus_ring(ui, &resp, rect, true);
    (
        resp,
        DiscFace {
            center: rect.center(),
            radius,
            ink,
        },
    )
}

/// Midas's gold: the hub's theme and its icon (midas `web/src/icon.svg`), which
/// is [`Palette::warn`]'s value because both are that theme's warning colour —
/// in the light theme too, where the hub's warning is an amber-brown that
/// reads on the light disc the mark sits on.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))] // the link is native-only
#[must_use]
pub fn midas_gold() -> Rgba8 {
    p().warn
}

/// The Midas mark — an M of five joined points, the hub and its machines — in
/// [`midas_gold`], drawn on a disc of `radius` at `c`. Its geometry is the
/// icon's, in that SVG's 64-unit square centred on (32, 32); the gold square
/// behind it there is the disc here.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn paint_midas_mark(p: &egui::Painter, c: egui::Pos2, radius: f32) {
    let gold = midas_gold();
    let s = radius / 26.0;
    let at = |x: f32, y: f32| c + egui::vec2(x - 32.0, y - 32.0) * s;
    let points = [
        at(20.0, 43.0),
        at(20.0, 21.0),
        at(32.0, 33.0),
        at(44.0, 21.0),
        at(44.0, 43.0),
    ];
    p.add(egui::Shape::line(
        points.to_vec(),
        Stroke::new(5.0 * s, gold),
    ));
    for pt in points {
        p.circle_filled(pt, 4.0 * s, gold);
    }
}

/// The look of one icon disc, shared by [`round_button`] and the medallion a
/// panel header wears — they have to be the same object at two sizes, or the
/// header stops reading as "the dock button you just pressed".
pub(super) struct Disc<'a> {
    pub(super) fill: Color32,
    pub(super) ring: Color32,
    pub(super) ink: Color32,
    pub(super) icon: &'a str,
    pub(super) icon_size: f32,
    /// Width of the ring. A disc that floats over the map needs the heavier one
    /// ([`map_disc`] uses the same value): its ring is the only thing separating
    /// it from a bright tile, and at 1 pt over pale beige paper the plate read as
    /// a flat hole with no edge at all. A medallion inside a panel header has the
    /// header band behind it and keeps the hairline.
    pub(super) ring_width: f32,
}

/// A translucent-black hairline just outside a floating plate.
///
/// **The cost of inverting the engaged state, and the fix for it.** An engaged
/// disc is [`Palette::accent`] now — a light plate — and the map's brightest regions are
/// the sky and the near-white paper the basemap is printed on, so over those the
/// plate and its lighter rim both dissolve and the control disappears at exactly
/// the moment it is *on*. That is the same failure the *off* state used to have
/// over bright tiles, which is why its ring went to 1.5 pt; the light plate needs
/// the other half of the answer.
///
/// Black at low alpha rather than [`Palette::ink`], for the reason [`Palette::overlay_line`] is
/// white at low alpha: it composites, so it is a soft contact shadow on anything
/// pale and simply not there on anything dark — where the light rim is already
/// doing the work. Both edges are always drawn and neither has a state to get
/// wrong.
pub(super) fn paint_contact_edge(p: &egui::Painter, center: egui::Pos2, radius: f32) {
    p.circle_stroke(
        center,
        radius,
        Stroke::new(1.0, Color32::from_black_alpha(110)),
    );
}
