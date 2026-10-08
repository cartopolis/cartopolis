//! The Layers panel's widgets: the row of category discs, the layer tile and the
//! base-map choice (`docs/mockups/layers-and-ring/v1.png`). Only drawing and
//! hit-testing live here; which layers exist and what they switch is
//! `data_layers::render_data_layers_panel`.
//!
//! A tile is the switch, whole: the panel used to be ~40 checkboxes three levels
//! deep, and a checkbox row is a 20 pt target on a phone. A tile is a thumb's
//! width and the full rectangle answers the tap.

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
            Self::Moving => "Moving",
            Self::Under => "Under",
            Self::Orbit => "Orbit",
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
        let resp = ui.interact(col, id, Sense::click());
        if resp.clicked() {
            *selected = *cat;
        }
        let on = *selected == *cat;
        let centre = Pos2::new(col.center().x, col.top() + DISC / 2.0);
        let painter = ui.painter();
        let fill = if on {
            theme::BRAND
        } else if resp.hovered() {
            theme::SURFACE_HOVER
        } else {
            theme::SURFACE_RAISED
        };
        painter.circle_filled(centre, DISC / 2.0, fill);
        painter.text(
            centre,
            Align2::CENTER_CENTER,
            cat.icon(),
            FontId::proportional(20.0),
            if on { Color32::WHITE } else { theme::TEXT },
        );
        painter.text(
            Pos2::new(centre.x, col.top() + DISC + 3.0),
            Align2::CENTER_TOP,
            cat.label(),
            FontId::proportional(theme::SIZE_CHIP),
            if on { theme::CREAM } else { theme::TEXT_DIM },
        );
        if counts[i] > 0 {
            let badge = Pos2::new(centre.x + DISC / 2.0 - 4.0, col.top() + 5.0);
            painter.circle_filled(badge, 8.0, theme::CREAM);
            painter.text(
                badge,
                Align2::CENTER_CENTER,
                counts[i].to_string(),
                FontId::proportional(10.0),
                theme::INK,
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

/// Height of a tile, points.
const TILE_H: f32 = 70.0;
const GAP: f32 = 7.0;

/// A grid of tiles, three to a row. Each entry is the tile and the switch it
/// drives; `open` is the tile whose options are showing, drawn with its ⋯ lit.
pub fn tile_grid(
    ui: &mut Ui,
    tiles: &mut [(Tile<'_>, &mut bool)],
    open: Option<usize>,
) -> GridResponse {
    let mut out = GridResponse::default();
    let w = ui.available_width();
    let tile_w = ((w - 2.0 * GAP) / 3.0).floor();
    for (row_i, row) in tiles.chunks_mut(3).enumerate() {
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(w, TILE_H), Sense::hover());
        for (col_i, (tile, on)) in row.iter_mut().enumerate() {
            let index = row_i * 3 + col_i;
            let rect = Rect::from_min_size(
                Pos2::new(
                    row_rect.left() + col_i as f32 * (tile_w + GAP),
                    row_rect.top(),
                ),
                Vec2::new(tile_w, TILE_H),
            );
            // Beside the switch on the top row, so the label below keeps the
            // tile's full width (it broke "Satellites" mid-word in the corner).
            let more_rect = Rect::from_min_size(
                Pos2::new(rect.right() - 66.0, rect.top() + 2.0),
                Vec2::new(30.0, 28.0),
            );
            let resp = ui.interact(rect, ui.id().with(("tile", tile.label)), Sense::click());
            // The ⋯ corner *after* the tile: egui gives a press to the widget
            // registered last, so this is what lets the corner win its patch.
            let more_resp = tile.more.then(|| {
                ui.interact(
                    more_rect,
                    ui.id().with(("tile_more", tile.label)),
                    Sense::click(),
                )
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
            paint_tile(ui, rect, tile, **on, resp.hovered(), open == Some(index));
        }
        ui.add_space(GAP);
    }
    out
}

fn paint_tile(ui: &Ui, rect: Rect, tile: &Tile<'_>, on: bool, hovered: bool, open: bool) {
    let painter = ui.painter();
    let dim = on && tile.unseen.is_some();
    let (fill, stroke) = if on && !dim {
        (
            Color32::from_rgba_unmultiplied(0xFD, 0x60, 0x12, 46),
            Stroke::new(1.2, theme::BRAND),
        )
    } else if hovered {
        (theme::SURFACE_HOVER, Stroke::new(1.0, theme::OVERLAY_LINE))
    } else {
        (theme::SURFACE_RAISED, Stroke::new(1.0, theme::OVERLAY_LINE))
    };
    let text = if dim {
        theme::TEXT_DIM
    } else if on {
        theme::CREAM
    } else {
        theme::TEXT
    };
    painter.rect(
        rect,
        CornerRadius::same(14),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    painter.text(
        rect.left_top() + Vec2::new(10.0, 9.0),
        Align2::LEFT_TOP,
        tile.icon,
        FontId::proportional(20.0),
        if on && !dim { theme::BRAND } else { text },
    );
    // The label, wrapped into the tile and anchored to its bottom.
    let galley = painter.layout(
        tile.label.to_string(),
        FontId::proportional(theme::SIZE_SUB),
        text,
        rect.width() - 16.0,
    );
    painter.galley(
        Pos2::new(rect.left() + 10.0, rect.bottom() - 9.0 - galley.size().y),
        galley,
        text,
    );
    // The switch, top right: a pill with a knob, so the state reads even
    // without the tint (and for a dimmed one, which loses it).
    let pill = Rect::from_min_size(
        rect.right_top() + Vec2::new(-34.0, 9.0),
        Vec2::new(25.0, 14.0),
    );
    painter.rect_filled(
        pill,
        CornerRadius::same(7),
        if on {
            theme::BRAND
        } else {
            theme::SURFACE_HOVER
        },
    );
    let knob_x = if on {
        pill.right() - 7.0
    } else {
        pill.left() + 7.0
    };
    painter.circle_filled(
        Pos2::new(knob_x, pill.center().y),
        5.0,
        if on { Color32::WHITE } else { theme::TEXT_DIM },
    );
    // Left of the switch, on the top row.
    let corner = Pos2::new(pill.left() - 6.0, pill.center().y);
    if tile.more {
        painter.text(
            corner,
            Align2::RIGHT_CENTER,
            icons::MORE,
            FontId::proportional(18.0),
            if open { theme::BRAND } else { theme::TEXT_DIM },
        );
    } else if tile.nl {
        let r = Rect::from_min_size(corner - Vec2::new(18.0, 6.0), Vec2::new(18.0, 12.0));
        painter.rect_stroke(
            r,
            CornerRadius::same(3),
            Stroke::new(1.0, theme::OVERLAY_LINE),
            StrokeKind::Inside,
        );
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            "NL",
            FontId::proportional(8.5),
            theme::TEXT_DIM,
        );
    }
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
    let cards = [
        (
            Basemap::Drawn,
            "Drawn map",
            Color32::from_rgb(0xE6, 0xE0, 0xD2),
            theme::INK,
        ),
        (
            Basemap::Aerial,
            "Aerial photo · NL",
            Color32::from_rgb(0x5E, 0x6B, 0x52),
            Color32::WHITE,
        ),
        (
            Basemap::None,
            "No ground",
            theme::SURFACE_HOVER,
            theme::TEXT,
        ),
    ];
    let mut picked = None;
    for (i, (choice, label, fill, ink)) in cards.into_iter().enumerate() {
        let rect = Rect::from_min_size(
            Pos2::new(row.left() + i as f32 * (card_w + GAP), row.top()),
            Vec2::new(card_w, row.height()),
        );
        let resp = ui.interact(rect, ui.id().with(("basemap", i)), Sense::click());
        if resp.clicked() && current != choice {
            picked = Some(choice);
        }
        let sel = current == choice;
        let painter = ui.painter();
        painter.rect(
            rect,
            CornerRadius::same(12),
            fill,
            if sel {
                Stroke::new(2.0, theme::BRAND)
            } else {
                Stroke::new(1.0, theme::OVERLAY_LINE)
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
        .layout_no_wrap(label.to_string(), font, theme::TEXT);
    let size = Vec2::new(galley.size().x + 30.0, 26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let painter = ui.painter();
    let alpha = if *on { 1.0 } else { 0.4 };
    painter.rect(
        rect,
        CornerRadius::same(13),
        theme::SURFACE_RAISED.gamma_multiply(if *on { 1.0 } else { 0.7 }),
        Stroke::new(1.0, theme::OVERLAY_LINE),
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
        theme::TEXT.gamma_multiply(alpha),
    );
    resp.clicked()
}
