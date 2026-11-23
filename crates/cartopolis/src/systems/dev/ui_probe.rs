//! `CARTO_UI_PROBE`: a card drawn with Bevy's own UI (`bevy_ui`) in the
//! client's colours and type, to photograph beside egui's Settings panel; and
//! a long list, to cost `bevy_ui`'s per-frame layout. Phase 0 of the
//! component-library study (`docs/mockups/component-library/README.md` §8),
//! which decides whether the study's Bevy backend starts on Bevy 0.20 or
//! waits for 0.21 (sub-pixel text positioning, incremental layout).
//!
//! `CARTO_UI_PROBE=1` spawns the card; `CARTO_UI_PROBE=2000` spawns that many
//! rows in a scrolling column instead. Nothing here is a component library:
//! the card is the controls of `theme::controls` written once more as `Node`
//! trees, with the same numbers (control height 40, radius 8, padding 16 / 12
//! in a field), so the two pictures differ only in what the toolkit does with
//! them. Dev-only, behind the `ui-probe` feature, which is what switches
//! `bevy_ui` on at all.

use bevy::prelude::*;
use bevy::text::{EditableText, FontWeight};
use bevy::ui::UiTargetCamera;
use bevy::ui_widgets::TextInput;

use crate::components::FreeCamera;
use crate::phase::ClientPhase;
use crate::systems::egui_manager::EguiRef;
use crate::systems::theme;

pub struct UiProbePlugin;

impl Plugin for UiProbePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<bevy::ui_widgets::TextInputPlugin>() {
            app.add_plugins(bevy::ui_widgets::UiWidgetsPlugins);
        }
        app.init_resource::<UiProbe>()
            .add_systems(Update, spawn_probe.in_set(ClientPhase::Input));
    }
}

/// Whether the probe has been spawned (or found nothing asked for), and how
/// many frames have passed: the palette is read at spawn, and a forced
/// `CARTO_UI_THEME` lands in `ClientPhase::Input` with no order against this
/// system, so the first frames could still read the other theme.
#[derive(Resource, Default)]
pub struct UiProbe {
    done: bool,
    frames: u32,
}

/// Frames to let pass before reading the palette and spawning.
const SPAWN_AFTER_FRAMES: u32 = 10;

enum Mode {
    Card,
    Rows(usize),
}

fn mode() -> Option<Mode> {
    let value = crate::debug_switches::env_value("CARTO_UI_PROBE")?;
    Some(match value.trim().parse::<usize>() {
        Ok(n) if n > 1 => Mode::Rows(n),
        _ => Mode::Card,
    })
}

/// An egui colour as Bevy's.
fn c(colour: egui::Color32) -> Color {
    let [r, g, b, a] = colour.to_array();
    Color::srgba_u8(r, g, b, a)
}

struct Faces {
    body: Handle<Font>,
    semibold: Handle<Font>,
}

impl Faces {
    fn text(&self, size: f32, colour: egui::Color32) -> (TextFont, TextColor) {
        (
            TextFont {
                font: self.body.clone().into(),
                font_size: FontSize::Px(size),
                ..default()
            },
            TextColor(c(colour)),
        )
    }

    fn strong(&self, size: f32, colour: egui::Color32) -> (TextFont, TextColor) {
        (
            TextFont {
                font: self.semibold.clone().into(),
                font_size: FontSize::Px(size),
                weight: FontWeight::SEMIBOLD,
                ..default()
            },
            TextColor(c(colour)),
        )
    }
}

/// Spawns the probe once the world camera exists, with `UiScale` set to
/// egui's `ui_scale` so a point here is the point egui lays out in.
fn spawn_probe(
    mut commands: Commands,
    mut probe: ResMut<UiProbe>,
    cameras: Query<Entity, With<FreeCamera>>,
    egui: EguiRef,
    mut fonts: ResMut<Assets<Font>>,
    mut ui_scale: ResMut<UiScale>,
) {
    if probe.done {
        return;
    }
    let Some(mode) = mode() else {
        probe.done = true;
        return;
    };
    let Ok(camera) = cameras.single() else {
        return;
    };
    probe.frames += 1;
    if probe.frames < SPAWN_AFTER_FRAMES {
        return;
    }
    probe.done = true;
    ui_scale.0 = egui.ui_scale();
    let faces = Faces {
        body: fonts.add(Font::from_bytes(theme::BODY_TTF.to_vec())),
        semibold: fonts.add(Font::from_bytes(theme::BRAND_TTF.to_vec())),
    };
    let compact = egui.compact();
    info!(
        ?compact,
        ui_scale = ui_scale.0,
        "ui probe: spawning bevy_ui {}",
        match mode {
            Mode::Card => "card".to_owned(),
            Mode::Rows(n) => format!("{n} rows"),
        }
    );
    match mode {
        Mode::Card => spawn_card(&mut commands, camera, &faces, compact),
        Mode::Rows(n) => spawn_rows(&mut commands, camera, &faces, n),
    }
}

/// The Settings panel's first screen, as `theme::controls` would draw it:
/// a panel title, a section head, a labelled text field with help, a body
/// paragraph, a primary and a ghost button, a switch row, and a short list
/// with hairlines.
fn spawn_card(commands: &mut Commands, camera: Entity, faces: &Faces, compact: bool) {
    let p = theme::p();
    let radius = f32::from(theme::RADIUS);
    let control = theme::control_height();
    let mut root = Node {
        position_type: PositionType::Absolute,
        left: px(16),
        top: px(16),
        flex_direction: FlexDirection::Column,
        row_gap: px(12),
        padding: px(16).all(),
        border: px(1).all(),
        border_radius: BorderRadius::all(px(f32::from(theme::RADIUS_LG))),
        ..default()
    };
    if compact {
        root.right = px(16);
    } else {
        root.width = px(360);
    }
    commands
        .spawn((
            root,
            BackgroundColor(c(p.panel)),
            BorderColor::all(c(p.panel_rim)),
            UiTargetCamera(camera),
        ))
        .with_children(|card| {
            // Panel title, as `theme::panel_header` sets it.
            card.spawn((
                Text::new("Settings"),
                faces.strong(theme::SIZE_PANEL_TITLE, p.cream),
            ));
            // Section head: sentence case with the count in the faint tone.
            card.spawn(Node {
                column_gap: px(6),
                align_items: AlignItems::Baseline,
                ..default()
            })
            .with_children(|row| {
                row.spawn((Text::new("Account"), faces.strong(theme::SIZE_SUB, p.text)));
                row.spawn((Text::new("1"), faces.text(theme::SIZE_SUB, p.text_dim)));
            });
            // `theme::labelled` around `theme::text_field`.
            card.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(4),
                ..default()
            })
            .with_children(|field| {
                field.spawn((
                    Text::new("Username"),
                    faces.text(theme::SIZE_SUB, p.text_dim),
                ));
                field
                    .spawn((
                        Node {
                            height: px(control),
                            align_items: AlignItems::Center,
                            padding: UiRect::horizontal(px(12)),
                            border: px(1).all(),
                            border_radius: BorderRadius::all(px(radius)),
                            ..default()
                        },
                        BackgroundColor(c(p.ink)),
                        BorderColor::all(c(p.control_edge)),
                    ))
                    .with_children(|well| {
                        well.spawn((
                            Node {
                                flex_grow: 1.0,
                                ..default()
                            },
                            TextInput,
                            EditableText {
                                allow_newlines: false,
                                ..EditableText::new("jeroen")
                            },
                            TextLayout::no_wrap(),
                            faces.text(theme::SIZE_VALUE, p.text),
                        ));
                    });
                field.spawn((
                    Text::new("Shown to the people you meet."),
                    faces.text(theme::SIZE_SUB, p.text_dim),
                ));
            });
            // A body paragraph, wrapped.
            card.spawn((
                Text::new(
                    "Auto is light while the real sun is above the horizon at the map, \
                     whatever the sky is set to. The scene's light changes only the sky.",
                ),
                faces.text(theme::SIZE_BODY, p.text),
            ));
            // Primary and ghost buttons, the web's measurements.
            card.spawn(Node {
                column_gap: px(8),
                ..default()
            })
            .with_children(|row| {
                row.spawn((
                    Node {
                        height: px(control),
                        padding: UiRect::horizontal(px(16)),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border_radius: BorderRadius::all(px(radius)),
                        ..default()
                    },
                    BackgroundColor(c(p.accent)),
                ))
                .with_children(|b| {
                    b.spawn((
                        Text::new("Save"),
                        faces.strong(theme::SIZE_BODY, p.on_accent),
                    ));
                });
                row.spawn((
                    Node {
                        height: px(control),
                        padding: UiRect::horizontal(px(16)),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        border_radius: BorderRadius::all(px(radius)),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .with_children(|b| {
                    b.spawn((Text::new("Cancel"), faces.text(theme::SIZE_BODY, p.text)));
                });
            });
            // A switch row: label left, the track right, knob on.
            card.spawn(Node {
                height: px(theme::row_height()),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            })
            .with_children(|row| {
                row.spawn((
                    Text::new("Street names"),
                    faces.text(theme::SIZE_BODY, p.text),
                ));
                row.spawn((
                    Node {
                        width: px(44),
                        height: px(24),
                        padding: px(2).all(),
                        justify_content: JustifyContent::FlexEnd,
                        align_items: AlignItems::Center,
                        border_radius: BorderRadius::all(px(12)),
                        ..default()
                    },
                    BackgroundColor(c(p.accent)),
                ))
                .with_children(|track| {
                    track.spawn((
                        Node {
                            width: px(20),
                            height: px(20),
                            border_radius: BorderRadius::all(px(10)),
                            ..default()
                        },
                        BackgroundColor(c(p.knob_on)),
                    ));
                });
            });
            // Three list rows with hairlines (`theme::list_row`).
            card.spawn(Node {
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .with_children(|list| {
                for (i, label) in ["General", "Graphics", "Audio & video"].iter().enumerate() {
                    list.spawn((
                        Node {
                            height: px(theme::row_height()),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::SpaceBetween,
                            border: UiRect::bottom(px(if i < 2 { 1.0 } else { 0.0 })),
                            ..default()
                        },
                        BorderColor::all(c(p.list_line)),
                    ))
                    .with_children(|row| {
                        row.spawn((Text::new(*label), faces.text(theme::SIZE_BODY, p.text)));
                        row.spawn((Text::new("›"), faces.text(theme::SIZE_BODY, p.text_dim)));
                    });
                }
            });
        });
}

/// `n` rows in one scrolling column over the whole viewport: a dot and a
/// line of text each, which is the shape of a chat or a list of places.
fn spawn_rows(commands: &mut Commands, camera: Entity, faces: &Faces, n: usize) {
    let p = theme::p();
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: px(0),
                bottom: px(0),
                flex_direction: FlexDirection::Column,
                overflow: Overflow {
                    x: OverflowAxis::Hidden,
                    y: OverflowAxis::Scroll,
                },
                padding: px(16).all(),
                ..default()
            },
            BackgroundColor(c(p.panel)),
            UiTargetCamera(camera),
        ))
        .with_children(|column| {
            for i in 0..n {
                column
                    .spawn((
                        Node {
                            height: px(theme::row_height()),
                            flex_shrink: 0.0,
                            align_items: AlignItems::Center,
                            column_gap: px(12),
                            border: UiRect::bottom(px(1)),
                            ..default()
                        },
                        BorderColor::all(c(p.list_line)),
                    ))
                    .with_children(|row| {
                        row.spawn((
                            Node {
                                width: px(12),
                                height: px(12),
                                flex_shrink: 0.0,
                                border_radius: BorderRadius::all(px(6)),
                                ..default()
                            },
                            BackgroundColor(c(p.accent)),
                        ));
                        row.spawn((
                            Text::new(format!("Row {i} · Groningen, Grote Markt")),
                            faces.text(theme::SIZE_BODY, p.text),
                        ));
                    });
            }
        });
}
