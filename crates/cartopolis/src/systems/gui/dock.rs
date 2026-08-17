//! The map's floating buttons: the bottom dock, and the layers disc in the
//! top-left corner.
//!
//! The dock used to be a wide strip of labelled toggles sharing the bottom of the
//! screen with the nearby-chat composer, camera picker and five text buttons — it
//! read as a taskbar, and the map ran out under it. The dock is what is left once
//! each of those goes where it belongs: the composer into the Chat panel (it is
//! chat, and the panel is already open when you are chatting), the camera picker
//! into the app menu, and Appearance and the Assistant with it (settings you
//! visit, not things you flip while looking at the map).
//!
//! What remains is the places you actually go, centred, as discs in the same
//! family as the compass in the corner. An icon and a tooltip carry a panel name
//! perfectly well when there are five of them and they never move.
//!
//! **The row reads outwards from the map.** Search sits at the left end because
//! it is where a session starts, the panels you navigate between are in the
//! middle, and the menu closes the row on the right — the way into everything
//! that is not a place: sign-in, the camera mode, Settings, the tools. It was the
//! app *mark* until the menu grew a hamburger; the history and the two location
//! actions it also used to reach are in the Search panel now, beside the field
//! they navigate from. Both of those discs used to sit in the opposite corner as
//! an address-bar card that unfolded a search field, which put the two most-used
//! controls in the app at the end of a diagonal from the thumb of anyone holding
//! a phone.
//!
//! **Layers is the one panel that did not come along.** It is the map's own
//! settings rather than a destination, and it belongs beside the compass and the
//! locate button in the top-left column for the same reason those are there: they
//! are all controls *for the view*, and the top-left is the corner a phone's map
//! chrome uses for them.
//!
//! **Chat is the one session-bound entry.** It needs a server to talk to, so it
//! only appears when there is one; the map panels do not, and the dock therefore
//! renders in `--offline` too — which the old bar, living inside
//! `chat::render_chat_panel` behind its `OnlineSession` guard, did not.

use bevy::prelude::*;

use crate::resources::{OnlineSession, RightPanel};
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Gap between the discs, on a viewport with room for it.
const SPACING: f32 = 10.0;
/// The tightest the row may be packed before the discs would have to shrink.
/// Below this the buttons read as one strip rather than as separate targets.
const SPACING_MIN: f32 = 3.0;

/// Gap between the discs at this viewport width.
///
/// **The row does not fit the web phone tier at full spacing.** Signed in it is
/// five discs — `5 × 40 + 4 × 10` = 240 pt — against a viewport of ≈214 pt
/// (393 CSS px ÷ `theme::WEB_PHONE_MAX_DPR`-derived scale). The `Area` is
/// `CENTER_BOTTOM` and sizes to its content with nothing clamping it, so the
/// overflow is symmetric: 13 pt off each end, which is exactly the app mark and
/// the Search disc — the two controls the whole dock layout exists to put under
/// a thumb, and the mark is the only route to Settings, sign-in and Exit.
///
/// Tightening the gaps is the cheap half of the fix and the one that costs
/// nothing: the discs keep their 40 pt (60 dp on Android, 73 on the web tier),
/// well clear of [`theme::MIN_TOUCH_TARGET_DP`], and only the air between them
/// goes. If a sixth entry is ever added this will stop being enough and the
/// answer will have to be that Chat folds into the mark's menu on the narrow
/// tier — the row cannot hold six.
#[must_use]
fn disc_spacing(viewport_w: f32, discs: usize) -> f32 {
    let gaps = discs.saturating_sub(1).max(1) as f32;
    let discs_w = discs as f32 * theme::ROUND_BUTTON_SIZE;
    let slack = viewport_w - discs_w;
    (slack / gaps).clamp(SPACING_MIN, SPACING)
}
/// Inset from the window edge — [`theme::OSD_MARGIN`] on all four sides, the
/// same as every other floating control. These were three separate numbers (14
/// at the bottom, 12 at the left, 8 at the top), which is why the layers disc
/// and the compass column under it did not sit in the same corner.
const BOTTOM_MARGIN: f32 = theme::OSD_MARGIN;
const LEFT_MARGIN: f32 = theme::OSD_MARGIN;
const TOP_MARGIN: f32 = theme::OSD_MARGIN;

/// The panels that get a disc, in screen order. Their glyphs and names come from
/// [`RightPanel::icons`] / [`RightPanel::label`] — the same table the panel
/// headers wear, so a dock button and the header it opens can never drift apart.
///
/// The app-mark disc that closes the row on the right is not in this list: it
/// opens a menu rather than a panel (`ui::render_main_menu`).
const ENTRIES: [RightPanel; 4] = [
    RightPanel::Search,
    RightPanel::Chat,
    RightPanel::Route,
    RightPanel::Places,
];

/// How much of the bottom edge the dock claims, in points.
///
/// Exported so that anything else anchored down there can sit *above* it rather
/// than guess. The discs are centred, so on a desktop window a corner-anchored
/// control misses them — and on a phone, where the row is most of the width, it
/// does not. `map_osd`'s thumbstick is the first caller; a second copy of this
/// arithmetic is how the two would drift when the dock grows another entry.
#[must_use]
pub fn reserved_bottom() -> f32 {
    BOTTOM_MARGIN + theme::ROUND_BUTTON_SIZE
}

/// Where the top-left map controls start, clear of the layers disc above them.
/// Read by `hud::render_map_controls`; one constant because the compass and
/// everything stacked under it have to agree.
///
/// The gap is [`theme::OSD_GROUP_GAP`], not [`theme::OSD_GAP`], and the
/// difference is the whole reason that constant exists: the layers disc opens a
/// *panel* and the compass column under it controls the *view*. With one gap for
/// both, four dark discs 8 pt apart read as one strip of chrome down the corner
/// and nothing said where the panel toggle ended.
#[must_use]
pub fn reserved_top() -> f32 {
    TOP_MARGIN + theme::ROUND_BUTTON_SIZE + theme::OSD_GROUP_GAP
}

/// Draw the dock and the layers disc, and route clicks into the (mutually
/// exclusive) panel slot.
pub fn render_dock(
    mut egui: ResMut<EguiManager>,
    mut right_panel: ResMut<RightPanel>,
    mut menu: ResMut<crate::systems::ui::MainMenu>,
    online: Option<Res<OnlineSession>>,
    driver: Res<crate::systems::driver::DriverState>,
    keyboard: Res<crate::systems::onscreen_keyboard::OnScreenKeyboard>,
) {
    // Driving owns the screen — its own chrome already occupies the bottom.
    if driver.active {
        menu.open = false;
        return;
    }
    // So does the on-screen keyboard, and for the same reason: it docks to the
    // bottom edge, which is exactly where these discs sit. An `Area` drawn later
    // paints over an earlier one, so leaving the dock up would not merely look
    // crowded — the discs would take taps meant for the bottom key row.
    if keyboard.is_open() {
        menu.open = false;
        return;
    }
    let has_session = online.is_some();
    // On a compact viewport a panel takes the *whole* screen
    // (`responsive_right_panel`), and these are floating `Area`s, which egui
    // always paints above panels — so a disc left up would sit on the panel's own
    // content with no map behind it to act on. There is no z-order that puts an
    // `Area` behind a panel, so the only answer is to drop them, which is what the
    // compass and the status HUD do as well. On a wide viewport the panel docks to
    // the right and never reaches these corners, so they stay.
    let viewport_w = egui.viewport_pts().x;
    let covered = egui.compact() && !right_panel.is(RightPanel::None);
    if covered {
        // The card would float over a panel that is the whole screen, with no
        // map behind either of them.
        menu.open = false;
    }
    let ctx = egui.ctx_mut();
    let icons: Vec<&'static str> = ENTRIES.iter().map(|p| p.icon()).collect();
    let layers_icon = RightPanel::Layers.icon();

    let mut clicked: Option<RightPanel> = None;
    let mut menu_clicked = false;
    // Chat comes and goes with the session, and the mark always closes the row.
    let disc_count = ENTRIES
        .iter()
        .filter(|p| **p != RightPanel::Chat || has_session)
        .count()
        + 1;
    if !covered {
        egui::Area::new(egui::Id::new("panel_dock"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -BOTTOM_MARGIN))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.x = disc_spacing(viewport_w, disc_count);
                ui.horizontal(|ui| {
                    for (panel, icon) in ENTRIES.iter().zip(&icons) {
                        if *panel == RightPanel::Chat && !has_session {
                            continue;
                        }
                        let open = right_panel.is(*panel);
                        if theme::round_button(ui, icon, panel.label(), open).clicked() {
                            clicked = Some(*panel);
                        }
                    }
                    // The menu closes the row: everything that is not a place.
                    //
                    // A hamburger, not the app mark. The mark is the brand, and a
                    // logo sitting in a row of pictures-of-what-they-do reads as
                    // decoration — nothing about it said there was a menu behind
                    // it, which is a problem for the one disc that is the only
                    // route to Settings, sign-in and Exit.
                    if theme::round_button(ui, crate::systems::icons::MENU, "Menu", menu.open)
                        .clicked()
                    {
                        menu_clicked = true;
                    }
                });
            });

        egui::Area::new(egui::Id::new("layers_disc"))
            .anchor(egui::Align2::LEFT_TOP, egui::vec2(LEFT_MARGIN, TOP_MARGIN))
            .show(ctx, |ui| {
                let open = right_panel.is(RightPanel::Layers);
                if theme::round_button(ui, layers_icon, RightPanel::Layers.label(), open).clicked()
                {
                    clicked = Some(RightPanel::Layers);
                }
            });
    }

    if let Some(panel) = clicked {
        right_panel.toggle(panel);
        // A panel and the menu are two things asking for the same attention, and
        // on a phone the panel is the whole screen the menu would float over.
        menu.open = false;
    }
    if menu_clicked {
        menu.open = !menu.open;
        // Tell the menu this frame's click was its own switch: it is a click
        // *outside* the card by definition, and the card's dismiss-on-click-outside
        // rule would otherwise close it in the frame it opened.
        menu.toggled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dock is a size-to-content `Area` centred on the bottom edge, and
    /// nothing clamps it — a row wider than the screen simply hangs off both
    /// ends, taking the outermost discs with it. Search is the leftmost and the
    /// mark is the rightmost, so an overflow loses exactly the two entries this
    /// layout exists to put within reach of a thumb.
    ///
    /// The row grew from four discs to five when the mark and Search arrived (and
    /// Layers left for the top-left corner), which is what makes this worth
    /// pinning: the next entry does not fit.
    #[test]
    fn the_dock_fits_a_phone() {
        // **Both documented phone viewports**, because they are 60 pt apart and
        // this test used to check only the roomier one — which is how a row that
        // overflows the web tier by 13 pt at each end shipped. 274 pt is 411 dp
        // at `ui_scale` 1.5 (an ordinary Android handset); ~214 pt is 393 CSS px
        // on the web phone tier, and it is the binding case.
        let discs = ENTRIES.len() + 1; // + the app mark
        for viewport in [274.0_f32, 214.0] {
            let row = discs as f32 * theme::ROUND_BUTTON_SIZE
                + (discs - 1) as f32 * disc_spacing(viewport, discs);
            assert!(
                row <= viewport,
                "a {discs}-disc dock is {row} pt in {viewport}"
            );
        }
    }

    /// The gaps close before anything else does, and they stop closing before
    /// the discs stop reading as separate buttons.
    #[test]
    fn spacing_tightens_only_as_far_as_it_has_to() {
        assert_eq!(disc_spacing(1280.0, 5), SPACING, "a desktop keeps full air");
        assert_eq!(disc_spacing(274.0, 5), SPACING, "so does an Android phone");
        let web = disc_spacing(214.0, 5);
        assert!(
            web < SPACING && web >= SPACING_MIN,
            "the web tier tightens: {web}"
        );
        // A viewport narrower than the discs themselves cannot be solved by
        // spacing, and must not answer with a negative gap.
        assert_eq!(disc_spacing(10.0, 5), SPACING_MIN);
    }

    /// The layers disc heads the top-left column that `hud` continues, so it is
    /// anchored at the same inset those discs centre on — it was flush to a
    /// 12 pt left margin under an 8 pt top one, against a column that centred
    /// every disc under it on a different axis.
    #[test]
    fn the_column_is_one_column() {
        assert_eq!(LEFT_MARGIN, theme::OSD_MARGIN);
        assert_eq!(TOP_MARGIN, theme::OSD_MARGIN);
        assert_eq!(BOTTOM_MARGIN, theme::OSD_MARGIN);
    }

    /// The two corners the map's floating controls own must not overlap the
    /// space each other reserves — `hud::render_map_controls` stacks the compass
    /// and everything under it from [`reserved_top`], and `map_osd` and the
    /// scale bar sit above [`reserved_bottom`].
    #[test]
    fn the_reserved_edges_clear_their_discs() {
        assert!(reserved_top() >= TOP_MARGIN + theme::ROUND_BUTTON_SIZE);
        assert!(reserved_bottom() >= BOTTOM_MARGIN + theme::ROUND_BUTTON_SIZE);
    }
}
