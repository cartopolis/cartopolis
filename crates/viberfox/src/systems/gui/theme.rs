//! The Viberfox look: one palette, one `egui::Style`, one place to change them.
//!
//! The colours are read off the logo (`assets/icon/viberfox.png`) rather than
//! invented — a teal gradient from `#0C2A33` up to `#164652`, the fox at
//! `#FD6012` with an amber `#FD9212` in its fur, and the muzzle's off-white
//! `#F2EFEA`. So the app's chrome and its icon are the same two colours, and the
//! boot splash hands over to a UI painted in the tones it was already showing.
//!
//! Two things this replaces, both of which the UI used to get by default:
//!
//!   - **egui's stock dark theme**, which is a neutral grey (`gray(27)` panels)
//!     with a blue selection. Nothing about it said Viberfox, and next to the
//!     map — which is bright — flat grey read as a hole rather than a surface.
//!   - **egui's stock spacing**, which is tuned for a debug inspector: 3 pt
//!     between rows and 1 pt of vertical button padding. In a panel of place
//!     rows and layer toggles that is a wall of text; the values here roughly
//!     double the vertical rhythm and give buttons a real hit area.
//!
//! Contrast was checked against the surface each colour actually lands on:
//! [`TEXT`] is 8.6:1 on [`SURFACE`], [`TEXT_DIM`] 5.2:1, [`FOX_TEXT`] 5.2:1.
//! Plain [`FOX`] is only 3.7:1, which is fine for a fill, a rule or a focus ring
//! but not for small text — that is why there is a separate, lighter [`FOX_TEXT`]
//! and why anything sitting *on* a fox-orange fill is drawn in [`INK`] (6.0:1)
//! rather than white (3.1:1).
//!
//! [`install`] is called once, from `EguiManager::default`, before the first
//! pass — early enough that the brand font family is bound on frame one, which
//! is what lets the splash paint its wordmark immediately.

use egui::{
    style::{Selection, WidgetVisuals, Widgets},
    Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
};

// ─── Palette ──────────────────────────────────────────────────────────────────

/// Deeper than any of the logo's tones: text-edit wells, code blocks, and the
/// boot backdrop. Also the text colour on a fox-orange fill.
pub const INK: Color32 = Color32::from_rgb(0x07, 0x1A, 0x20);
/// The logo gradient's bottom. Panel and window fill — the app's base surface.
pub const SURFACE: Color32 = Color32::from_rgb(0x0C, 0x2A, 0x33);
/// The logo gradient's middle. One step up from [`SURFACE`]: buttons at rest,
/// striped rows, anything that should read as sitting *on* a panel.
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x12, 0x3B, 0x46);
/// The logo gradient's top. Hover, and open menus over a panel.
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x18, 0x4C, 0x5A);
/// Hairlines: separators, widget frames, indent guides.
pub const LINE: Color32 = Color32::from_rgb(0x1F, 0x5A, 0x69);

/// The fox. Focus rings, hover outlines, sliders, the pressed state — the one
/// saturated colour in the UI, so it always means "this is the live thing".
/// Not for small text (3.7:1 on [`SURFACE`]); use [`FOX_TEXT`].
pub const FOX: Color32 = Color32::from_rgb(0xFD, 0x60, 0x12);
/// The fox, lifted — the outline around an active widget's orange fill.
pub const FOX_BRIGHT: Color32 = Color32::from_rgb(0xFF, 0x84, 0x3C);
/// The fox as *text*, lightened until it clears 4.5:1 on [`SURFACE`]. Links,
/// speaker names, emphasised values.
pub const FOX_TEXT: Color32 = Color32::from_rgb(0xFF, 0x94, 0x57);
/// The fox mixed roughly halfway into the panel: a selected row that still
/// reads as orange while leaving [`CREAM`] on it at 5.7:1.
pub const FOX_MUTED: Color32 = Color32::from_rgb(0x96, 0x44, 0x1C);
/// The amber in the logo's fur. The second accent — reserved for *held* states
/// (north-up lock, follow-and-rotate), so they never collide with the fox.
pub const AMBER: Color32 = Color32::from_rgb(0xFD, 0x92, 0x12);

/// The muzzle. Headings and the one or two things that must read brightest.
pub const CREAM: Color32 = Color32::from_rgb(0xF2, 0xEF, 0xEA);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xDC, 0xE6, 0xE8);
/// Secondary text: units, coordinates, hints. Also `index.html`'s muted tone,
/// so the pre-wasm splash and this one agree.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x9F, 0xB3, 0xBF);
/// Warnings — a desaturated gold that reads as caution without competing with
/// [`AMBER`] or the fox.
pub const WARN: Color32 = Color32::from_rgb(0xD8, 0xC0, 0x7A);
/// Errors. Warm rather than pure red so it belongs to the same palette.
pub const ERROR: Color32 = Color32::from_rgb(0xE8, 0x8B, 0x7D);

/// [`SURFACE`] at ~84% — the fill of the boxes that float *over* the map
/// (status HUD, stats, bottom bar). Translucent so the world still moves behind
/// them; opaque enough that the text on top never has to fight a bright tile.
pub const OVERLAY: Color32 = Color32::from_rgba_unmultiplied_const(0x0C, 0x2A, 0x33, 214);
/// The hairline around an [`OVERLAY`] box: enough to separate it from the map,
/// not enough to draw a frame around the view.
pub const OVERLAY_LINE: Color32 = Color32::from_rgba_unmultiplied_const(0x1F, 0x5A, 0x69, 190);
/// Backing for the small text chips painted straight onto the map — place-pin
/// labels, the drag ghost, a feature's hover card. Darker and more opaque than
/// [`OVERLAY`] on purpose: these are a few words over an arbitrary tile, so
/// legibility beats letting the map through.
pub const LABEL_CHIP: Color32 = Color32::from_rgba_unmultiplied_const(0x07, 0x1A, 0x20, 235);
/// Halo behind map text that has no chip to sit on — the street names
/// (`systems::street_labels`), which are painted *along* their street and would
/// black it out under a chip. Near-white and only mostly opaque: it has to lift
/// [`INK`] off tarmac, grass and water without reading as a second word.
pub const LABEL_HALO: Color32 = Color32::from_rgba_unmultiplied_const(0xF6, 0xF4, 0xF0, 205);

// ─── Fonts ────────────────────────────────────────────────────────────────────

/// The wordmark face (SIL OFL — see `assets/fonts/README.md`). Embedded rather
/// than loaded as an asset because the splash draws on frame one, before any
/// asset pipeline has run — and on the web that would be an HTTP round trip.
/// egui reads TTF/OTF, not woff2.
/// Also the face the street names on the map are rasterised in
/// (`systems::street_labels`): a semibold reads at the size a road marking gets,
/// where egui's light body face breaks up.
pub const BRAND_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-SemiBold.ttf"
));
const BRAND_FAMILY: &str = "viberfox-brand";

/// The brand face: Plus Jakarta Sans SemiBold, backed by egui's own stack so a
/// glyph it lacks (emoji, arrows, the box-drawing the panels use) still resolves
/// instead of rendering as tofu. Used for headings and the splash wordmark —
/// *not* for body text, which this weight would make heavy and hard to read.
pub fn brand_family() -> FontFamily {
    FontFamily::Name(BRAND_FAMILY.into())
}

fn fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        BRAND_FAMILY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(BRAND_TTF)),
    );
    // Prepend to a *copy* of the proportional stack rather than pushing onto it:
    // the brand face wins where it has the glyph, egui's fallbacks cover the rest,
    // and `Proportional` itself is left alone so body text keeps its own face.
    let mut stack = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    stack.insert(0, BRAND_FAMILY.to_owned());
    fonts.families.insert(brand_family(), stack);
    fonts
}

// ─── Style ────────────────────────────────────────────────────────────────────

/// Corner radius shared by buttons, menus and windows. One value, because a UI
/// that rounds three ways reads as three UIs.
const RADIUS: u8 = 6;

fn widgets() -> Widgets {
    let visuals = |bg: Color32, line: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, line),
        fg_stroke: Stroke::new(1.0, fg),
        corner_radius: CornerRadius::same(RADIUS),
        expansion: 0.0,
    };
    Widgets {
        // Separators, indent guides and plain labels all read off this one.
        noninteractive: visuals(SURFACE, LINE, TEXT),
        inactive: visuals(SURFACE_RAISED, LINE, TEXT),
        // Hover is where the fox first appears: the surface lifts a step and
        // gains an orange outline, so "clickable" is a colour, not a guess.
        hovered: WidgetVisuals {
            expansion: 1.0,
            ..visuals(SURFACE_HOVER, FOX, CREAM)
        },
        // Pressed / focused. Two different oranges on purpose:
        //   - `bg_fill` is the *graphical* fill (slider grab, checkbox box), so it
        //     gets the full fox;
        //   - `weak_bg_fill` is the button fill, which carries a label, so it gets
        //     the muted one — [`CREAM`] is 5.7:1 on that and only 3.1:1 on [`FOX`].
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit an orange fill turned
        // every bold value in the UI near-invisible.
        active: WidgetVisuals {
            weak_bg_fill: FOX_MUTED,
            expansion: 1.0,
            ..visuals(FOX, FOX_BRIGHT, CREAM)
        },
        // An open menu button keeps the hover surface but drops the orange ring —
        // the popup below it is the thing to look at now.
        open: visuals(SURFACE_HOVER, LINE, CREAM),
    }
}

/// The full brand [`egui::Visuals`], built by overriding [`egui::Visuals::dark`]
/// rather than from scratch so the fields this doesn't have an opinion about
/// (notably the dark-mode font colour transfer function) stay at egui's tuned
/// defaults.
pub fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.widgets = widgets();
    v.selection = Selection {
        bg_fill: FOX_MUTED,
        stroke: Stroke::new(1.0, CREAM),
    };
    v.hyperlink_color = FOX_TEXT;
    v.faint_bg_color = SURFACE_RAISED;
    v.extreme_bg_color = INK;
    v.code_bg_color = INK;
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;
    // Pin the weak/disabled tone instead of letting egui derive it by alpha —
    // this is the same grey-blue the two splashes use, and it is measured.
    v.weak_text_color = Some(TEXT_DIM);

    v.panel_fill = SURFACE;
    v.window_fill = SURFACE;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.window_corner_radius = CornerRadius::same(RADIUS + 4);
    v.menu_corner_radius = CornerRadius::same(RADIUS + 2);
    // Softer and further than egui's default: these float over a moving map, and
    // a tight hard shadow reads as a sticker rather than a raised surface.
    v.window_shadow = Shadow {
        offset: [0, 12],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.popup_shadow = Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };

    v.text_cursor.stroke = Stroke::new(2.0, FOX_BRIGHT);
    // The filled part of a slider takes the accent, so a value reads at a glance.
    v.slider_trailing_fill = true;
    v
}

/// The full brand [`egui::Style`] — [`visuals`] plus the spacing and type scale.
pub fn style() -> egui::Style {
    let mut style = egui::Style {
        visuals: visuals(),
        ..Default::default()
    };

    let s = &mut style.spacing;
    // The "cramped" fix is mostly here. egui's defaults are 3 pt between rows and
    // vec2(4, 1) of button padding; those are inspector values, and a panel of
    // place rows built on them has no rhythm at all.
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(9.0, 4.0);
    // 24 pt of guaranteed height per clickable row (egui ships 18).
    s.interact_size = egui::vec2(40.0, 24.0);
    s.window_margin = Margin::same(10);
    s.menu_margin = Margin::same(8);
    s.menu_spacing = 4.0;
    s.indent = 20.0;
    s.icon_spacing = 6.0;
    s.slider_width = 120.0;
    s.combo_width = 120.0;
    s.scroll.bar_width = 10.0;

    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        // The one place the brand face appears in the app chrome.
        (TextStyle::Heading, FontId::new(19.0, brand_family())),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    style
}

/// Install the fonts and the style on a context.
///
/// Called from `EguiManager::default` *before* the first pass, which matters:
/// `set_fonts` only takes effect from the next `begin_pass`, and painting with
/// an unbound `FontFamily::Name` panics in epaint. Installing at construction
/// means [`brand_family`] is safe to use from frame one.
///
/// The style is set for **both** themes. This integration never feeds egui a
/// `system_theme`, so it would fall back to dark on its own — but a light-theme
/// context left on egui's stock style is a latent white flash, and the app has
/// exactly one look either way.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let style = std::sync::Arc::new(style());
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

// ─── Logo ─────────────────────────────────────────────────────────────────────

/// Side of the logo texture, in texels. Drawn at [`LOGO_SIZE`] points, so this is
/// ~2× for a hidpi screen; egui has no mip chain, and the 256 px source scaled to
/// 26 pt in the sampler shimmers as the window moves under it.
const LOGO_TEXELS: u32 = 64;

/// The address bar's logo button, in points.
pub const LOGO_SIZE: f32 = 26.0;

/// The app mark as an egui image, decoded from the same embedded PNG the OS
/// window icon and the XDG desktop entry use — one asset, three surfaces.
///
/// Returns `None` only if that PNG fails to decode, which in practice means a
/// clone without Git LFS (the pointer file is 130 bytes of text). The bar then
/// falls back to a text mark rather than panicking, because a broken checkout
/// should still boot far enough to say so.
pub fn logo_image() -> Option<egui::ColorImage> {
    let png = crate::systems::window_icon::ICON_PNG;
    let img = match image::load_from_memory(png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(error = %e, "logo: embedded icon PNG did not decode (Git LFS?)");
            return None;
        }
    };
    let small = image::imageops::resize(
        &img.into_rgba8(),
        LOGO_TEXELS,
        LOGO_TEXELS,
        image::imageops::FilterType::Lanczos3,
    );
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [LOGO_TEXELS as usize; 2],
        small.as_raw(),
    ))
}

// ─── Glyphs ───────────────────────────────────────────────────────────────────

/// The first of `candidates` the loaded font can actually draw.
///
/// egui bundles Ubuntu-Light plus a small emoji subset, and a character outside
/// both renders as an empty box. That subset is *much* thinner than it looks:
/// probing the loaded font shows it has none of `⇅ ⬤ ⬆ ⬅ ➡ ← → ↑ ↰ ↱ ✕ 📍 🖈`, and
/// of the plausible symbols only `⌖ ◎ ○ • ⚑ ↻ ⟲ ⏵ ⏴ ⏶ ⏷ ☰`. So no icon in the app
/// is hard-coded to a guess: each is picked from a preference list whose last
/// entry is Latin-1 and therefore cannot fail.
///
/// Lives here rather than beside any one panel because the answer depends on
/// [`fonts`] — change the font stack and every icon list has to be re-probed
/// against it, from one place.
pub fn first_drawable(ctx: &egui::Context, candidates: &[&'static str]) -> &'static str {
    let font = egui::TextStyle::Body.resolve(&ctx.style_of(ctx.theme()));
    ctx.fonts_mut(|f| {
        candidates
            .iter()
            .copied()
            .find(|c| f.has_glyphs(&font, c))
            .unwrap_or_else(|| candidates.last().copied().unwrap_or("?"))
    })
}

// ─── Shared frames ────────────────────────────────────────────────────────────

/// The frame every box that floats *over the map* shares — the status HUD, the
/// stats readout, the bottom action bar.
///
/// One function because these were three hand-rolled copies of the same
/// `Frame::NONE.fill(...)` that had already drifted apart (two alpha values, two
/// corner radii, three margins), and a "the overlays look inconsistent" bug is
/// invisible until you put two of them on screen at once.
pub fn overlay_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(OVERLAY)
        .stroke(Stroke::new(1.0, OVERLAY_LINE))
        .corner_radius(CornerRadius::same(RADIUS + 2))
        .inner_margin(Margin::symmetric(11, 8))
}

/// Diameter of a round icon button, in points. Matches the compass disc in the
/// opposite corner — the map's floating controls are one family.
pub const ROUND_BUTTON_SIZE: f32 = 40.0;

/// A round icon button that floats over the map: the dock along the bottom, and
/// anything else that wants the compass's visual language.
///
/// Painted rather than assembled from `Button`, because egui's button is a
/// rounded *rect* whose corner radius is capped well below half its height — a
/// "circular" button built that way is a squircle that reads as a mistake next to
/// the genuinely round compass. `selected` is the open-panel state and takes the
/// fox fill, the same "this is on" colour the rest of the UI uses.
pub fn round_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let size = egui::Vec2::splat(ROUND_BUTTON_SIZE);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let hovered = resp.hovered();
    let (fill, ring, ink) = match (selected, hovered) {
        (true, _) => (FOX_MUTED, FOX_BRIGHT, CREAM),
        (false, true) => (SURFACE_HOVER, FOX, CREAM),
        (false, false) => (OVERLAY, OVERLAY_LINE, TEXT),
    };
    paint_disc(
        ui.painter(),
        rect.center(),
        ROUND_BUTTON_SIZE * 0.5 - 1.0,
        Disc {
            fill,
            ring,
            ink,
            icon,
            icon_size: 17.0,
        },
    );
    resp.on_hover_text(tooltip)
}

/// The look of one icon disc, shared by [`round_button`] and the medallion a
/// panel header wears — they have to be the same object at two sizes, or the
/// header stops reading as "the dock button you just pressed".
struct Disc<'a> {
    fill: Color32,
    ring: Color32,
    ink: Color32,
    icon: &'a str,
    icon_size: f32,
}

fn paint_disc(p: &egui::Painter, center: egui::Pos2, radius: f32, d: Disc<'_>) {
    p.circle_filled(center, radius, d.fill);
    p.circle_stroke(center, radius, Stroke::new(1.0, d.ring));
    p.text(
        center,
        egui::Align2::CENTER_CENTER,
        d.icon,
        FontId::proportional(d.icon_size),
        d.ink,
    );
}

// ─── Panel header ─────────────────────────────────────────────────────────────

/// Height of the band [`panel_header`] paints, in points.
const HEADER_HEIGHT: f32 = 38.0;
/// Diameter of the header's icon medallion. Small enough to sit on one text
/// line, large enough that the dock's glyph survives the shrink.
const HEADER_MEDALLION: f32 = 24.0;
/// Side of the header's close button.
const CLOSE_BUTTON_SIZE: f32 = 22.0;

/// The ✕ every panel closes with — **painted, not typed.**
///
/// `✕` is not in the bundled font (see [`first_drawable`]), so every panel that
/// spelled its close button `ui.button("✕")` drew an empty box; the multiplication
/// sign that survives the probe is a comma-sized cross that reads as a typo at
/// button size. Two line segments have neither problem: they land on the pixel
/// grid at any scale and cannot depend on a font subset at all.
pub fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::Vec2::splat(CLOSE_BUTTON_SIZE), egui::Sense::click());
    // Frameless at rest: a header carries one obvious action, and boxing it makes
    // the title bar look like it has a second panel in it.
    let (fill, ring, ink) = if resp.is_pointer_button_down_on() {
        (FOX_MUTED, FOX_BRIGHT, CREAM)
    } else if resp.hovered() {
        (SURFACE_HOVER, FOX, CREAM)
    } else {
        (Color32::TRANSPARENT, Color32::TRANSPARENT, TEXT_DIM)
    };
    let p = ui.painter();
    p.rect(
        rect,
        CornerRadius::same(RADIUS - 2),
        fill,
        Stroke::new(1.0, ring),
        egui::StrokeKind::Inside,
    );
    let x = rect.shrink(7.0);
    let stroke = Stroke::new(1.6, ink);
    p.line_segment([x.left_top(), x.right_bottom()], stroke);
    p.line_segment([x.right_top(), x.left_bottom()], stroke);
    resp.on_hover_text("Close")
}

/// The header every right-hand panel opens with: a raised band carrying the
/// panel's own icon, its name, and the close button. Returns whether close was
/// clicked.
///
/// Three things it fixes, all of which the six hand-rolled copies shared. The
/// close glyph was `✕`, which the font cannot draw ([`close_button`]). The
/// heading sat directly on the panel fill, so a panel opened as a wall of one
/// colour with a slightly larger word at the top of it. And nothing tied the
/// header to the control that opened it — the medallion is the dock's disc in
/// its *selected* colours at a smaller radius, so the lit button at the bottom
/// of the screen and the badge at the top of the panel are visibly one thing.
///
/// Depth is a three-part recipe and all three parts matter: a soft shadow under
/// the band (it floats over the content, so the content must be able to pass
/// beneath it), a fill one step up from the panel, and a hairline highlight
/// along the top edge — the lit edge is what turns a lighter rectangle into a
/// raised one.
pub fn panel_header(ui: &mut egui::Ui, icons: &[&'static str], title: &str) -> bool {
    panel_header_with(ui, icons, title, |_| {})
}

/// [`panel_header`] plus room for extra actions, laid out right-to-left from the
/// close button (the Places panel's "+ Folder").
pub fn panel_header_with(
    ui: &mut egui::Ui,
    icons: &[&'static str],
    title: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> bool {
    let icon = first_drawable(ui.ctx(), icons);
    let avail = ui.available_rect_before_wrap();
    let band = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), HEADER_HEIGHT));
    let radius = CornerRadius::same(RADIUS + 1);

    let p = ui.painter();
    p.add(
        Shadow {
            offset: [0, 3],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(90),
        }
        .as_shape(band, radius),
    );
    p.rect(
        band,
        radius,
        SURFACE_RAISED,
        Stroke::new(1.0, LINE),
        egui::StrokeKind::Inside,
    );
    p.line_segment(
        [
            egui::pos2(band.left() + 7.0, band.top() + 1.5),
            egui::pos2(band.right() - 7.0, band.top() + 1.5),
        ],
        Stroke::new(1.0, SURFACE_HOVER),
    );

    let mut close = false;
    {
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(band.shrink2(egui::vec2(7.0, 7.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = 8.0;
        let (medallion, _) =
            row.allocate_exact_size(egui::Vec2::splat(HEADER_MEDALLION), egui::Sense::hover());
        paint_disc(
            row.painter(),
            medallion.center(),
            HEADER_MEDALLION * 0.5 - 0.5,
            Disc {
                fill: FOX_MUTED,
                ring: FOX_BRIGHT,
                ink: CREAM,
                icon,
                icon_size: 13.0,
            },
        );
        row.add(
            egui::Label::new(
                egui::RichText::new(title)
                    .font(FontId::new(17.0, brand_family()))
                    .color(CREAM),
            )
            .truncate()
            .selectable(false),
        );
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = close_button(ui).clicked();
            extra(ui);
        });
    }
    ui.advance_cursor_after_rect(band);
    // Clears the shadow, which reaches below the band.
    ui.add_space(4.0);
    close
}

/// A checkbox whose box is square. egui derives the checkbox frame's rounding
/// from the shared widget visuals ([`RADIUS`], sized for buttons), and on a
/// ~14 pt box that radius reads as a circle — an unchecked toggle then looks
/// like a radio button, which promises "pick one" instead of "on/off". Scopes
/// the rounding down for just this widget; every other visual stays the
/// theme's. Use this, not `ui.checkbox`, everywhere.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.scope(|ui| {
        let square = CornerRadius::same(2);
        let w = &mut ui.style_mut().visuals.widgets;
        w.noninteractive.corner_radius = square;
        w.inactive.corner_radius = square;
        w.hovered.corner_radius = square;
        w.active.corner_radius = square;
        ui.checkbox(checked, text)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance per WCAG 2.x, on a `Color32`'s sRGB bytes.
    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The palette's whole claim is that it is *legible*, and a colour is very
    /// easy to nudge into looking nicer and reading worse. Pin the pairs that
    /// actually occur: text on a panel, text on a selected row, text on the
    /// pressed-button orange.
    #[test]
    fn text_colours_clear_their_backgrounds() {
        for (name, fg, bg, min) in [
            ("body on panel", TEXT, SURFACE, 4.5),
            ("body on raised", TEXT, SURFACE_RAISED, 4.5),
            ("dim on panel", TEXT_DIM, SURFACE, 4.5),
            ("heading on panel", CREAM, SURFACE, 4.5),
            ("link on panel", FOX_TEXT, SURFACE, 4.5),
            ("selected row", CREAM, FOX_MUTED, 4.5),
            ("label on pressed button", CREAM, FOX_MUTED, 4.5),
            ("profile initial on the accent", INK, FOX, 4.5),
            ("warning on panel", WARN, SURFACE, 4.5),
            ("error on panel", ERROR, SURFACE, 4.5),
            // Not text: a fill, a rule or a focus ring, which WCAG puts at 3:1.
            ("focus ring on panel", FOX, SURFACE, 3.0),
        ] {
            let got = contrast(fg, bg);
            assert!(got >= min, "{name}: contrast {got:.2} < {min}");
        }
    }

    /// egui derives three app-wide text colours from the widget table rather
    /// than exposing them directly — plain text from `noninteractive`, weak text
    /// from `weak_text_color`, and **`RichText::strong()` from `widgets.active`**.
    /// That last one is the trap: `active` is also the pressed-button state, so
    /// picking its `fg_stroke` to suit an orange fill silently recolours every
    /// bold value in the app. Assert the three land where the palette says.
    #[test]
    fn derived_text_colours_come_from_the_palette() {
        let v = visuals();
        assert_eq!(v.text_color(), TEXT);
        assert_eq!(v.weak_text_color(), TEXT_DIM);
        assert_eq!(v.strong_text_color(), CREAM);
    }

    /// The brand family must be bound before anything paints a heading —
    /// `install` exists to guarantee that, and an unbound family is a panic deep
    /// in epaint rather than a missing-font fallback.
    #[test]
    fn install_binds_the_brand_family_for_the_first_frame() {
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                ui.heading("Places");
            },
        );
    }

    /// No panel may wear a glyph the font cannot draw. [`first_drawable`] only
    /// promises to fall back to the *last* candidate, so a list whose tail is
    /// itself missing (`🧭`, `🤖`, `🧍` all were) still renders an empty box — and
    /// now that the header repeats the dock's icon, one bad list draws tofu twice.
    #[test]
    fn every_panel_icon_resolves_to_a_glyph_the_font_has() {
        use crate::resources::RightPanel;
        let ctx = egui::Context::default();
        install(&ctx);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| {
                for panel in [
                    RightPanel::Layers,
                    RightPanel::Places,
                    RightPanel::Route,
                    RightPanel::Assistant,
                    RightPanel::Chat,
                    RightPanel::Appearance,
                ] {
                    let icon = first_drawable(ui.ctx(), panel.icons());
                    let font = TextStyle::Body.resolve(ui.style());
                    assert!(
                        ui.ctx().fonts_mut(|f| f.has_glyphs(&font, icon)),
                        "{}: resolved to {icon:?}, which the font cannot draw",
                        panel.label(),
                    );
                }
            },
        );
    }
}
