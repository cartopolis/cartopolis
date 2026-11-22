use super::*;

// ─── Style ────────────────────────────────────────────────────────────────────

pub(super) fn widgets(c: &Palette) -> Widgets {
    let visuals = |bg: Color32, line: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, line),
        fg_stroke: Stroke::new(1.0, fg),
        corner_radius: CornerRadius::same(RADIUS),
        expansion: 0.0,
    };
    Widgets {
        // Separators, indent guides and plain labels all read off this one.
        noninteractive: visuals(c.surface, c.line, c.text),
        // At rest and under the pointer as the midas hub's `m-button`: a dark
        // surface that lifts a step under the pointer. The hub's hairline is
        // quieter than [`Palette::line`]; this one keeps the 3:1 a control boundary
        // needs (`a_control_is_distinguishable_from_the_surface_it_sits_on`).
        inactive: visuals(c.surface, c.control_edge, c.text),
        hovered: visuals(c.surface_raised, c.accent_word, c.cream),
        // Pressed / focused — **one fill, not two.** `weak_bg_fill` (the button
        // fill) carries a [`Palette::cream`] label, so both fills are the deep
        // [`Palette::accent_muted`], which carries [`Palette::cream`] at 10.5:1
        // (dark) and looks the same whether a control is a button or a
        // checkbox; its rim is [`Palette::accent_word`], clear of that fill.
        //
        // `fg_stroke` must stay bright regardless: egui derives
        // `Visuals::strong_text_color` from it, so *every* `RichText::strong()` in
        // the app is this colour. Setting it to ink to suit a light fill would
        // turn every bold value in the UI near-invisible.
        active: WidgetVisuals {
            expansion: 1.0,
            ..visuals(c.accent_muted, c.accent_word, c.cream)
        },
        // An open menu button keeps the hover surface but drops the bright ring —
        // the popup below it is the thing to look at now.
        open: visuals(c.surface_hover, c.line, c.cream),
    }
}

/// The full brand [`egui::Visuals`] for the palette in force ([`p`]).
#[must_use]
pub fn visuals() -> egui::Visuals {
    visuals_of(p())
}

/// The brand [`egui::Visuals`] for one palette, built by overriding
/// [`egui::Visuals::dark`] (or [`egui::Visuals::light`]) rather than from
/// scratch so the fields this doesn't have an opinion about (notably the
/// font colour transfer function) stay at egui's tuned defaults.
#[must_use]
pub fn visuals_of(c: &Palette) -> egui::Visuals {
    let mut v = if c.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.widgets = widgets(c);
    v.selection = Selection {
        bg_fill: c.accent_muted,
        stroke: Stroke::new(1.0, c.cream),
    };
    v.hyperlink_color = c.accent_text;
    v.faint_bg_color = c.surface_raised;
    v.extreme_bg_color = c.ink;
    // A text field is a card, not a well: an ink well read as a black slot in
    // the dark theme (2026-11-10).
    v.text_edit_bg_color = Some(c.card);
    v.code_bg_color = c.ink;
    v.warn_fg_color = c.warn;
    v.error_fg_color = c.error;
    // Pin the weak/disabled tone instead of letting egui derive it by alpha —
    // this is the same grey-blue the two splashes use, and it is measured.
    v.weak_text_color = Some(c.text_dim);
    // Disabled is the one tone egui still derived by alpha, and its default 0.5
    // put a disabled label at 4.1:1 on the panel — *dimmer than `TEXT_DIM` hint
    // text at 6.9:1*, so the sentence explaining why a control was unavailable
    // was brighter than the control. It also took a button's frame to 1.1:1
    // against the panel, i.e. gone. 0.65 keeps disabled clearly below the live
    // state without making it a colour nobody can find.
    v.disabled_alpha = 0.65;

    // The midas hub's layers: a page ([`Palette::panel`]) and cards on it ([`Palette::glass`],
    // which is [`Palette::surface`]), solid.
    v.panel_fill = c.panel;
    v.window_fill = c.glass;
    // The hub's hairline. The same one the boxes over the map wear: a docked
    // panel and a floating readout are the same material at two sizes.
    v.window_stroke = Stroke::new(1.0, c.panel_rim);
    v.window_corner_radius = CornerRadius::same(RADIUS_LG);
    // The popups are the cards over the map (a place, a building), so they take
    // the windows' radius: at 8 pt beside 14 pt panes they read as a
    // different, cheaper material (2026-10-31).
    v.menu_corner_radius = CornerRadius::same(RADIUS_LG);
    // The hub's `--lift-2`, as near as one egui shadow comes: soft, a little
    // below. These float over a moving map, and a tight hard shadow reads as a
    // sticker rather than a raised surface.
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha((150.0 * c.shadow) as u8),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 22,
        spread: 0,
        color: Color32::from_black_alpha((125.0 * c.shadow) as u8),
    };

    v.text_cursor.stroke = Stroke::new(2.0, c.accent_bright);
    // The filled part of a slider takes the accent, so a value reads at a glance.
    v.slider_trailing_fill = true;
    v
}

/// The full brand [`egui::Style`] — [`visuals`] plus the spacing and type scale,
/// sized for this run's pointer ([`touch_layout`]).
pub fn style() -> egui::Style {
    style_for(touch_layout())
}

/// [`style`] with the pointer stated rather than detected, so the touch tier's
/// sizing is testable on a workstation.
pub fn style_for(touch: bool) -> egui::Style {
    let mut style = egui::Style {
        visuals: visuals(),
        ..Default::default()
    };
    // What egui's own widgets ease over (a checkbox, a collapsing header, a
    // scroll fade): a *state* change (`motion::STATE`). A surface's slide sets
    // its own time round its call (`motion::with_panel_time`).
    style.animation_time = crate::systems::motion::STATE;

    let s = &mut style.spacing;
    // The "cramped" fix is mostly here. egui's defaults are 3 pt between rows and
    // vec2(4, 1) of button padding; those are inspector values, and a panel of
    // place rows built on them has no rhythm at all.
    // Raised from 6 on 2026-11-08 ("make the UI feel less cramped").
    s.item_spacing = egui::vec2(8.0, 8.0);
    // The touch tier is not "the same UI, bigger" — the *type* stays where it is
    // (it is already sharp on a phone; see `egui_manager::phone_ui_scale`) and
    // only the parts a finger has to hit grow, to [`MIN_TOUCH_TARGET_DP`]. That
    // is the whole difference between the two tiers.
    s.button_padding = if touch {
        egui::vec2(14.0, 10.0)
    } else {
        egui::vec2(12.0, 6.0)
    };
    // [`ROW_PTS`] of guaranteed height per clickable row on a pointer (egui
    // ships 18), [`TOUCH_ROW_PTS`] under a finger.
    s.interact_size = egui::vec2(40.0, if touch { TOUCH_ROW_PTS } else { ROW_PTS });
    s.window_margin = Margin::same(14);
    s.menu_margin = Margin::same(10);
    s.menu_spacing = 6.0;
    s.indent = 20.0;
    s.icon_spacing = 6.0;
    s.slider_width = 120.0;
    s.combo_width = 120.0;
    // **Solid, not floating.** egui's default bar floats over the content, so
    // a row's right-hand button sat under it and could not be pressed (the
    // Search panel's route buttons, 2026-11-22); a solid bar takes its own
    // column and the rows are laid out beside it, as the web's are.
    s.scroll = egui::style::ScrollStyle::solid();
    // A 10 pt scrollbar is a 15 dp drag handle; a finger needs the same 48 dp
    // everything else does, and a bar it cannot grab is a panel that only scrolls
    // by fling.
    s.scroll.bar_width = if touch { 14.0 } else { 8.0 };
    s.scroll.bar_inner_margin = 6.0;
    // Always there to be grabbed, faint until the pointer comes near.
    s.scroll.dormant_handle_opacity = 0.25;
    s.scroll.dormant_background_opacity = 0.0;

    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(SIZE_CHIP, FontFamily::Proportional),
        ),
        (
            TextStyle::Body,
            FontId::new(SIZE_BODY, FontFamily::Proportional),
        ),
        (
            TextStyle::Button,
            FontId::new(SIZE_BODY, FontFamily::Proportional),
        ),
        // The one place the brand face appears in the app chrome.
        (
            TextStyle::Heading,
            FontId::new(SIZE_HEADING, brand_family()),
        ),
        (
            TextStyle::Monospace,
            FontId::new(SIZE_MONO, FontFamily::Monospace),
        ),
    ]
    .into();

    style
}

/// Install the fonts and the style on a context.
///
/// Called from `EguiManager::default` *before* the first pass, which matters:
/// `set_fonts` only takes effect from the next `begin_pass`, and painting with
/// an unbound `FontFamily::Name` panics in epaint. Installing at construction
/// means [`brand_family`] is safe to use from frame one.
///
/// The style is set for **both** themes. This integration never feeds egui a
/// `system_theme`, so it would fall back to dark on its own — but a light-theme
/// context left on egui's stock style is a latent white flash, and the app has
/// exactly one look at a time, chosen by [`set_light`] rather than by egui's
/// theme: both of egui's slots hold the palette in force.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    let style = std::sync::Arc::new(style());
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

/// Re-apply the palette in force to a context's style, after [`set_light`].
///
/// Only the visuals: the spacing, the type scale and the debug switches
/// `EguiManager::default` turns off are left as they are.
pub fn apply_visuals(ctx: &egui::Context) {
    let v = visuals();
    ctx.all_styles_mut(|style| style.visuals = v.clone());
}
