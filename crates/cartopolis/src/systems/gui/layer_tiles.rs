//! The Layers panel's widgets: the row of category discs, the layer tile and the
//! base-map choice (`docs/mockups/layers-and-ring/v1.png`). Only drawing and
//! hit-testing live here; which layers exist and what they switch is
//! `data_layers::render_data_layers_panel`.
//!
//! A layer is a row with its switch, whole: the panel used to be ~40 checkboxes
//! three levels deep, and a checkbox row is a 20 pt target on a phone. A row is
//! the full width of the panel and answers the tap anywhere on it (it was a
//! tile, three to a row, until 2026-11-10).

use egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::systems::icons;
use crate::systems::theme;

/// The six places a layer can be: what the panel groups by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayerCategory {
    #[default]
    Map,
    City,
    Places,
    Moving,
    Under,
    Orbit,
}

impl LayerCategory {
    pub const ALL: [Self; 6] = [
        Self::Map,
        Self::City,
        Self::Places,
        Self::Moving,
        Self::Under,
        Self::Orbit,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Map => "Map",
            Self::City => "City",
            Self::Places => "Places",
            Self::Moving => "Traffic",
            Self::Under => "Below",
            Self::Orbit => "Space",
        }
    }

    #[must_use]
    pub fn icon(self) -> &'static str {
        match self {
            Self::Map => icons::CAT_MAP,
            Self::City => icons::CAT_CITY,
            Self::Places => icons::PIN,
            Self::Moving => icons::CAT_MOVING,
            Self::Under => icons::CAT_UNDER,
            Self::Orbit => icons::CAT_ORBIT,
        }
    }

    /// The line under the discs: what the category holds, in a person's words.
    #[must_use]
    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Map => "The ground itself: pick one",
            Self::City => "What stands on the ground",
            Self::Places => "Markers on the map",
            Self::Moving => "Everything on the map that moves",
            Self::Under => "Opens the ground near you · Netherlands only",
            Self::Orbit => "The whole planet, seen from space",
        }
    }
}

/// Diameter of a category disc, points.
const DISC: f32 = 42.0;

/// The category row: one disc per [`LayerCategory`], the selected one in brand
/// orange, each with a count of the layers switched on inside it.
pub fn category_strip(ui: &mut Ui, selected: &mut LayerCategory, counts: [usize; 6]) {
    let w = ui.available_width();
    let cell = w / 6.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, DISC + 20.0), Sense::hover());
    for (i, cat) in LayerCategory::ALL.iter().enumerate() {
        let col = Rect::from_min_size(
            Pos2::new(rect.left() + cell * i as f32, rect.top()),
            Vec2::new(cell, rect.height()),
        );
        let id = ui.id().with(("layer_cat", i));
        let resp = theme::named(ui, ui.interact(col, id, Sense::click()), cat.label());
        if resp.clicked() {
            *selected = *cat;
        }
        let on = *selected == *cat;
        let centre = Pos2::new(col.center().x, col.top() + DISC / 2.0);
        let painter = ui.painter();
        let fill = if on {
            theme::p().accent
        } else if resp.hovered() {
            theme::p().surface_hover
        } else {
            theme::p().surface_raised
        };
        painter.circle_filled(centre, DISC / 2.0, fill);
        painter.text(
            centre,
            Align2::CENTER_CENTER,
            cat.icon(),
            FontId::proportional(20.0),
            if on {
                theme::p().on_accent
            } else {
                theme::p().text
            },
        );
        painter.text(
            Pos2::new(centre.x, col.top() + DISC + 3.0),
            Align2::CENTER_TOP,
            cat.label(),
            FontId::proportional(theme::SIZE_CHIP),
            if on {
                theme::p().cream
            } else {
                theme::p().text_dim
            },
        );
        if counts[i] > 0 {
            let badge = Pos2::new(centre.x + DISC / 2.0 - 4.0, col.top() + 5.0);
            painter.circle_filled(badge, 8.0, theme::p().gold);
            painter.text(
                badge,
                Align2::CENTER_CENTER,
                counts[i].to_string(),
                FontId::proportional(10.0),
                theme::p().on_gold,
            );
        }
    }
}

/// One layer, as a tile.
pub struct Tile<'a> {
    pub icon: &'static str,
    pub label: &'a str,
    /// Works only in the Netherlands: a small badge says so.
    pub nl: bool,
    /// Has options inside (its sources, its kinds): a ⋯ corner opens them.
    pub more: bool,
    /// On, but cannot show from here, and why — drawn dimmed, with the reason
    /// on hover and under the grid.
    pub unseen: Option<&'a str>,
}

/// What a tap on a tile grid did, beyond flipping the switch it landed on.
#[derive(Default)]
pub struct GridResponse {
    /// The index of the tile whose ⋯ was tapped.
    pub more: Option<usize>,
    /// Whether any switch changed.
    pub changed: bool,
}

/// Gap between the base-map cards, points.
const GAP: f32 = 7.0;

/// Height of one layer row, points: a list row in the Settings panel's
/// family, so a finger has the whole line.
const ROW_H: f32 = 48.0;

/// The layers of a group, as one card of rows — an icon, the layer's name and
/// its switch on the right, a hairline between rows — the way the Settings
/// panel draws its lists (`theme::list_group`). Each entry is the row and the
/// switch it drives; `open` is the row whose options are showing, drawn with
/// its ⋯ lit.
///
/// **A list since 2026-11-10.** It was a grid of tiles, three to a row, each
/// its own bordered card with a small switch in its corner; beside the
/// Settings panel's grouped rows the person called the tiles "cheap". The
/// whole row still answers the tap, as the tile did.
pub fn tile_grid(
    ui: &mut Ui,
    tiles: &mut [(Tile<'_>, &mut bool)],
    open: Option<usize>,
) -> GridResponse {
    let mut out = GridResponse::default();
    let w = ui.available_width();
    let n = tiles.len();
    let (card, _) = ui.allocate_exact_size(Vec2::new(w, ROW_H * n as f32), Sense::hover());
    let corner = CornerRadius::same(16);
    ui.painter()
        .add(theme::card_shadow().as_shape(card, corner));
    ui.painter().rect(
        card,
        corner,
        theme::p().card,
        Stroke::new(1.0, theme::p().list_line),
        StrokeKind::Inside,
    );
    for (index, (tile, on)) in tiles.iter_mut().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(card.left(), card.top() + index as f32 * ROW_H),
            Vec2::new(w, ROW_H),
        );
        // The ⋯ sits left of the switch.
        let more_rect = Rect::from_center_size(
            Pos2::new(rect.right() - 66.0, rect.center().y),
            Vec2::new(30.0, ROW_H),
        );
        let resp = theme::named(
            ui,
            ui.interact(rect, ui.id().with(("tile", tile.label)), Sense::click()),
            tile.label,
        );
        // The ⋯ *after* the row: egui gives a press to the widget registered
        // last, so this is what lets the ⋯ win its patch.
        let more_resp = tile.more.then(|| {
            let resp = ui.interact(
                more_rect,
                ui.id().with(("tile_more", tile.label)),
                Sense::click(),
            );
            theme::named(ui, resp, &format!("More: {}", tile.label))
        });
        let more_hit = more_resp.as_ref().is_some_and(|r| r.clicked());
        if more_hit {
            out.more = Some(index);
        } else if resp.clicked() {
            **on = !**on;
            out.changed = true;
        }
        if let Some(why) = tile.unseen.filter(|_| **on) {
            resp.clone().on_hover_text(why);
        }
        paint_row(
            ui,
            rect,
            tile,
            **on,
            resp.hovered(),
            open == Some(index),
            index + 1 < n,
        );
    }
    ui.add_space(8.0);
    out
}

fn paint_row(
    ui: &Ui,
    rect: Rect,
    tile: &Tile<'_>,
    on: bool,
    hovered: bool,
    open: bool,
    rule: bool,
) {
    let painter = ui.painter();
    let dim = on && tile.unseen.is_some();
    if hovered {
        painter.rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(14),
            theme::p().surface_raised,
        );
    }
    if rule {
        painter.hline(
            (rect.left() + 48.0)..=rect.right(),
            rect.bottom() - 0.5,
            Stroke::new(1.0, theme::p().list_line),
        );
    }
    let text = if dim {
        theme::p().text_dim
    } else {
        theme::p().text
    };
    painter.text(
        Pos2::new(rect.left() + 24.0, rect.center().y),
        Align2::CENTER_CENTER,
        tile.icon,
        FontId::proportional(18.0),
        if on && !dim {
            theme::p().accent_word
        } else {
            theme::p().text_dim
        },
    );
    // The name, cut to the room left of the ⋯ and the switch.
    let room = rect.width() - 48.0 - if tile.more { 92.0 } else { 62.0 };
    let galley = painter.layout(
        tile.label.to_string(),
        FontId::proportional(theme::SIZE_BODY),
        text,
        room,
    );
    let label_h = galley.size().y;
    let label_w = galley.size().x;
    painter.galley(
        Pos2::new(rect.left() + 48.0, rect.center().y - label_h * 0.5),
        galley,
        text,
    );
    if tile.nl {
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + 48.0 + label_w + 6.0, rect.center().y - 6.0),
            Vec2::new(18.0, 12.0),
        );
        painter.rect_stroke(
            r,
            CornerRadius::same(3),
            Stroke::new(1.0, theme::p().overlay_line),
            StrokeKind::Inside,
        );
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            "NL",
            FontId::proportional(8.5),
            theme::p().text_dim,
        );
    }
    if tile.more {
        painter.text(
            Pos2::new(rect.right() - 66.0, rect.center().y),
            Align2::CENTER_CENTER,
            icons::MORE,
            FontId::proportional(18.0),
            if open {
                theme::p().accent
            } else {
                theme::p().text_dim
            },
        );
    }
    // The switch, right: a pill with a knob.
    let pill = Rect::from_center_size(
        Pos2::new(rect.right() - 30.0, rect.center().y),
        Vec2::new(34.0, 20.0),
    );
    painter.rect_filled(
        pill,
        CornerRadius::same(10),
        if on {
            theme::p().accent
        } else {
            theme::p().surface_hover
        },
    );
    let knob_x = if on {
        pill.right() - 10.0
    } else {
        pill.left() + 10.0
    };
    painter.circle_filled(
        Pos2::new(knob_x, pill.center().y),
        7.0,
        if on {
            theme::p().knob_on
        } else {
            theme::p().text_dim
        },
    );
}

/// The ground: one choice of three, as cards.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basemap {
    Drawn,
    Aerial,
    None,
}

/// The three base-map cards. Returns the card tapped, if any.
pub fn basemap_choice(ui: &mut Ui, current: Basemap) -> Option<Basemap> {
    let w = ui.available_width();
    let card_w = ((w - 2.0 * GAP) / 3.0).floor();
    let (row, _) = ui.allocate_exact_size(Vec2::new(w, 54.0), Sense::hover());
    // Three plain cards, the chosen one rimmed and its icon in the accent.
    // They were painted in the ground's own colours (paper beige, an aerial
    // olive), which the person found ugly beside the rest (2026-11-10).
    let cards = [
        (Basemap::Drawn, "Drawn map"),
        (Basemap::Aerial, "Aerial photo"),
        (Basemap::None, "No ground"),
    ];
    let mut picked = None;
    for (i, (choice, label)) in cards.into_iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(row.left() + i as f32 * (card_w + GAP), row.top()),
            Vec2::new(card_w, row.height()),
        );
        let resp = theme::named(
            ui,
            ui.interact(rect, ui.id().with(("basemap", i)), Sense::click()),
            label,
        );
        if resp.clicked() && current != choice {
            picked = Some(choice);
        }
        let sel = current == choice;
        let ink = if sel {
            theme::p().accent_word
        } else {
            theme::p().text
        };
        let painter = ui.painter();
        painter.add(theme::card_shadow().as_shape(rect, CornerRadius::same(12)));
        painter.rect(
            rect,
            CornerRadius::same(12),
            if resp.hovered() && !sel {
                theme::p().surface_raised
            } else {
                theme::p().card
            },
            if sel {
                Stroke::new(2.0, theme::p().accent)
            } else {
                Stroke::new(1.0, theme::p().list_line)
            },
            StrokeKind::Inside,
        );
        let icon = match choice {
            Basemap::Drawn => icons::CAT_MAP,
            Basemap::Aerial => icons::AERIAL,
            Basemap::None => icons::NO_GROUND,
        };
        painter.text(
            rect.left_top() + Vec2::new(8.0, 6.0),
            Align2::LEFT_TOP,
            icon,
            FontId::proportional(17.0),
            ink,
        );
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -6.0),
            Align2::LEFT_BOTTOM,
            label,
            FontId::proportional(theme::SIZE_CHIP),
            ink,
        );
    }
    picked
}

/// A toggle chip with a colour dot — the satellite kinds, which double as the
/// legend for the colours drawn on the globe.
pub fn swatch_chip(ui: &mut Ui, on: &mut bool, color: Color32, label: &str) -> bool {
    let font = FontId::proportional(theme::SIZE_SUB);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, theme::p().text);
    let size = Vec2::new(galley.size().x + 30.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = theme::named(ui, resp, label);
    if resp.clicked() {
        *on = !*on;
    }
    let painter = ui.painter();
    let alpha = if *on { 1.0 } else { 0.4 };
    painter.rect(
        rect,
        CornerRadius::same(13),
        theme::p()
            .surface_raised
            .gamma_multiply(if *on { 1.0 } else { 0.7 }),
        Stroke::new(1.0, theme::p().overlay_line),
        StrokeKind::Inside,
    );
    painter.circle_filled(
        Pos2::new(rect.left() + 13.0, rect.center().y),
        4.5,
        color.gamma_multiply(alpha),
    );
    painter.galley(
        Pos2::new(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::p().text.gamma_multiply(alpha),
    );
    resp.clicked()
}
