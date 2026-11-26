// ─── Fonts ────────────────────────────────────────────────────────────────────

/// The wordmark face (SIL OFL — see `assets/fonts/README.md`). Embedded rather
/// than loaded as an asset because the splash draws on frame one, before any
/// asset pipeline has run — and on the web that would be an HTTP round trip.
/// The library's semibold (headings), and the face the street names on the
/// map are rasterised in (`systems::street_labels`): a semibold reads at the
/// size a road marking gets, where a light face breaks up.
pub const BRAND_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-SemiBold.ttf"
));

/// The body face: Plus Jakarta Sans **Regular**, the brand face's own family
/// one weight down — one family at two weights is what makes a heading and
/// its rows read as one voice (the library's `FontMap::regular`).
pub const BODY_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/PlusJakartaSans-Regular.ttf"
));

/// The monospace face, Hack (MIT / Bitstream Vera — `assets/fonts/README.md`):
/// the library's code fields (`FontMap::mono`), the transit line badges and
/// the departure boards. It came with egui's default fonts until egui went.
pub const MONO_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/Hack-Regular.ttf"
));

/// The emoji face, Noto Emoji (OFL): the one every text face falls back to
/// (`FontMap::emoji`), so a chat's 😮 is drawn. egui's font stack ended in
/// it, and nothing drew one the day that went.
pub const EMOJI_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/fonts/NotoEmoji-Regular.ttf"
));

/// How much larger than its point size the icon face draws (the library's
/// `FontMap::icon_scale`) — the scale egui's font tweak gave the icons, kept
/// so they did not come out a size smaller in the library
/// (`docs/notes/icon-font.md` has the optical fit).
pub const ICON_SCALE: f32 = 1.15;

// ─── Type scale ───────────────────────────────────────────────────────────────

/// Captions and badges, in points: the word under a control, the initial in a
/// profile dot, a unit. The floor — nothing in the app should be smaller, and
/// three sizes below body (9.5, 10, 10.5) all meant this one.
pub const SIZE_TINY: f32 = 10.0;
/// Chips and switchers: `Auto`/`Day`/`Night`, a coordinate readout in an overlay
/// box, anything that is a *setting* or a value rather than a sentence.
pub const SIZE_CHIP: f32 = 11.0;
/// The secondary line under a row's title: an address under a place name, a hint
/// under a field, a status under a heading. **This is the one that drifted** —
/// it was 13.0 in Layers, 12.5 in Route, 12.0 in the sign-in card and 11.0 in the
/// HUD, four answers to one question.
pub const SIZE_SUB: f32 = 12.5;
/// Body text: what a plain label and a button already get.
pub const SIZE_BODY: f32 = 14.0;
/// The one number a box is *about*: the HUD's clock, a primary button's
/// label. One per box — a second is just body text.
pub const SIZE_VALUE: f32 = 15.0;
/// A panel header's title, in the brand face.
pub const SIZE_TITLE: f32 = 17.0;
/// A heading, in the brand face.
pub const SIZE_HEADING: f32 = 19.0;
/// A panel's own name in its title bar: the Chats screen's large title, so
/// every panel opens the same way.
pub const SIZE_PANEL_TITLE: f32 = 20.5;
/// Monospace ([`MONO_TTF`]) — the code-ish body size. Monospace runs
/// visually larger than proportional at the same point size, which is why this is
/// a point under [`SIZE_BODY`] rather than equal to it.
pub const SIZE_MONO: f32 = 13.0;

/// The splash wordmark. Not a scale entry: it exists exactly twice, here and in
/// `index.html`, which has to mirror it (see `systems::splash`'s module note).
pub const SPLASH_WORDMARK: f32 = 40.0;
/// The motto under the wordmark. Mirrored in `index.html` for the same reason.
pub const SPLASH_MOTTO: f32 = 16.0;

// ─── Corner radius ────────────────────────────────────────────────────────────

/// The corner radius: buttons, menus, chips, the panel header band. **Reach for
/// this one unless there is a reason not to** — a UI that rounds eight ways (the
/// number this file's single private constant was competing with) reads as eight
/// UIs.
///
/// Public because the alternative is what actually happened: a caller that needed
/// the app's radius read it back off `visuals.widgets.inactive.corner_radius`.
///
/// **The whole scale was opened up with the move to glass** (6 → 8, 3 → 4,
/// 10 → 14). A translucent surface is defined by its edge rather than by its
/// fill, and a tight corner on a sheet you can see through reads as a cut-out;
/// the softer radius is half of what makes the chrome look like a pane rather
/// than a rectangle. It costs nothing else — the radii are only ever read
/// through these four names.
pub const RADIUS: u8 = 8;
/// Small: the checkbox square, the close button, a label chip painted over the
/// map. For anything under ~24 pt, where [`RADIUS`] is half the height and the
/// box stops reading as a box (an unchecked checkbox becomes a radio button).
/// Held down at 4 while the others opened up, for that reason: on a ~14 pt
/// checkbox even [`RADIUS`] is already half the height.
pub const RADIUS_SM: u8 = 4;
/// Large: windows, cards, and the boxes that float over the map
/// (the library's overlay surfaces). A bigger surface needs a bigger radius to look equally
/// rounded — the same value at two sizes does not.
pub const RADIUS_LG: u8 = 14;

// ─── Touch targets ────────────────────────────────────────────────────────────

/// Material's minimum touch target, in dp (density-independent pixels, 1/160 in
/// — about 7.6 mm, roughly a fingertip). Every platform's guideline is within a
/// millimetre of this one.
///
/// The app was under it everywhere a finger drives: egui's `interact_size`
/// was 24 pt tall and 24 pt is ~36 dp on the phone tier, and the close button was
/// 22 pt (~33 dp). Raising the *UI scale* is not the fix — past ~1.7× the
/// viewport drops below the old egui side panel's 240 pt floor and a docked
/// panel is wider than the screen it docks in — so the interactive sizes are
/// raised on the touch tier instead, and this is the number they are raised to.
pub const MIN_TOUCH_TARGET_DP: f32 = 48.0;

/// `index.html`'s `MAX_DPR` on the phone tier — device pixels per CSS pixel the
/// render budget will draw (`docs/notes/wasm-performance.md`). It matters here
/// because it is *also* the magnification the stage transform applies to every
/// egui point on the web, i.e. the thing that decides how big a point physically
/// is. Duplicated rather than plumbed, because the page decides it before any
/// Rust exists; if it changes there, [`dp_per_point`] changes with it.
pub const WEB_PHONE_MAX_DPR: f32 = 1.5;

/// How many dp one egui point covers — the only way to size anything in a
/// physical unit from inside this process.
///
///   - **Native (and Android):** winit's scale factor *is* the display density,
///     so one window point is one dp and an egui point is `ui_scale` of those.
///     → `ui_scale`.
///   - **Web:** a CSS pixel is a dp on a phone, but the render budget sizes the
///     canvas to viewport ÷ k and scales it back up by k, so one window point
///     covers k CSS px. → `ui_scale · k`.
///
/// `k` is `max(1, dpr / MAX_DPR, √(pixel budget))` in `index.html` and only the
/// middle term is reconstructible from here, so this **under**-estimates k —
/// which over-estimates how many points 48 dp is, making targets slightly larger
/// than the guideline rather than smaller. That is the safe direction for a
/// floor, and the reason this is not plumbed through from JS for a number that is
/// only accurate to a rounding anyway.
///
/// Lives here rather than beside any one widget because the dock discs, the close
/// button, the driver rail and the on-screen keyboard all ask the same question,
/// and `onscreen_keyboard.rs` having had to compute its own is how a second
/// answer gets into the tree.
#[must_use]
pub fn dp_per_point(ui_scale: f32, display_scale: f32, web_phone: bool) -> f32 {
    let stage_k = if web_phone {
        (display_scale / WEB_PHONE_MAX_DPR).max(1.0)
    } else {
        1.0
    };
    (ui_scale * stage_k).max(0.1)
}

/// [`MIN_TOUCH_TARGET_DP`] converted to points, given a [`dp_per_point`].
#[must_use]
pub fn min_touch_target_pts(dp_per_point: f32) -> f32 {
    MIN_TOUCH_TARGET_DP / dp_per_point.max(0.1)
}

/// Does this run lay out for a finger?
///
/// [`crate::quality`] is deliberately the *one* detector in this client (see its
/// module note): the browser decides once, in `index.html`, because the canvas
/// resolution is a page-layout fact Rust cannot reach, and a second media query
/// here is how the UI ends up finger-sized on a machine rendering at desktop
/// resolution.
#[must_use]
pub fn touch_layout() -> bool {
    crate::quality::is_phone()
}

/// Guaranteed height of one interactive row, pointer tier.
/// 28 since 2026-11-08 (was 24): "the menu items are too cramped".
pub(super) const ROW_PTS: f32 = 28.0;
/// The same on the touch tier. The floor is [`MIN_TOUCH_TARGET_DP`] at the
/// tier's own [`dp_per_point`], which is ≥1.5 on both platforms that reach it
/// (native Android's `phone_ui_scale`, and the web's stage transform): 32 pt is
/// 48 dp at exactly 1.5. It is 40 since 2026-11-08, above the floor, for room
/// rather than reach.
pub const TOUCH_ROW_PTS: f32 = 40.0;

/// The height of one interactive row in this run, in points.
#[must_use]
pub fn row_height() -> f32 {
    if touch_layout() {
        TOUCH_ROW_PTS
    } else {
        ROW_PTS
    }
}
