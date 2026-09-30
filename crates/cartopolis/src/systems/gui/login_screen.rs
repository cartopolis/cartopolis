//! The sign-in screen: the face of `systems::session`.
//!
//! **Why it is a modal over the running world and not a gate in front of it.** The
//! simulator is an extension of offline mode, not a different application — the
//! map streams, the camera flies and the avatar walks with no sim at all — so a
//! screen that held the world back until you chose would be a loading screen
//! pretending to be a login. Opening it over the live map instead means staying
//! offline costs nothing (you are already there), and signing in is something
//! that happens *to* the world you are looking at rather than before it exists.
//!
//! **It is a form, not a picker.** It used to open on three rows — Explore
//! offline / Solo simulator / Connect to a server — which asked a first-time user
//! to choose between three architectures before they had seen anything, and two
//! of the three answers were "carry on as you are". Offline is now simply where
//! everybody starts (`SessionPrefs::default`) and is reached by not signing in or
//! by logging out. What is left is one question — which server, as whom — and the
//! card is that question.
//!
//! **The app menu is the only thing that opens it, and a live session is the only
//! thing that closes it.** It is reached from the top of that menu, never shown on
//! launch: the remembered session (`UserStore::data.session`) enters itself, and
//! being asked to log in every time you open a map is the thing this design most
//! wanted to avoid. It used to raise itself over the map whenever a session failed
//! or dropped, which made a modal an *event* — a card the app pushed in front of
//! you, mid-flight, for something you had not asked about — so a failed sign-in
//! now reports itself in the menu it was started from (`ui::render_main_menu`),
//! next to the entry that offers to try again. A failure on a card that is still
//! open still lands on that card: the question is up, so the answer belongs there.
//!
//! Leaving is likewise the menu's, not this card's (`session::sign_out`): one
//! entry, one outcome, and no modal to dismiss afterwards.
//!
//! Drawn as a floating `Area` plus a `Window`, both pinned to the top of
//! `Order::Foreground` every frame with `move_to_top`. That is what puts it above
//! the main menu, which is itself a `Foreground` area (a `Middle`-order
//! window would render *under* the menu it was opened from). It draws nothing at
//! all when closed, so it costs no layers and no input the rest of the time — and
//! it never touches `EguiManager::ui_mut`, so it stays out of the panel
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
    can_simulate, SessionKind, SessionPrefs, SessionRequest, SessionState, SessionStatus,
};
use crate::systems::theme;
use crate::systems::user_store::UserStore;

/// Card width in points, *before* clamping. Wide enough for `user@host:port` on
/// one line, narrow enough to read as a form rather than as a dialog box.
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
/// right-hand ~200 pt — including part of the primary action — is off the screen
/// with no gesture that can reach it.
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
    server: String,
    username: String,
    password: String,
    /// Keep this server's session token, so it stops asking for the password —
    /// and, since a remembered session is exactly one that can be entered without
    /// asking, the same tick is what `SessionPrefs::auto_enter` gets. They were
    /// two checkboxes ("Remember me on this device" / "Enter this way next time")
    /// and the combination that made them different — remember the token but ask
    /// anyway, or auto-enter with no token, which fails — is not one anybody wants.
    remember: bool,
    /// A request is in flight and this card is waiting to see how it lands.
    awaiting: bool,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self {
            open: false,
            anim: 0.0,
            server: String::new(),
            username: String::new(),
            password: String::new(),
            remember: true,
            awaiting: false,
        }
    }
}

impl LoginScreen {
    /// Is a request this card issued still in flight?
    ///
    /// Exposed for `systems::dismiss`: the card's own close button is disabled
    /// while it is waiting, and Escape must not be a way around that — an
    /// unbounded modal you can dismiss halfway through a handshake leaves the
    /// session in a state nothing on screen describes.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.awaiting
    }

    /// The one rule that closes this card: a live session.
    ///
    /// A method rather than two lines in the system so it can be checked without a
    /// renderer — and because "closed" has to mean the same thing for a sign-in
    /// this card issued and for one that arrived from anywhere else (startup's
    /// auto-enter, `--connect`). It is deliberately not conditioned on `awaiting`:
    /// the card asks which server, as whom, and being *in* a world is the answer to
    /// that question however you got there.
    fn settle(&mut self, session: &SessionState) {
        if session.is_online() {
            self.awaiting = false;
            self.open = false;
        }
    }

    /// Seed the fields from the remembered session (called once at startup, before
    /// the first frame).
    pub fn from_prefs(prefs: &SessionPrefs) -> Self {
        Self {
            server: prefs.server.clone(),
            username: prefs.username.clone(),
            // A remembered *server* session is the only one that was signed in
            // to, so it is the only one whose tick means anything here.
            remember: prefs.kind != SessionKind::Remote || prefs.auto_enter,
            ..Self::default()
        }
    }

    /// What to write back to `user.json` on a successful submit. Always
    /// [`SessionKind::Remote`] — this card asks one question now, and the answer
    /// to it is a server.
    fn prefs(&self) -> SessionPrefs {
        SessionPrefs {
            kind: SessionKind::Remote,
            server: self.server.clone(),
            username: self.username.clone(),
            auto_enter: self.remember,
        }
    }

    /// What the form currently asks for, or `None` when it is not answerable yet
    /// (an empty server or user). Returning `None` rather than surfacing a
    /// validation error is deliberate: the primary button simply does nothing,
    /// which is what an empty required field means.
    ///
    /// **Takes the password**, leaving the field empty: it goes to the network
    /// thread inside an `AuthCredential` and nothing else on this screen — or in
    /// `user.json` — ever wants it again.
    /// Fill the form for a scripted capture (`--ui signin`), which has no
    /// `user.json` to seed it from and would otherwise photograph the empty
    /// state. Only ever writes over an empty field, so a preset cannot undo what
    /// a remembered session put there.
    pub fn seed_for_capture(&mut self, server: &str, username: &str) {
        if self.server.is_empty() {
            self.server = server.to_string();
        }
        if self.username.is_empty() {
            self.username = username.to_string();
        }
    }

    /// Is there a question here the server could be asked? The same rule
    /// [`Self::build_request`] refuses on, stated separately so the button can be
    /// disabled *before* the press rather than swallowing it.
    #[must_use]
    fn answerable(&self) -> bool {
        !normalize_server(&self.server).is_empty()
            && !self.username.trim().is_empty()
            && can_simulate()
    }

    fn build_request(&mut self) -> Option<SessionRequest> {
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
/// still a real socket: loopback, which is a dev sim on this machine, and an
/// explicit `tcp://`, which is the opt-out for a LAN dev sim.
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
/// The taller of the two bands the on-screen keyboard leaves — the whole
/// viewport when there is no keyboard.
///
/// A pure function so the case that matters can be checked without a screen: the
/// keyboard docks to whichever edge is *further* from the focused field, so the
/// band is above it as often as below, and a version of this that only ever
/// lifted would put the card under the slab half the time.
#[must_use]
fn free_band(viewport: egui::Vec2, slab: Option<egui::Rect>) -> egui::Rect {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
    let Some(slab) = slab else {
        return screen;
    };
    let above = egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, slab.min.y));
    let below = egui::Rect::from_min_max(egui::pos2(0.0, slab.max.y), screen.max);
    // A slab that somehow covers everything leaves the screen rather than a
    // negative band: a card squeezed to nothing is worse than one behind a
    // keyboard the user can close.
    let best = if above.height() >= below.height() {
        above
    } else {
        below
    };
    if best.height() > 1.0 {
        best
    } else {
        screen
    }
}

pub fn render_login_screen(
    mut egui: ResMut<EguiManager>,
    mut screen: ResMut<LoginScreen>,
    mut session: ResMut<SessionState>,
    mut store: ResMut<UserStore>,
    identity: Res<AuthIdentity>,
    time: Res<Time<Real>>,
    splash: Res<crate::systems::splash::BootSplash>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
) {
    // **A live session closes this card, whoever opened it and whoever started the
    // sign-in.** Not only the request this card issued: a startup auto-enter that
    // lands, or a `--connect` that comes up while the card is up, has answered the
    // one question the card exists to ask, and a form still asking it over a world
    // you are already in is a modal with nothing to submit. Tested by
    // `an_online_session_closes_the_card`.
    screen.settle(&session);

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
    let dt = time.delta_secs();
    screen.anim = (screen.anim + dt / FADE_S).min(1.0);
    // Ease-out: fast to visible, settling into place.
    let t = 1.0 - (1.0 - screen.anim) * (1.0 - screen.anim);

    let viewport = egui.viewport_pts();
    let card_w = card_width(viewport.x);
    // **The keyboard takes its half of the screen, and the card has to live in
    // the other half.** This is the one card in the app whose whole purpose is
    // typing, and it was laid out against the full viewport: centred, it put the
    // password field and the Sign in button under a slab that covers ~40 % of a
    // phone, and the `ScrollArea` below could not help because the card's *frame*
    // was already down there. Which layer paints on top is not the issue and
    // never was — the keyboard raises itself last, so it is drawn above the card
    // and its keys work. What was underneath them was the thing being typed into.
    //
    // `free` is the band the keyboard leaves. `layout` docks the slab to whichever
    // edge is further from the focused field, so this is a *shift*, not a lift:
    // the card moves down as readily as up. `slab()` is last frame's rect (the
    // keyboard is drawn after this system, by construction — it must raise itself
    // last), so the card follows one frame behind the keyboard opening. That is
    // the same contract `place_pins` reads it under, and a frame of lag on a slab
    // that appears once per sign-in is not worth an ordering inversion.
    let free = free_band(viewport, keyboard.slab());
    // How far the card's centre moves from the viewport's, to sit in that band.
    let keyboard_shift = free.center().y - viewport.y * 0.5;
    // What is left for the card's own content once the margins are taken. The
    // rows below scroll inside this rather than running off the bottom edge —
    // the same reason they are clamped sideways.
    let card_h = (free.height() - 2.0 * CARD_MARGIN - 2.0 * CARD_PAD_Y as f32).max(160.0);
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
            // Centred on the card, not on the screen — the glow is what says
            // "the thing you are looking at is here", so it has to follow the
            // card into the band the keyboard leaves.
            let card = egui::Rect::from_center_size(
                rect.center() + egui::vec2(0.0, keyboard_shift),
                egui::vec2(
                    card_outer_width(viewport.x),
                    (free.height() * 0.62).min(560.0),
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
        // The same glass every panel and window in the app is made of — see
        // `theme::GLASS`. A card over a dimmed map is exactly the case the
        // translucency was chosen for.
        .fill(theme::GLASS)
        .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_LG))
        .inner_margin(egui::Margin::symmetric(CARD_PAD_X, CARD_PAD_Y))
        .shadow(egui::Shadow {
            offset: [0, 10],
            blur: 28,
            spread: 0,
            color: dim(theme::INK, 0.55 * t),
        });
    let mut submit = false;
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
            egui::vec2(0.0, keyboard_shift + (1.0 - t) * RISE),
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
                    close |= card_ui(ui, &mut screen, &session, &identity, &mut submit);
                });
        });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, card_id));

    // ── Keyboard ─────────────────────────────────────────────────────────────
    // Escape is `systems::dismiss`'s, not this card's: it is one rung of a
    // ladder that has to close exactly one thing per press, and four files
    // reading the same key independently is how one press would close two.
    //
    // Enter submits from anywhere on the card — including out of the password
    // field, which is where a keyboard-driven sign-in ends. Note the comment
    // that used to say so was only true with a field focused, since that was
    // the condition for the key arriving at all; it is true now.
    let enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    submit |= enter && !session.status.is_busy();

    if submit {
        if let Some(request) = screen.build_request() {
            // Says the request left the card, which is the one step between the
            // button and `network::start_link` that used to have no evidence at
            // all: a sign-in that produced nothing could equally have been a
            // press that never reached the button, a form the card refused, or a
            // socket that failed — and the log said the same thing (nothing) for
            // all three.
            info!(server = %screen.server, user = %screen.username, "sign-in requested");
            store.set_session_prefs(screen.prefs());
            session.request(request);
            // Stay open while it connects: the spinner, and any rejection, belong
            // on this card. `settle` closes it at the top of the frame the session
            // comes up.
            screen.awaiting = true;
            return;
        }
    }
    // Dismiss on arrival, not on the click: a sign-in that is still authenticating
    // has not happened yet, and closing over it would leave the user looking at a
    // map with no way to find out whether they got in. Arrival is handled at the
    // top of this system — so all that is left here is the other outcome, a
    // request that came back offline, whose reason is now on the card in front of
    // the person who asked for it.
    if screen.awaiting {
        if !session.status.is_busy() {
            screen.awaiting = false;
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
) -> bool {
    let mut close = false;

    // ── Brand row ────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        // The mark is painted, not sampled, so this card needs no texture and no
        // decode — see `theme::paint_mark`.
        let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(44.0), egui::Sense::hover());
        theme::paint_mark(ui.painter(), rect.center(), 22.0, 1.0);
        ui.add_space(4.0);
        // **The ✕ takes its corner first.** Laid out after the text it was one
        // more widget in a left-to-right row, so on a phone the wordmark simply
        // ran under the cross — `wordmark_ui` paints at the size it is given and
        // no width constraint shrinks it. Right-to-left here means the close
        // button is placed against the right edge and the text column gets what
        // is actually left.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            // No way out while a connection is in flight — closing the card would
            // hide the only place the outcome is reported.
            ui.add_enabled_ui(!session.status.is_busy(), |ui| {
                close |= theme::close_button(ui).clicked();
            });
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.add_space(2.0);
                // The brand steps down on a narrow card rather than wrapping:
                // "cartopolis" is one word and half of it on a second line is not
                // a wordmark.
                let brand = if ui.available_width() < 150.0 {
                    21.0
                } else {
                    26.0
                };
                theme::wordmark_ui(ui, brand);
                ui.label(
                    egui::RichText::new("Sign in to share a world with other people")
                        .size(theme::SIZE_SUB)
                        .color(theme::TEXT_DIM),
                );
            });
        });
    });

    ui.add_space(12.0);
    status_line(ui, session, identity);
    ui.add_space(10.0);

    // ── The one question ─────────────────────────────────────────────────────
    field(ui, "Server", |ui| {
        crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut screen.server)
                // A bare hostname, because that is now what works and what
                // the docs hand out. A hint carrying `:4747` taught the one
                // shape that cannot reach a TLS sim.
                //
                // **"e.g." is not decoration.** egui paints a hint in
                // `weak_text_color`, which this palette sets to `TEXT_DIM` —
                // 15 % darker than body text, and at this size on this surface
                // that is not a difference anybody reads as "empty". The hint
                // was *exactly* the string a filled field would hold, so an
                // untouched Server field was indistinguishable from one
                // containing the right answer, and the button below it then
                // refused the form without a word (`build_request` returns
                // `None`). A hint that is an example rather than an impostor
                // cannot be mistaken for a value however dim the theme is.
                .hint_text("e.g. cartopolis.org")
                .desired_width(f32::INFINITY),
        );
    });
    field(ui, "User", |ui| {
        crate::systems::text_touch::field(
            ui,
            egui::TextEdit::singleline(&mut screen.username)
                .hint_text("your account name")
                .desired_width(f32::INFINITY),
        );
    });
    field(ui, "Password", |ui| {
        theme::password_field(ui, &mut screen.password);
    });
    ui.add_space(6.0);
    theme::checkbox(ui, &mut screen.remember, "Stay signed in on this device")
        .on_hover_text("Keep the session token this server issues, so it stops asking — and come back to this world on the next launch");
    ui.add_space(12.0);

    // ── Actions ──────────────────────────────────────────────────────────────
    let busy = session.status.is_busy();
    // One verb, the card's full width. There used to be a "Log out" beside it,
    // for the case where a failure on a live session raised this card over a world
    // you were still in — a case that no longer exists now that nothing but the app
    // menu opens the card and a live session closes it. Leaving is that menu's
    // (`session::sign_out`): one entry, one tap, no modal to dismiss afterwards.
    //
    // **The button says which of its three states it is in**, because a press on
    // this one starts a network round trip and the answer is seconds away. Live,
    // it takes the press. Waiting on the handshake, it holds the status the
    // spinner below it repeats. And *unanswerable* — an empty server or user —
    // it is disabled rather than live-and-inert: `build_request` refuses those,
    // so the button that looked pressable simply did nothing, which is
    // indistinguishable from a sign-in that failed silently and is most of what
    // "clicking it gives no feedback" was.
    let ready = screen.answerable();
    ui.add_enabled_ui(!busy && ready, |ui| {
        // `theme::primary_button_sized`, not a hand-built filled `Button`: this
        // card and the Route panel's "▶ Driver mode" were the same rank and did
        // not look alike, which is what that helper exists to end.
        let label = if busy {
            session.status.label()
        } else {
            "Sign in"
        };
        let button = theme::primary_button_sized(ui, label, ui.available_width());
        *submit |= button.clicked();
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
    } else if !ready {
        // Says which field is missing rather than what is wrong with the form:
        // the card asks two questions and this names the one still unanswered.
        ui.add_space(8.0);
        let missing = if normalize_server(&screen.server).is_empty() {
            "Enter the server to sign in to"
        } else {
            "Enter your account name"
        };
        ui.label(
            egui::RichText::new(missing)
                .size(theme::SIZE_SUB)
                .color(theme::TEXT_DIM),
        );
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

    /// Loopback is the one place a default port still means a socket: a dev
    /// sim on this machine, which `resolve_target` keeps on plain TCP.
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

    /// The screen is seeded from `user.json` and writes the same shape back, so a
    /// round trip must not lose a field. `auto_enter` rides the one remaining
    /// tick, so a remembered session comes back ticked and comes out remembered.
    #[test]
    fn prefs_round_trip_through_the_screen() {
        let prefs = SessionPrefs {
            kind: SessionKind::Remote,
            server: "sim.example.org:4747".into(),
            username: "alice".into(),
            auto_enter: true,
        };
        assert_eq!(LoginScreen::from_prefs(&prefs).prefs(), prefs);

        // Signed in once without staying signed in: the tick comes back clear.
        let once = SessionPrefs {
            auto_enter: false,
            ..prefs.clone()
        };
        assert_eq!(LoginScreen::from_prefs(&once).prefs(), once);
    }

    /// **Signing in closes the card.** Whoever opened it and whoever started the
    /// sign-in: `awaiting` is the request *this* card issued, and a session that
    /// came up from startup's auto-enter or from `--connect` answers the same
    /// question, so it cannot be the condition.
    /// A session in a given state, with an optional reason on it. `SessionState`
    /// keeps its link private, so it is built and then written rather than
    /// struct-literalled.
    fn session(status: SessionStatus, error: Option<&str>) -> SessionState {
        let mut session = SessionState::default();
        session.status = status;
        session.error = error.map(str::to_string);
        session
    }

    #[test]
    fn an_online_session_closes_the_card() {
        let online = session(SessionStatus::Online, None);

        // The ordinary path: submitted here, waiting on the handshake.
        let mut screen = LoginScreen {
            open: true,
            awaiting: true,
            ..LoginScreen::default()
        };
        screen.settle(&online);
        assert!(!screen.open, "a live session left the form on screen");
        assert!(!screen.awaiting);

        // Opened from the menu, signed in from somewhere else.
        let mut watching = LoginScreen {
            open: true,
            ..LoginScreen::default()
        };
        watching.settle(&online);
        assert!(!watching.open);

        // An error left over from an earlier attempt is not a reason to stay up:
        // it is stale the moment a session comes up behind it.
        let mut after_a_failure = LoginScreen {
            open: true,
            awaiting: true,
            ..LoginScreen::default()
        };
        after_a_failure.settle(&session(
            SessionStatus::Online,
            Some("sim.example: rejected"),
        ));
        assert!(!after_a_failure.open);
    }

    /// The other outcomes leave it up: mid-handshake there is nothing to report
    /// yet, and a rejection has to be readable by the person who asked for it.
    #[test]
    fn anything_short_of_a_session_leaves_the_card_up() {
        for status in [
            SessionStatus::Connecting,
            SessionStatus::Authenticating,
            SessionStatus::Offline,
        ] {
            let mut screen = LoginScreen {
                open: true,
                awaiting: true,
                ..LoginScreen::default()
            };
            screen.settle(&session(status.clone(), Some("sim.example: no")));
            assert!(screen.open, "{status:?} closed the card");
        }
    }

    /// Drive the real card structure for six frames, pressing at `at` (when
    /// given) on frames 3/4. Returns `(submitted, dismissed_by_backdrop,
    /// card_rect)`.
    fn drive_card(at: Option<egui::Pos2>) -> (bool, bool, Option<egui::Rect>) {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let viewport = egui::vec2(936.0, 620.0);
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
        let mut screen = LoginScreen {
            open: true,
            anim: 1.0,
            server: "cartopolis.org".into(),
            username: "jeroen".into(),
            password: "hunter2".into(),
            ..LoginScreen::default()
        };
        let sess = SessionState::default();
        let identity = AuthIdentity::default();
        let mut submitted = false;
        let mut dismissed_any = false;
        let mut last_rect = None;
        for frame in 0..6 {
            let mut raw = egui::RawInput::default();
            raw.screen_rect = Some(screen_rect);
            if let Some(pos) = at {
                raw.events = match frame {
                    2 => vec![egui::Event::PointerMoved(pos)],
                    3 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    4 => vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    _ => vec![],
                };
            }
            let mut submit = false;
            let card_rect = std::cell::Cell::new(None);
            let dismissed = std::cell::Cell::new(false);
            let screen_ref = &mut screen;
            let backdrop_id =
                egui::LayerId::new(egui::Order::Foreground, egui::Id::new("sign_in_dim"));
            let _ = ctx.run_ui(raw, |root| {
                let ctx = &root.ctx().clone();
                let hit = egui::Area::new(backdrop_id.id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(egui::pos2(0.0, 0.0))
                    .show(ctx, |ui| {
                        let (_, resp) =
                            ui.allocate_exact_size(viewport, egui::Sense::click_and_drag());
                        resp.clicked()
                    })
                    .inner;
                ctx.move_to_top(backdrop_id);
                dismissed.set(hit);

                let card_id = egui::Id::new("sign_in_card");
                let frame_style = egui::Frame::NONE
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(CARD_PAD_X, CARD_PAD_Y));
                let inner = egui::Window::new("sign in")
                    .id(card_id)
                    .order(egui::Order::Foreground)
                    .title_bar(false)
                    .resizable(false)
                    .movable(false)
                    .collapsible(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .frame(frame_style)
                    .max_width(card_outer_width(viewport.x))
                    .show(ctx, |ui| {
                        ui.set_width(card_width(viewport.x));
                        egui::ScrollArea::vertical()
                            .max_height(520.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                card_ui(ui, screen_ref, &sess, &identity, &mut submit);
                            });
                    });
                ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, card_id));
                if let Some(inner) = inner {
                    card_rect.set(Some(inner.response.rect));
                }
            });
            submitted |= submit;
            dismissed_any |= dismissed.get();
            if let Some(r) = card_rect.get() {
                last_rect = Some(r);
            }
        }
        (submitted, dismissed_any, last_rect)
    }

    /// **The click on the card's one button has to reach the button.** The card
    /// is drawn as a full-screen `click_and_drag` backdrop with a `Window` over
    /// it, both on `Order::Foreground`, both raised with `move_to_top` every
    /// frame — an arrangement where "which of the two took the press" is decided
    /// by layer order rather than by geometry, and where a mistake shows up as a
    /// button that visibly does nothing.
    #[test]
    fn clicking_sign_in_asks_for_a_session() {
        let (_, _, card) = drive_card(None);
        let card = card.expect("the card drew nothing");
        eprintln!("card rect: {card:?}");
        let mut hits = Vec::new();
        let mut y = card.min.y + 2.0;
        while y < card.max.y {
            let (submitted, dismissed, _) = drive_card(Some(egui::pos2(card.center().x, y)));
            if submitted || dismissed {
                hits.push((y, submitted, dismissed));
            }
            y += 4.0;
        }
        eprintln!("hits: {hits:?}");
        assert!(
            hits.iter().any(|h| h.1),
            "no point down the middle of the card submitted"
        );
        assert!(
            !hits.iter().any(|h| h.2),
            "the backdrop took a press inside the card"
        );
    }

    /// **An unanswerable form disables the button rather than swallowing the
    /// press.** `build_request` refuses an empty server or user by returning
    /// `None`, and for as long as the button stayed live that refusal was
    /// completely silent: no spinner, no error, no connection — the exact
    /// symptom of a sign-in that "does nothing". The two rules have to agree,
    /// which is what this pins.
    #[test]
    fn an_unanswerable_form_cannot_be_submitted() {
        let full = LoginScreen {
            server: "cartopolis.org".into(),
            username: "jeroen".into(),
            ..LoginScreen::default()
        };
        assert!(full.answerable());

        for (server, user) in [
            ("", "jeroen"),
            ("   ", "jeroen"),
            ("cartopolis.org", ""),
            ("cartopolis.org", "  "),
            ("", ""),
        ] {
            let mut screen = LoginScreen {
                server: server.into(),
                username: user.into(),
                ..LoginScreen::default()
            };
            assert!(
                !screen.answerable(),
                "({server:?}, {user:?}) claimed to be answerable"
            );
            assert!(
                screen.build_request().is_none(),
                "({server:?}, {user:?}) built a request the button is disabled for"
            );
        }
    }

    /// The Server hint may not be a string a *filled* field could hold. egui
    /// paints it in `weak_text_color`, which is 15 % under body text here — not
    /// a difference that reads as "empty" — so a bare `cartopolis.org` made an
    /// untouched field look like the right answer already typed.
    #[test]
    fn the_server_hint_reads_as_an_example() {
        // The hint the field is built with, kept in step by hand with the
        // `field(ui, "Server", ...)` call above.
        let hint = "e.g. cartopolis.org";
        assert!(
            normalize_server(hint) != "cartopolis.org",
            "the hint is still something a filled Server field could contain"
        );
    }

    /// A fresh install starts offline and is never asked about it — the card is
    /// only ever opened from the app menu.
    #[test]
    fn a_fresh_install_starts_offline() {
        assert_eq!(SessionPrefs::default().kind, SessionKind::Offline);
        assert!(SessionPrefs::default().auto_enter);
    }

    /// The card submits a *server* session or nothing at all: there is no mode to
    /// pick any more, so an empty field is the only thing that can refuse.
    #[test]
    fn the_card_only_ever_asks_for_a_server() {
        let mut screen = LoginScreen::default();
        assert!(screen.build_request().is_none(), "nothing typed yet");
        screen.server = "cartopolis.org".into();
        assert!(screen.build_request().is_none(), "no account name");
        screen.username = "  alice ".into();
        screen.password = "hunter2".into();
        let request = screen.build_request().expect("a complete form");
        assert!(matches!(
            request,
            SessionRequest::Remote { ref server, .. } if server == "cartopolis.org"
        ));
        assert!(
            screen.password.is_empty(),
            "the password goes with the request and is not kept"
        );
        assert_eq!(screen.username, "alice", "the trimmed name is kept");
    }

    /// The card has to end up in the band the keyboard leaves — this is the one
    /// card in the app that exists to be typed into, and it was centred on the
    /// full viewport with the slab over its password field and its submit
    /// button.
    #[test]
    fn the_card_takes_the_band_the_keyboard_leaves() {
        let viewport = egui::vec2(274.0, 610.0);
        // No keyboard: the whole screen, and no shift at all.
        let all = free_band(viewport, None);
        assert_eq!(all.height(), 610.0);
        assert!((all.center().y - viewport.y * 0.5).abs() < 1e-3);

        // Docked to the bottom (the usual case — a field in the upper half).
        let bottom = egui::Rect::from_min_max(egui::pos2(0.0, 380.0), egui::pos2(274.0, 610.0));
        let above = free_band(viewport, Some(bottom));
        assert_eq!(above.max.y, 380.0, "the band must stop at the slab");
        assert!(
            above.center().y < viewport.y * 0.5,
            "the card moves up, not down"
        );

        // Docked to the top: `layout` flips it when the focused field is low, and
        // a rule that only ever lifted would then push the card *into* it.
        let top = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(274.0, 230.0));
        let below = free_band(viewport, Some(top));
        assert_eq!(below.min.y, 230.0);
        assert!(below.center().y > viewport.y * 0.5, "the card moves down");

        // Neither band is usable: fall back to the screen rather than to a card
        // squeezed to nothing.
        let everything = egui::Rect::from_min_size(egui::Pos2::ZERO, viewport);
        assert_eq!(free_band(viewport, Some(everything)).height(), 610.0);
    }
}
