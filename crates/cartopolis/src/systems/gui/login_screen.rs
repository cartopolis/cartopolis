//! The sign-in screen: the face of `systems::session`.
//!
//! **Why it is a modal over the running world and not a gate in front of it.** The
//! simulator is an extension of offline mode, not a different application — the
//! map streams, the camera flies and the avatar walks with no sim at all — so a
//! screen that held the world back until you chose would be a loading screen
//! pretending to be a login. Opening it over the live map instead means "Explore
//! offline" costs nothing (you are already there), and signing in is something
//! that happens *to* the world you are looking at rather than before it exists.
//!
//! It is reached from the app menu beside Developer tools, not shown on launch:
//! the remembered session (`UserStore::data.session`) enters itself, and being
//! asked to log in every time you open a map is the thing this design most wanted
//! to avoid. It opens by itself for exactly one reason — a session that failed or
//! dropped — because that is the only time the app has a question it cannot answer.
//!
//! Drawn as a floating `Area` plus a `Window`, both pinned to the top of
//! `Order::Foreground` every frame with `move_to_top`. That is what puts it above
//! the main menu, which is itself a `Foreground` area (a `Middle`-order
//! window would render *under* the menu it was opened from). It draws nothing at
//! all when closed, so it costs no layers and no input the rest of the time — and
//! it never touches `EguiManager::ui_mut`, so it stays out of the Phase 7b panel
//! chain by construction.
//!
//! **The backdrop is a widget, not a wash.** A full-screen `Sense::click_and_drag`
//! rectangle is what makes the modal modal: the world's mouse consumers all gate on
//! `egui_wants_pointer_input()`, which is only true when egui has actually claimed
//! the pointer (see the note in CLAUDE.md), so a merely *painted* dim layer would
//! let a drag on it spin the camera behind the card.

use bevy::prelude::*;
use cartopolis_core::AuthCredential;

use crate::resources::AuthIdentity;
use crate::systems::egui_manager::EguiManager;
use crate::systems::network;
use crate::systems::session::{
    can_simulate, can_solo, SessionKind, SessionMode, SessionPrefs, SessionRequest, SessionState,
    SessionStatus,
};
use crate::systems::theme;
use crate::systems::user_store::UserStore;

/// Card width in points, *before* clamping. Wide enough for `user@host:port` on
/// one line, narrow enough that the mode rows read as a list rather than a table.
const CARD_W: f32 = 440.0;

/// Margin kept clear around the card, per side.
const CARD_MARGIN: f32 = 12.0;

/// The card frame's horizontal inner margin, per side. Part of the width the
/// window occupies on screen — which is the arithmetic the original 484 pt card
/// missed: `CARD_W` was the *content* width and the 22 pt padding was added to it
/// twice more.
const CARD_PAD_X: i8 = 22;
/// The same, vertically. Counted out of the scroll area's height below for the
/// same reason.
const CARD_PAD_Y: i8 = 18;

/// The card's actual width on this viewport.
///
/// **A modal that does not fit cannot be panned to.** This `Window` is
/// `movable(false)`, `resizable(false)` and centre-anchored, so 440 pt of card
/// plus 44 pt of margins in a 274 pt phone viewport is not "a bit cramped": the
/// right-hand ~200 pt — including the Sign out button and part of the primary
/// action — is off the screen with no gesture that can reach it.
///
/// Floored well below usable for the same reason the address bar's is: on a
/// viewport narrower than the floor the card is clipped either way, and a
/// negative width is a panic in egui's layout rather than an ugly card.
fn card_width(viewport_w: f32) -> f32 {
    let chrome = 2.0 * CARD_MARGIN + 2.0 * CARD_PAD_X as f32;
    CARD_W.min(viewport_w - chrome).max(180.0)
}

/// What the card occupies on screen: its content plus its own padding.
fn card_outer_width(viewport_w: f32) -> f32 {
    card_width(viewport_w) + 2.0 * CARD_PAD_X as f32
}
/// How far the card travels on the way in, in points. Small — this is a hint of
/// motion under the fade, not a slide.
const RISE: f32 = 14.0;
/// Seconds for the open animation.
const FADE_S: f32 = 0.18;
/// Default port of a `simulator`, appended when the user types a bare host.
const DEFAULT_PORT: u16 = 4747;

#[derive(Resource)]
pub struct LoginScreen {
    pub open: bool,
    /// 0 → 1 while opening. Drives both the backdrop alpha and the card's rise, so
    /// they cannot get out of step.
    anim: f32,
    kind: SessionKind,
    server: String,
    username: String,
    password: String,
    /// Keep this server's session token, so it stops asking for the password.
    remember: bool,
    auto_enter: bool,
    /// The error already shown. Compared against `SessionState::error` so a *new*
    /// failure opens the screen and an old one does not re-open it every frame.
    error_shown: Option<String>,
    /// A request is in flight and this card is waiting to see how it lands.
    awaiting: bool,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self {
            open: false,
            anim: 0.0,
            kind: SessionKind::default(),
            server: String::new(),
            username: String::new(),
            password: String::new(),
            remember: true,
            auto_enter: true,
            error_shown: None,
            awaiting: false,
        }
    }
}

impl LoginScreen {
    /// Seed the fields from the remembered session (called once at startup, before
    /// the first frame).
    pub fn from_prefs(prefs: &SessionPrefs) -> Self {
        Self {
            kind: prefs.kind,
            server: prefs.server.clone(),
            username: prefs.username.clone(),
            auto_enter: prefs.auto_enter,
            ..Self::default()
        }
    }

    fn prefs(&self) -> SessionPrefs {
        SessionPrefs {
            kind: self.kind,
            server: self.server.clone(),
            username: self.username.clone(),
            auto_enter: self.auto_enter,
        }
    }

    /// What the form currently asks for, or `None` when it is not answerable yet
    /// (an empty server or user, a mode this build cannot run). Returning `None`
    /// rather than surfacing a validation error is deliberate: the primary button
    /// simply does nothing, which is what an empty required field means.
    ///
    /// **Takes the password**, leaving the field empty: it goes to the network
    /// thread inside an `AuthCredential` and nothing else on this screen — or in
    /// `user.json` — ever wants it again.
    fn build_request(&mut self) -> Option<SessionRequest> {
        match self.kind {
            SessionKind::Offline => Some(SessionRequest::Offline),
            SessionKind::Solo => can_solo().then_some(SessionRequest::Solo { credential: None }),
            SessionKind::Remote => {
                let server = normalize_server(&self.server);
                let username = self.username.trim().to_string();
                if server.is_empty() || username.is_empty() || !can_simulate() {
                    return None;
                }
                self.server = server.clone();
                self.username = username.clone();
                Some(SessionRequest::Remote {
                    server,
                    credential: AuthCredential::Password {
                        username,
                        password: std::mem::take(&mut self.password),
                    },
                    persist_token: self.remember,
                })
            }
        }
    }
}

/// The modes this build can actually offer, in the order they are shown.
///
/// Offline is always here (it is the absence of a sim, so nothing can take it
/// away); Solo needs the `solo` feature compiled in; Remote is shown even where
/// it cannot run, disabled, because "not in this build" is real information
/// about a feature that exists — see the call site.
fn offered_kinds() -> Vec<SessionKind> {
    let mut kinds = vec![SessionKind::Offline];
    if can_solo() {
        kinds.push(SessionKind::Solo);
    }
    kinds.push(SessionKind::Remote);
    kinds
}

/// Complete what the person typed into something [`network::resolve_target`] can
/// act on — **without inventing a port for an address that reaches the network**.
///
/// This used to append [`DEFAULT_PORT`] to every bare host, which was right when
/// a remote sim was a plaintext TCP socket on 4747 and became wrong the day
/// `resolve_target` started sending a bare hostname to `wss://host/ws` over 443.
/// The two rules compose into `wss://cartopolis.org:4747/ws` — so the address
/// `docs/notes/hosted-simulator.md` tells people to type was the one address the
/// sign-in card could not reach, while `--connect cartopolis.org` (which skips
/// this function) worked. A default port is only meaningful where the port is
/// still a real socket: loopback, which is solo's in-process sim, and an explicit
/// `tcp://`, which is the opt-out for a LAN dev sim.
fn normalize_server(raw: &str) -> String {
    let host = raw.trim();
    if host.is_empty() {
        return String::new();
    }
    // A scheme was typed: the person has said exactly what they want, including
    // whether there is a port. Nothing to complete.
    if host.contains("://") {
        return host.to_string();
    }
    // Only two shapes are unambiguously missing a port: a name or IPv4 with no
    // colon at all, and a bracketed IPv6 literal. A *bare* IPv6 address is all
    // colons, and appending to it would produce something that parses as neither
    // an address nor an address-with-port — so it is left exactly as typed and
    // fails later with the server's own message.
    let bracketed = host.starts_with('[') && host.ends_with(']');
    let portless = bracketed || !host.contains(':');
    if portless && network::is_loopback_host(host) {
        format!("{host}:{DEFAULT_PORT}")
    } else {
        host.to_string()
    }
}

/// Draw the sign-in screen when it is open, and apply what it is asked for.
pub fn render_login_screen(
    mut egui: ResMut<EguiManager>,
    mut screen: ResMut<LoginScreen>,
    mut session: ResMut<SessionState>,
    mut store: ResMut<UserStore>,
    identity: Res<AuthIdentity>,
    time: Res<Time<Real>>,
    splash: Res<crate::systems::splash::BootSplash>,
) {
    // A session that failed or dropped is the one thing that opens this by itself:
    // the client has just fallen back to offline and the user did not ask it to.
    if session.error.is_some() && session.error != screen.error_shown {
        screen.error_shown = session.error.clone();
        // Only the *arrival* replays the animation; a failure reported onto a card
        // that is already up must not make it flicker.
        screen.anim = if screen.open { screen.anim } else { 0.0 };
        screen.open = true;
    }
    if session.error.is_none() {
        screen.error_shown = None;
    }

    // Tell the keyboard gate before anything can early-out, so the flag falls back
    // to false the frame the screen closes.
    egui.set_modal(screen.open);
    if !screen.open {
        screen.anim = 0.0;
        return;
    }
    // Wait behind the boot splash rather than over it: both draw a logo and a
    // wordmark, and the splash is already the screen that says "not yet". A card
    // held open through startup simply fades in as the splash fades out.
    if splash.covering() {
        egui.set_modal(false);
        screen.anim = 0.0;
        return;
    }
    // Opening on the mode you are actually in: the card is a switch, and a switch
    // that shows the wrong position is a lie. Only on the opening frame, or the
    // rows would fight the user's clicks.
    if screen.anim == 0.0 && session.mode != SessionMode::Offline {
        screen.kind = session.mode.kind();
    }
    // A remembered mode this build cannot run has no row to show it on, and its
    // primary button silently does nothing (`build_request` answers `None`).
    // Offline is the honest fallback — it is what the client is already doing.
    if screen.kind == SessionKind::Solo && !can_solo() {
        screen.kind = SessionKind::Offline;
    }
    let dt = time.delta_secs();
    screen.anim = (screen.anim + dt / FADE_S).min(1.0);
    // Ease-out: fast to visible, settling into place.
    let t = 1.0 - (1.0 - screen.anim) * (1.0 - screen.anim);

    let viewport = egui.viewport_pts();
    let card_w = card_width(viewport.x);
    // What is left for the card's own content once the margins are taken. The
    // rows below scroll inside this rather than running off the bottom edge —
    // the same reason they are clamped sideways.
    let card_h = (viewport.y - 2.0 * CARD_MARGIN - 2.0 * CARD_PAD_Y as f32).max(160.0);
    let ctx = egui.ctx_mut().clone();

    // ── Backdrop ─────────────────────────────────────────────────────────────
    let backdrop_id = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("sign_in_dim"));
    let dismissed = egui::Area::new(backdrop_id.id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(&ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(viewport, egui::Sense::click_and_drag());
            let p = ui.painter();
            p.rect_filled(rect, 0.0, dim(theme::INK, 0.78 * t));
            // A soft accent-tinted glow behind the card, built from nested rounded
            // rects because egui has no gradient primitive — the same trick the
            // boot splash uses to stand in for `index.html`'s radial one.
            let card = egui::Rect::from_center_size(
                rect.center(),
                egui::vec2(
                    card_outer_width(viewport.x),
                    (rect.height() * 0.62).min(560.0),
                ),
            );
            for i in 1..=5 {
                let k = i as f32;
                p.rect_filled(
                    card.expand(k * 14.0),
                    egui::CornerRadius::same(18 + i * 3),
                    dim(theme::ACCENT, 0.020 * t * (6.0 - k) / 5.0),
                );
            }
            resp.clicked()
        })
        .inner;
    ctx.move_to_top(backdrop_id);

    // ── Card ─────────────────────────────────────────────────────────────────
    let card_id = egui::Id::new("sign_in_card");
    let frame = egui::Frame::NONE
        .fill(theme::SURFACE)
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_LG))
        .inner_margin(egui::Margin::symmetric(CARD_PAD_X, CARD_PAD_Y))
        .shadow(egui::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: dim(theme::INK, 0.55 * t),
        });
    let mut submit = false;
    let mut sign_out = false;
    let mut close = dismissed;
    egui::Window::new("sign in")
        .id(card_id)
        .order(egui::Order::Foreground)
        .title_bar(false)
        .resizable(false)
        .movable(false)
        .collapsible(false)
        .anchor(
            egui::Align2::CENTER_CENTER,
            egui::vec2(0.0, (1.0 - t) * RISE),
        )
        .frame(frame)
        .max_width(card_outer_width(viewport.x))
        .show(&ctx, |ui| {
            ui.set_width(card_w);
            ui.set_opacity(t);
            // A short viewport (a phone in landscape is ~185 pt tall here) cuts
            // the card off the same way a narrow one cuts it off sideways, and a
            // fixed, centre-anchored window has no way to reach what it hides.
            egui::ScrollArea::vertical()
                .max_height(card_h)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    close |= card_ui(
                        ui,
                        &mut screen,
                        &session,
                        &identity,
                        &mut submit,
                        &mut sign_out,
                    );
                });
        });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, card_id));

    // ── Keyboard ─────────────────────────────────────────────────────────────
    let (escape, enter) = ctx.input(|i| {
        (
            i.key_pressed(egui::Key::Escape),
            i.key_pressed(egui::Key::Enter),
        )
    });
    close |= escape;
    // Enter submits from anywhere on the card — including out of the password
    // field, which is where a keyboard-driven sign-in ends.
    submit |= enter && !session.status.is_busy();

    if sign_out {
        if let SessionMode::Remote { server } = session.mode.clone() {
            store.clear_session_token(&server);
        }
        screen.kind = SessionKind::Offline;
        store.set_session_prefs(screen.prefs());
        session.request(SessionRequest::Offline);
        screen.awaiting = true;
        return;
    }
    if submit {
        if let Some(request) = screen.build_request() {
            store.set_session_prefs(screen.prefs());
            session.request(request);
            // Stay open while it connects: the spinner, and any rejection, belong
            // on this card. It dismisses itself below once the session is up.
            screen.awaiting = true;
            return;
        }
    }
    // Dismiss on arrival, not on the click: a sign-in that is still authenticating
    // has not happened yet, and closing over it would leave the user looking at a
    // map with no way to find out whether they got in.
    if screen.awaiting {
        if session.status == SessionStatus::Online || session.status == SessionStatus::Offline {
            screen.awaiting = false;
            if session.error.is_none() {
                screen.open = false;
            }
        }
        return;
    }
    if close && !session.status.is_busy() {
        screen.open = false;
    }
}

/// Everything inside the card. Returns whether it asked to be closed.
fn card_ui(
    ui: &mut egui::Ui,
    screen: &mut LoginScreen,
    session: &SessionState,
    identity: &AuthIdentity,
    submit: &mut bool,
    sign_out: &mut bool,
) -> bool {
    let mut close = false;

    // ── Brand row ────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        // The mark is painted, not sampled, so this card needs no texture and no
        // decode — see `theme::paint_mark`.
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(44.0), egui::Sense::hover());
        theme::paint_mark(ui.painter(), rect.center(), 22.0, 1.0);
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(2.0);
            theme::wordmark_ui(ui, 26.0);
            ui.label(
                egui::RichText::new("Choose how you enter the world")
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            // No way out while a connection is in flight — closing the card would
            // hide the only place the outcome is reported.
            ui.add_enabled_ui(!session.status.is_busy(), |ui| {
                close |= theme::close_button(ui).clicked();
            });
        });
    });

    ui.add_space(12.0);
    status_line(ui, session, identity);
    ui.add_space(10.0);

    // ── Mode rows ────────────────────────────────────────────────────────────
    //
    // A mode this build cannot run is **omitted**, not greyed out. The two are
    // not the same question: `can_simulate()` is false only on the web, where
    // "Connect to a server" is a real feature waiting on a transport and saying
    // so is useful — while `can_solo()` is false whenever the `solo` feature is
    // off, which is the Android and web builds, where an in-process simulator is
    // not coming and a permanently dead row is just a smaller card for nothing.
    // It also matters on the one viewport where the card barely fits.
    for kind in offered_kinds() {
        let enabled = kind != SessionKind::Remote || can_simulate();
        if mode_row(ui, screen.kind == kind, kind, enabled).clicked() && enabled {
            screen.kind = kind;
        }
        ui.add_space(6.0);
    }

    if screen.kind == SessionKind::Remote {
        ui.add_space(2.0);
        field(ui, "Server", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut screen.server)
                    // A bare hostname, because that is now what works and what
                    // the docs hand out. A hint carrying `:4747` taught the one
                    // shape that cannot reach a TLS sim.
                    .hint_text("cartopolis.org")
                    .desired_width(f32::INFINITY),
            );
        });
        field(ui, "User", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut screen.username)
                    .hint_text("your account name")
                    .desired_width(f32::INFINITY),
            );
        });
        field(ui, "Password", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut screen.password)
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
        });
        ui.add_space(2.0);
        theme::checkbox(ui, &mut screen.remember, "Remember me on this device")
            .on_hover_text("Keep the session token this server issues, so it stops asking");
    }

    ui.add_space(10.0);
    theme::checkbox(ui, &mut screen.auto_enter, "Enter this way next time")
        .on_hover_text("Skip this screen on the next launch");
    ui.add_space(12.0);

    // ── Actions ──────────────────────────────────────────────────────────────
    let busy = session.status.is_busy();
    ui.horizontal(|ui| {
        let label = match screen.kind {
            SessionKind::Offline => "Explore offline",
            SessionKind::Solo => "Start my world",
            SessionKind::Remote => "Sign in",
        };
        let width = if session.is_online() {
            ui.available_width() - 108.0
        } else {
            ui.available_width()
        };
        ui.add_enabled_ui(!busy, |ui| {
            // `theme::primary_button_sized`, not a hand-built filled `Button`:
            // this card and the Route panel's "▶ Driver mode" were the same rank
            // and did not look alike, which is what that helper exists to end.
            *submit |= theme::primary_button_sized(ui, label, width).clicked();
        });
        if session.is_online() {
            ui.add_enabled_ui(!busy, |ui| {
                *sign_out |= ui
                    .add(egui::Button::new("Sign out").min_size(egui::vec2(100.0, 34.0)))
                    .on_hover_text("Leave this world and go back to the offline map")
                    .clicked();
            });
        }
    });

    if busy {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Spinner::new()
                    .size(theme::SIZE_VALUE)
                    .color(theme::ACCENT),
            );
            ui.label(
                egui::RichText::new(session.status.label())
                    .size(theme::SIZE_SUB)
                    .color(theme::TEXT_DIM),
            );
        });
    } else if let Some(error) = &session.error {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(error)
                .size(theme::SIZE_SUB)
                .color(theme::ERROR)
                .strong(),
        );
    }
    close
}

/// Where you are right now — the line that makes "sign out" mean something.
fn status_line(ui: &mut egui::Ui, session: &SessionState, identity: &AuthIdentity) {
    let (dot, text) = match &session.status {
        SessionStatus::Online => (
            theme::ACCENT_BRIGHT,
            match identity.username.as_deref() {
                Some(user) => format!("{user} · {}", session.mode.where_()),
                None => session.mode.where_(),
            },
        ),
        SessionStatus::Offline => (theme::TEXT_DIM, "Offline — the map only".to_string()),
        busy => (theme::WARN, busy.label().to_string()),
    };
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, dot);
        ui.label(
            egui::RichText::new(text)
                .size(theme::SIZE_SUB)
                .color(theme::TEXT),
        );
    });
}

/// One selectable mode: an accent bar, a title and a line of what it gets you.
///
/// Painted rather than assembled from `SelectableLabel`, which is a single line of
/// text and gives no room for the blurb — and the blurb is the point, since the
/// difference between these three is not a preference but three different worlds.
fn mode_row(ui: &mut egui::Ui, selected: bool, kind: SessionKind, enabled: bool) -> egui::Response {
    const H: f32 = 52.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), H),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let hovered = enabled && resp.hovered();
    let (fill, stroke) = match (selected, hovered) {
        (true, _) => (theme::SURFACE_HOVER, theme::ACCENT),
        (false, true) => (theme::SURFACE_RAISED, theme::LINE),
        (false, false) => (theme::SURFACE_RAISED, theme::OVERLAY_LINE),
    };
    let p = ui.painter();
    p.rect(
        rect,
        egui::CornerRadius::same(8),
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    if selected {
        // The accent bar, inset so it reads as part of the row's own edge.
        let bar = egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(1.0, 8.0),
            egui::vec2(3.0, H - 16.0),
        );
        p.rect_filled(bar, egui::CornerRadius::same(2), theme::ACCENT);
    }
    let (title, blurb) = if enabled {
        (theme::CREAM, theme::TEXT_DIM)
    } else {
        (theme::TEXT_DIM, theme::TEXT_DIM)
    };
    let x = rect.left() + 16.0;
    p.text(
        egui::pos2(x, rect.top() + 13.0),
        egui::Align2::LEFT_CENTER,
        kind.label(),
        egui::FontId::proportional(theme::SIZE_BODY),
        title,
    );
    let blurb_text = if enabled {
        kind.blurb().to_string()
    } else {
        "Not available in this build".to_string()
    };
    p.text(
        egui::pos2(x, rect.top() + 34.0),
        egui::Align2::LEFT_CENTER,
        blurb_text,
        egui::FontId::proportional(theme::SIZE_CHIP),
        blurb,
    );
    resp
}

/// A labelled row in the remote-server form. The label column is fixed so the
/// three fields line up — a `horizontal` with two labels of different widths puts
/// each field somewhere else, which reads as three unrelated boxes.
fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(66.0, 20.0), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(theme::SIZE_SUB),
            theme::TEXT_DIM,
        );
        add(ui);
    });
}

/// A palette colour at a fraction of its opacity. Every alpha on this screen goes
/// through the open animation, so they are all written the same way.
fn dim(c: egui::Color32, a: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A bare hostname is left bare**, because `resolve_target` sends it to
    /// `wss://host/ws` on 443. Completing it with the TCP sim port produced
    /// `wss://host:4747/ws`, which is why the public address in
    /// `docs/notes/hosted-simulator.md` could be typed into this card and never
    /// connect.
    #[test]
    fn a_remote_host_keeps_the_port_it_was_given() {
        assert_eq!(normalize_server("cartopolis.org"), "cartopolis.org");
        assert_eq!(normalize_server("  sim.example.org  "), "sim.example.org");
        assert_eq!(normalize_server("  10.0.0.4  "), "10.0.0.4");
    }

    /// Loopback is the one place a default port still means a socket: it is
    /// solo's in-process sim, which `resolve_target` keeps on plain TCP.
    #[test]
    fn loopback_still_gets_the_default_port() {
        assert_eq!(normalize_server("localhost"), "localhost:4747");
        assert_eq!(normalize_server("127.0.0.1"), "127.0.0.1:4747");
        assert_eq!(normalize_server("[::1]"), "[::1]:4747");
    }

    /// An explicit scheme is the person saying exactly what they want — the
    /// `tcp://` and `ws://` opt-outs `resolve_target` documents.
    #[test]
    fn an_explicit_scheme_is_left_alone() {
        assert_eq!(
            normalize_server("tcp://sim.example.org"),
            "tcp://sim.example.org"
        );
        assert_eq!(normalize_server("wss://host/ws"), "wss://host/ws");
    }

    #[test]
    fn an_explicit_port_is_left_alone() {
        assert_eq!(
            normalize_server("sim.example.org:9000"),
            "sim.example.org:9000"
        );
        assert_eq!(normalize_server("127.0.0.1:4747"), "127.0.0.1:4747");
        assert_eq!(normalize_server("[::1]:4747"), "[::1]:4747");
    }

    /// The regression this card shipped with: the address the project documents
    /// as *the* one to type has to survive the field and come out of
    /// `resolve_target` on 443.
    #[test]
    fn the_documented_public_address_reaches_the_public_simulator() {
        let typed = normalize_server("cartopolis.org");
        assert!(matches!(
            network::resolve_target(&typed),
            network::Target::WebSocket(ref url) if url == "wss://cartopolis.org/ws"
        ));
    }

    /// A bare IPv6 literal is all colons; appending `:4747` to it would produce
    /// an address that parses as neither.
    /// A bare IPv6 literal is all colons: there is no port to add that would not
    /// corrupt it, so it goes through untouched. Bracketed, the port is safe.
    #[test]
    fn ipv6_literals_are_not_guessed_at() {
        assert_eq!(normalize_server("::1"), "::1");
        assert_eq!(normalize_server("fe80::1"), "fe80::1");
        assert_eq!(normalize_server("[::1]"), "[::1]:4747");
    }

    #[test]
    fn empty_stays_empty_so_submit_can_refuse_it() {
        assert_eq!(normalize_server("   "), "");
    }

    /// The card is `movable(false)`, `resizable(false)` and centre-anchored, so
    /// whatever it does not fit is unreachable rather than merely awkward.
    #[test]
    fn the_card_fits_the_viewport_it_is_centred_in() {
        // A desktop is wide enough for the preferred width.
        assert_eq!(card_width(1280.0), CARD_W);
        // A phone (411 dp at `ui_scale` 1.5) is not — and the padding counts:
        // the original card was 440 pt of content in a frame with 22 pt either
        // side, i.e. 484 pt on screen, which is where "1.7× the screen" came from.
        let phone = card_outer_width(274.0);
        assert!(
            phone + 2.0 * CARD_MARGIN <= 274.0,
            "card {phone} pt does not fit a 274 pt viewport"
        );
        // …and never a negative width, whatever the viewport claims to be.
        assert!(card_width(0.0) > 0.0);
    }

    /// Offline is always offered — it is the absence of a simulator, so nothing
    /// can take it away — and Solo is offered only where it can actually run.
    #[test]
    fn only_runnable_modes_are_offered() {
        let kinds = offered_kinds();
        assert_eq!(kinds.first(), Some(&SessionKind::Offline));
        assert!(kinds.contains(&SessionKind::Remote));
        assert_eq!(kinds.contains(&SessionKind::Solo), can_solo());
    }

    /// The screen is seeded from `user.json` and writes the same shape back, so a
    /// round trip must not lose a field.
    #[test]
    fn prefs_round_trip_through_the_screen() {
        let prefs = SessionPrefs {
            kind: SessionKind::Remote,
            server: "sim.example.org:4747".into(),
            username: "alice".into(),
            auto_enter: false,
        };
        assert_eq!(LoginScreen::from_prefs(&prefs).prefs(), prefs);
    }
}
