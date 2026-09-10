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

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::grid::WORLD_CELL_EDGE;
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
/// Width of the transcript column, in points. Fixed rather than sized to
/// content, for the reason `hud`'s module note gives at length: a size-to-content
/// `Area` whose contents reach for the available width grows a little every
/// frame until it has swallowed the screen.
const TRANSCRIPT_WIDTH: f32 = 300.0;

/// A bubble hangs this far above the avatar's feet, in metres. The Quaternius
/// characters are ~1.8 m and authored origin-at-feet (`systems::avatar`), so
/// this clears the top of the head by a little.
const BUBBLE_HEIGHT_M: f64 = 2.15;
/// Bubbles hold for longer than they fade, and shorter than the transcript: the
/// transcript is the record, a bubble is the moment.
const BUBBLE_HOLD_SECS: f64 = 7.0;
const BUBBLE_FADE_SECS: f64 = 1.5;
/// At most this many lines per speaker. A bubble is a speech balloon, not a
/// scrollback — and without a cap one person can paper over the view.
const BUBBLE_LINES: usize = 3;
/// Widest a bubble gets, in points.
const BUBBLE_WIDTH: f32 = 190.0;

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
    }

    /// Close it, keeping whatever was typed — a dismissal is not a delete.
    pub fn close(&mut self) {
        self.composing = false;
        self.claim_focus = false;
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
    mut egui: ResMut<EguiManager>,
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

    let edge = WORLD_CELL_EDGE as f64;
    let cell_world = |cell: &GridCell, tf: &Transform| {
        DVec3::new(
            cell.x as f64 * edge + tf.translation.x as f64,
            cell.y as f64 * edge + tf.translation.y as f64,
            cell.z as f64 * edge + tf.translation.z as f64,
        )
    };
    let cam_world = cell_world(cam_cell, cam_tf);
    let cam_view = GlobalTransform::from(Transform::from_rotation(cam_tf.rotation));

    // Where every speaker on screen is standing. Remote avatars are spawned as
    // plain root entities (`network::sync_remote_avatar_entities`), so their
    // translation is already the absolute sim position; the local avatar is in
    // the grid (`lib.rs::attach_avatar_to_bigspace`) and needs its cell added.
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
            theme::chip_frame()
                .stroke(egui::Stroke::new(1.0, theme::OVERLAY_LINE))
                .show(ui, |ui| {
                    ui.set_max_width(BUBBLE_WIDTH);
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(speaker)
                                .size(theme::SIZE_CHIP)
                                .strong()
                                .color(theme::ACCENT_TEXT),
                        );
                        for body in bodies {
                            ui.label(
                                egui::RichText::new(body)
                                    .size(theme::SIZE_SUB)
                                    .color(theme::CREAM),
                            );
                        }
                    });
                });
        });
}

// ─── The transcript ───────────────────────────────────────────────────────────

/// The last few lines from every channel, bottom-left, fading.
pub fn render_chat_transcript(
    mut egui: ResMut<EguiManager>,
    mut chat: ResMut<ChatState>,
    overlay: Res<ChatOverlay>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    camera_state: Res<crate::resources::CameraState>,
    touch: Res<crate::systems::touch::TouchGestures>,
) {
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
    let lines: Vec<(String, crate::systems::chat::ChatLine)> = chat
        .recent(now, window, want)
        .into_iter()
        .map(|(c, l)| {
            let prefix = if c.key == "local" {
                String::new()
            } else {
                format!("[{}] ", c.label)
            };
            (prefix, l.clone())
        })
        .collect();
    if lines.is_empty() {
        return;
    }
    let my_id = chat.my_id;

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
    let mut open_report: Option<(String, Option<String>)> = None;

    egui::Area::new(egui::Id::new("chat_transcript"))
        .order(egui::Order::Middle)
        // The rule this module exists to keep: an `Area` that is merely faded is
        // still a live click target over the map.
        .interactable(interactive)
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -bottom),
        )
        .show(&ctx, |ui| {
            ui.set_max_width(TRANSCRIPT_WIDTH);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (prefix, line) in &lines {
                // Per line, not per box: the oldest line at the top of the stack
                // is the one that should be going, and fading the whole column
                // together makes the transcript blink rather than scroll.
                let alpha = if overlay.composing {
                    1.0
                } else {
                    fade(now - line.arrived, HOLD_SECS, FADE_SECS)
                };
                if alpha <= 0.0 {
                    continue;
                }
                let mine = my_id.is_some_and(|me| me == line.sender_id);
                let name = if mine {
                    "You".to_string()
                } else {
                    line.sender.clone()
                };
                theme::chip_frame().show(ui, |ui| {
                    ui.set_opacity(alpha);
                    ui.set_max_width(TRANSCRIPT_WIDTH);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let label = egui::Label::new(
                            egui::RichText::new(format!("{prefix}{name}:"))
                                .size(theme::SIZE_SUB)
                                .strong()
                                // One tone for every speaker. This used to be
                                // amber for your own lines against everyone
                                // else's orange, and the monochrome palette has
                                // no second accent to spend on it — but it also
                                // does not need one: `my_id` is read a line above
                                // to write the name as "You", which says the same
                                // thing in a word rather than in a hue nobody has
                                // a key to.
                                .color(theme::ACCENT_TEXT),
                        );
                        if interactive {
                            // The name is the handle for what you can do *about*
                            // a person. It senses clicks only here, because an
                            // inert transcript must let the map have them.
                            let name = ui.add(label.sense(egui::Sense::click()));
                            person_menu(
                                &name,
                                &line.sender,
                                Some(&line.body),
                                |action| match action {
                                    PersonAction::Block => block_now = Some(line.sender.clone()),
                                    PersonAction::Report(msg) => {
                                        open_report = Some((line.sender.clone(), msg))
                                    }
                                },
                            );
                        } else {
                            ui.add(label);
                        }
                        ui.label(
                            egui::RichText::new(&line.body)
                                .size(theme::SIZE_SUB)
                                .color(theme::TEXT),
                        );
                    });
                });
            }
        });

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
        });
    }
}

// ─── The composer ─────────────────────────────────────────────────────────────

/// Height of the composer pill, in points — the row plus its frame.
fn composer_height() -> f32 {
    theme::row_height().max(34.0) + 16.0
}

/// How much of the bottom edge to leave for the on-screen keyboard.
///
/// Zero unless the composer is open on a platform with no IME — see the module
/// note on why this is *predicted* rather than measured off an open slab.
fn keyboard_reserve(egui: &EguiManager, composing: bool) -> f32 {
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
    mut egui: ResMut<EguiManager>,
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
    if !overlay.composing {
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
    let scope_label = chat
        .channels
        .get(chat.active)
        .map(|c| c.label.clone())
        .unwrap_or_else(|| "Local".into());
    let is_local = chat
        .channels
        .get(chat.active)
        .is_some_and(|c| c.key == "local");
    let scope_count = chat.channels.len();

    let mut send_now = false;
    let mut close_now = false;
    let mut cycle_scope = false;
    let mut toggle_shout = false;
    let claim = std::mem::take(&mut overlay.claim_focus);
    let shout = chat.shout;
    let ctx = egui.ctx_mut().clone();

    egui::Area::new(egui::Id::new("chat_composer"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -bottom))
        .show(&ctx, |ui| {
            ui.set_width(width);
            theme::overlay_frame().show(ui, |ui| {
                ui.set_width(width - 24.0);
                ui.horizontal(|ui| {
                    if theme::chip(ui, &scope_label, false)
                        .on_hover_text(if scope_count > 1 {
                            "Where this goes — tap to change"
                        } else {
                            "Where this goes"
                        })
                        .clicked()
                    {
                        cycle_scope = true;
                    }
                    // Shout is a property of the *local* channel only; a room or
                    // the global channel has no distance to shout across. It is a
                    // chip in the row rather than the checkbox on a line of its
                    // own it used to be — one of three chrome rows that stood
                    // between the history and the thumb.
                    if is_local
                        && theme::chip(ui, "Shout", shout)
                            .on_hover_text("Carry further than a normal say")
                            .clicked()
                    {
                        toggle_shout = true;
                    }
                    // Right-to-left so the field takes whatever is left: the pill
                    // is as wide as the viewport allows, and a fixed field width
                    // either overflows a phone or leaves a gap on a desktop.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // ✕ outermost. The dock hides itself while the keys are
                        // up (an `Area` drawn later would take taps meant for
                        // them), so on a phone this is the only visible way out
                        // of the composer besides Back.
                        if theme::close_button(ui).clicked() {
                            close_now = true;
                        }
                        let ready = !chat.input.trim().is_empty();
                        let send = theme::action_enabled_because(
                            ui,
                            ready,
                            icons::FORWARD,
                            "Type a message first",
                        );
                        // `desired_width`, not `add_sized(available_size())`:
                        // this layout's available *height* is the whole rest of
                        // the row, and sizing to it grows a single-line field
                        // into a block.
                        let field = crate::systems::text_touch::field(
                            ui,
                            egui::TextEdit::singleline(&mut chat.input)
                                .id(egui::Id::new("chat_composer_field"))
                                .desired_width(ui.available_width())
                                .hint_text("Say something…"),
                        );
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
        if let (Some(session), Some(channel)) = (online.as_ref(), chat.channels.get(chat.active)) {
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
