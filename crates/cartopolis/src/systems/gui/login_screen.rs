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
//! now reports itself in the menu it was started from (`ui::drive_main_menu`),
//! next to the entry that offers to try again. A failure on a card that is still
//! open still lands on that card: the question is up, so the answer belongs there.
//!
//! Leaving is likewise the menu's, not this card's (`session::sign_out`): one
//! entry, one outcome, and no modal to dismiss afterwards.
//!
//! The component library draws the card (`systems::lib_login`) and takes its
//! presses through this file's [`LoginScreen`] methods; [`drive_login_screen`]
//! closes it on a live session, says whether it owns the screen
//! ([`LoginScreen::modal`]) and collects a sign-in the browser finished.

use bevy::prelude::*;
use cartopolis_core::AuthCredential;

use crate::resources::AuthIdentity;
use crate::systems::session::{
    can_simulate, SessionKind, SessionPrefs, SessionRequest, SessionState, SessionStatus,
};
use crate::systems::user_store::UserStore;

/// Default port of a `simulator`, appended when the user types a bare host.
const DEFAULT_PORT: u16 = 4747;

/// The server a fresh install offers to sign in to.
pub const DEFAULT_SERVER: &str = "cartopolis.org";

#[derive(Resource)]
pub struct LoginScreen {
    pub open: bool,
    pub(crate) server: String,
    pub(crate) username: String,
    pub(crate) password: String,
    /// Keep this server's session token, so it stops asking for the password —
    /// and, since a remembered session is exactly one that can be entered without
    /// asking, the same tick is what `SessionPrefs::auto_enter` gets. They were
    /// two checkboxes ("Remember me on this device" / "Enter this way next time")
    /// and the combination that made them different — remember the token but ask
    /// anyway, or auto-enter with no token, which fails — is not one anybody wants.
    pub(crate) remember: bool,
    /// A request is in flight and this card is waiting to see how it lands.
    awaiting: bool,
    /// The Server field is open. It folds to one line naming the server once
    /// there is one, since almost everybody signs in to [`DEFAULT_SERVER`]
    /// and a field asking for it was the card's first question.
    pub(crate) edit_server: bool,
    /// Signing in with Discord, Google and the rest, through the browser
    /// (`external_signin`): what the server offers, and a round trip out.
    pub(crate) external: crate::systems::external_signin::External,
    /// The card is up and owns the screen: open, and not waiting behind the
    /// boot splash. The game keys yield to it ([`Self::modal`]).
    modal: bool,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self {
            open: false,
            server: String::new(),
            username: String::new(),
            password: String::new(),
            remember: true,
            awaiting: false,
            edit_server: false,
            external: Default::default(),
            modal: false,
        }
    }
}

impl LoginScreen {
    /// Does the card own the screen this frame — open, and not waiting behind
    /// the boot splash? A modal: the game keys yield to it even with no field
    /// focused (`UiInput::ui_owns_keyboard`).
    #[must_use]
    pub fn modal(&self) -> bool {
        self.modal
    }

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
            // A fresh install signs in to the public server unless told
            // otherwise: an empty field here was a dead end for anyone who did
            // not already know a server name (`docs/mockups/ux-review`).
            server: if prefs.server.trim().is_empty() {
                DEFAULT_SERVER.to_string()
            } else {
                prefs.server.clone()
            },
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
    pub(crate) fn answerable(&self) -> bool {
        !normalize_server(&self.server).is_empty()
            && !self.username.trim().is_empty()
            && can_simulate()
    }

    /// The field still unanswered, named under a disabled button: the card
    /// asks two questions and this says which one is left.
    #[must_use]
    pub(crate) fn missing(&self) -> Option<&'static str> {
        if self.answerable() {
            None
        } else if normalize_server(&self.server).is_empty() {
            Some("Enter the server to sign in to")
        } else {
            Some("Enter your account name")
        }
    }

    /// The server as the foot of the card names it, or `None` when there is
    /// none yet and the field is the way to give one.
    #[must_use]
    pub(crate) fn server_shown(&self) -> Option<String> {
        if normalize_server(&self.server).is_empty() {
            None
        } else {
            Some(self.server.trim().to_string())
        }
    }

    /// Ask for the session the form describes — the press of Sign in, or
    /// Enter. Writes the remembered prefs and stays open waiting (`settle`
    /// closes the card the frame the session comes up).
    /// `false` when the form is not answerable or a session is under way.
    pub(crate) fn submit(&mut self, store: &mut UserStore, session: &mut SessionState) -> bool {
        if session.status.is_busy() {
            return false;
        }
        let Some(request) = self.build_request() else {
            return false;
        };
        // Says the request left the card, which is the one step between the
        // button and `network::start_link` that used to have no evidence at
        // all: a sign-in that produced nothing could equally have been a
        // press that never reached the button, a form the card refused, or a
        // socket that failed — and the log said the same thing (nothing) for
        // all three.
        info!(server = %self.server, user = %self.username, "sign-in requested");
        store.set_session_prefs(self.prefs());
        session.request(request);
        // Stay open while it connects: the spinner, and any rejection, belong
        // on this card.
        self.awaiting = true;
        true
    }

    /// Start signing in with one of the providers the server offers.
    pub(crate) fn choose_external(&mut self, provider: crate::systems::external_signin::Offered) {
        if let Some(base) = self.api_base() {
            self.external.start(&base, provider);
        }
    }

    /// A request that came back offline is no longer awaited: its reason is
    /// on the card in front of the person who asked.
    pub(crate) fn settle_awaiting(&mut self, session: &SessionState) {
        if self.awaiting && !session.status.is_busy() {
            self.awaiting = false;
        }
    }

    /// The ✕ or the backdrop: closes the card, unless a connection is in
    /// flight — closing would hide the only place the outcome is reported.
    pub(crate) fn dismiss(&mut self, session: &SessionState) {
        if !self.awaiting && !session.status.is_busy() {
            self.open = false;
        }
    }

    /// The server's HTTP address, for the other ways in it offers: `None` with
    /// no server typed, or a dev sim on this machine, whose HTTP port nothing
    /// here knows (`model_assets::derive_api_base`).
    fn api_base(&self) -> Option<String> {
        let server = normalize_server(&self.server);
        if server.is_empty() {
            return None;
        }
        crate::systems::model_assets::derive_api_base(&server)
    }

    /// Where the server signs people up — its web page's `/register` — or
    /// `None` where there is no web page to send anybody to (a dev simulator)
    /// or no way to open one.
    #[must_use]
    pub fn register_url(&self) -> Option<String> {
        if !crate::systems::data_layers::open_url_supported() {
            return None;
        }
        self.api_base().map(|base| format!("{base}/register"))
    }

    /// The request for a session the browser's round trip left for the app.
    fn token_request(&mut self, token: String, username: String) -> Option<SessionRequest> {
        let server = normalize_server(&self.server);
        if server.is_empty() || token.is_empty() || !can_simulate() {
            return None;
        }
        self.server = server.clone();
        self.username = username;
        Some(SessionRequest::Remote {
            server,
            credential: AuthCredential::Token(token),
            persist_token: self.remember,
        })
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
    if portless && crate::platform::http::is_loopback_host(host) {
        format!("{host}:{DEFAULT_PORT}")
    } else {
        host.to_string()
    }
}

/// Close the card on a live session, say whether it owns the screen, and
/// collect a sign-in the browser finished. The card itself, its presses and
/// letting go of `awaiting` are the library's (`systems::lib_login`).
pub fn drive_login_screen(
    mut screen: ResMut<LoginScreen>,
    mut session: ResMut<SessionState>,
    mut store: ResMut<UserStore>,
    time: Res<Time<Real>>,
    splash: Res<crate::systems::splash::BootSplash>,
    mut input: ResMut<crate::systems::ui_input::UiInput>,
) {
    // **A live session closes this card, whoever opened it and whoever started the
    // sign-in.** Not only the request this card issued: a startup auto-enter that
    // lands, or a `--connect` that comes up while the card is up, has answered the
    // one question the card exists to ask, and a form still asking it over a world
    // you are already in is a modal with nothing to submit. Tested by
    // `an_online_session_closes_the_card`.
    screen.settle(&session);

    // Tell the keyboard gate before anything can early-out, so the flag falls
    // back to false the frame the screen closes. Behind the boot splash the
    // card waits rather than drawing over it: both show a logo and a wordmark,
    // and the splash is already the screen that says "not yet".
    let modal = screen.open && !splash.covering();
    if screen.modal != modal {
        screen.modal = modal;
    }
    // …and the game keys, which yield to it (`UiInput::ui_owns_keyboard`):
    // the card has the user's attention even with no field focused.
    if input.modal() != screen.modal() {
        input.set_modal(screen.modal());
    }
    if !modal {
        return;
    }
    // The other ways in follow the server field, and a sign-in handed to the
    // browser is collected here on whichever frame the browser finishes it.
    let base = screen.api_base();
    screen.external.follow(base);
    if let Some(got) = screen.external.tick(time.elapsed_secs_f64()) {
        if let Some(request) = screen.token_request(got.token.clone(), got.username) {
            info!(server = %screen.server, user = %screen.username, "sign-in collected from the browser");
            // Over any token kept for this server: `connect` tries the kept one
            // first, and it may be somebody else's.
            if screen.remember {
                store.set_session_token(&screen.server, &got.token);
            }
            store.set_session_prefs(screen.prefs());
            session.request(request);
            screen.awaiting = true;
        }
    }
}

/// The status line's dot, by what it says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StatusDot {
    Online,
    Offline,
    Busy,
}

/// What the status line says.
pub(crate) fn status_of(session: &SessionState, identity: &AuthIdentity) -> (StatusDot, String) {
    match &session.status {
        SessionStatus::Online => (
            StatusDot::Online,
            match identity.username.as_deref() {
                Some(user) => format!("{user} · {}", session.mode.where_()),
                None => session.mode.where_(),
            },
        ),
        SessionStatus::Offline => (StatusDot::Offline, "Not signed in".to_string()),
        busy => (StatusDot::Busy, busy.label().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::network;

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

    /// The Server hint may not be a string a *filled* field could hold. A
    /// placeholder is painted dim, not dim enough to read as "empty", so a bare
    /// `cartopolis.org` made an untouched field look like the right answer
    /// already typed.
    #[test]
    fn the_server_hint_reads_as_an_example() {
        // The hint the field is built with, kept in step by hand with the
        // Server field's `placeholder` in `lib_login`.
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

    /// **The open card stops the game keys, with no field focused.** egui's
    /// card said so through `EguiManager::set_modal`; the library's says it
    /// through `UiInput`, written by [`drive_login_screen`] in `Input`, before
    /// anything that walks reads the keys. Behind the boot splash the card
    /// waits, and so do the keys' owners: the splash is not a modal.
    #[test]
    fn the_open_card_owns_the_game_keys() {
        use crate::systems::splash::BootSplash;
        use crate::systems::ui_input::UiInput;
        use crate::systems::user_store::UserStore;

        let mut app = App::new();
        app.init_resource::<SessionState>()
            .init_resource::<Time<Real>>()
            .init_resource::<UserStore>()
            .init_resource::<UiInput>()
            .insert_resource(BootSplash::gone())
            .insert_resource(LoginScreen {
                open: true,
                ..LoginScreen::default()
            })
            .add_systems(Update, drive_login_screen);
        let keys = ButtonInput::<KeyCode>::default();
        let owns = |app: &App| {
            app.world()
                .resource::<UiInput>()
                .ui_owns_keyboard(false, &keys)
        };

        app.update();
        assert!(owns(&app), "WASD would walk the avatar behind the card");

        app.world_mut().resource_mut::<LoginScreen>().open = false;
        app.update();
        assert!(!owns(&app), "the closed card kept the keys");

        app.insert_resource(BootSplash::default());
        app.world_mut().resource_mut::<LoginScreen>().open = true;
        app.update();
        assert!(
            !owns(&app),
            "a card waiting behind the splash took the keys"
        );
    }
}
