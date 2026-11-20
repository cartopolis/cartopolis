//! Chat over the map: a bubble above whoever spoke, a chip with the newest
//! line, and a sheet that holds one chat at a time — with talking nearby and a
//! private chat's call in it.
//!
//! **The panel was the problem.** On a compact viewport a right panel is the
//! whole screen (`egui_manager::responsive_right_panel`), so in a client whose
//! Local channel is scoped by *how far away someone is standing*, saying
//! anything meant covering the world and the people in it. This module is chat
//! with no panel: floating `Area`s, none of which is up unless it has something
//! to say. `docs/notes/chat-without-a-panel.md` has the design and what it
//! replaced; [`crate::systems::chat`] keeps the state and the log.
//!
//! ## The three pieces
//!
//! **[`render_chat_bubbles`]** puts a Local line over the head of the person who
//! said it. `NetMessage::ChatMessage` already carries `sender_id`, which is the
//! same number as `RemoteAvatar::sim_id`, so the line finds its entity with no
//! name matching and no protocol change.
//!
//! **[`render_chat_chip`]** is the map at rest: one line over the switch, the
//! newest message with its chat's disc and the unread count, gone after
//! [`CHIP_HOLD_SECS`]. A tap opens the sheet on that chat. While talking nearby
//! is on it stays, with mute and leave on it.
//!
//! **[`render_chat_sheet`]** is chat while you are in it: a row of tabs (Here,
//! Everyone, private chats with something new), the open chat's lines at the
//! messenger's size, and the field. Talking nearby and a private chat's
//! encrypted call start from the tab row and show as a strip in the sheet.
//!
//! Until 2026-11-14 the second and third were a transcript of every chat merged,
//! each run headed "name · Everyone", and a composer pill with a scope chip;
//! on a phone five short messages filled the screen and the pill's hint named a
//! different chat from the one on screen (`docs/mockups/chat-overlay`, v3).
//!
//! ## Four things about it are load-bearing
//!
//! **The chip senses taps only while it can be read.** An `Area` that is
//! merely *faded* still eats the taps aimed at the map under it — the trap
//! `place_pins` culls for and `map_osd` hides for.
//!
//! **A bubble is painted, never interactive**: it sits *over the world*, where
//! a tap means pick, walk or place, and a chat bubble is not allowed to be in
//! the way of that.
//!
//! **The sheet takes the keyboard's height on the frame the keys choose their
//! edge.** `onscreen_keyboard::layout` picks its edge from where the focused
//! field landed *this* frame, and flips to the **top** when the bottom slab
//! would cover it. So the sheet reserves [`onscreen_keyboard::expected_height`]
//! while the keys are up and on the frame its field claims them, and says
//! [`onscreen_keyboard::field_makes_room`] every frame, so a tap into the field
//! with the keys down leaves them at the bottom and the sheet rises onto them.
//!
//! **Everything ages on [`ChatState::clock`], not on `sent_at_ms`.** See that
//! field: a fade is about how long *this screen* has shown a line.

use crate::systems::egui_manager::EguiMut;
use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::msg;

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{person_menu, ChatState, PersonAction, ReportDraft};
use crate::systems::egui_manager::EguiManager;
use crate::systems::icons;
use crate::systems::onscreen_keyboard;
use crate::systems::theme;
use cartopolis_core::{NetMessage, ReportReason};

/// How recent the newest line must be for the sheet to open on its chat.
const HOLD_SECS: f64 = 10.0;
const FADE_SECS: f64 = 4.0;
/// The chip holds the newest line this long, then fades over
/// [`CHIP_FADE_SECS`]: half a minute, the time a remark stays news.
const CHIP_HOLD_SECS: f64 = 30.0;
const CHIP_FADE_SECS: f64 = 4.0;
/// Widest the chip gets, in points. Fixed rather than sized to content, for
/// the reason `hud`'s module note gives at length: a size-to-content `Area`
/// whose contents reach for the available width grows a little every frame
/// until it has swallowed the screen.
const TRANSCRIPT_WIDTH: f32 = 360.0;

/// The chip on a viewport `viewport_w` points wide: the whole width less the
/// margins on a phone, capped on anything wider.
#[must_use]
fn transcript_width(viewport_w: f32) -> f32 {
    (viewport_w - 2.0 * theme::OSD_MARGIN).clamp(160.0, TRANSCRIPT_WIDTH)
}

/// The size of a line's words, as the messenger's thread sets them.
const BODY_PT: f32 = 14.5;
/// A speaker's name over their run.
const NAME_PT: f32 = 12.5;
/// The round initial beside someone else's run.
const FACE_PT: f32 = 26.0;

/// The top of the sheet with the keys up, as a fraction of the screen's
/// height: the world keeps the top third, with the person being answered in it.
const SHEET_TOP_FRACTION: f32 = 0.36;
/// The sheet's height with the keys down, as a fraction of the screen's.
const SHEET_REST_FRACTION: f32 = 0.5;
/// The least a sheet is, whatever the keys took: a tab row, two lines, a field.
const SHEET_MIN_HEIGHT: f32 = 220.0;
/// The card the sheet is on a wide screen.
const SHEET_CARD_WIDTH: f32 = 400.0;
const SHEET_CARD_HEIGHT: f32 = 440.0;
/// Lines the sheet keeps for scrolling back; the rest is in Community.
const SHEET_LINES: usize = 80;
/// A private chat has a tab while its newest line is younger than this.
const TAB_RECENT_SECS: f64 = 3600.0;
/// A tab's height.
const TAB_HEIGHT: f32 = 34.0;
/// The words that say a chat is public, as the mockup's `--public`.
const PUBLIC_INK: egui::Color32 = egui::Color32::from_rgb(0xB3, 0x9D, 0xFF);
/// The voice and call strip's ground: the call bar's green, deep.
const VOICE_FILL: egui::Color32 = egui::Color32::from_rgb(0x14, 0x26, 0x1F);
/// The ring round someone talking.
const TALKING_RING: egui::Color32 = egui::Color32::from_rgb(0x4F, 0xD1, 0x8F);

/// A bubble hangs this far above the avatar's feet, in metres. The MakeHuman
/// bodies stand origin-at-feet (`human_avatar`) and the tallest the designer
/// makes is about 2 m, so this clears the top of the head by a little.
const BUBBLE_HEIGHT_M: f64 = 2.15;
/// Bubbles are the moment; the chip and the sheet are the record.
const BUBBLE_HOLD_SECS: f64 = 7.0;
const BUBBLE_FADE_SECS: f64 = 1.5;
/// At most this many lines per speaker. A bubble is a speech balloon, not a
/// scrollback — and without a cap one person can paper over the view.
const BUBBLE_LINES: usize = 3;
/// Widest a bubble gets, in points.
const BUBBLE_WIDTH: f32 = 200.0;
/// How far above a head a balloon reaches, roughly: a name and two lines.
const BUBBLE_REACH: f32 = 90.0;

/// Is the chat sheet open?
#[derive(Resource, Default)]
pub struct ChatOverlay {
    /// The sheet is up. (Named for the composer it replaced: `dismiss` and
    /// the dock read it.)
    pub composing: bool,
    /// Take the keyboard on the next frame the field is drawn. Set when the
    /// sheet opens and when a message is sent (so a reply needs no second
    /// tap), and cleared once claimed.
    claim_focus: bool,
    /// Open on the conversation on screen, once: without it the sheet kept
    /// whichever chat it was last left on, while the line that made you open
    /// it was in another.
    follow: bool,
    /// The row of extras under the field (Shout, the full chat) is open.
    plus: bool,
    /// Where the chip or the sheet was drawn last frame, so a bubble over a
    /// head does not land on it.
    covers: Option<egui::Rect>,
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
    /// Open the sheet and put the caret in it.
    pub fn open(&mut self) {
        self.composing = true;
        self.claim_focus = true;
        self.follow = true;
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
        self.plus = false;
    }
}

/// Lines one person said back to back in one chat, drawn as one box under one
/// name. Eight lines from two people used to be eight boxes and eight
/// `[Global] name:` prefixes — a wall of chrome around very few words.
pub struct Run {
    /// The chat's key (`local`, `geo:`, a private chat's).
    pub key: String,
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
    for (key, _, line) in lines {
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
pub fn seed_demo_chat(chat: &mut ChatState, overlay: &mut ChatOverlay, open: bool) {
    const ME: u64 = 1;
    const THEM: u64 = 2;
    chat.my_id = Some(ME);
    if chat.channels.iter().all(|c| c.lines.is_empty()) {
        // Everyone and a private chat first, so the newest line, which the
        // sheet opens on, is Here.
        chat.receive(
            "geo:",
            9,
            "Bram".into(),
            "Anyone else seeing rain over Assen?".into(),
            None,
        );
        chat.open_channel(
            "dm:a1b2c3",
            &crate::systems::private_chat::private_label("Anna"),
        );
        chat.receive(
            "dm:a1b2c3",
            7,
            "Anna".into(),
            "Shall we start at ten on Saturday?".into(),
            None,
        );
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
            "local",
            THEM,
            "Wieke".into(),
            "I'm by the fish stall".into(),
            Some(6.0),
        );
    }
    overlay.demo = true;
    if open && !overlay.composing {
        overlay.open();
        // The harness re-stamps every line each frame, so "the newest" is a
        // tie; open on Here, the conversation the speakers are having.
        overlay.follow = false;
        if let Some(i) = chat.channels.iter().position(|c| c.key == "local") {
            chat.active = i;
        }
    }
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
        if let Some(transcript) = overlay.covers {
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
        if alpha < 1.0 {
            // A fade reads the clock; egui has to be told it moves.
            ctx.request_repaint();
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
                .fill(theme::p().glass)
                .stroke(egui::Stroke::new(1.0, crate::systems::chats::gline()))
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
                                    egui::RichText::new(body)
                                        .size(BODY_PT)
                                        .color(theme::p().cream),
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
                theme::p().glass,
                egui::Stroke::NONE,
            ));
        });
}

/// The lift every surface over the map has, so it reads as a thing laid on the
/// world rather than a hole cut in it.
fn lift() -> egui::Shadow {
    egui::Shadow {
        offset: [0, 2],
        blur: 10,
        spread: 0,
        color: egui::Color32::from_black_alpha(90),
    }
}

// ─── The chip ─────────────────────────────────────────────────────────────────

/// The newest line on the map at rest, as one line over the switch: which chat
/// (its disc), who and what, and how many lines wait unread. It fades after
/// [`CHIP_HOLD_SECS`]; a tap opens the sheet on that chat.
///
/// It replaced a transcript of the last four lines of every chat, each run
/// headed by a name and a chat name, drawn in bubbles at the thread's size:
/// on a phone five short messages filled the screen from the top bar to the
/// keyboard (`docs/mockups/chat-overlay`, v3).
///
/// While talking nearby is on it stays, and carries the voice's mute and
/// leave: a microphone that is open must never be out of reach.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_chip(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut overlay: ResMut<ChatOverlay>,
    view: Res<crate::systems::chats::ChatsView>,
    mut voice: ResMut<crate::systems::voice::VoiceState>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    camera_state: Res<crate::resources::CameraState>,
    touch: Res<crate::systems::touch::TouchGestures>,
) {
    // Where it was drawn, for the bubbles to keep clear of; nowhere until it is.
    if !overlay.composing {
        overlay.covers = None;
    }
    let compact = egui.compact();
    let panel_open = !right_panel.is(RightPanel::None);
    if suppressed(
        online.is_some() || overlay.demo,
        driver.active,
        compact,
        panel_open,
    ) || overlay.composing
        || keyboard.is_open()
    {
        return;
    }
    // A chat's World view narrows the chip to that chat, whose composer
    // `chats::render_chat_world` draws.
    let only = chat.world.clone();
    let now = chat.clock;
    let newest = chat
        .recent(now, CHIP_HOLD_SECS + CHIP_FADE_SECS, usize::MAX)
        .into_iter()
        .rev()
        .filter(|(c, _)| only.as_deref().is_none_or(|k| c.key == k))
        .find(|(_, l)| !chat.is_mine(l))
        .map(|(c, l)| {
            (
                c.key.clone(),
                c.label.clone(),
                l.sender.clone(),
                summary(l),
                l.arrived,
            )
        });
    let talking = voice.mode != crate::systems::voice::VoiceMode::Off;
    if newest.is_none() && !talking {
        return;
    }
    let unread = if only.is_some() {
        0
    } else {
        view.unread_total(&chat)
    };
    let alpha = if talking {
        1.0
    } else {
        newest
            .as_ref()
            .map_or(0.0, |n| fade(now - n.4, CHIP_HOLD_SECS, CHIP_FADE_SECS))
    };
    if alpha <= 0.0 {
        return;
    }
    let ctx = egui.ctx_mut().clone();
    if alpha < 1.0 {
        ctx.request_repaint();
    }

    let cluster = crate::systems::map_osd::cluster_for(
        camera_state.mode,
        driver.active,
        panel_open,
        keyboard.is_open(),
        theme::touch_layout() || touch.ever(),
    );
    let corner = crate::systems::map_osd::reserved_left_bottom(cluster);
    // Above the interaction prompt (`interact::render_interaction_prompt`)
    // when it is up: it sits in the same band over the switch, and a chip
    // drawn over its buttons hid them. Last frame's rect, as it draws later.
    let prompt = egui::Id::new("interact_prompt");
    let prompt_up = ctx.memory(|m| {
        m.areas()
            .is_visible(&egui::LayerId::new(egui::Order::Background, prompt))
    });
    let corner = match ctx.memory(|m| m.area_rect(prompt)) {
        Some(r) if prompt_up => corner.max(egui.viewport_pts().y - r.top() + theme::OSD_GAP),
        _ => corner,
    };
    let width = transcript_width(egui.viewport_pts().x);
    let muted = voice.mode == crate::systems::voice::VoiceMode::PushToTalk;
    let mut open_on: Option<Option<String>> = None;
    let mut toggle_mute = false;
    let mut leave_voice = false;

    let shown = egui::Area::new(egui::Id::new("chat_chip"))
        .order(egui::Order::Middle)
        // Live only while it can be read: a chip on its way out leaves the
        // map its taps, the rule the old transcript was inert for.
        .interactable(alpha > 0.5)
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            egui::vec2(theme::OSD_MARGIN, -corner),
        )
        .show(&ctx, |ui| {
            ui.set_opacity(alpha);
            ui.set_width(width);
            let frame = crate::systems::theme::glass_frame()
                .fill(if talking {
                    VOICE_FILL
                } else {
                    theme::p().glass
                })
                .corner_radius(egui::CornerRadius::same(18))
                .inner_margin(egui::Margin::symmetric(8, 6))
                .shadow(lift())
                .show(ui, |ui| {
                    ui.set_width(width - 16.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if let Some((key, label, _, _, _)) = &newest {
                            let kind = crate::systems::chats::kind_of(key);
                            let name = crate::systems::chats::display_name(key, label);
                            crate::systems::chats::avatar(ui, &name, kind, 26.0);
                        } else {
                            crate::systems::chats::avatar(
                                ui,
                                "Here",
                                crate::systems::chats::ChatKind::Here,
                                26.0,
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if talking {
                                if crate::systems::chats::circle_button(
                                    ui,
                                    icons::CLEAR,
                                    false,
                                    true,
                                    "Stop talking nearby",
                                )
                                .clicked()
                                {
                                    leave_voice = true;
                                }
                                let (icon, tip) = if muted {
                                    (icons::MIC_OFF, "Unmute")
                                } else {
                                    (icons::MIC, "Mute")
                                };
                                if crate::systems::chats::circle_button(ui, icon, !muted, true, tip)
                                    .clicked()
                                {
                                    toggle_mute = true;
                                }
                            }
                            if unread > 0 {
                                unread_pill(ui, unread);
                            }
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    let text = match &newest {
                                        Some((_, _, sender, body, _)) => {
                                            let tone = crate::systems::chats::tone_for(sender);
                                            let mut job = egui::text::LayoutJob::default();
                                            job.append(
                                                sender,
                                                0.0,
                                                egui::TextFormat::simple(
                                                    egui::FontId::proportional(NAME_PT),
                                                    crate::systems::chats::blend(
                                                        tone,
                                                        egui::Color32::WHITE,
                                                        0.5,
                                                    ),
                                                ),
                                            );
                                            job.append(
                                                body,
                                                6.0,
                                                egui::TextFormat::simple(
                                                    egui::FontId::proportional(BODY_PT),
                                                    theme::p().text,
                                                ),
                                            );
                                            job
                                        }
                                        None => egui::text::LayoutJob::simple_singleline(
                                            "Talking nearby".into(),
                                            egui::FontId::proportional(BODY_PT),
                                            theme::p().cream,
                                        ),
                                    };
                                    let mut text = text;
                                    text.wrap = egui::text::TextWrapping::truncate_at_width(
                                        ui.available_width().max(20.0),
                                    );
                                    ui.add(egui::Label::new(text).selectable(false));
                                },
                            );
                        });
                    });
                });
            // The whole chip opens the sheet, on the chat of the line in it.
            let resp = ui.interact(
                frame.response.rect,
                egui::Id::new("chat_chip_open"),
                egui::Sense::click(),
            );
            if theme::named(ui, resp, "Open the chat").clicked() {
                open_on = Some(newest.as_ref().map(|n| n.0.clone()));
            }
        });
    overlay.covers = Some(shown.response.rect);

    if toggle_mute {
        voice.set_mode(if muted {
            crate::systems::voice::VoiceMode::Open
        } else {
            crate::systems::voice::VoiceMode::PushToTalk
        });
    } else if leave_voice {
        voice.set_mode(crate::systems::voice::VoiceMode::Off);
    } else if let Some(key) = open_on {
        if let Some(i) = key.and_then(|k| chat.channels.iter().position(|c| c.key == k)) {
            chat.active = i;
        }
        overlay.open();
        // The chat was chosen here; don't let the opening pick another.
        overlay.follow = false;
    }
}

/// What a line says in one line: its words, or what it carries.
fn summary(line: &crate::systems::chat::ChatLine) -> String {
    let body = line.body.replace('\n', " ");
    if !body.trim().is_empty() {
        body
    } else if line.point.is_some() {
        format!("{} A point", icons::PIN)
    } else if line.picture.is_some() {
        format!("{} A picture", icons::CAMERA)
    } else if line.meeting.is_some() {
        format!("{} A plan", icons::EVENT)
    } else {
        String::new()
    }
}

/// A count of unread lines, the messenger's pill.
fn unread_pill(ui: &mut egui::Ui, n: usize) {
    let text = if n > 99 {
        "99+".to_string()
    } else {
        n.to_string()
    };
    egui::Frame::NONE
        .fill(theme::p().accent)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_PILL))
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(NAME_PT)
                    .strong()
                    .color(theme::p().on_accent),
            );
        });
}

// ─── The sheet ────────────────────────────────────────────────────────────────

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

/// Height of a one-line composer pill, in points — its round buttons (38) plus
/// the pill's margin on both sides.
fn composer_height() -> f32 {
    38.0 + 2.0 * 6.0
}

/// Where a bottom composer's bottom edge sits and where the lines above it
/// do, as offsets from the bottom of the viewport. A chat's World view
/// (`chats::render_chat_world`) stacks its pill with it.
#[must_use]
pub fn stack_bottoms(composing: bool, keyboard_reserve: f32, corner: f32) -> (f32, f32) {
    let composer = if keyboard_reserve > 0.0 {
        keyboard_reserve + theme::OSD_GAP
    } else {
        // Above the dock, which owns the bottom edge when there is no keyboard.
        crate::systems::dock::reserved_bottom() + theme::OSD_MARGIN
    };
    let lines = if composing {
        composer + composer_height() + theme::OSD_GAP
    } else {
        corner
    };
    (composer, lines)
}

/// Where the sheet goes on a viewport `viewport` points in size.
///
/// On a phone it spans the width and sits on the keyboard when the keys are up
/// (`keys` is their reserve, zero when they are down), reaching no higher
/// than [`SHEET_TOP_FRACTION`] of the screen so the world keeps its top third
/// and the person being answered stays in view; with the keys down it takes
/// [`SHEET_REST_FRACTION`]. Wider, it is a card in the bottom-left corner,
/// above the switch, so a panel docked on the right never covers it.
#[must_use]
pub fn sheet_rect(viewport: egui::Vec2, compact: bool, keys: f32, dock_bottom: f32) -> egui::Rect {
    if compact {
        let bottom = viewport.y - keys;
        let top = if keys > 0.0 {
            viewport.y * SHEET_TOP_FRACTION
        } else {
            viewport.y * (1.0 - SHEET_REST_FRACTION)
        };
        // Never less than a usable sheet, whatever the keys took.
        let top = top.min(bottom - SHEET_MIN_HEIGHT).max(0.0);
        egui::Rect::from_min_max(egui::pos2(0.0, top), egui::pos2(viewport.x, bottom))
    } else {
        let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(SHEET_CARD_WIDTH);
        let bottom = viewport.y - keys.max(dock_bottom) - theme::OSD_MARGIN;
        let height = SHEET_CARD_HEIGHT
            .min(bottom - viewport.y * 0.25)
            .max(SHEET_MIN_HEIGHT);
        egui::Rect::from_min_size(
            egui::pos2(theme::OSD_MARGIN, bottom - height),
            egui::vec2(width, height),
        )
    }
}

/// The chats that get a tab, as indices into `chat.channels`, in tab order:
/// Here and Everyone always, then each private chat with a line in the last
/// [`TAB_RECENT_SECS`], the newest first. The chat being written in and the
/// one this device is calling in keep their tab whatever their age.
///
/// Decided 2026-11-14: a tab for every private chat would fill the row with
/// the long tail of the messenger's list, which is what Community is for.
#[must_use]
pub fn sheet_tabs(chat: &ChatState, now: f64, keep: &[&str]) -> Vec<usize> {
    use crate::systems::chats::{kind_of, ChatKind};
    let mut out: Vec<usize> = Vec::new();
    for want in [ChatKind::Here, ChatKind::Everyone] {
        if let Some(i) = chat.channels.iter().position(|c| kind_of(&c.key) == want) {
            out.push(i);
        }
    }
    let active = chat.channels.get(chat.active).map(|c| c.key.as_str());
    let mut private: Vec<(f64, usize)> = chat
        .channels
        .iter()
        .enumerate()
        .filter(|(_, c)| ChatState::is_private(&c.key))
        .filter_map(|(i, c)| {
            let newest = c.lines.last().map_or(f64::NEG_INFINITY, |l| l.arrived);
            let kept = active == Some(c.key.as_str()) || keep.contains(&c.key.as_str());
            (kept || now - newest <= TAB_RECENT_SECS).then_some((newest, i))
        })
        .collect();
    private.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.extend(private.into_iter().map(|(_, i)| i));
    out
}

/// What the sheet asked for this frame.
#[derive(Default)]
struct SheetActs {
    tab: Option<usize>,
    close: bool,
    send: bool,
    voice: Option<crate::systems::voice::VoiceMode>,
    call: Option<crate::systems::calls::CallRequest>,
    unfold_call: bool,
    toggle_mic: bool,
    toggle_camera: bool,
    toggle_shout: bool,
    open_in_community: bool,
    person: Option<(String, PersonAction)>,
}

/// The chat sheet: a row of chats, the open chat's lines, the field.
///
/// One chat at a time, so no line carries a chat name and the field needs no
/// scope chip: the tab says where the words go. It replaced a composer pill
/// whose hint read "Say something nearby…" under a conversation that was all
/// Everyone (`docs/mockups/chat-overlay`, v3).
///
/// Talking nearby (proximity voice, public) and a private chat's encrypted
/// call start from the tab row, and while on they are a strip in the sheet,
/// so the conversation carries on under the voices.
#[allow(clippy::too_many_arguments)]
pub fn render_chat_sheet(
    mut egui: EguiMut,
    mut chat: ResMut<ChatState>,
    mut overlay: ResMut<ChatOverlay>,
    mut view: ResMut<crate::systems::chats::ChatsView>,
    mut voice: ResMut<crate::systems::voice::VoiceState>,
    mut calls: ResMut<crate::systems::calls::Calls>,
    online: Option<Res<OnlineSession>>,
    mut right_panel: ResMut<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
) {
    let compact = egui.compact();
    let panel_open = !right_panel.is(RightPanel::None);
    // Note the sheet does *not* step aside for the keyboard: it is the one
    // surface allowed over it, and it is what the keys are being typed into.
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
    // `chats::render_chat_world`.
    if !overlay.composing || chat.world.is_some() {
        return;
    }

    // Open on the conversation on screen, once: the chat of the newest line.
    if std::mem::take(&mut overlay.follow) {
        if let Some(i) = conversation_on_screen(&chat) {
            chat.active = i;
        }
    }
    let call_key = calls
        .active_group()
        .map(crate::systems::private_chat::channel_key);
    let keep: Vec<&str> = call_key.as_deref().into_iter().collect();
    let tabs = sheet_tabs(&chat, chat.clock, &keep);
    if !tabs.contains(&chat.active) {
        if let Some(&first) = tabs.first() {
            chat.active = first;
        }
    }
    let Some(channel) = chat.channels.get(chat.active) else {
        overlay.close();
        return;
    };
    let key = channel.key.clone();
    let kind = crate::systems::chats::kind_of(&key);
    let n_lines = channel.lines.len();
    // Read, since it is on screen.
    view.mark_seen(&key, n_lines);

    let field_id = egui::Id::new("chat_composer_field");
    let claim = std::mem::take(&mut overlay.claim_focus);
    let wants_keys = onscreen_keyboard::platform_wants_keyboard();
    // The keys' room is kept while they are up, and on the frame the field
    // asks for them (`claim`): that is the frame they choose their edge on.
    // Tapped into while they are down, the field says it makes room
    // (`field_makes_room`) and the sheet rises on the next frame.
    let keys = if wants_keys && (keyboard.is_open() || claim) {
        keyboard_reserve(&egui, true)
    } else {
        0.0
    };
    let viewport = egui.viewport_pts();
    let rect = sheet_rect(
        viewport,
        compact,
        keys,
        crate::systems::dock::reserved_bottom(),
    );
    overlay.covers = Some(rect);

    // Who is in what, for the strip.
    let voice_on = voice.mode != crate::systems::voice::VoiceMode::Off;
    let muted_voice = voice.mode == crate::systems::voice::VoiceMode::PushToTalk;
    let in_call_here = call_key.as_deref() == Some(key.as_str());
    let in_other_call = calls.active_group().is_some() && !in_call_here;
    let group = crate::systems::private_chat::group_of(&key);
    let others_calling = group.as_ref().is_some_and(|g| !calls.in_call(g).is_empty());
    let voice_people = voice_people(&chat, &voice);
    let call_people: Vec<(u64, String)> = if in_call_here {
        calls.others()
    } else {
        Vec::new()
    };
    let call_status = format!(
        "Call · {} · {}",
        call_people.len() + 1,
        crate::systems::calls::call_clock(calls.elapsed_secs())
    );
    let can_voice =
        crate::systems::voice::VoiceState::can_capture() && !in_other_call && !in_call_here;
    let shout = chat.shout && kind == crate::systems::chats::ChatKind::Here;

    let tab_items: Vec<(usize, String, crate::systems::chats::ChatKind, bool)> = tabs
        .iter()
        .filter_map(|&i| {
            let c = chat.channels.get(i)?;
            let kind = crate::systems::chats::kind_of(&c.key);
            let name = crate::systems::chats::display_name(&c.key, &c.label);
            let unread = i != chat.active
                && kind != crate::systems::chats::ChatKind::Here
                && view.unread_of(&chat, &c.key) > 0;
            Some((i, name, kind, unread))
        })
        .collect();
    let mut lines: Vec<(String, String, crate::systems::chat::ChatLine)> = chat.channels
        [chat.active]
        .lines
        .iter()
        .filter(|l| !chat.is_blocked(&l.sender))
        .map(|l| (key.clone(), String::new(), l.clone()))
        .collect();
    lines.drain(..lines.len().saturating_sub(SHEET_LINES));
    let runs = runs(&lines, |l| chat.is_mine(l));
    let ctx = egui.ctx_mut().clone();
    let mut act = SheetActs::default();

    egui::Area::new(egui::Id::new("chat_sheet"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(&ctx, |ui| {
            let corners = if compact {
                egui::CornerRadius {
                    nw: 22,
                    ne: 22,
                    sw: 0,
                    se: 0,
                }
            } else {
                egui::CornerRadius::same(18)
            };
            let inner = egui::Frame::NONE
                .fill(theme::p().glass)
                .stroke(egui::Stroke::new(1.0, crate::systems::chats::gline()))
                .corner_radius(corners)
                .shadow(lift())
                .inner_margin(egui::Margin::symmetric(10, 6));
            inner.show(ui, |ui| {
                let size = rect.size() - egui::vec2(20.0, 12.0);
                ui.set_min_size(size);
                ui.set_max_size(size);
                ui.spacing_mut().item_spacing.y = 6.0;

                // The grab handle: drag it down to close.
                if compact {
                    let (handle, resp) =
                        ui.allocate_exact_size(egui::vec2(size.x, 10.0), egui::Sense::drag());
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(handle.center(), egui::vec2(38.0, 4.0)),
                        egui::CornerRadius::same(2),
                        theme::p().line,
                    );
                    if resp.drag_stopped() && resp.drag_delta().y >= 0.0 {
                        let moved = ui.input(|i| {
                            i.pointer
                                .press_origin()
                                .zip(i.pointer.interact_pos())
                                .map_or(0.0, |(a, b)| b.y - a.y)
                        });
                        if moved > 40.0 {
                            act.close = true;
                        }
                    }
                }

                // ── The tab row ────────────────────────────────────────────
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::systems::chats::circle_button(
                            ui,
                            icons::CLEAR,
                            false,
                            true,
                            "Close the chat",
                        )
                        .clicked()
                        {
                            act.close = true;
                        }
                        match kind {
                            crate::systems::chats::ChatKind::Here if can_voice => {
                                let tip = if voice_on {
                                    "Stop talking nearby"
                                } else {
                                    "Talk with the people near you (public)"
                                };
                                if voice_key(ui, voice_on, tip).clicked() {
                                    act.voice = Some(if voice_on {
                                        crate::systems::voice::VoiceMode::Off
                                    } else {
                                        crate::systems::voice::VoiceMode::Open
                                    });
                                }
                            }
                            crate::systems::chats::ChatKind::Private
                            | crate::systems::chats::ChatKind::Topic
                                if !in_call_here =>
                            {
                                if let Some(g) = group.clone() {
                                    let tip = if others_calling {
                                        "Join the call"
                                    } else {
                                        "Call (end-to-end encrypted)"
                                    };
                                    if crate::systems::chats::circle_button(
                                        ui,
                                        icons::CALL,
                                        others_calling,
                                        true,
                                        tip,
                                    )
                                    .clicked()
                                    {
                                        act.call =
                                            Some(crate::systems::calls::CallRequest::Join(g));
                                    }
                                }
                            }
                            _ => {}
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            egui::ScrollArea::horizontal()
                                .id_salt("chat_sheet_tabs")
                                .scroll_bar_visibility(
                                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                )
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        for (i, name, kind, unread) in &tab_items {
                                            if tab(ui, name, *kind, *i == chat.active, *unread)
                                                .clicked()
                                            {
                                                act.tab = Some(*i);
                                            }
                                        }
                                    });
                                });
                        });
                    });
                });

                // Who reads this chat, in one quiet line.
                ui.label(
                    egui::RichText::new(crate::systems::chats::subtitle(kind))
                        .size(NAME_PT - 1.0)
                        .color(match kind {
                            crate::systems::chats::ChatKind::Private
                            | crate::systems::chats::ChatKind::Topic => theme::p().text_dim,
                            _ => PUBLIC_INK,
                        }),
                );

                // ── The strip: the voices, or the call ─────────────────────
                if kind == crate::systems::chats::ChatKind::Here && voice_on {
                    strip(ui, |ui| {
                        for (name, speaking) in &voice_people {
                            face(ui, name, *speaking);
                        }
                        let talker = voice_people
                            .iter()
                            .find(|(_, s)| *s)
                            .map_or("nobody is talking", |(n, _)| n.as_str());
                        strip_label(ui, "Talking nearby", &format!("public · {talker}"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::call_button(
                                ui,
                                icons::CLEAR,
                                "Stop talking nearby",
                                theme::CallTone::HangUp,
                                theme::CALL_BUTTON_SMALL,
                            )
                            .clicked()
                            {
                                act.voice = Some(crate::systems::voice::VoiceMode::Off);
                            }
                            let (icon, tip, tone) = if muted_voice {
                                (icons::MIC_OFF, "Unmute", theme::CallTone::Off)
                            } else {
                                (icons::MIC, "Mute", theme::CallTone::On)
                            };
                            if theme::call_button(ui, icon, tip, tone, theme::CALL_BUTTON_SMALL)
                                .clicked()
                            {
                                act.voice = Some(if muted_voice {
                                    crate::systems::voice::VoiceMode::Open
                                } else {
                                    crate::systems::voice::VoiceMode::PushToTalk
                                });
                            }
                        });
                    });
                } else if in_call_here {
                    strip(ui, |ui| {
                        strip_label(
                            ui,
                            &call_status,
                            &format!("{} end-to-end encrypted", icons::LOCK),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::call_button(
                                ui,
                                icons::CALL_END,
                                "Hang up",
                                theme::CallTone::HangUp,
                                theme::CALL_BUTTON_SMALL,
                            )
                            .clicked()
                            {
                                act.call = Some(crate::systems::calls::CallRequest::Leave);
                            }
                            if theme::call_button(
                                ui,
                                icons::OPEN_EXTERNAL,
                                "Full screen",
                                theme::CallTone::On,
                                theme::CALL_BUTTON_SMALL,
                            )
                            .clicked()
                            {
                                act.unfold_call = true;
                            }
                            if crate::systems::calls::camera_available() {
                                let (icon, tip, tone) = if calls.camera_on {
                                    (icons::VIDEOCAM, "Camera off", theme::CallTone::On)
                                } else {
                                    (icons::VIDEOCAM_OFF, "Camera on", theme::CallTone::Off)
                                };
                                if theme::call_button(ui, icon, tip, tone, theme::CALL_BUTTON_SMALL)
                                    .clicked()
                                {
                                    act.toggle_camera = true;
                                }
                            }
                            let (icon, tip, tone) = if calls.muted {
                                (icons::MIC_OFF, "Unmute", theme::CallTone::Off)
                            } else {
                                (icons::MIC, "Mute", theme::CallTone::On)
                            };
                            if theme::call_button(ui, icon, tip, tone, theme::CALL_BUTTON_SMALL)
                                .clicked()
                            {
                                act.toggle_mic = true;
                            }
                        });
                    });
                }

                // ── The lines ──────────────────────────────────────────────
                let below = composer_height() + 4.0;
                let lines_h = (ui.available_height() - below).max(40.0);
                let column = ui.available_width();
                egui::ScrollArea::vertical()
                    .id_salt(("chat_sheet_lines", key.as_str()))
                    .max_height(lines_h)
                    .min_scrolled_height(lines_h)
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(column);
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if runs.is_empty() {
                            ui.add_space(lines_h * 0.35);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("Nothing said yet")
                                        .size(NAME_PT)
                                        .color(theme::p().text_dim),
                                );
                            });
                        }
                        // A short conversation sits on the field, as a
                        // messenger's does, not at the top of the sheet: the
                        // runs are pushed down by what they measured last frame.
                        let measured = egui::Id::new("chat_sheet_runs_h");
                        let last_h: f32 = ui.data(|d| d.get_temp(measured)).unwrap_or(lines_h);
                        if !runs.is_empty() && last_h < lines_h {
                            ui.add_space(lines_h - last_h - 6.0);
                        }
                        let top = ui.cursor().top();
                        for run in &runs {
                            if let Some(hit) = sheet_run(ui, run, column) {
                                act.person = Some((run.sender.clone(), hit));
                            }
                        }
                        let h = ui.cursor().top() - top;
                        ui.data_mut(|d| d.insert_temp(measured, h));
                    });

                // ── The field ──────────────────────────────────────────────
                if overlay.plus {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        if kind == crate::systems::chats::ChatKind::Here
                            && chip(ui, &format!("{} Shout", icons::FLAG), shout).clicked()
                        {
                            act.toggle_shout = true;
                        }
                        if chip(
                            ui,
                            &format!("{} Pictures, points and plans", icons::ADD),
                            false,
                        )
                        .on_hover_text("Open this chat in Community")
                        .clicked()
                        {
                            act.open_in_community = true;
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if crate::systems::chats::circle_button(
                        ui,
                        icons::ADD,
                        overlay.plus || shout,
                        true,
                        "More",
                    )
                    .clicked()
                    {
                        overlay.plus = !overlay.plus;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = !chat.input.trim().is_empty();
                        let send = crate::systems::chats::circle_button(
                            ui,
                            icons::SEND,
                            true,
                            ready,
                            "Send",
                        );
                        let hint = if shout {
                            "Shout to the street…"
                        } else {
                            "Message"
                        };
                        let field = egui::Frame::NONE
                            .fill(theme::p().surface_raised)
                            .corner_radius(egui::CornerRadius::same(19))
                            .inner_margin(egui::Margin::symmetric(14, 9))
                            .show(ui, |ui| {
                                // `desired_width`, not `add_sized`: this layout's
                                // available *height* is the rest of the row.
                                crate::systems::text_touch::field(
                                    ui,
                                    egui::TextEdit::singleline(&mut chat.input)
                                        .id(field_id)
                                        .frame(egui::Frame::NONE)
                                        .font(egui::FontId::proportional(BODY_PT))
                                        .text_color(theme::p().text)
                                        .desired_width(ui.available_width())
                                        .hint_text(
                                            egui::RichText::new(hint).color(theme::p().line),
                                        ),
                                )
                            })
                            .inner;
                        if claim {
                            field.request_focus();
                        }
                        let entered =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        act.send = (send.clicked() || entered) && ready;
                    });
                });
            });
        });
    if wants_keys {
        // The sheet lifts itself onto the keys: the keyboard must not dodge
        // to the top edge on the frame the field is tapped.
        onscreen_keyboard::field_makes_room(&ctx, field_id);
    }

    // ── Acting on it ──────────────────────────────────────────────────────
    if let Some(i) = act.tab {
        if i != chat.active {
            chat.active = i;
            overlay.plus = false;
        }
    }
    if act.toggle_shout {
        chat.shout = !chat.shout;
    }
    if let Some(mode) = act.voice {
        voice.set_mode(mode);
    }
    if let Some(request) = act.call {
        calls.request = Some(request);
    }
    if act.unfold_call {
        calls.minimized = false;
    }
    if act.toggle_mic {
        calls.muted = !calls.muted;
    }
    if act.toggle_camera {
        calls.camera_on = !calls.camera_on;
    }
    if act.open_in_community {
        view.open_chat(&key);
        *right_panel = RightPanel::Chat;
        overlay.close();
    }
    if let Some((name, action)) = act.person {
        person_act(&mut chat, online.as_deref(), name, action);
    }
    if act.send {
        send(&mut chat, online.as_deref());
        // Stay open and keep the caret: a conversation is more than one line.
        overlay.claim_focus = true;
    }
    if act.close {
        overlay.close();
    }
}

/// Send what is in the field to the open chat.
fn send(chat: &mut ChatState, online: Option<&OnlineSession>) {
    let private = chat
        .channels
        .get(chat.active)
        .filter(|c| ChatState::is_private(&c.key))
        .map(|c| c.key.clone());
    let body = chat.input.trim().to_string();
    if let Some(key) = private {
        // Sealed and sent by `systems::private_chat`. This branch comes first
        // and returns nothing to the plain path: a private line must never
        // reach `ChatSend`, where the server would read and store it.
        chat.private_outbox.push((key, body));
    } else if let (Some(session), Some(channel)) = (online, chat.channels.get(chat.active)) {
        // The session is what *sends*, not what draws: a scripted `--ui chat`
        // capture has none.
        let scope = crate::systems::chat::scope_for(&channel.key, chat.shout);
        session.send(
            "chat send",
            NetMessage::ChatSend(msg::ChatSend {
                request_id: 0,
                scope,
                body,
            }),
        );
    }
    chat.input.clear();
}

/// Do what a speaker's name menu asked.
fn person_act(
    chat: &mut ChatState,
    online: Option<&OnlineSession>,
    username: String,
    action: PersonAction,
) {
    match action {
        PersonAction::Message => chat.private_start.push(username),
        PersonAction::Block => {
            if let Some(session) = online {
                session.send(
                    "set blocked",
                    NetMessage::SetBlocked(msg::SetBlocked {
                        request_id: 0,
                        username,
                        blocked: true,
                    }),
                );
            }
        }
        PersonAction::Report(message) => {
            chat.report = Some(ReportDraft {
                username,
                message,
                reason: ReportReason::Harassment,
                note: String::new(),
                // Raised from a chat line, so it is about what somebody said
                // rather than about an object they put in the street.
                asset: None,
                community: None,
            });
        }
        // The sheet holds Here, Everyone and private chats: no community, no
        // moderation; those live on the panel.
        _ => {}
    }
}

/// Who is heard nearby, by name, and whether they are talking now: the people
/// voice is playing, named from what they last said in Here (a voice frame
/// carries only an id).
fn voice_people(
    chat: &ChatState,
    voice: &crate::systems::voice::VoiceState,
) -> Vec<(String, bool)> {
    let here = chat.channels.iter().find(|c| c.key == "local");
    let name_of = |id: u64| {
        here.and_then(|c| c.lines.iter().rev().find(|l| l.sender_id == id))
            .map_or_else(|| "Someone".to_string(), |l| l.sender.clone())
    };
    let mut out: Vec<(String, bool)> = voice
        .speakers
        .iter()
        .map(|(id, s)| (name_of(*id), s.talking()))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let me = chat.my_name.clone().unwrap_or_else(|| "You".into());
    out.push((me, voice.transmitting));
    out
}

/// One chat's tab: its disc, its name, a dot when something waits in it.
fn tab(
    ui: &mut egui::Ui,
    name: &str,
    kind: crate::systems::chats::ChatKind,
    on: bool,
    unread: bool,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        name.to_string(),
        egui::FontId::proportional(NAME_PT + 0.5),
        if on {
            theme::p().cream
        } else {
            theme::p().text_dim
        },
    );
    let disc = 20.0;
    let pad = egui::vec2(8.0, 6.0);
    let dot = if unread { 10.0 } else { 0.0 };
    let size = egui::vec2(pad.x * 2.0 + disc + 6.0 + galley.size().x + dot, TAB_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let resp = theme::named(ui, resp, name);
    let p = ui.painter();
    let (fill, stroke) = if on {
        (
            theme::p().accent_muted,
            egui::Stroke::new(1.0, theme::p().accent),
        )
    } else if resp.hovered() {
        (theme::p().surface_hover, egui::Stroke::NONE)
    } else {
        (theme::p().surface_raised, egui::Stroke::NONE)
    };
    p.rect(
        rect,
        egui::CornerRadius::same(12),
        fill,
        stroke,
        egui::StrokeKind::Inside,
    );
    let c = egui::pos2(rect.min.x + pad.x + disc / 2.0, rect.center().y);
    crate::systems::chats::paint_avatar(p, c, name, kind, disc);
    let text_at = egui::pos2(
        c.x + disc / 2.0 + 6.0,
        rect.center().y - galley.size().y / 2.0,
    );
    let text_w = galley.size().x;
    p.galley(text_at, galley, theme::p().cream);
    if unread {
        p.circle_filled(
            egui::pos2(text_at.x + text_w + 7.0, rect.center().y),
            3.5,
            theme::p().accent,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The round key that starts talking nearby: a microphone, green while on.
fn voice_key(ui: &mut egui::Ui, on: bool, tip: &str) -> egui::Response {
    if on {
        theme::call_button(ui, icons::MIC, tip, theme::CallTone::Accept, 38.0)
    } else {
        crate::systems::chats::circle_button(ui, icons::MIC, false, true, tip)
    }
}

/// A small choice chip under the field.
fn chip(ui: &mut egui::Ui, text: &str, on: bool) -> egui::Response {
    let resp = egui::Frame::NONE
        .fill(if on {
            theme::p().accent_muted
        } else {
            theme::p().surface_raised
        })
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(NAME_PT).color(if on {
                theme::p().cream
            } else {
                theme::p().text
            }));
        })
        .response;
    let resp = ui.interact(resp.rect, ui.next_auto_id(), egui::Sense::click());
    theme::named(ui, resp, text).on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The voice or call strip: a green card across the sheet.
fn strip(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    let width = ui.available_width();
    egui::Frame::NONE
        .fill(VOICE_FILL)
        .stroke(egui::Stroke::new(
            1.0,
            theme::CALL_ACCEPT_GREEN.gamma_multiply(0.6),
        ))
        .corner_radius(egui::CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(width - 20.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                body(ui);
            });
        });
}

/// The strip's two lines: what it is, and its detail.
fn strip_label(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(
            egui::RichText::new(title)
                .size(BODY_PT - 0.5)
                .strong()
                .color(theme::DARK.cream),
        );
        ui.label(
            egui::RichText::new(detail)
                .size(NAME_PT - 1.0)
                .color(theme::DARK.cream.gamma_multiply(0.75)),
        );
    });
}

/// A person heard nearby: their initial, ringed green while they talk.
fn face(ui: &mut egui::Ui, name: &str, speaking: bool) {
    let d = 30.0;
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(d), egui::Sense::hover());
    let p = ui.painter();
    if speaking {
        p.circle_stroke(
            rect.center(),
            d / 2.0 + 2.0,
            egui::Stroke::new(2.5, TALKING_RING),
        );
    }
    crate::systems::chats::paint_avatar(
        p,
        rect.center(),
        name,
        crate::systems::chats::ChatKind::Private,
        d,
    );
}

/// One run of lines in the sheet, at the messenger's size: someone else's
/// under their round initial with their name once, in their own colour, then
/// their lines; yours on the right, each in a quiet accent bubble. Answers an
/// act from the speaker's name menu.
fn sheet_run(ui: &mut egui::Ui, run: &Run, column: f32) -> Option<PersonAction> {
    use crate::systems::chats::{self, ChatKind};
    let mut hit = None;
    if run.mine {
        ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let last = run.lines.len().saturating_sub(1);
            for (i, line) in run.lines.iter().enumerate() {
                egui::Frame::NONE
                    .fill(theme::p().accent_muted)
                    .corner_radius(egui::CornerRadius {
                        nw: 14,
                        sw: 14,
                        ne: if i == 0 { 14 } else { 4 },
                        se: if i == last { 4 } else { 4 },
                    })
                    .inner_margin(egui::Margin::symmetric(11, 5))
                    .show(ui, |ui| {
                        ui.set_max_width(column * 0.78);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(summary(line))
                                    .size(BODY_PT)
                                    .color(theme::p().cream),
                            )
                            .wrap(),
                        );
                    });
            }
        });
        return None;
    }
    let tone = chats::tone_for(&run.sender);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        chats::avatar(ui, &run.sender, ChatKind::Private, FACE_PT);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.set_max_width(column - FACE_PT - 8.0);
            // The name is the handle for what you can do *about* a person.
            let name = ui.add(
                egui::Label::new(
                    egui::RichText::new(&run.sender)
                        .size(NAME_PT)
                        .strong()
                        .color(chats::blend(tone, egui::Color32::WHITE, 0.5)),
                )
                .sense(egui::Sense::click()),
            );
            let last_body = run.lines.last().map(|l| l.body.as_str());
            person_menu(
                &name,
                &run.sender,
                last_body,
                &crate::systems::chat::PersonExtras::default(),
                |action| hit = Some(action),
            );
            for line in &run.lines {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(summary(line))
                            .size(BODY_PT)
                            .color(theme::p().text),
                    )
                    .wrap(),
                );
            }
        });
    });
    hit
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

    /// Here and Everyone always have a tab; a private chat only while it has
    /// something from the last hour, or is the one being written in or called
    /// in — newest first.
    #[test]
    fn the_tabs_are_the_public_chats_and_the_recent_private_ones() {
        let mut chat = ChatState::default();
        chat.open_channel("dm:old", "🔒 Old");
        chat.open_channel("dm:new", "🔒 New");
        chat.open_channel("dm:call", "🔒 Call");
        chat.clock = 10.0;
        chat.receive("dm:old", 3, "old".into(), "long ago".into(), None);
        chat.receive("dm:call", 4, "call".into(), "long ago too".into(), None);
        chat.clock = 10.0 + TAB_RECENT_SECS + 60.0;
        chat.receive("dm:new", 5, "new".into(), "just now".into(), None);
        let here = chat.channels.iter().position(|c| c.key == "local");
        chat.active = here.expect("Here is a channel");
        let keys = |tabs: Vec<usize>| -> Vec<String> {
            tabs.into_iter()
                .map(|i| chat.channels[i].key.clone())
                .collect()
        };
        assert_eq!(
            keys(sheet_tabs(&chat, chat.clock, &[])),
            vec!["local", "geo:", "dm:new"]
        );
        assert_eq!(
            keys(sheet_tabs(&chat, chat.clock, &["dm:call"])),
            vec!["local", "geo:", "dm:new", "dm:call"]
        );
    }

    /// On a phone the sheet sits on the keys and leaves the world the top
    /// third; with the keys down it takes the lower half. On a wide screen it is
    /// a card above the switch.
    #[test]
    fn the_sheet_sits_on_the_keys_and_leaves_the_top_of_the_world() {
        let phone = egui::vec2(400.0, 900.0);
        let keys = sheet_rect(phone, true, 320.0, 70.0);
        assert_eq!(keys.bottom(), 900.0 - 320.0);
        assert!(keys.top() >= 900.0 * SHEET_TOP_FRACTION - 0.5);
        assert_eq!(keys.width(), 400.0);
        let rest = sheet_rect(phone, true, 0.0, 70.0);
        assert_eq!(rest.bottom(), 900.0);
        assert!((rest.top() - 450.0).abs() < 0.5);
        // Keys taller than the room leave a usable sheet, never a sliver.
        let squeezed = sheet_rect(egui::vec2(400.0, 640.0), true, 300.0, 70.0);
        assert!(squeezed.height() >= SHEET_MIN_HEIGHT - 0.5);
        let desk = sheet_rect(egui::vec2(1400.0, 900.0), false, 0.0, 70.0);
        assert_eq!(desk.width(), SHEET_CARD_WIDTH);
        assert!(desk.bottom() <= 900.0 - 70.0);
        assert_eq!(desk.left(), theme::OSD_MARGIN);
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
