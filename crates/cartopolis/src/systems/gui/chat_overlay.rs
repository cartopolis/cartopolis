//! Chat over the map: a bubble above whoever spoke, an ambient transcript that
//! fades, and a composer that only exists while you are writing.
//!
//! **The panel was the problem.** On a compact viewport a right panel is the
//! whole screen (`egui_manager::responsive_right_panel`), so in a client whose
//! Local channel is scoped by *how far away someone is standing*, saying
//! anything meant covering the world and the people in it. This module is chat
//! with no panel at all: three floating `Area`s, none of which is up unless it
//! has something to say. `docs/notes/chat-without-a-panel.md` has the design and
//! what it replaced; [`crate::systems::chat`] keeps the state and the log.
//!
//! ## The three pieces
//!
//! **[`render_chat_bubbles`]** puts a Local line over the head of the person who
//! said it. `NetMessage::ChatMessage` already carries `sender_id`, which is the
//! same number as `RemoteAvatar::sim_id`, so the line finds its entity with no
//! name matching and no protocol change. This is also the first thing in the
//! client that identifies a nearby person at all — there are no nameplates.
//!
//! **[`render_chat_transcript`]** is the fallback and the record: the last few
//! lines from every channel merged, in the bottom-left corner, faded out after
//! [`HOLD_SECS`]. It carries Global and rooms (which have no avatar to hang on)
//! and the Local lines whose speaker is off-screen.
//!
//! **[`render_chat_composer`]** is a pill, not a bar: the scope chip, the field
//! and Send, above the keyboard, and gone the moment you are done.
//!
//! ## Four things about it are load-bearing
//!
//! **The transcript is `interactable(false)` unless the composer is open.** That
//! is what makes this panel-less rather than a small panel: egui paints floating
//! `Area`s above everything, and an `Area` that is merely *invisible* still eats
//! the taps aimed at the map under it — the trap `place_pins` culls for and
//! `map_osd` hides for. So the rule is "you can act on chat while you are in the
//! conversation, and the map has the taps the rest of the time"; the durable
//! path to Block and Report is the panel, which is always there.
//!
//! **A bubble is painted, never interactive**, for the sharper version of the
//! same reason: it sits *over the world*, where a tap means pick, walk or place,
//! and a chat bubble is not allowed to be in the way of that.
//!
//! **The composer reserves the keyboard's height before the keyboard exists.**
//! `onscreen_keyboard::layout` picks its edge from where the focused field
//! landed *this* frame, and flips to the **top** when the bottom slab would
//! cover it. A composer that waited to be told would spend its first frame at
//! the bottom, get covered, and push the keyboard onto the top edge — over the
//! transcript — for the rest of the session. So while the composer is open on a
//! platform with no IME it clears [`onscreen_keyboard::expected_height`]
//! unconditionally, whether the keys are up or not. This is the thing the panel
//! could not do at all: by the time the keyboard runs, a panel has already
//! composed (that module's note says so), and an `Area` can still move.
//!
//! **Everything ages on [`ChatState::clock`], not on `sent_at_ms`.** See that
//! field: a fade is about how long *this screen* has shown a line.

use crate::systems::egui_manager::EguiMut;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{person_menu, ChatState, PersonAction, ReportDraft};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
use crate::systems::onscreen_keyboard;
use crate::systems::theme;
use cartopolis_core::{NetMessage, ReportReason};

/// How long a line stays at full strength before it starts to go.
const HOLD_SECS: f64 = 10.0;
/// And how long the fade itself takes. Slow on purpose: a line that snaps out
/// reads as a glitch, and the whole point of an ambient transcript is that
/// nothing about it demands attention.
const FADE_SECS: f64 = 4.0;
/// Lines the transcript shows at rest. Four is about what can be read at a
/// glance without becoming a wall of text over a map.
const TRANSCRIPT_LINES: usize = 4;
/// And while the composer is open, when chat is what you are doing.
const TRANSCRIPT_LINES_OPEN: usize = 8;
/// Widest the transcript column gets, in points. Fixed rather than sized to
/// content, for the reason `hud`'s module note gives at length: a size-to-content
/// `Area` whose contents reach for the available width grows a little every
/// frame until it has swallowed the screen.
const TRANSCRIPT_WIDTH: f32 = 360.0;

/// The transcript column on a viewport `viewport_w` points wide: the whole
/// width less the margins on a phone, capped on anything wider.
#[must_use]
fn transcript_width(viewport_w: f32) -> f32 {
    (viewport_w - 2.0 * theme::OSD_MARGIN).clamp(160.0, TRANSCRIPT_WIDTH)
}

/// The size of a line's words, as the messenger's thread sets them.
const BODY_PT: f32 = 14.5;
/// A speaker's name over their run, and the chat tag beside it.
const NAME_PT: f32 = 12.5;
/// The round initial beside someone else's run.
const FACE_PT: f32 = 26.0;

/// A bubble hangs this far above the avatar's feet, in metres. The MakeHuman
/// bodies stand origin-at-feet (`human_avatar`) and the tallest the designer
/// makes is about 2 m, so this clears the top of the head by a little.
const BUBBLE_HEIGHT_M: f64 = 2.15;
/// Bubbles hold for longer than they fade, and shorter than the transcript: the
/// transcript is the record, a bubble is the moment.
const BUBBLE_HOLD_SECS: f64 = 7.0;
const BUBBLE_FADE_SECS: f64 = 1.5;
/// At most this many lines per speaker. A bubble is a speech balloon, not a
/// scrollback — and without a cap one person can paper over the view.
const BUBBLE_LINES: usize = 3;
/// Widest a bubble gets, in points.
const BUBBLE_WIDTH: f32 = 200.0;
/// How far above a head a balloon reaches, roughly: a name and two lines.
const BUBBLE_REACH: f32 = 90.0;

/// Widest the composer pill gets. Beyond this a desktop window would put a
/// single-line field across a metre of screen.
const COMPOSER_MAX_WIDTH: f32 = 520.0;

/// Is chat on screen at all, and is the composer open?
#[derive(Resource, Default)]
pub struct ChatOverlay {
    /// The composer pill is up.
    pub composing: bool,
    /// Take the keyboard on the next frame the field is drawn. Set when the
    /// composer opens and when a message is sent (so a reply needs no second
    /// tap), and cleared once claimed.
    claim_focus: bool,
    /// Point the scope chip at the conversation on screen, once, when the
    /// composer opens. Without it the chip kept whatever it was last left on —
    /// Here, usually — while every line above it was Everyone, so the obvious
    /// reply went to nobody nearby.
    follow: bool,
    /// Where the transcript was drawn last frame, so a bubble over a head
    /// does not land on it — the two drawn over each other were the same
    /// words twice in a tangle.
    transcript: Option<egui::Rect>,
    /// `--ui chat` seeded this run and there is no session behind it.
    ///
    /// The only thing it unlocks is the [`suppressed`] test for a session —
    /// which is otherwise the difference between a feature that can be
    /// screenshotted before it is merged and one that cannot. Every scripted
    /// capture is offline (`--shot` implies it), so without this the overlay is
    /// literally unphotographable in the harness the repo checks its UI with.
    /// Set nowhere but `shot_harness::open_scripted_ui`.
    pub demo: bool,
}

impl ChatOverlay {
    /// Open the composer and put the caret in it.
    pub fn open(&mut self) {
        self.composing = true;
        self.claim_focus = true;
        self.follow = true;
    }

    /// Put the caret back in the field on the next frame it is drawn.
    pub fn reclaim(&mut self) {
        self.claim_focus = true;
    }

    /// Whether the field should take the keyboard this frame; asks once. For
    /// a composer drawn elsewhere: a chat's World view draws its own field
    /// for this button (`chats::render_chat_world`).
    pub fn take_claim(&mut self) -> bool {
        std::mem::take(&mut self.claim_focus)
    }

    /// Close it, keeping whatever was typed — a dismissal is not a delete.
    pub fn close(&mut self) {
        self.composing = false;
        self.claim_focus = false;
        self.follow = false;
    }
}

/// Lines one person said back to back in one chat, drawn as one box under one
/// name. Eight lines from two people used to be eight boxes and eight
/// `[Global] name:` prefixes — a wall of chrome around very few words.
pub struct Run {
    /// The chat's key (`local`, `geo:`, a private chat's).
    pub key: String,
    /// The chat as the messenger names it: Here, Everyone, a person.
    pub chat: String,
    pub sender: String,
    pub mine: bool,
    pub lines: Vec<crate::systems::chat::ChatLine>,
}

/// Fold `(key, chat name, line)` in arrival order into [`Run`]s.
#[must_use]
pub fn runs(
    lines: &[(String, String, crate::systems::chat::ChatLine)],
    mine: impl Fn(&crate::systems::chat::ChatLine) -> bool,
) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (key, chat, line) in lines {
        let is_mine = mine(line);
        match out.last_mut() {
            Some(run)
                if run.key == *key
                    && run.mine == is_mine
                    && (is_mine || run.sender.eq_ignore_ascii_case(&line.sender)) =>
            {
                run.lines.push(line.clone());
            }
            _ => out.push(Run {
                key: key.clone(),
                chat: chat.clone(),
                sender: line.sender.clone(),
                mine: is_mine,
                lines: vec![line.clone()],
            }),
        }
    }
    out
}

/// The chat the composer should answer in when it opens: the one the newest
/// line on screen came from, if anything is on screen at all.
#[must_use]
fn conversation_on_screen(chat: &ChatState) -> Option<usize> {
    let (channel, _) = chat
        .recent(chat.clock, HOLD_SECS + FADE_SECS, 1)
        .into_iter()
        .next()?;
    chat.channels.iter().position(|c| c.key == channel.key)
}

/// What the empty field says: where the words will go, so the chip beside it
/// is a confirmation rather than something to decode.
#[must_use]
fn hint_for(key: &str, chat: &str, shout: bool) -> String {
    use crate::systems::chats::{kind_of, ChatKind};
    match kind_of(key) {
        ChatKind::Here if shout => "Shout to the street…".into(),
        ChatKind::Here => "Say something nearby…".into(),
        ChatKind::Everyone => "Message everyone…".into(),
        _ => format!("Message {chat}…"),
    }
}

/// How visible a line of age `age` is: 1 while it is held, ramping to 0 over the
/// fade, and 0 once it is gone.
///
/// Pure, and tested, because "the transcript never goes away" and "the
/// transcript vanishes instantly" are the same one-character mistake and neither
/// is visible in a still.
#[must_use]
pub fn fade(age: f64, hold: f64, fade_over: f64) -> f32 {
    if age <= hold {
        return 1.0;
    }
    if fade_over <= 0.0 {
        return 0.0;
    }
    (1.0 - (age - hold) / fade_over).clamp(0.0, 1.0) as f32
}

/// A speaker spawned by `--ui chat`, so the next shot can despawn it.
#[derive(Component)]
pub struct ScriptedSpeaker;

/// Seed the overlay for a scripted capture: a short exchange, one demo speaker
/// standing beside the avatar, and the composer open.
///
/// The counterpart of `bookmarks::seed_demo_places` and for the same reason it
/// exists — a scripted run has no server, so the thing the feature is *for*
/// would otherwise photograph as an empty screen.
///
/// One of the two speakers is **the local avatar**: `my_id` is claimed for it, so
/// its bubble needs no entity that a `--offline` run does not already have. The
/// other is spawned, which is what makes a *conversation* rather than a
/// monologue visible in one frame.
pub fn seed_demo_chat(chat: &mut ChatState, overlay: &mut ChatOverlay) {
    const ME: u64 = 1;
    const THEM: u64 = 2;
    chat.my_id = Some(ME);
    if chat.channels.iter().all(|c| c.lines.is_empty()) {
        chat.receive(
            "local",
            THEM,
            "Wieke".into(),
            "Are you coming down to the Vismarkt?".into(),
            Some(6.0),
        );
        chat.receive(
            "local",
            THEM,
            "Wieke".into(),
            "The stroopwafel stall is open".into(),
            Some(6.0),
        );
        chat.receive("local", ME, "You".into(), "On my way".into(), Some(0.0));
        chat.receive(
            "geo:",
            9,
            "Bram".into(),
            "Anyone else seeing rain over Assen?".into(),
            None,
        );
    }
    overlay.demo = true;
    overlay.open();
}

/// Where the seeded speaker stands, relative to the avatar: a few metres away
/// and slightly to one side, so their bubble does not sit on top of yours.
pub const DEMO_SPEAKER_OFFSET: Vec3 = Vec3::new(2.5, 0.0, -1.5);

/// Advance the clock every line is aged against.
///
/// Its own system, and deliberately not folded into one of the drawing ones:
/// those return early in half a dozen places (no session, driver mode, a panel
/// covering the screen), and a clock that stops whenever nothing is drawn would
/// make every line already on screen immortal.
pub fn advance_chat_clock(time: Res<Time>, mut chat: ResMut<ChatState>) {
    chat.clock = time.elapsed_secs_f64();
}

/// Whether the overlay may draw at all this frame.
///
/// One predicate for the three pieces so they cannot disagree — the state that
/// must not exist is "bubbles up, composer gone", a chat you can read and cannot
/// answer. **The keyboard is deliberately not one of the tests**: each piece has
/// its own answer to it (the composer sits above the slab, the bubbles cull
/// against it, the transcript stands down unless it is the composer's), and
/// folding it in here would take the transcript away from under the reply being
/// written into it.
#[must_use]
fn suppressed(online: bool, driving: bool, compact: bool, panel_open: bool) -> bool {
    if !online {
        return true;
    }
    // Driver mode owns the screen and strips the map to the route; a speech
    // bubble over a motorway is the same judgement `place_pins` already made
    // about its teardrops.
    if driving {
        return true;
    }
    // A panel on a compact viewport *is* the screen, and these are floating
    // `Area`s, which egui paints above panels — the dock's rule verbatim. On a
    // wide viewport the panel docks right and the map area is real, so the
    // overlay stays and culls itself against it.
    compact && panel_open
}

/// Subtract the on-screen keyboard from a map area.
///
/// Culling, not clipping, and the reason is `place_pins`'s verbatim: the slab is
/// a floating `Area` and reserves nothing in the root `Ui`, so
/// `available_rect_before_wrap` cannot know about it.
#[must_use]
fn clear_of_keyboard(mut rect: egui::Rect, slab: Option<egui::Rect>) -> egui::Rect {
    let Some(slab) = slab else { return rect };
    if slab.top() > rect.top() {
        rect.max.y = rect.max.y.min(slab.top());
    } else {
        rect.min.y = rect.min.y.max(slab.bottom());
    }
    rect
}

// ─── The bubbles ──────────────────────────────────────────────────────────────

/// Draw each speaker's recent Local lines over their head.
///
/// Projected exactly the way `place_pins` projects a saved place, and for the
/// same two reasons: the camera basis is rebuilt from **this** frame's
/// `Transform` (big_space propagates in `PostUpdate`, so the `GlobalTransform`
/// in the world is the previous frame's, and a bubble built from it swims over
/// an avatar that is standing still), and the position is taken as a
/// camera-relative delta in f64 so the f32 projection never has to cancel the
/// camera's own offset back out.
pub fn render_chat_bubbles(
    mut egui: EguiMut,
    chat: Res<ChatState>,
    overlay: Res<ChatOverlay>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    remotes: Query<(&RemoteAvatar, &Transform), Without<FreeCamera>>,
    local: Query<(&GridCell, &Transform), (With<Avatar>, Without<FreeCamera>)>,
) {
    // Redrawn on the frames the speakers or the camera move, as the pins are.
    let mut anchored = crate::systems::egui_manager::WorldAnchored::new(egui.ctx(), "chat_bubbles");
    let compact = egui.compact();
    if suppressed(
        online.is_some() || overlay.demo,
        driver.active,
        compact,
        !right_panel.is(RightPanel::None),
    ) {
        return;
    }
    let Ok((cam, cam_cell, cam_tf)) = camera.single() else {
        return;
    };
    // Read off the shared root `Ui` before borrowing the context, so the two
    // borrows of the manager don't overlap. The keyboard is subtracted by hand
    // because it reserves nothing in that `Ui`; bubbles stay up while the
    // composer is open, which is the point — you can see who you are answering.
    let map_rect = clear_of_keyboard(egui.ui_mut().available_rect_before_wrap(), keyboard.slab());
    if map_rect.width() < BUBBLE_WIDTH || map_rect.height() < 3.0 * theme::SIZE_BODY {
        return;
    }
    let ui_scale = egui.ui_scale();
    let ctx = egui.ctx_mut().clone();

    let now = chat.clock;
    let live = chat.recent(now, BUBBLE_HOLD_SECS + BUBBLE_FADE_SECS, 64);
    // Only proximity chat hangs on a person: a room line has no distance and its
    // speaker may be on the other side of the world.
    let mut said: std::collections::HashMap<u64, Vec<&crate::systems::chat::ChatLine>> =
        std::collections::HashMap::new();
    for (channel, line) in live {
        if channel.key != "local" || line.sender_id == 0 {
            continue;
        }
        let per = said.entry(line.sender_id).or_default();
        per.push(line);
        if per.len() > BUBBLE_LINES {
            per.remove(0);
        }
    }
    if said.is_empty() {
        return;
    }

    let cell_world =
        |cell: &GridCell, tf: &Transform| crate::grid::anchored_position_f64(cell, tf.translation);
    let cam_world = cell_world(cam_cell, cam_tf);
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));

    // Where every speaker on screen is standing. Remote avatars are spawned as
    // plain root entities (`network::sync_remote_avatar_entities`), so their
    // translation is already the absolute sim position; the local avatar is in
    // the grid (`local_avatar::attach_avatar_to_bigspace`) and needs its cell added.
    //
    // Both are read as absolute metres and differenced against the camera, which
    // is exact while the cells are near zero — where an online session lives,
    // the sim world being a region about the origin. It would stop being exact
    // if a remote avatar were ever a cell away, and so would the avatar's own
    // rendering, which is outside the grid entirely: the bubble is wrong exactly
    // when the person it points at is, which is the right way round.
    let mut heads: Vec<(u64, DVec3)> = remotes
        .iter()
        .filter(|(r, _)| said.contains_key(&r.sim_id))
        .map(|(r, tf)| (r.sim_id, tf.translation.as_dvec3()))
        .collect();
    if let (Some(mine), Ok((cell, tf))) = (chat.my_id, local.single()) {
        if said.contains_key(&mine) {
            heads.push((mine, cell_world(cell, tf)));
        }
    }

    for (id, foot) in heads {
        let Some(lines) = said.get(&id) else { continue };
        let delta = foot + DVec3::Y * BUBBLE_HEIGHT_M - cam_world;
        let Ok(screen) = cam.world_to_viewport(&cam_view, delta.as_vec3()) else {
            continue; // behind the camera
        };
        let head = egui::pos2(screen.x / ui_scale, screen.y / ui_scale);
        if !map_rect.contains(head) {
            continue;
        }
        // The transcript carries the same words; a balloon that would land on
        // it is left out rather than drawn over it — the two in a tangle were
        // the worst of both. Last frame's rect, as the transcript draws after
        // this; its size changes by a line at a time.
        if let Some(transcript) = overlay.transcript {
            let reach = egui::Rect::from_min_max(
                egui::pos2(head.x - BUBBLE_WIDTH / 2.0, head.y - BUBBLE_REACH),
                egui::pos2(head.x + BUBBLE_WIDTH / 2.0, head.y),
            );
            if reach.intersects(transcript) {
                continue;
            }
        }
        // The newest line decides the whole bubble's opacity: a balloon whose
        // top half is fading while its bottom half is fresh reads as a rendering
        // fault, not as age.
        let youngest = lines
            .iter()
            .map(|l| now - l.arrived)
            .fold(f64::MAX, f64::min);
        let alpha = fade(youngest, BUBBLE_HOLD_SECS, BUBBLE_FADE_SECS);
        if alpha <= 0.0 {
            continue;
        }
        let speaker = lines.last().map(|l| l.sender.clone()).unwrap_or_default();
        let bodies: Vec<String> = lines.iter().map(|l| l.body.clone()).collect();
        anchored.at(head);
        bubble(&ctx, map_rect, id, head, alpha, &speaker, &bodies);
    }
}

/// One speech balloon, bottom edge `BUBBLE_GAP` above `head`.
///
/// **Not interactive.** `interactable(false)` is the whole contract: this floats
/// over the world, where a tap means pick a thing, walk somewhere or drop a pin,
/// and egui would otherwise hand every one of those to a chat bubble that
/// happened to be in front of it.
fn bubble(
    ctx: &egui::Context,
    map_rect: egui::Rect,
    id: u64,
    head: egui::Pos2,
    alpha: f32,
    speaker: &str,
    bodies: &[String],
) {
    /// Gap between the head and the bottom of the balloon, in points.
    const BUBBLE_GAP: f32 = 10.0;
    egui::Area::new(egui::Id::new(("chat_bubble", id)))
        .order(egui::Order::Middle)
        .interactable(false)
        // The balloon grows downwards from wherever it is placed, and it grows
        // with the number of lines — so it is *pivoted* on its bottom centre
        // rather than positioned by its top-left, which is what keeps its tail on
        // the speaker while the box above it changes size. Without this a
        // three-line bubble hangs off the avatar's chin.
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(head.x, head.y - BUBBLE_GAP))
        // To the map area, not the viewport: a bubble nudged back on screen by
        // the default constraint would be nudged onto a docked side panel.
        .constrain_to(map_rect)
        .show(ctx, |ui| {
            ui.set_opacity(alpha);
            ui.set_max_width(BUBBLE_WIDTH);
            // The transcript's glass bubble, and the speaker's name in the
            // colour their initial has there, so the balloon and the line it
            // also appears as read as one person.
            let tone = crate::systems::chats::tone_for(speaker);
            let frame = egui::Frame::NONE
                .fill(theme::GLASS)
                .stroke(egui::Stroke::new(1.0, crate::systems::chats::GLINE))
                .corner_radius(egui::CornerRadius::same(14))
                .inner_margin(egui::Margin::symmetric(11, 7))
                .shadow(lift())
                .show(ui, |ui| {
                    ui.set_max_width(BUBBLE_WIDTH);
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(speaker).size(NAME_PT).strong().color(
                            crate::systems::chats::blend(tone, egui::Color32::WHITE, 0.5),
                        ));
                        for body in bodies {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(body).size(BODY_PT).color(theme::CREAM),
                                )
                                .wrap(),
                            );
                        }
                    });
                });
            // The tail, pointing down at whoever said it.
            let r = frame.response.rect;
            let tip = egui::pos2(r.center().x, r.bottom() + 7.0);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(tip.x - 7.0, r.bottom()),
                    egui::pos2(tip.x + 7.0, r.bottom()),
                    tip,
                ],
                theme::GLASS,
                egui::Stroke::NONE,
            ));
        });
}

// ─── The transcript ───────────────────────────────────────────────────────────

/// The last few lines from every channel, bottom-left, fading.
pub fn render_chat_transcript(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut overlay: ResMut<ChatOverlay>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    camera_state: Res<crate::resources::CameraState>,
    touch: Res<crate::systems::touch::TouchGestures>,
) {
    // Where it was drawn, for the bubbles to keep clear of; nowhere until it is.
    overlay.transcript = None;
    let compact = egui.compact();
    let panel_open = !right_panel.is(RightPanel::None);
    if suppressed(
        online.is_some() || overlay.demo,
        driver.active,
        compact,
        panel_open,
    ) {
        return;
    }
    // A chat's World view (`chats::render_chat_world`) draws no lines of its
    // own: this transcript is its chat, narrowed to that one chat and without
    // the label every line would otherwise carry.
    let only = chat.world.clone();
    // The keys belong to *some* field, and unless it is the composer's there is
    // nothing here worth the half of the screen they leave. While composing the
    // transcript stays: it stacks above the pill, which stacks above the slab,
    // and reading what you are answering is the whole point.
    if keyboard.is_open() && !overlay.composing {
        return;
    }
    let now = chat.clock;
    let want = if overlay.composing {
        TRANSCRIPT_LINES_OPEN
    } else {
        TRANSCRIPT_LINES
    };
    // While composing the window is opened right up: you are in the conversation,
    // and a line fading out from under the reply you are writing is the one
    // moment ambience is wrong.
    let window = if overlay.composing {
        f64::INFINITY
    } else {
        HOLD_SECS + FADE_SECS
    };
    let mut lines: Vec<(String, String, crate::systems::chat::ChatLine)> = chat
        .recent(now, window, usize::MAX)
        .into_iter()
        .filter(|(c, _)| only.as_deref().is_none_or(|k| c.key == k))
        .map(|(c, l)| {
            (
                c.key.clone(),
                crate::systems::chats::display_name(&c.key, &c.label),
                l.clone(),
            )
        })
        .collect();
    if lines.len() > want {
        lines.drain(0..lines.len() - want);
    }
    if lines.is_empty() {
        return;
    }
    let runs = runs(&lines, |l| chat.is_mine(l));
    // Where the composer is sending, so a run in that same chat needs no tag.
    let active_key = chat.channels.get(chat.active).map(|c| c.key.clone());

    // Clear of whatever the touch controls have claimed in this corner — the
    // thumbstick is bottom-left, which is exactly where a transcript wants to be.
    let cluster = crate::systems::map_osd::cluster_for(
        camera_state.mode,
        driver.active,
        panel_open,
        keyboard.is_open(),
        theme::touch_layout() || touch.ever(),
    );
    let corner = crate::systems::map_osd::reserved_left_bottom(cluster);
    let (_, bottom) = stack_bottoms(
        overlay.composing,
        keyboard_reserve(&egui, overlay.composing),
        corner,
    );

    let interactive = overlay.composing;
    let ctx = egui.ctx_mut().clone();
    let mut block_now: Option<String> = None;
    let mut message_now: Option<String> = None;
    let mut open_report: Option<(String, Option<String>)> = None;
    let mut reply_in: Option<String> = None;

    let column = transcript_width(egui.viewport_pts().x);
    let transcript = egui::Area::new(egui::Id::new("chat_transcript"))
        .order(egui::Order::Middle)
        // The rule this module exists to keep: an `Area` that is merely faded is
        // still a live click target over the map.
        .interactable(interactive)
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -bottom),
        )
        .show(&ctx, |ui| {
            // A fixed width, not a maximum: own runs sit on the right edge of
            // the column, and a column sized to its content has no right edge
            // to put them on. Inert at rest, so the empty part takes no taps.
            ui.set_width(column);
            ui.spacing_mut().item_spacing.y = 8.0;
            for run in &runs {
                // A run fades with its newest line, as a bubble does: a box
                // whose top half is going while its bottom half is fresh reads
                // as a fault. Runs still go one at a time, oldest first, so the
                // transcript scrolls away rather than blinking out.
                let alpha = if overlay.composing {
                    1.0
                } else {
                    run.lines
                        .iter()
                        .map(|l| fade(now - l.arrived, HOLD_SECS, FADE_SECS))
                        .fold(0.0, f32::max)
                };
                if alpha <= 0.0 {
                    continue;
                }
                // Which chat a run is in, only where it is not the one being
                // answered and not Here (the default, and what the bubbles carry).
                let tag = (only.is_none()
                    && run.key != "local"
                    && !(overlay.composing && active_key.as_deref() == Some(run.key.as_str())))
                .then_some(run.chat.as_str());
                match draw_run(ui, run, alpha, interactive, tag, column) {
                    RunHit::None => {}
                    RunHit::Reply => reply_in = Some(run.key.clone()),
                    RunHit::Person(action) => match action {
                        PersonAction::Message => message_now = Some(run.sender.clone()),
                        PersonAction::Block => block_now = Some(run.sender.clone()),
                        PersonAction::Report(msg) => open_report = Some((run.sender.clone(), msg)),
                        // The transcript is public chat: no community, no
                        // moderation, and the admin section lives on the panel.
                        _ => {}
                    },
                }
            }
        });
    overlay.transcript = Some(transcript.response.rect);

    if let Some(key) = reply_in {
        if let Some(i) = chat.channels.iter().position(|c| c.key == key) {
            chat.active = i;
        }
        // The tap took focus off the field; give it back, or the keys go down
        // under the reply that was about to be typed.
        overlay.reclaim();
    }

    if let Some(name) = message_now {
        chat.private_start.push(name);
    }
    if let Some(username) = block_now {
        if let Some(session) = online.as_ref() {
            session.send(
                "set blocked",
                NetMessage::SetBlocked {
                    request_id: 0,
                    username,
                    blocked: true,
                },
            );
        }
    }
    if let Some((username, message)) = open_report {
        chat.report = Some(ReportDraft {
            username,
            message,
            reason: ReportReason::Harassment,
            note: String::new(),
            // Raised from a chat line, so it is about what somebody said rather
            // than about an object they put in the street.
            asset: None,
            community: None,
        });
    }
}

/// What a tap on a run asked for.
enum RunHit {
    None,
    /// Its words: answer in its chat.
    Reply,
    /// Its speaker's name: something from the person menu.
    Person(PersonAction),
}

/// The lift every bubble over the map has, so it reads as a thing laid on the
/// world rather than a hole cut in it.
fn lift() -> egui::Shadow {
    egui::Shadow {
        offset: [0, 2],
        blur: 10,
        spread: 0,
        color: egui::Color32::from_black_alpha(90),
    }
}

/// One run, in the messenger's thread style (`chats::bubble`): someone
/// else's on the left in glass under their round initial and their name in
/// their own colour, yours on the right in the accent, the corner nearest the
/// speaker tucked in on the last line only.
///
/// The same look as the chat you open from the dock, on purpose: these are
/// the same messages, and an overlay in a cheaper dialect of them read as a
/// debug log laid over the map.
fn draw_run(
    ui: &mut egui::Ui,
    run: &Run,
    alpha: f32,
    interactive: bool,
    tag: Option<&str>,
    column: f32,
) -> RunHit {
    use crate::systems::chats::{self, ChatKind, BUBBLE_R, TAIL_R};
    let mut hit = RunHit::None;
    let max_w = (column - FACE_PT - 8.0) * 0.86;
    let last = run.lines.len().saturating_sub(1);
    let (fill, ink) = if run.mine {
        (theme::ACCENT, theme::INK)
    } else {
        (theme::GLASS, theme::CREAM)
    };
    let tone = chats::tone_for(&run.sender);
    let bubbles = |ui: &mut egui::Ui, hit: &mut RunHit| {
        ui.spacing_mut().item_spacing.y = 2.0;
        for (i, line) in run.lines.iter().enumerate() {
            // Square-ish on the speaker's side between lines of one run, and
            // the tail only on the last, as a messenger stacks them.
            let near = |end: bool| if end { TAIL_R } else { 6 };
            let corners = if run.mine {
                egui::CornerRadius {
                    nw: BUBBLE_R,
                    sw: BUBBLE_R,
                    ne: if i == 0 { BUBBLE_R } else { 6 },
                    se: near(i == last),
                }
            } else {
                egui::CornerRadius {
                    ne: BUBBLE_R,
                    se: BUBBLE_R,
                    nw: if i == 0 { BUBBLE_R } else { 6 },
                    sw: near(i == last),
                }
            };
            let frame = egui::Frame::NONE
                .fill(fill)
                .stroke(if run.mine {
                    egui::Stroke::NONE
                } else {
                    egui::Stroke::new(1.0, chats::GLINE)
                })
                .corner_radius(corners)
                .inner_margin(egui::Margin::symmetric(12, 7))
                .shadow(lift())
                .show(ui, |ui| {
                    ui.set_max_width(max_w);
                    ui.add(
                        egui::Label::new(egui::RichText::new(&line.body).size(BODY_PT).color(ink))
                            .wrap(),
                    );
                });
            // Tapping what someone said answers in that chat: the scope chip
            // follows, so a reply to an Everyone line goes to Everyone without
            // a trip round the chip's ring. Senses only while composing, so an
            // ambient transcript leaves the map its taps.
            if interactive
                && ui
                    .interact(
                        frame.response.rect,
                        egui::Id::new(("chat_line", line.seq)),
                        egui::Sense::click(),
                    )
                    .clicked()
            {
                *hit = RunHit::Reply;
            }
        }
    };
    let name_row = |ui: &mut egui::Ui, hit: &mut RunHit| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            if run.mine {
                ui.label(
                    egui::RichText::new("You")
                        .size(NAME_PT)
                        .strong()
                        .color(theme::TEXT_DIM),
                );
            } else {
                // Their colour, lifted toward white so it reads on glass and on
                // a bright sky alike: the one thing in a column of names that is
                // theirs, as their initial is in the chat list.
                let label = egui::Label::new(
                    egui::RichText::new(&run.sender)
                        .size(NAME_PT)
                        .strong()
                        .color(chats::blend(tone, egui::Color32::WHITE, 0.5)),
                );
                if interactive {
                    // The name is the handle for what you can do *about* a
                    // person. It senses clicks only here, because an inert
                    // transcript must let the map have them.
                    let name = ui.add(label.sense(egui::Sense::click()));
                    let last_body = run.lines.last().map(|l| l.body.as_str());
                    person_menu(
                        &name,
                        &run.sender,
                        last_body,
                        &crate::systems::chat::PersonExtras::default(),
                        |action| *hit = RunHit::Person(action),
                    );
                } else {
                    ui.add(label);
                }
            }
            if let Some(tag) = tag {
                ui.label(
                    egui::RichText::new(format!("· {tag}"))
                        .size(NAME_PT)
                        .color(theme::TEXT_DIM),
                );
            }
        });
    };

    ui.scope(|ui| {
        ui.set_opacity(alpha);
        if run.mine {
            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                // Your own name only says something when the chat is not the
                // one you are typing into.
                if tag.is_some() {
                    name_row(ui, &mut hit);
                }
                bubbles(ui, &mut hit);
            });
        } else {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                chats::avatar(ui, &run.sender, ChatKind::Private, FACE_PT);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    name_row(ui, &mut hit);
                    bubbles(ui, &mut hit);
                });
            });
        }
    });
    hit
}

// ─── The composer ─────────────────────────────────────────────────────────────

/// Height of the composer pill, in points — its round buttons (38) plus the
/// pill's margin on both sides.
fn composer_height() -> f32 {
    38.0 + 2.0 * 6.0
}

/// How much of the bottom edge to leave for the on-screen keyboard.
///
/// Zero unless the composer is open on a platform with no IME — see the module
/// note on why this is *predicted* rather than measured off an open slab.
pub(crate) fn keyboard_reserve(egui: &EguiManager, composing: bool) -> f32 {
    if !composing || !onscreen_keyboard::platform_wants_keyboard() {
        return 0.0;
    }
    onscreen_keyboard::expected_height(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui.viewport_pts()),
        egui.ui_scale(),
        egui.display_scale(),
    )
}

/// Where the composer's bottom edge sits and where the transcript's does, as
/// offsets from the bottom of the viewport.
///
/// One function for both because they are a **stack**, and the two halves
/// getting their answer separately is how the transcript ends up under the pill
/// it is supposed to sit above. `corner` is what the touch controls have already
/// claimed in the bottom-left ([`crate::systems::map_osd::reserved_left_bottom`]).
#[must_use]
pub fn stack_bottoms(composing: bool, keyboard_reserve: f32, corner: f32) -> (f32, f32) {
    let composer = if keyboard_reserve > 0.0 {
        keyboard_reserve + theme::OSD_GAP
    } else {
        // Above the dock, which owns the bottom edge when there is no keyboard.
        crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN
    };
    let transcript = if composing {
        composer + composer_height() + theme::OSD_GAP
    } else {
        corner
    };
    (composer, transcript)
}

/// The composer pill: scope, field, send.
pub fn render_chat_composer(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut overlay: ResMut<ChatOverlay>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
) {
    let compact = egui.compact();
    let panel_open = !right_panel.is(RightPanel::None);
    // Note the composer does *not* consult the keyboard: it is the one surface
    // allowed over it, and it is what the keys are being typed into.
    if suppressed(
        online.is_some() || overlay.demo,
        driver.active,
        compact,
        panel_open,
    ) {
        overlay.close();
        return;
    }
    // In a chat's World view the pill is that chat's, drawn by
    // `chats::render_chat_world`: a private chat sends through the engine, not
    // through a scope this pill can cycle to.
    if !overlay.composing || chat.world.is_some() {
        return;
    }

    let viewport = egui.viewport_pts();
    let (bottom, _) = stack_bottoms(true, keyboard_reserve(&egui, true), 0.0);
    let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(COMPOSER_MAX_WIDTH);

    // The scope chip cycles rather than opening a list: Local, Global and
    // whatever rooms are joined is a short ring, and a popup over the keyboard
    // is a second thing covering the screen. The tabs it replaces were a wrapping
    // strip of text-height targets that pushed the history down as rooms
    // arrived — see `docs/notes/chat-without-a-panel.md`.
    if std::mem::take(&mut overlay.follow) {
        if let Some(i) = conversation_on_screen(&chat) {
            chat.active = i;
        }
    }
    // The messenger's names for these chats (Here, Everyone), not the wire's
    // (`Local`, `Global`): one chat, one name, wherever it is shown.
    let (scope_key, scope_name) = chat
        .channels
        .get(chat.active)
        .map(|c| {
            (
                c.key.clone(),
                crate::systems::chats::display_name(&c.key, &c.label),
            )
        })
        .unwrap_or_else(|| ("local".into(), "Here".into()));
    let is_local = scope_key == "local";
    let scope_kind = crate::systems::chats::kind_of(&scope_key);
    let hint = hint_for(&scope_key, &scope_name, chat.shout);
    let scope_count = chat.channels.len();

    let mut send_now = false;
    let mut close_now = false;
    let mut cycle_scope = false;
    let mut toggle_shout = false;
    let claim = std::mem::take(&mut overlay.claim_focus);
    let shout = chat.shout;
    let ctx = egui.ctx_mut().clone();

    // The messenger's own composer (`chats::compose_row`), so typing on the map
    // looks like typing in a chat: one glass pill, a round field, round
    // buttons. The row of chips and a boxed field it replaces read as a form.
    egui::Area::new(egui::Id::new("chat_composer"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(&ctx, |ui| {
            ui.set_width(width);
            crate::systems::chats::glass_frame()
                .corner_radius(egui::CornerRadius::same(26))
                .inner_margin(egui::Margin::same(6))
                .shadow(lift())
                .show(ui, |ui| {
                    ui.set_width(width - 12.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        // Where this goes, as the chat list draws that chat: the
                        // pin disc for Here, the globe for Everyone. It cycles
                        // rather than opening a list — a popup over the keyboard
                        // is a second thing covering the screen
                        // (`docs/notes/chat-without-a-panel.md`) — and the field's
                        // hint says in words what the disc says in a picture.
                        let (rect, scope) =
                            ui.allocate_exact_size(egui::Vec2::splat(38.0), egui::Sense::click());
                        crate::systems::chats::paint_avatar(
                            ui.painter(),
                            rect.center(),
                            &scope_name,
                            scope_kind,
                            38.0,
                        );
                        if scope_count > 1 {
                            // A small swap mark on the disc: it is a switch.
                            let c = rect.right_bottom() - egui::vec2(5.0, 5.0);
                            ui.painter().circle_filled(c, 8.0, theme::SURFACE_RAISED);
                            ui.painter().text(
                                c,
                                egui::Align2::CENTER_CENTER,
                                icons::SWAP,
                                egui::FontId::proportional(11.0),
                                theme::CREAM,
                            );
                        }
                        if scope
                            .on_hover_text(format!("To {scope_name} — tap to change"))
                            .clicked()
                        {
                            cycle_scope = true;
                        }
                        // Right-to-left so the field takes whatever is left.
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // ✕ outermost. The dock hides itself while the keys
                            // are up (an `Area` drawn later would take taps meant
                            // for them), so on a phone this is the only visible
                            // way out of the composer besides Back.
                            if crate::systems::chats::circle_button(
                                ui,
                                icons::CLEAR,
                                false,
                                true,
                                "Close",
                            )
                            .clicked()
                            {
                                close_now = true;
                            }
                            let ready = !chat.input.trim().is_empty();
                            let send = crate::systems::chats::circle_button(
                                ui,
                                icons::SEND,
                                true,
                                ready,
                                "Send",
                            );
                            let field = egui::Frame::NONE
                                .fill(theme::SURFACE_RAISED)
                                .corner_radius(egui::CornerRadius::same(19))
                                .inner_margin(egui::Margin::symmetric(14, 9))
                                .show(ui, |ui| {
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            // Shout is a property of Here only;
                                            // Everyone has no distance to shout
                                            // across. Inside the field's end, as
                                            // the messenger keeps its pin there.
                                            if is_local
                                                && ui
                                                    .add(
                                                        egui::Label::new(
                                                            egui::RichText::new("Shout")
                                                                .size(NAME_PT)
                                                                .strong()
                                                                .color(if shout {
                                                                    theme::ACCENT
                                                                } else {
                                                                    theme::TEXT_DIM
                                                                }),
                                                        )
                                                        .sense(egui::Sense::click()),
                                                    )
                                                    .on_hover_text(
                                                        "Carry further than a normal say",
                                                    )
                                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                                    .clicked()
                                            {
                                                toggle_shout = true;
                                            }
                                            // `desired_width`, not
                                            // `add_sized(available_size())`: this
                                            // layout's available *height* is the
                                            // rest of the row, and sizing to it
                                            // grows a one-line field into a block.
                                            crate::systems::text_touch::field(
                                                ui,
                                                egui::TextEdit::singleline(&mut chat.input)
                                                    .id(egui::Id::new("chat_composer_field"))
                                                    .frame(egui::Frame::NONE)
                                                    .font(egui::FontId::proportional(BODY_PT))
                                                    .text_color(theme::TEXT)
                                                    .desired_width(ui.available_width())
                                                    .hint_text(
                                                        egui::RichText::new(hint.as_str())
                                                            .color(theme::LINE),
                                                    ),
                                            )
                                        },
                                    )
                                    .inner
                                })
                                .inner;
                            if claim {
                                field.request_focus();
                            }
                            let entered =
                                field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            send_now = (send.clicked() || entered) && ready;
                        });
                    });
                });
        });

    if cycle_scope && scope_count > 0 {
        chat.active = (chat.active + 1) % scope_count;
    }
    if toggle_shout {
        chat.shout = !chat.shout;
    }
    if send_now {
        // The session is what *sends*, not what draws: a scripted `--ui chat`
        // capture has none, and a pill that refused to appear without one would
        // be the feature photographing as an empty screen.
        let private = chat
            .channels
            .get(chat.active)
            .filter(|c| crate::systems::chat::ChatState::is_private(&c.key))
            .map(|c| c.key.clone());
        if let Some(key) = private {
            // Sealed and sent by `systems::private_chat`. This branch comes first
            // and returns nothing to the plain path: a private line must never
            // reach `ChatSend`, where the server would read and store it.
            let body = chat.input.trim().to_string();
            chat.private_outbox.push((key, body));
        } else if let (Some(session), Some(channel)) =
            (online.as_ref(), chat.channels.get(chat.active))
        {
            let scope = crate::systems::chat::scope_for(&channel.key, chat.shout);
            let body = chat.input.trim().to_string();
            session.send(
                "chat send",
                NetMessage::ChatSend {
                    request_id: 0,
                    scope,
                    body,
                },
            );
        }
        chat.input.clear();
        // Stay open and keep the caret: a conversation is more than one line, and
        // on a phone re-opening the composer costs a tap and the keyboard's whole
        // slide-in animation.
        overlay.claim_focus = true;
    }
    if close_now {
        overlay.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::chat::ChatState;

    #[test]
    fn a_line_holds_then_fades_then_goes() {
        assert_eq!(fade(0.0, 10.0, 4.0), 1.0);
        assert_eq!(fade(10.0, 10.0, 4.0), 1.0, "the hold is inclusive");
        assert!((fade(12.0, 10.0, 4.0) - 0.5).abs() < 1e-5);
        assert_eq!(fade(14.0, 10.0, 4.0), 0.0);
        assert_eq!(fade(999.0, 10.0, 4.0), 0.0, "and stays gone");
    }

    /// The transcript is one stream however many channels fed it, in the order a
    /// reader actually saw the lines.
    #[test]
    fn recent_merges_channels_in_arrival_order() {
        let mut chat = ChatState::default();
        chat.clock = 1.0;
        chat.receive("local", 7, "near".into(), "hello".into(), Some(3.0));
        chat.clock = 2.0;
        chat.receive("geo:", 8, "far".into(), "world".into(), None);
        chat.clock = 3.0;
        chat.receive("local", 7, "near".into(), "again".into(), Some(3.0));

        let got: Vec<&str> = chat
            .recent(3.0, 100.0, 10)
            .iter()
            .map(|(_, l)| l.body.as_str())
            .collect();
        assert_eq!(got, vec!["hello", "world", "again"]);
    }

    /// Old enough and it is not in the stream at all — which is what stops the
    /// transcript growing into a panel by another name.
    #[test]
    fn recent_drops_what_is_past_the_window() {
        let mut chat = ChatState::default();
        chat.clock = 0.0;
        chat.receive("local", 7, "near".into(), "ancient".into(), None);
        chat.clock = 100.0;
        chat.receive("local", 7, "near".into(), "fresh".into(), None);
        let got = chat.recent(100.0, 14.0, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.body, "fresh");
    }

    /// The scrollback a channel replays on sign-in is history: it lands in the
    /// log at the time it was sent, and never pops up as if just said.
    #[test]
    fn replayed_scrollback_does_not_pop_up() {
        let mut chat = ChatState::default();
        chat.clock = 50.0;
        chat.receive("geo:", 8, "far".into(), "last week".into(), None);
        let now_ms = chat
            .channels
            .iter()
            .find(|c| c.key == "geo:")
            .unwrap()
            .lines[0]
            .at_ms;
        let week_ago = now_ms - 7 * 24 * 3_600_000;
        chat.date_last("geo:", week_ago);
        chat.receive("geo:", 8, "far".into(), "just now".into(), None);
        chat.date_last("geo:", now_ms - 5_000);
        let got = chat.recent(50.0, 14.0, 10);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1.body, "just now");
        let lines = &chat
            .channels
            .iter()
            .find(|c| c.key == "geo:")
            .unwrap()
            .lines;
        assert_eq!(lines.len(), 2, "history stays in the log");
        assert_eq!(lines[0].at_ms, week_ago);
    }

    fn seeded(
        lines: &[(&str, u64, &str)],
    ) -> (
        ChatState,
        Vec<(String, String, crate::systems::chat::ChatLine)>,
    ) {
        let mut chat = ChatState::default();
        chat.my_id = Some(1);
        chat.my_name = Some("jeroen".into());
        for (i, (key, id, who)) in lines.iter().enumerate() {
            chat.clock = i as f64;
            chat.receive(key, *id, (*who).into(), format!("line {i}"), None);
        }
        let got = chat
            .recent(chat.clock, 100.0, 100)
            .into_iter()
            .map(|(c, l)| (c.key.clone(), c.label.clone(), l.clone()))
            .collect();
        (chat, got)
    }

    /// Back-to-back lines from one person in one chat are one box; a new
    /// speaker or a new chat starts the next.
    #[test]
    fn a_run_is_one_speaker_in_one_chat() {
        let (chat, lines) = seeded(&[
            ("geo:", 5, "viberfox"),
            ("geo:", 5, "viberfox"),
            ("geo:", 1, "jeroen"),
            ("local", 1, "jeroen"),
            ("local", 5, "viberfox"),
        ]);
        let got: Vec<(String, bool, usize)> = runs(&lines, |l| chat.is_mine(l))
            .iter()
            .map(|r| (r.key.clone(), r.mine, r.lines.len()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("geo:".into(), false, 2),
                ("geo:".into(), true, 1),
                ("local".into(), true, 1),
                ("local".into(), false, 1),
            ]
        );
    }

    /// Avatar ids start again per connection, so a line replayed from an
    /// earlier sign-in is still yours by name — but never a prim's (id 0),
    /// whatever it is called.
    #[test]
    fn own_lines_are_known_by_name_across_reconnects() {
        let (chat, lines) = seeded(&[
            ("geo:", 42, "Jeroen"),
            ("local", 0, "jeroen"),
            ("geo:", 7, "wieke"),
        ]);
        let mine: Vec<bool> = lines.iter().map(|(_, _, l)| chat.is_mine(l)).collect();
        assert_eq!(mine, vec![true, false, false]);
    }

    /// Opening the composer answers in the chat of the newest line on screen.
    #[test]
    fn the_composer_opens_on_the_conversation_on_screen() {
        let (mut chat, _) = seeded(&[("local", 5, "a"), ("geo:", 5, "a")]);
        chat.active = 0;
        let i = conversation_on_screen(&chat).expect("a line is on screen");
        assert_eq!(chat.channels[i].key, "geo:");
        // Nothing on screen: the chip stays where it was.
        chat.clock += 1000.0;
        assert_eq!(conversation_on_screen(&chat), None);
    }

    #[test]
    fn the_hint_says_where_the_words_go() {
        assert_eq!(hint_for("local", "Here", false), "Say something nearby…");
        assert_eq!(hint_for("local", "Here", true), "Shout to the street…");
        assert_eq!(hint_for("geo:", "Everyone", false), "Message everyone…");
    }

    /// The cap keeps the *newest*, not the first ones it happened to see.
    #[test]
    fn recent_keeps_the_newest_when_it_has_to_choose() {
        let mut chat = ChatState::default();
        for i in 0..10 {
            chat.clock = i as f64;
            chat.receive("local", 7, "near".into(), format!("line {i}"), None);
        }
        let got: Vec<String> = chat
            .recent(10.0, 100.0, 3)
            .iter()
            .map(|(_, l)| l.body.clone())
            .collect();
        assert_eq!(got, vec!["line 7", "line 8", "line 9"]);
    }

    /// A blocked person disappears from the overlay as well as from the panel.
    /// The server withholds their *new* messages; this is the half only the
    /// client can do.
    #[test]
    fn recent_hides_a_blocked_speaker() {
        let mut chat = ChatState::default();
        chat.clock = 1.0;
        chat.receive("local", 7, "loud".into(), "noise".into(), None);
        chat.receive("local", 8, "quiet".into(), "hello".into(), None);
        chat.blocked = vec!["LOUD".into()];
        let got: Vec<&str> = chat
            .recent(1.0, 100.0, 10)
            .iter()
            .map(|(_, l)| l.sender.as_str())
            .collect();
        assert_eq!(got, vec!["quiet"]);
    }

    /// The composer clears the keyboard when there is one and the dock when
    /// there is not — never neither, which is how a field ends up behind the
    /// keys it is being typed into.
    /// The composer clears the keyboard when there is one and the dock when
    /// there is not — never neither, which is how a field ends up behind the
    /// keys it is being typed into.
    #[test]
    fn the_composer_clears_whatever_owns_the_bottom_edge() {
        let (with_keys, _) = stack_bottoms(true, 300.0, 0.0);
        assert!(
            with_keys > 300.0,
            "the pill must clear the slab: {with_keys}"
        );
        let (without, _) = stack_bottoms(true, 0.0, 0.0);
        assert!(
            without >= crate::systems::dock::reserved_bottom(),
            "the pill must clear the dock: {without}"
        );
    }

    /// And the transcript clears the composer. They are a stack; a transcript
    /// that answered the bottom edge on its own would be under the pill it is
    /// meant to sit above — exactly the failure the panel had with the keyboard.
    #[test]
    fn the_transcript_stacks_on_the_composer() {
        let corner = 64.0;
        let (composer, transcript) = stack_bottoms(true, 300.0, corner);
        assert!(
            transcript >= composer + composer_height(),
            "transcript {transcript} sits inside the composer at {composer}"
        );
        // Closed, it goes back to the corner the touch controls left it.
        let (_, idle) = stack_bottoms(false, 0.0, corner);
        assert_eq!(idle, corner);
    }

    /// Every reason the overlay stands down, in one place — the state that must
    /// not exist is "bubbles up, composer gone".
    #[test]
    fn the_overlay_stands_down_for_the_screens_that_own_the_view() {
        assert!(suppressed(false, false, false, false), "offline");
        assert!(suppressed(true, true, false, false), "driving");
        assert!(
            suppressed(true, false, true, true),
            "a panel on a phone is the screen"
        );
        assert!(
            !suppressed(true, false, false, true),
            "a docked panel on a wide viewport leaves a map to draw on"
        );
        assert!(!suppressed(true, false, true, false), "a bare phone map");
    }

    /// The keyboard is subtracted from whichever end of the map area it took —
    /// it flips edges, so an overlay that only ever trimmed the bottom would put
    /// its content under the keys half the time.
    #[test]
    fn the_keyboard_is_cut_from_whichever_edge_it_took() {
        let screen = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 800.0));
        assert_eq!(clear_of_keyboard(screen, None), screen);
        let bottom = egui::Rect::from_min_max(egui::pos2(0.0, 500.0), egui::pos2(400.0, 800.0));
        assert_eq!(clear_of_keyboard(screen, Some(bottom)).max.y, 500.0);
        let top = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 300.0));
        assert_eq!(clear_of_keyboard(screen, Some(top)).min.y, 300.0);
    }
}
