//! Chat over the map: a bubble above whoever spoke, a chip with the newest
//! line, and a sheet that holds one chat at a time — with talking nearby and a
//! private chat's call in it.
//!
//! **The panel was the problem.** On a compact viewport a right panel is the
//! whole screen, so in a client whose
//! Local channel is scoped by *how far away someone is standing*, saying
//! anything meant covering the world and the people in it. This module is chat
//! with no panel: floating pieces, none of which is up unless it has something
//! to say. `docs/notes/chat-without-a-panel.md` has the design and what it
//! replaced; [`crate::systems::chat`] keeps the state and the log.
//!
//! The three systems here decide everything — whether the overlay may show,
//! what it says, where it goes — and publish it in [`ChatOverlay::lib_view`];
//! the component library draws it (`lib_chat_overlay`) and hands its taps
//! back ([`ChatOverlay::lib_chip`], [`ChatOverlay::lib_sheet`]).
//!
//! ## The three pieces
//!
//! **[`place_chat_bubbles`]** puts a Local line over the head of the person who
//! said it. `NetMessage::ChatMessage` already carries `sender_id`, which is the
//! same number as `RemoteAvatar::sim_id`, so the line finds its entity with no
//! name matching and no protocol change.
//!
//! **[`drive_chat_chip`]** is the map at rest: one line over the switch, the
//! newest message with its chat's disc and the unread count, gone after
//! [`CHIP_HOLD_SECS`]. A tap opens the sheet on that chat. While talking nearby
//! is on it stays, with mute and leave on it.
//!
//! **[`drive_chat_sheet`]** is chat while you are in it: a row of tabs (Here,
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
//! **The chip senses taps only while it can be read.** A chip that is
//! merely *faded* would still eat the taps aimed at the map under it — the
//! trap `place_pins` culls for and `map_osd` hides for ([`ChipView::live`]).
//!
//! **A bubble is drawn, never interactive**: it sits *over the world*, where
//! a tap means pick, walk or place, and a chat bubble is not allowed to be in
//! the way of that.
//!
//! **The sheet takes the keyboard's height on the frame the keys choose their
//! edge.** `onscreen_keyboard::layout` picks its edge from where the focused
//! field landed *this* frame, and flips to the **top** when the bottom slab
//! would cover it. So the sheet reserves [`onscreen_keyboard::expected_height`]
//! while the keys are up and on the frame its field claims them.
//!
//! **Everything ages on [`ChatState::clock`], not on `sent_at_ms`.** See that
//! field: a fade is about how long *this screen* has shown a line.

use bevy::math::DVec3;
use bevy::prelude::*;
use big_space::prelude::GridCell;
use cartopolis_core::msg;

use crate::components::{Avatar, FreeCamera, RemoteAvatar};
use crate::resources::{OnlineSession, RightPanel};
use crate::systems::chat::{ChatState, PersonAction, ReportDraft};
use crate::systems::icons;
use crate::systems::onscreen_keyboard;
use crate::systems::theme;
use crate::systems::ui_input::{UiInput, UiMetrics};
use cartopolis_core::{NetMessage, ReportReason};

/// How recent the newest line must be for the sheet to open on its chat.
const HOLD_SECS: f64 = 10.0;
const FADE_SECS: f64 = 4.0;
/// The chip holds the newest line this long, then fades over
/// [`CHIP_FADE_SECS`]: half a minute, the time a remark stays news.
const CHIP_HOLD_SECS: f64 = 30.0;
const CHIP_FADE_SECS: f64 = 4.0;
/// Widest the chip gets, in points. Fixed rather than sized to content: a
/// line as long as the newest message would otherwise take the screen.
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
    /// Make room for the keyboard on the next frame the sheet is up, and take
    /// it in a chat's World view ([`Self::take_claim`]). Set when the sheet
    /// opens and when a message is sent (so a reply needs no second tap),
    /// and cleared once claimed.
    claim_focus: bool,
    /// Open on the conversation on screen, once: without it the sheet kept
    /// whichever chat it was last left on, while the line that made you open
    /// it was in another.
    follow: bool,
    /// The row of extras under the field (Shout, the full chat) is open.
    plus: bool,
    /// Where the chip or the sheet was placed last frame, in UI points, so a
    /// bubble over a head does not land on it.
    covers: Option<Rect>,
    /// `--ui chat` seeded this run and there is no session behind it.
    ///
    /// The only thing it unlocks is the [`suppressed`] test for a session —
    /// which is otherwise the difference between a feature that can be
    /// screenshotted before it is merged and one that cannot. Every scripted
    /// capture is offline (`--shot` implies it), so without this the overlay is
    /// literally unphotographable in the harness the repo checks its UI with.
    /// Set nowhere but `shot_harness::open_scripted_ui`.
    pub demo: bool,
    /// What the library (`systems::lib_chat_overlay`) draws this frame.
    pub lib_view: ChatOverlayView,
    /// A tap on the library's chip, done by [`drive_chat_chip`].
    pub lib_chip: Option<ChipAct>,
    /// What the library's sheet asked for, done by [`drive_chat_sheet`].
    pub lib_sheet: SheetActs,
}

/// The overlay as the library draws it: placed, measured and worded by
/// this module's systems.
#[derive(Default, Clone, Debug)]
pub struct ChatOverlayView {
    pub bubbles: Vec<BubbleItem>,
    /// The chip, and how strongly it shows (it fades).
    pub chip: Option<(ChipView, f32)>,
    pub sheet: Option<SheetView>,
}

/// A bubble over a head, placed: its bottom-left corner, its width as its
/// words measure ([`text_size`]), the tail's distance from its left edge.
#[derive(Clone, Debug, PartialEq)]
pub struct BubbleItem {
    pub key: String,
    pub left: f32,
    pub bottom: f32,
    pub width: f32,
    pub tail: f32,
    pub alpha: f32,
    pub speaker: String,
    /// The speaker's name's colour: their tone, lightened (content).
    pub tone: [u8; 3],
    pub bodies: Vec<String>,
}

/// The chip, placed and worded.
#[derive(Clone, Debug, PartialEq)]
pub struct ChipView {
    /// Left, bottom (from the viewport's bottom) and width, in points.
    pub left: f32,
    pub bottom: f32,
    pub width: f32,
    /// It takes taps only while it can be read.
    pub live: bool,
    /// The chat's disc: its name and kind.
    pub chat: (String, crate::systems::discs::ChatKind),
    /// The key of the chat the newest line is in: what a tap opens.
    pub open: Option<String>,
    /// Who said the newest line, in their colour, or none while only
    /// talking nearby keeps the chip up.
    pub sender: Option<(String, [u8; 3])>,
    pub body: String,
    pub unread: usize,
    pub talking: bool,
    pub muted: bool,
}

/// What a tap on the library's chip asked for.
#[cfg_attr(not(feature = "ui-lib"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
pub enum ChipAct {
    /// Open the sheet, on this chat.
    Open(Option<String>),
    Mute,
    Leave,
}

/// The sheet, placed and worded.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetView {
    /// Left, top, width, height, in points.
    pub rect: [f32; 4],
    pub compact: bool,
    /// Each tab: its chat's index, name, kind, whether something waits in
    /// it, whether it is the open one.
    pub tabs: Vec<(usize, String, crate::systems::discs::ChatKind, bool, bool)>,
    pub kind: crate::systems::discs::ChatKind,
    /// Who reads this chat, and whether that is the public.
    pub subtitle: String,
    pub public: bool,
    /// The round key beside the tabs: talking nearby (on or not), or a call
    /// (others are in it or not).
    pub voice_key: Option<bool>,
    pub call_key: Option<bool>,
    pub strip: Option<SheetStrip>,
    pub runs: Vec<RunView>,
    /// The extras under the field are open, Shout is on, the open chat is
    /// Here, the field has something to send.
    pub plus: bool,
    pub shout: bool,
    pub here: bool,
    pub ready: bool,
}

/// The strip across the sheet: the voices nearby, or the call.
#[derive(Clone, Debug, PartialEq)]
pub enum SheetStrip {
    Voice {
        /// Who is heard, and whether they talk now.
        people: Vec<(String, bool)>,
        talker: String,
        muted: bool,
    },
    Call {
        status: String,
        /// Whether the camera is on, when there is one.
        camera: Option<bool>,
        muted: bool,
    },
}

/// One speaker's run of lines in the sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct RunView {
    pub sender: String,
    pub mine: bool,
    pub tone: [u8; 3],
    pub lines: Vec<String>,
    /// The last line's words, for a report raised from the name's menu.
    pub last: Option<String>,
}

/// A speaker's name's colour: their tone, half way to white.
fn name_tone(speaker: &str) -> [u8; 3] {
    let tone = crate::systems::chats::tone_for(speaker);
    let lighten = |c: u8| ((f32::from(c) + 255.0) / 2.0).round() as u8;
    [lighten(tone.r()), lighten(tone.g()), lighten(tone.b())]
}

/// The size `text` takes at `size` points in the body face, wrapped at
/// `wrap` points (greedily, at spaces and newlines): near enough to place a
/// bubble by, which the library then draws with its own text layout.
/// Measured with `fontdue` over [`theme::BODY_TTF`], the face the library's
/// text and the bubbles' words are set in.
#[must_use]
pub(crate) fn text_size(text: &str, size: f32, wrap: f32) -> Vec2 {
    static FONT: std::sync::OnceLock<Option<fontdue::Font>> = std::sync::OnceLock::new();
    let font = FONT
        .get_or_init(|| {
            fontdue::Font::from_bytes(theme::BODY_TTF, fontdue::FontSettings::default()).ok()
        })
        .as_ref();
    let line_height = font
        .and_then(|f| f.horizontal_line_metrics(size))
        .map_or(size * 1.3, |m| m.new_line_size);
    let advance = |c: char| font.map_or(size * 0.5, |f| f.metrics(c, size).advance_width);
    let space = advance(' ');
    let mut widest = 0.0_f32;
    let mut lines = 0_usize;
    for paragraph in text.split('\n') {
        lines += 1;
        let mut line = 0.0_f32;
        for (i, word) in paragraph.split(' ').enumerate() {
            let w: f32 = word.chars().map(advance).sum();
            if i == 0 {
                line = w;
            } else if line + space + w > wrap && line > 0.0 {
                widest = widest.max(line);
                lines += 1;
                line = w;
            } else {
                line += space + w;
            }
        }
        widest = widest.max(line);
    }
    Vec2::new(widest.min(wrap), lines as f32 * line_height)
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
    // A panel on a compact viewport *is* the screen, and these float over
    // panels — the dock's rule verbatim. On a wide viewport the panel docks
    // right and the map area is real, so the overlay stays and culls itself
    // against it.
    compact && panel_open
}

/// Subtract the on-screen keyboard from a map area.
///
/// Culling, not clipping, and the reason is `place_pins`'s verbatim: the slab
/// floats over the map and reserves nothing in the map's rect, so that rect
/// cannot know about it.
#[must_use]
fn clear_of_keyboard(mut rect: Rect, slab: Option<Rect>) -> Rect {
    let Some(slab) = slab else { return rect };
    if slab.min.y > rect.min.y {
        rect.max.y = rect.max.y.min(slab.min.y);
    } else {
        rect.min.y = rect.min.y.max(slab.max.y);
    }
    rect
}

/// The map's rect in UI points: what the panels left of the screen at the
/// end of the last pass (`EguiVerdict::map_rect`, in window points), or the
/// whole viewport before there is one, less the on-screen keyboard.
fn map_rect(
    ui_input: &UiInput,
    metrics: &UiMetrics,
    keyboard: &onscreen_keyboard::OnScreenKeyboard,
) -> Rect {
    let map = ui_input.egui_verdict().map_rect.map_or_else(
        || Rect::from_corners(Vec2::ZERO, metrics.viewport_pts()),
        |r| Rect::from_corners(metrics.window_to_ui(r.min), metrics.window_to_ui(r.max)),
    );
    let slab = keyboard
        .slab()
        .map(|s| Rect::new(s.min.x, s.min.y, s.max.x, s.max.y));
    clear_of_keyboard(map, slab)
}

// ─── The bubbles ──────────────────────────────────────────────────────────────

/// Place each speaker's recent Local lines over their head, for the library
/// to draw ([`ChatOverlayView::bubbles`]).
///
/// Projected exactly the way `place_pins` projects a saved place, and for the
/// same two reasons: the camera basis is rebuilt from **this** frame's
/// `Transform` (big_space propagates in `PostUpdate`, so the `GlobalTransform`
/// in the world is the previous frame's, and a bubble built from it swims over
/// an avatar that is standing still), and the position is taken as a
/// camera-relative delta in f64 so the f32 projection never has to cancel the
/// camera's own offset back out.
#[allow(clippy::too_many_arguments)]
pub fn place_chat_bubbles(
    metrics: Res<UiMetrics>,
    ui_input: Res<UiInput>,
    chat: Res<ChatState>,
    mut overlay: ResMut<ChatOverlay>,
    online: Option<Res<OnlineSession>>,
    right_panel: Res<RightPanel>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
    camera: Query<(&Camera, &GridCell, &Transform), With<FreeCamera>>,
    remotes: Query<(&RemoteAvatar, &Transform), Without<FreeCamera>>,
    local: Query<(&GridCell, &Transform), (With<Avatar>, Without<FreeCamera>)>,
) {
    // Placed here, drawn by the library. An early return leaves it none.
    overlay.lib_view.bubbles.clear();
    let mut bubbles: Vec<BubbleItem> = Vec::new();
    let compact = metrics.compact();
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
    // The keyboard is subtracted by hand because it reserves nothing in the
    // map's rect; bubbles stay up while the composer is open, which is the
    // point — you can see who you are answering.
    let map_rect = map_rect(&ui_input, &metrics, &keyboard);
    if map_rect.width() < BUBBLE_WIDTH || map_rect.height() < 3.0 * theme::SIZE_BODY {
        return;
    }
    let ui_scale = metrics.ui_scale();

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
        let head = Vec2::new(screen.x / ui_scale, screen.y / ui_scale);
        if !map_rect.contains(head) {
            continue;
        }
        // The transcript carries the same words; a balloon that would land on
        // it is left out rather than drawn over it — the two in a tangle were
        // the worst of both. Last frame's rect, as the chip and the sheet are
        // placed after this; their size changes by a line at a time.
        if let Some(transcript) = overlay.covers {
            let reach = Rect::new(
                head.x - BUBBLE_WIDTH / 2.0,
                head.y - BUBBLE_REACH,
                head.x + BUBBLE_WIDTH / 2.0,
                head.y,
            );
            if !reach.intersect(transcript).is_empty() {
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
        bubbles.push(bubble_item(map_rect, id, head, alpha, speaker, bodies));
    }
    overlay.lib_view.bubbles = bubbles;
}

/// Gap between the head and the bottom of a balloon, in points.
const BUBBLE_GAP: f32 = 10.0;

/// One speech balloon, placed: centred over the head, [`BUBBLE_GAP`] above
/// it, as wide as its widest line wrapped at [`BUBBLE_WIDTH`], and kept on
/// the map. The speaker's name is in the colour their initial has in the
/// sheet, so the balloon and the line it also appears as read as one person.
fn bubble_item(
    map_rect: Rect,
    id: u64,
    head: Vec2,
    alpha: f32,
    speaker: String,
    bodies: Vec<String>,
) -> BubbleItem {
    let column = BUBBLE_WIDTH - 22.0;
    let name = text_size(&speaker, NAME_PT, column);
    let (mut wide, mut high) = (name.x, name.y);
    for body in &bodies {
        let g = text_size(body, BODY_PT, column);
        wide = wide.max(g.x);
        high += g.y + 1.0;
    }
    let width = wide.min(column) + 22.0;
    let height = high + 14.0;
    let left = (head.x - width / 2.0)
        .min(map_rect.max.x - width)
        .max(map_rect.min.x);
    let bottom = (head.y - BUBBLE_GAP)
        .max(map_rect.min.y + height)
        .min(map_rect.max.y);
    BubbleItem {
        key: format!("bubble:{id}"),
        left,
        bottom,
        width,
        // Over the speaker, wherever the map pushed the box.
        // (`max`, or a bubble narrower than two tails would panic `clamp`.)
        tail: (head.x - left).clamp(14.0, (width - 14.0).max(14.0)),
        alpha,
        tone: name_tone(&speaker),
        speaker,
        bodies,
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
pub fn drive_chat_chip(
    metrics: Res<UiMetrics>,
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
    interact: Res<crate::systems::interact::InteractState>,
) {
    // Where it was placed, for the bubbles to keep clear of; nowhere until it is.
    if !overlay.composing {
        overlay.covers = None;
    }
    // The library's tap of the last frame, done below if the chip is still
    // up; and no chip for it until this frame says there is one.
    overlay.lib_view.chip = None;
    let lib_act = overlay.lib_chip.take();
    let compact = metrics.compact();
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

    let viewport = metrics.viewport_pts();
    let cluster = crate::systems::map_osd::cluster_for(
        camera_state.mode,
        driver.active,
        panel_open,
        keyboard.is_open(),
        theme::touch_layout() || touch.ever(),
    );
    let corner = crate::systems::map_osd::reserved_left_bottom(cluster);
    // Above the interaction prompt (`interact::render_interaction_prompt`,
    // which runs before this) when it is up: it sits in the same band over
    // the switch, and a chip placed over its buttons hid them.
    let corner = match interact.lib_view.as_ref() {
        Some(prompt) => corner.max(viewport.y - (prompt.y - PROMPT_HEIGHT) + theme::OSD_GAP),
        None => corner,
    };
    let width = transcript_width(viewport.x);
    let muted = voice.mode == crate::systems::voice::VoiceMode::PushToTalk;
    let mut open_on: Option<Option<String>> = None;
    let mut toggle_mute = false;
    let mut leave_voice = false;

    // The library draws it from this, and its tap comes back here.
    let chat_disc = match &newest {
        Some((key, label, _, _, _)) => (
            crate::systems::chats::display_name(key, label),
            crate::systems::chats::kind_of(key),
        ),
        None => ("Here".to_string(), crate::systems::chats::ChatKind::Here),
    };
    let live = alpha > 0.5;
    let chip = ChipView {
        left: theme::OSD_MARGIN,
        bottom: corner,
        width,
        live,
        chat: chat_disc,
        open: newest.as_ref().map(|n| n.0.clone()),
        sender: newest.as_ref().map(|n| (n.2.clone(), name_tone(&n.2))),
        body: newest
            .as_ref()
            .map_or_else(|| "Talking nearby".to_string(), |n| n.3.clone()),
        unread,
        talking,
        muted,
    };
    // Roughly its height: a 26 pt disc in a 6 pt margin, and the rim.
    overlay.covers = Some(Rect::new(
        theme::OSD_MARGIN,
        viewport.y - corner - 40.0,
        theme::OSD_MARGIN + width,
        viewport.y - corner,
    ));
    overlay.lib_view.chip = Some((chip, alpha));
    match lib_act.filter(|_| live) {
        Some(ChipAct::Open(key)) => open_on = Some(key),
        Some(ChipAct::Mute) => toggle_mute = true,
        Some(ChipAct::Leave) => leave_voice = true,
        None => {}
    }

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

/// How high the interaction prompt stands over its bottom centre, roughly, in
/// points: one row of its buttons in the overlay frame.
const PROMPT_HEIGHT: f32 = 44.0;

// ─── The sheet ────────────────────────────────────────────────────────────────

/// How much of the bottom edge to leave for the on-screen keyboard.
///
/// Zero unless the composer is open on a platform with no IME — see the module
/// note on why this is *predicted* rather than measured off an open slab.
pub(crate) fn keyboard_reserve(metrics: &UiMetrics, composing: bool) -> f32 {
    if !composing || !onscreen_keyboard::platform_wants_keyboard() {
        return 0.0;
    }
    onscreen_keyboard::expected_height_in(
        metrics.viewport_pts(),
        metrics.ui_scale(),
        metrics.display_scale(),
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
pub fn sheet_rect(viewport: Vec2, compact: bool, keys: f32, dock_bottom: f32) -> Rect {
    if compact {
        let bottom = viewport.y - keys;
        let top = if keys > 0.0 {
            viewport.y * SHEET_TOP_FRACTION
        } else {
            viewport.y * (1.0 - SHEET_REST_FRACTION)
        };
        // Never less than a usable sheet, whatever the keys took.
        let top = top.min(bottom - SHEET_MIN_HEIGHT).max(0.0);
        Rect::new(0.0, top, viewport.x, bottom)
    } else {
        let width = (viewport.x - 2.0 * theme::OSD_MARGIN).min(SHEET_CARD_WIDTH);
        let bottom = viewport.y - keys.max(dock_bottom) - theme::OSD_MARGIN;
        let height = SHEET_CARD_HEIGHT
            .min(bottom - viewport.y * 0.25)
            .max(SHEET_MIN_HEIGHT);
        Rect::new(
            theme::OSD_MARGIN,
            bottom - height,
            theme::OSD_MARGIN + width,
            bottom,
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

/// What the sheet asked for this frame: the library's sheet
/// (`lib_chat_overlay`) fills it through [`ChatOverlay::lib_sheet`], and
/// [`drive_chat_sheet`] does it.
#[derive(Default)]
pub struct SheetActs {
    pub tab: Option<usize>,
    pub close: bool,
    pub send: bool,
    pub voice: Option<crate::systems::voice::VoiceMode>,
    pub call: Option<crate::systems::calls::CallRequest>,
    /// Join (or start) the open chat's call: the library's call key, which
    /// does not know the chat's group; made a `call` from it here.
    pub join_call: bool,
    pub unfold_call: bool,
    pub toggle_mic: bool,
    pub toggle_camera: bool,
    pub toggle_shout: bool,
    /// The "+" under the field: the extras' row, open or shut.
    pub toggle_plus: bool,
    pub open_in_community: bool,
    pub person: Option<(String, PersonAction)>,
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
pub fn drive_chat_sheet(
    metrics: Res<UiMetrics>,
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
    // What the library's sheet asked for last frame, done below if the
    // sheet is still up; and no sheet for it until this frame says so.
    overlay.lib_view.sheet = None;
    let lib_act = std::mem::take(&mut overlay.lib_sheet);
    let compact = metrics.compact();
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

    let claim = std::mem::take(&mut overlay.claim_focus);
    let wants_keys = onscreen_keyboard::platform_wants_keyboard();
    // The keys' room is kept while they are up, and on the frame the field
    // asks for them (`claim`): that is the frame they choose their edge on.
    let keys = if wants_keys && (keyboard.is_open() || claim) {
        keyboard_reserve(&metrics, true)
    } else {
        0.0
    };
    let viewport = metrics.viewport_pts();
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

    // The library draws the sheet from this; what it asked for is done
    // below. Its field is bridged to `chat.input` by `lib_chat_overlay`.
    use crate::systems::chats::ChatKind;
    let ready = !chat.input.trim().is_empty();
    let mut act = lib_act;
    act.send &= ready;
    if std::mem::take(&mut act.join_call) {
        act.call = group.clone().map(crate::systems::calls::CallRequest::Join);
    }
    let strip = if kind == ChatKind::Here && voice_on {
        let talker = voice_people
            .iter()
            .find(|(_, s)| *s)
            .map_or("nobody is talking", |(n, _)| n.as_str());
        Some(SheetStrip::Voice {
            people: voice_people.clone(),
            talker: format!("public · {talker}"),
            muted: muted_voice,
        })
    } else if in_call_here {
        Some(SheetStrip::Call {
            status: call_status.clone(),
            camera: crate::systems::calls::camera_available().then_some(calls.camera_on),
            muted: calls.muted,
        })
    } else {
        None
    };
    let sheet = SheetView {
        rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
        compact,
        tabs: tab_items
            .iter()
            .map(|(i, name, kind, unread)| (*i, name.clone(), *kind, *unread, *i == chat.active))
            .collect(),
        kind,
        subtitle: crate::systems::chats::subtitle(kind),
        public: !matches!(kind, ChatKind::Private | ChatKind::Topic),
        voice_key: (kind == ChatKind::Here && can_voice).then_some(voice_on),
        call_key: (matches!(kind, ChatKind::Private | ChatKind::Topic)
            && !in_call_here
            && group.is_some())
        .then_some(others_calling),
        strip,
        runs: runs
            .iter()
            .map(|run| RunView {
                sender: run.sender.clone(),
                mine: run.mine,
                tone: name_tone(&run.sender),
                lines: run.lines.iter().map(summary).collect(),
                last: run.lines.last().map(|l| l.body.clone()),
            })
            .collect(),
        plus: overlay.plus,
        shout,
        here: kind == ChatKind::Here,
        ready,
    };
    overlay.lib_view.sheet = Some(sheet);

    // ── Acting on it ──────────────────────────────────────────────────────
    if act.toggle_plus {
        overlay.plus = !overlay.plus;
    }
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
        let phone = Vec2::new(400.0, 900.0);
        let keys = sheet_rect(phone, true, 320.0, 70.0);
        assert_eq!(keys.max.y, 900.0 - 320.0);
        assert!(keys.min.y >= 900.0 * SHEET_TOP_FRACTION - 0.5);
        assert_eq!(keys.width(), 400.0);
        let rest = sheet_rect(phone, true, 0.0, 70.0);
        assert_eq!(rest.max.y, 900.0);
        assert!((rest.min.y - 450.0).abs() < 0.5);
        // Keys taller than the room leave a usable sheet, never a sliver.
        let squeezed = sheet_rect(Vec2::new(400.0, 640.0), true, 300.0, 70.0);
        assert!(squeezed.height() >= SHEET_MIN_HEIGHT - 0.5);
        let desk = sheet_rect(Vec2::new(1400.0, 900.0), false, 0.0, 70.0);
        assert!((desk.width() - SHEET_CARD_WIDTH).abs() < 1e-3);
        assert!(desk.max.y <= 900.0 - 70.0);
        assert_eq!(desk.min.x, theme::OSD_MARGIN);
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
        let screen = Rect::new(0.0, 0.0, 400.0, 800.0);
        assert_eq!(clear_of_keyboard(screen, None), screen);
        let bottom = Rect::new(0.0, 500.0, 400.0, 800.0);
        assert_eq!(clear_of_keyboard(screen, Some(bottom)).max.y, 500.0);
        let top = Rect::new(0.0, 0.0, 400.0, 300.0);
        assert_eq!(clear_of_keyboard(screen, Some(top)).min.y, 300.0);
    }

    /// A bubble's words are measured, not guessed: a longer line is wider,
    /// a line past the column wraps onto a second, and nothing is wider than
    /// the column it wraps at.
    #[test]
    fn text_is_measured_and_wrapped_at_the_column() {
        let short = text_size("On my way", BODY_PT, 178.0);
        let long = text_size("The stroopwafel stall is open", BODY_PT, 1000.0);
        assert!(short.x > 0.0 && long.x > short.x, "{short} {long}");
        let wrapped = text_size(
            "Are you coming down to the Vismarkt? The stroopwafel stall is open",
            BODY_PT,
            178.0,
        );
        assert!(wrapped.x <= 178.0, "{wrapped}");
        assert!(wrapped.y >= 2.0 * short.y - 0.5, "{wrapped} {short}");
    }
}
