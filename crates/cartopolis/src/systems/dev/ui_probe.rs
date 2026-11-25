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
            .insert_resource(crate::systems::lib_settings::LibScreens::from_env())
            // Cartopolis' own tokens: the same `Tokens` the web's `theme.css`
            // is written from, so the library draws the app's palette here
            // and the site's there from one table.
            .add_plugins(mithril_bevy::MithrilUiPlugin {
                tokens: theme::app_tokens(),
            })
            .add_systems(Update, spawn_probe.in_set(ClientPhase::Input))
            // Before egui's systems: a screen claimed only in `PostUpdate` was drawn
            // by egui for a frame first.
            .add_systems(PreUpdate, crate::systems::lib_search::claim)
            // After every Update system has had its say on `RightPanel`, and
            // before the library applies the frame's operations.
            .add_systems(
                PostUpdate,
                (
                    crate::systems::lib_host::prepare_bevy,
                    crate::systems::lib_host::follow_world_target,
                    crate::systems::lib_settings::drive_lib_settings,
                    crate::systems::lib_layers::drive_lib_layers,
                    crate::systems::lib_search::drive_lib_search,
                    crate::systems::lib_route::drive_lib_route,
                    crate::systems::lib_places::drive_lib_places,
                    crate::systems::lib_place_card::drive_lib_place_card,
                    crate::systems::lib_building_card::drive_lib_building_card,
                    crate::systems::lib_host::bridge_input,
                )
                    .chain()
                    .before(mithril_bevy::DriveUi),
            )
            .add_systems(
                PostUpdate,
                (
                    crate::systems::lib_app_update::drive_lib_app_update,
                    crate::systems::lib_keyboard::drive_lib_keyboard,
                    crate::systems::lib_avatar::drive_lib_avatar,
                    crate::systems::lib_models::drive_lib_models,
                    crate::systems::lib_script::drive_lib_script,
                    crate::systems::lib_plans::drive_lib_plans,
                    crate::systems::lib_scenario::drive_lib_scenario,
                    crate::systems::lib_design_room::drive_lib_design_room,
                )
                    .chain()
                    .after(crate::systems::lib_host::prepare_bevy)
                    .before(crate::systems::lib_host::bridge_input),
            );
        // The screens that talk, the app's only (`comms`): between the
        // screens above and the input bridge.
        #[cfg(feature = "comms")]
        app.add_systems(
            PostUpdate,
            (
                crate::systems::lib_chats::drive_lib_chats,
                crate::systems::lib_admin::drive_lib_admin,
                crate::systems::lib_person_card::drive_lib_person_card,
                crate::systems::lib_reminder::drive_lib_reminder,
                crate::systems::lib_filter_chip::drive_lib_filter_chip,
                crate::systems::lib_post_composer::drive_lib_post_composer,
                crate::systems::lib_login::drive_lib_login,
                crate::systems::lib_welcome::drive_lib_welcome,
            )
                .chain()
                .after(crate::systems::lib_host::prepare_bevy)
                .before(crate::systems::lib_host::bridge_input),
        );
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
    /// The component library through its Bevy renderer (`mithril_bevy`):
    /// one of each component, as the browser's parity page shows them.
    Library,
}

fn mode() -> Option<Mode> {
    let value = crate::debug_switches::env_value("CARTO_UI_PROBE")?;
    if value.trim() == "mithril" {
        return Some(Mode::Library);
    }
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
    // The library's host is thread-local: this system must run where it lives.
    _main: bevy::ecs::system::NonSendMarker,
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
            Mode::Library => "the component library".to_owned(),
        }
    );
    match mode {
        Mode::Card => spawn_card(&mut commands, camera, &faces, compact),
        Mode::Rows(n) => spawn_rows(&mut commands, camera, &faces, n),
        Mode::Library => {
            commands.insert_resource(mithril_bevy::UiCamera(camera));
            commands.insert_resource(mithril_bevy::FontMap {
                regular: faces.body.clone(),
                semibold: faces.semibold.clone(),
                mono: faces.body.clone(),
                icons: None,
            });
            let p = theme::p();
            mithril_bevy::with_host(|host| {
                host.env.dark = p.ink == theme::DARK.ink;
                host.env.coarse_pointer = theme::touch_layout();
                host.mount(library::page(compact));
            });
            // The same page as HTML, for the browser beside the capture
            // (`CARTO_UI_PROBE_HTML=/tmp/page.html`). A dev tool: `std::fs`.
            if let Some(path) = std::env::var_os("CARTO_UI_PROBE_HTML") {
                let html = library::html(compact);
                if let Err(e) = std::fs::write(&path, html) {
                    tracing::warn!(?path, error = %e, "ui probe: could not write the page's HTML");
                }
            }
        }
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

/// The library's components, one of each in a fixed state, as the browser's
/// parity page shows them (`examples/parity` in jeroen/mithril): what the
/// Bevy renderer is photographed against.
mod library {
    use std::rc::Rc;

    use mithril::prelude::*;

    /// The page's HTML, from the library's string renderer: what the browser
    /// draws from the same tree, for the comparison.
    pub fn html(compact: bool) -> String {
        use std::cell::RefCell;

        use mithril::view::{mount, StringRenderer};
        let r = Rc::new(RefCell::new(StringRenderer::new()));
        let root = r.borrow().root();
        let mounted = mount(page(compact), &r, &root, None);
        let html = r.borrow().html();
        drop(mounted);
        html
    }

    /// A column of specimens. `compact` makes it the viewport's width.
    pub fn page(compact: bool) -> View {
        let style = if compact {
            "position: absolute; left: 0; right: 0; top: 0; bottom: 0; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 20px; background: var(--m-bg)"
        } else {
            "position: absolute; left: 16px; top: 16px; bottom: 16px; width: 420px; overflow-y: auto; padding: 16px; display: flex; flex-direction: column; gap: 20px; background: var(--m-bg); border-radius: 12px"
        };
        let specimens: Vec<(&'static str, View)> = vec![
            (
                "Buttons",
                view! {
                    <div style="display: flex; gap: 8px; flex-wrap: wrap">
                        <Button variant=Variant::Primary>"Save"</Button>
                        <Button>"Cancel"</Button>
                        <Button variant=Variant::Ghost>"Ghost"</Button>
                        <Button variant=Variant::Danger size=Size::Sm>"Delete"</Button>
                        <Button variant=Variant::Ghost loading=true>"Wait"</Button>
                    </div>
                },
            ),
            (
                "Badge · spinner · progress",
                view! {
                    <div style="display: flex; flex-direction: column; gap: 12px">
                        <div style="display: flex; gap: 8px; align-items: center">
                            <Badge tone=Tone::Success dot=true>"Live"</Badge>
                            <Badge>"3"</Badge>
                            <Spinner size=Size::Sm />
                        </div>
                        <Progress value=Some(40.0) label="Upload" show_value=true />
                    </div>
                },
            ),
            (
                "Alert",
                view! {
                    <Alert tone=Tone::Danger title="Failed" ondismiss={Some(Rc::new(|| {}) as Rc<dyn Fn()>)}>
                        "Try again"
                    </Alert>
                },
            ),
            (
                "Card",
                view! {
                    <Card title="Settings" subtitle="Account" actions={view! { <Badge>"1"</Badge> }} footer={view! { <Button variant=Variant::Primary size=Size::Sm>"Save"</Button> }}>
                        "Auto is light while the real sun is above the horizon at the map, whatever the sky is set to."
                    </Card>
                },
            ),
            (
                "Text fields",
                view! {
                    <div style="display: flex; flex-direction: column; gap: 12px">
                        <FieldText model={signal(String::from("sam"))} label="Username" help="Shown to the people you meet." required=true id="p1" />
                        <FieldText model={signal(String::new())} label="Email" error="Required" placeholder="you@example.org" id="p2" />
                        <FieldTextarea model={signal(String::from("hi"))} label="Bio" rows=3 maxlength=Some(80) id="p3" />
                        <FieldNumber model={signal(Some(5.0))} min=Some(0.0) max=Some(10.0) unit="kg" label="Weight" id="p4" />
                    </div>
                },
            ),
            (
                "Toggles",
                view! {
                    <div style="display: flex; flex-direction: column; gap: 12px">
                        <FieldCheckbox model={signal(true)} label="Agree" help="please" id="p5" />
                        <FieldCheckbox model={signal(false)} label="Not yet" id="p6" />
                        <FieldSwitch model={signal(true)} label="Street names" id="p7" />
                        <FieldSwitch model={signal(false)} label="Shadows" id="p8" />
                    </div>
                },
            ),
            (
                "Choices",
                view! {
                    <div style="display: flex; flex-direction: column; gap: 12px">
                        <FieldSelect model={signal(Some(String::from("b")))}
                            options={vec![Opt::from(String::from("a")), Opt::from(String::from("b")), Opt::new(String::from("c"), "C")]}
                            placeholder="Pick" label="Letter" id="p9" />
                        <FieldRadio model={signal(String::from("x"))}
                            options={vec![Opt::from(String::from("x")), Opt::from(String::from("y"))]}
                            inline=true label="Axis" id="p10" />
                        <FieldSegmented model={signal(String::from("md"))}
                            options={vec![Opt::from(String::from("sm")), Opt::from(String::from("md")), Opt::from(String::from("lg"))]}
                            label="Size" id="p11" />
                        <FieldSlider model={signal(25.0)} max=50.0 step=5.0 label="Volume" id="p12" />
                    </div>
                },
            ),
            (
                "Tabs · collapse",
                view! {
                    <div style="display: flex; flex-direction: column; gap: 12px">
                        <Tabs model={signal(String::from("a"))} variant=TabsVariant::Pill
                            tabs={vec![
                                TabItem::new(String::from("a"), "General").content(|| "Panel A".into_view()),
                                TabItem::new(String::from("b"), "Graphics").badge(3),
                            ]} />
                        <Collapse model={signal(true)} title="More">"body"</Collapse>
                    </div>
                },
            ),
        ];
        let cards: Vec<View> = specimens
            .into_iter()
            .map(|(name, body)| {
                view! {
                    <section style="display: flex; flex-direction: column; gap: 8px">
                        <h2 style="font-size: 13px; font-weight: 600; color: var(--m-text-dim); text-transform: uppercase; letter-spacing: 0.6px; margin: 0">{name}</h2>
                        {body}
                    </section>
                }
            })
            .collect();
        view! { <main style={style}>{cards}</main> }
    }
}
