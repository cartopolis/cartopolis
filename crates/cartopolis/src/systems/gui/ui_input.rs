//! What the interface knows about the screen and the input, as two plain
//! resources nobody needs egui to read.
//!
//! - [`UiMetrics`]: the UI scale, the viewport in UI points, whether it is
//!   compact, and the window→UI conversion. Computed from the window (or the
//!   [`HeadlessSurface`] of a `--shot` run) and the scale settings, with the
//!   same arithmetic `egui_manager` lays egui out with — `EguiManager` holds
//!   one of these, so the two cannot disagree.
//! - [`UiInput`]: who owns this frame's input — a world click
//!   ([`UiInput::owns_click`]), a drag on the map ([`UiInput::map_dragged`]),
//!   the game keys ([`UiInput::ui_owns_keyboard`]). Two writers OR their
//!   verdicts together: egui's ([`EguiVerdict`], written by
//!   `egui_manager::publish_ui_input` right after egui's pass begins) and the
//!   component library's (`lib_host::bridge_input`, from `mithril_bevy`'s
//!   `UiPointer` and the focused `EditableText`).
//!
//! Both are ordinary `Send` resources, so a system reads them as `Res<…>` on
//! every target — unlike `EguiManager`, which is non-send on the web
//! (`EguiRef`). A system that only asks these questions takes these, not egui
//! (stage S2 of taking egui out: `docs/notes/egui-shared-root-ui.md` has why
//! each question is asked the way it is, `touch-input.md` the brisk tap).

use bevy::prelude::*;

use crate::resources::HeadlessSurface;

/// Viewport widths (in points) below this are treated as "compact" (phone-ish):
/// the bottom bar wraps and the right-side panels expand to fill the screen
/// instead of docking beside the map at a fixed 320 pt.
pub const COMPACT_WIDTH_PTS: f32 = 640.0;

/// Default UI magnification on the phone tier — **and why the web's is 1.0.**
///
/// The question is physical size, not taste. The UI lays out in points, the
/// display scale factor turns those into pixels, and a phone's UI is therefore
/// already *sharp*; it is simply built for a mouse. `theme::style`'s
/// `interact_size` of 40×24 pt is the target to size, against a ~7.6 mm platform
/// guideline (Material's 48 dp) and a fingertip about that wide.
///
/// **On the web that has already been fixed, by something else.** The render
/// budget's stage transform (`index.html`, `docs/notes/wasm-performance.md`)
/// sizes the canvas to viewport ÷ k and scales it back up by k, where k is
/// `dpr / 1.5` on the phone tier. winit measures the *untransformed* box, so
/// Bevy's logical viewport shrinks by k and every UI point is then painted
/// across k CSS pixels. On the reported Fairphone 4 (dpr 2.75) k ≈ 1.83, which
/// takes a 24 pt row from 6.3 mm to **11.6 mm** on its own. Magnifying again on
/// top of that overshoots the guideline by half and — worse — takes the viewport
/// to 143 pt, well under the 240 pt floor `responsive_right_panel` gives a
/// compact panel, i.e. a panel wider than the screen it docks in.
///
/// Native Android has no such transform: nothing there has magnified anything,
/// and a point is a dp (1/160 in), so the same row is **3.8 mm**. That is the
/// platform this constant is still for.
///
/// So the default is per-platform and the *override* is the general answer —
/// see [`resolve_ui_scale`]. Both numbers are arguments from arithmetic, not
/// measurements; neither has been checked on glass.
pub(crate) const fn phone_ui_scale() -> f32 {
    if cfg!(target_arch = "wasm32") {
        1.0
    } else {
        1.5
    }
}

/// Clamp for an explicitly requested scale. A typo in a URL should make the UI
/// awkward, not unrecoverable — at 20× there is no longer a button to press to
/// get back.
pub(crate) const UI_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.5..=3.0;

/// How much to magnify the UI over the display's own scale factor.
///
/// Resolved at startup, from two sources in order:
///
/// 1. An explicit `?ui_scale=1.3` (web) / `CARTO_UI_SCALE=1.3` (desktop).
///    **This is the primary interface, not an escape hatch.** The physical size
///    of a point varies across devices with the display scale, the platform's
///    unit and — on the web — the render budget's stage transform, and this
///    function cannot see any of the three well enough to be right by default.
/// 2. Otherwise [`phone_ui_scale`] on the phone tier, and 1.0 off it.
///
/// **The tier comes from [`crate::quality`], which is deliberately the only
/// thing in the client that decides what kind of machine this is.** An earlier
/// version of this asked the browser `(pointer: coarse)` directly, which was a
/// second detector answering a nearly-identical question — and `quality`'s
/// module note is explicit that the browser decides once, in `index.html`,
/// because the canvas resolution is a page-layout fact Rust cannot reach. Two
/// detectors is how the UI ends up finger-sized on a machine rendering at
/// desktop resolution, or the reverse.
///
/// The cost of borrowing that answer is that `quality` asks for a coarse pointer
/// **and** a viewport under 900 px, so a large tablet is `Desktop` and gets
/// mouse-sized targets despite being driven by a finger. That is the wrong
/// answer for such a device and the right one for a touchscreen laptop, and it
/// is what `CARTO_UI_SCALE` is for until someone has a tablet to test on.
pub(crate) fn resolve_ui_scale() -> f32 {
    if let Some(raw) = crate::env_value("CARTO_UI_SCALE") {
        match raw.trim().parse::<f32>() {
            Ok(v) if v.is_finite() => {
                let clamped = v.clamp(*UI_SCALE_RANGE.start(), *UI_SCALE_RANGE.end());
                if clamped != v {
                    tracing::warn!("ui scale {v} out of range, using {clamped}");
                }
                return clamped;
            }
            _ => tracing::warn!("ignoring unparseable ui scale {raw:?}"),
        }
    }
    if crate::quality::is_phone() {
        phone_ui_scale()
    } else {
        1.0
    }
}

// ─── UiMetrics ───────────────────────────────────────────────────────────────

/// The surface the interface lays out against, and the scale it lays out at.
///
/// Refreshed each frame in `PreUpdate` by [`update_ui_metrics`] from the
/// primary window, or the offscreen target of a headless `--shot` run
/// ([`HeadlessSurface`]). A frame with neither keeps the last surface seen —
/// the window is despawned in `Last` at shutdown while `Update` runs on.
///
/// The stored scale is the *effective* points-per-pixel — the display's own
/// scale factor multiplied by [`Self::ui_scale`] — so everything derived from
/// it ([`Self::viewport_pts`], [`Self::compact`]) is already in UI points.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct UiMetrics {
    /// Physical pixels of the surface.
    physical: UVec2,
    /// Physical pixels per UI point: display scale × [`Self::ui_scale`].
    scale: f32,
    /// UI magnification on top of the display's scale factor. `1.0` everywhere
    /// a mouse is the pointer; larger on touch ([`resolve_ui_scale`]). The only
    /// thing that makes UI points differ from the window's logical points.
    ui_scale: f32,
}

impl Default for UiMetrics {
    fn default() -> Self {
        let ui_scale = resolve_ui_scale();
        static SAID: std::sync::Once = std::sync::Once::new();
        if ui_scale != 1.0 {
            SAID.call_once(|| {
                tracing::info!("UI scale {ui_scale}× (coarse pointer or CARTO_UI_SCALE)");
            });
        }
        Self::with_ui_scale(ui_scale)
    }
}

impl UiMetrics {
    /// Metrics at a given magnification, before any surface is seen: the
    /// primary window's default resolution in `main.rs`, at display scale 1.
    #[must_use]
    pub fn with_ui_scale(ui_scale: f32) -> Self {
        Self {
            physical: UVec2::new(1280, 720),
            scale: ui_scale,
            ui_scale,
        }
    }

    /// Record the surface this frame is laid out against, given the
    /// *display's* scale factor; [`Self::ui_scale`] is folded in here so there
    /// is one place the two are combined.
    ///
    /// The scale is sanitised on the way in: it divides every size below, and
    /// a display handing back 0 (or NaN) — which is what a monitor being
    /// switched off under us looks like — would otherwise turn the whole
    /// viewport into infinities.
    pub fn set_surface(&mut self, physical: UVec2, display_scale: f32) {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        self.physical = physical;
        self.scale = display_scale * self.ui_scale;
    }

    /// Take the surface from the primary window, else the headless target;
    /// with neither, keep the last one.
    pub fn observe(&mut self, windows: &Query<&Window>, headless: Option<&HeadlessSurface>) {
        if let Some((physical, scale)) = surface_of(windows, headless) {
            self.set_surface(physical, scale);
        }
    }

    /// Physical pixels of the surface.
    #[must_use]
    pub fn physical(&self) -> UVec2 {
        self.physical
    }

    /// Physical pixels per UI point (display scale × UI scale).
    #[must_use]
    pub fn pixels_per_point(&self) -> f32 {
        self.scale
    }

    /// Viewport size in UI points (physical pixels ÷ effective scale).
    #[must_use]
    pub fn viewport_pts(&self) -> Vec2 {
        Vec2::new(
            self.physical.x as f32 / self.scale,
            self.physical.y as f32 / self.scale,
        )
    }

    /// The UI magnification in force ([`resolve_ui_scale`]). Also the factor
    /// between the window's logical points and the UI's, and what Bevy's
    /// `UiScale` is set to for the component library.
    #[must_use]
    pub fn ui_scale(&self) -> f32 {
        self.ui_scale
    }

    /// The *display's* own scale factor, with [`Self::ui_scale`] divided back
    /// out — physical pixels per window logical point, which is what winit
    /// reports and what `devicePixelRatio` is on the web. For reasoning about
    /// *physical* size (a touch target in millimetres).
    #[must_use]
    pub fn display_scale(&self) -> f32 {
        self.scale / self.ui_scale.max(f32::EPSILON)
    }

    /// True on a narrow (phone-ish) viewport — see [`COMPACT_WIDTH_PTS`].
    #[must_use]
    pub fn compact(&self) -> bool {
        self.viewport_pts().x < COMPACT_WIDTH_PTS
    }

    /// Convert a position in the window's logical points — what
    /// `Window::cursor_position`, `TouchInput::position` and
    /// `Camera::world_to_viewport` speak — into UI points. The two coincide
    /// only while [`Self::ui_scale`] is 1.0, so an overlay that skips this is
    /// right on every desktop and slides off its subject on a phone.
    #[must_use]
    pub fn window_to_ui(&self, p: Vec2) -> Vec2 {
        Vec2::new(p.x / self.ui_scale, p.y / self.ui_scale)
    }
}

/// The surface to lay out against: the primary window, or the offscreen target
/// in a headless `--shot` run. Physical pixels plus the logical→physical scale.
/// `None` once the window has been despawned and no headless target stands in.
pub(crate) fn surface_of(
    windows: &Query<&Window>,
    headless: Option<&HeadlessSurface>,
) -> Option<(UVec2, f32)> {
    if let Some(w) = windows.iter().next() {
        return Some((
            UVec2::new(w.physical_width(), w.physical_height()),
            w.scale_factor() as f32,
        ));
    }
    headless.map(|h| (h.size, h.scale))
}

/// Refresh [`UiMetrics`] from this frame's surface (`PreUpdate`).
pub fn update_ui_metrics(
    mut metrics: ResMut<UiMetrics>,
    windows: Query<&Window>,
    headless: Option<Res<HeadlessSurface>>,
) {
    let mut next = *metrics;
    next.observe(&windows, headless.as_deref());
    metrics.set_if_neq(next);
}

// ─── UiInput ─────────────────────────────────────────────────────────────────

/// egui's answers for this frame, taken right after its pass begins
/// (`egui_manager::publish_ui_input`). Plain values: what egui decided, not
/// egui itself.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EguiVerdict {
    /// `egui_wants_pointer_input()`: only meaningful while a pointer button
    /// is down (`docs/notes/touch-input.md`), which is when every caller asks.
    pub wants_pointer: bool,
    /// A text field has the keyboard (`text_edit_focused()`, never
    /// `egui_wants_keyboard_input()` — `egui-shared-root-ui.md`).
    pub text_focused: bool,
    /// A drag is under way that started on the bare map.
    pub map_dragged: bool,
    /// The window cursor (window logical points) [`Self::press_over_ui`] was
    /// decided at — the cursor every world click reads this frame, taken after
    /// `touch::recognize_touch_gestures` has moved it to the finger.
    pub cursor: Option<Vec2>,
    /// A press at [`Self::cursor`] lands on egui: on a floating area or modal,
    /// or on a panel (the root layer outside the map). `egui_manager`'s
    /// `press_over_ui`.
    pub press_over_ui: bool,
    /// What the panels left of egui's root `Ui` at the end of the last pass,
    /// in **window** logical points: the map. Only for a cursor other than
    /// [`Self::cursor`], which no caller passes today.
    pub map_rect: Option<Rect>,
}

/// Who owns this frame's input: the interface (egui's panels or the
/// library's screens) or the world under it. See the module header.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct UiInput {
    egui: EguiVerdict,
    /// The pointer is over a library screen (`mithril_bevy::UiPointer`). Set
    /// each frame by `lib_host::bridge_input`, for the next frame.
    lib_pointer: bool,
    /// A library text field has the keyboard (`InputFocus` on an
    /// `EditableText`). Set by `lib_host::bridge_input`, for the next frame.
    lib_keyboard: bool,
    /// A modal owns the screen: the sign-in card (`LoginScreen::modal`), set
    /// each frame by `login_screen::drive_login_screen` in `Input`, before
    /// anything that moves reads the keys. Pointer input takes care of
    /// itself (the card's backdrop is a library screen under the pointer);
    /// the keyboard does not, since the gate otherwise yields only to a
    /// focused field, which would leave WASD walking the avatar behind a
    /// card that has the user's attention.
    modal: bool,
    /// The UI moved on its own last pass (an egui animation in progress), as
    /// opposed to merely wanting a look within a quarter second. Written after
    /// `end_egui_frame`; the idle throttle counts it as busy.
    ui_animating: bool,
}

impl UiInput {
    /// Does this frame's `just_pressed(Left)` belong to the interface rather
    /// than to the world under it? The gate for a *world click* —
    /// `picking::prim_picking` and `building_select::pick_building` must agree
    /// (`picking::PrimClick`).
    ///
    /// A library screen under the pointer owns it outright. A mouse click
    /// asks egui's `wants_pointer`, which is right for a mouse (the button is
    /// still down). **A finger's click is decided by where it landed**,
    /// because it arrives a frame after egui saw it, and a brisk tap has
    /// already lifted: with no button down egui claims every point
    /// (`EguiManager::owns_click` has the whole story).
    #[must_use]
    pub fn owns_click(&self, touch_click: bool, cursor: Option<Vec2>) -> bool {
        if self.lib_pointer {
            return true;
        }
        match (touch_click, cursor) {
            (true, Some(at)) if Some(at) == self.egui.cursor => self.egui.press_over_ui,
            // A point the verdict was not taken at: only the panels are known,
            // not egui's floating areas.
            (true, Some(at)) => self.egui.map_rect.is_some_and(|map| !map.contains(at)),
            _ => self.egui.wants_pointer,
        }
    }

    /// egui's `egui_wants_pointer_input()` alone — the gate the camera's drags
    /// and the gizmo use. Only meaningful while a pointer button is down; a
    /// world click gates on [`Self::owns_click`] instead.
    #[must_use]
    pub fn wants_pointer(&self) -> bool {
        self.egui.wants_pointer
    }

    /// Is a drag under way that started on the bare map — not on a panel or
    /// a floating area? A sheet over the map steps aside for it. (Its one
    /// caller, `scenario`'s sheet, still draws in egui and asks the manager.)
    #[allow(dead_code)]
    #[must_use]
    pub fn map_dragged(&self) -> bool {
        self.egui.map_dragged
    }

    /// True when the keyboard belongs to the UI and the game (avatar
    /// movement, gizmo keys) must ignore it: a text field — egui's or the
    /// library's — is focused, a modal is up, the prim edit dialog is open, or
    /// a Ctrl/Super chord is held. Pass `edit_dialog_open = false` for actions
    /// that stay live while editing a prim (the gizmo's Move/Rotate/Scale).
    #[must_use]
    pub fn ui_owns_keyboard(
        &self,
        edit_dialog_open: bool,
        keyboard: &ButtonInput<KeyCode>,
    ) -> bool {
        self.egui.text_focused
            || self.lib_keyboard
            || self.modal
            || edit_dialog_open
            || keyboard.pressed(KeyCode::ControlLeft)
            || keyboard.pressed(KeyCode::ControlRight)
            // Super for the same reason Control is: on macOS the accelerator
            // is Cmd, so a held Cmd suppresses the game keys or `Cmd+W` walks
            // the avatar forward on its way to closing the window.
            || keyboard.pressed(KeyCode::SuperLeft)
            || keyboard.pressed(KeyCode::SuperRight)
    }

    /// The pointer is over a library screen.
    #[must_use]
    pub fn overlay_owns_pointer(&self) -> bool {
        self.lib_pointer
    }

    /// A library text field has the keyboard.
    #[must_use]
    pub fn overlay_owns_keyboard(&self) -> bool {
        self.lib_keyboard
    }

    /// Whether the UI moved on its own last pass.
    #[must_use]
    pub fn ui_animating(&self) -> bool {
        self.ui_animating
    }

    /// The map: what the side panels left at the end of the last frame, in
    /// **window** logical points, or `None` before the first frame.
    #[must_use]
    pub fn map_rect(&self) -> Option<Rect> {
        self.egui.map_rect
    }

    /// egui's verdict for this frame, as written.
    #[must_use]
    pub fn egui_verdict(&self) -> EguiVerdict {
        self.egui
    }

    /// egui's verdict for this frame (`egui_manager::publish_ui_input`).
    pub fn set_egui(&mut self, verdict: EguiVerdict) {
        self.egui = verdict;
    }

    /// The library's verdict, for the next frame (`lib_host::bridge_input`).
    pub fn set_overlay(&mut self, owns_pointer: bool, owns_keyboard: bool) {
        self.lib_pointer = owns_pointer;
        self.lib_keyboard = owns_keyboard;
    }

    /// Whether a modal owns the screen this frame (`login_screen`).
    #[cfg_attr(not(feature = "comms"), allow(dead_code))]
    pub fn set_modal(&mut self, modal: bool) {
        self.modal = modal;
    }

    /// A modal owns the screen.
    #[must_use]
    #[cfg_attr(not(feature = "comms"), allow(dead_code))]
    pub fn modal(&self) -> bool {
        self.modal
    }

    /// Whether the UI animated on its last pass (`egui_manager`).
    pub fn set_ui_animating(&mut self, animating: bool) {
        self.ui_animating = animating;
    }
}

/// Registers [`UiMetrics`] and [`UiInput`] and keeps the metrics current.
/// `EguiPlugin` adds it too when it is missing (a test app), and writes
/// egui's half of [`UiInput`].
pub struct UiInputPlugin;

impl Plugin for UiInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiMetrics>()
            .init_resource::<UiInput>()
            .add_systems(PreUpdate, update_ui_metrics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(keys: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut input = ButtonInput::default();
        for k in keys {
            input.press(*k);
        }
        input
    }

    /// A zero/NaN scale factor divides every viewport size, so it is
    /// sanitised on the way in.
    #[test]
    fn a_degenerate_scale_factor_never_reaches_the_viewport() {
        let mut m = UiMetrics::with_ui_scale(1.0);
        for bad in [0.0, -1.0, f32::NAN] {
            m.set_surface(UVec2::new(1280, 720), bad);
            assert_eq!(m.viewport_pts(), Vec2::new(1280.0, 720.0));
        }
        m.set_surface(UVec2::new(2560, 1440), 2.0);
        assert_eq!(m.viewport_pts(), Vec2::new(1280.0, 720.0));
        assert!(!m.compact());
    }

    /// A handset through the phone magnification: 393 pt of viewport at a
    /// 2.75 scale factor, 262 pt after 1.5×, and compact; the display scale
    /// comes back out on its own, and window points divide by the UI scale.
    #[test]
    fn the_ui_scale_shrinks_the_viewport_and_converts_window_points() {
        let mut m = UiMetrics::with_ui_scale(1.5);
        m.set_surface(UVec2::new(1080, 2280), 2.75);
        assert!((m.viewport_pts().x - 261.8).abs() < 0.1);
        assert!(m.compact());
        assert!((m.display_scale() - 2.75).abs() < 1e-4);
        assert!((m.pixels_per_point() - 4.125).abs() < 1e-4);
        assert_eq!(
            m.window_to_ui(Vec2::new(300.0, 600.0)),
            Vec2::new(200.0, 400.0)
        );
    }

    /// The metrics follow the window, and keep the last surface once it is
    /// gone; a headless run lays out against its target.
    #[test]
    fn the_metrics_follow_the_window_and_the_headless_target() {
        let mut app = App::new();
        app.insert_resource(UiMetrics::with_ui_scale(1.0))
            .add_systems(Update, update_ui_metrics);
        app.insert_resource(HeadlessSurface {
            size: UVec2::new(800, 600),
            scale: 2.0,
        });
        app.update();
        let m = *app.world().resource::<UiMetrics>();
        assert_eq!(m.viewport_pts(), Vec2::new(400.0, 300.0));
        assert!(m.compact());

        let mut window = Window::default();
        window.resolution.set_physical_resolution(1920, 1080);
        window.resolution.set_scale_factor(1.0);
        app.world_mut().spawn(window);
        app.update();
        assert_eq!(
            app.world().resource::<UiMetrics>().viewport_pts(),
            Vec2::new(1920.0, 1080.0)
        );

        app.world_mut().remove_resource::<HeadlessSurface>();
        let windows: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<Window>>()
            .iter(app.world())
            .collect();
        for w in windows {
            app.world_mut().despawn(w);
        }
        app.update();
        assert_eq!(
            app.world().resource::<UiMetrics>().viewport_pts(),
            Vec2::new(1920.0, 1080.0),
            "a frame with no surface keeps the last one"
        );
    }

    /// The brisk tap, as `UiInput` answers it: a finger's click is decided by
    /// where it landed even when egui, which has seen it lift, claims the
    /// pointer; a mouse click asks egui; a library screen owns any click.
    #[test]
    fn a_fingers_click_is_decided_where_it_landed() {
        let on_map = Vec2::new(300.0, 300.0);
        let mut input = UiInput::default();
        input.set_egui(EguiVerdict {
            wants_pointer: true,
            cursor: Some(on_map),
            press_over_ui: false,
            map_rect: Some(Rect::new(0.0, 0.0, 1000.0, 720.0)),
            ..default()
        });
        assert!(
            !input.owns_click(true, Some(on_map)),
            "the map is the world's"
        );
        assert!(input.owns_click(false, Some(on_map)), "a mouse asks egui");
        assert!(input.owns_click(true, None), "no cursor asks egui");
        // A point the verdict was not taken at falls back to the panels.
        assert!(input.owns_click(true, Some(Vec2::new(1200.0, 300.0))));
        assert!(!input.owns_click(true, Some(Vec2::new(500.0, 300.0))));

        assert!(!input.map_dragged());
        input.set_egui(EguiVerdict {
            map_dragged: true,
            ..input.egui_verdict()
        });
        assert!(input.map_dragged());

        input.set_overlay(true, false);
        assert!(input.owns_click(true, Some(on_map)), "a library screen's");
        assert!(input.overlay_owns_pointer());
    }

    /// The keyboard yields to a text field of either kind, a modal, the edit
    /// dialog or a held accelerator — and to nothing else.
    #[test]
    fn the_game_keys_yield_only_to_text_entry_a_modal_or_a_chord() {
        let none = held(&[]);
        let mut input = UiInput::default();
        assert!(!input.ui_owns_keyboard(false, &none));
        assert!(input.ui_owns_keyboard(true, &none), "the edit dialog");
        assert!(input.ui_owns_keyboard(false, &held(&[KeyCode::ControlLeft])));
        assert!(input.ui_owns_keyboard(false, &held(&[KeyCode::SuperRight])));
        assert!(!input.ui_owns_keyboard(false, &held(&[KeyCode::ShiftLeft])));

        input.set_overlay(false, true);
        assert!(input.ui_owns_keyboard(false, &none), "a library field");
        assert!(input.overlay_owns_keyboard());

        let mut input = UiInput::default();
        input.set_egui(EguiVerdict {
            text_focused: true,
            ..default()
        });
        assert!(input.ui_owns_keyboard(false, &none), "an egui field");

        let mut input = UiInput::default();
        input.set_modal(true);
        assert!(input.ui_owns_keyboard(false, &none), "a modal");
    }
}
