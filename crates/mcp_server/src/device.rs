//! What the app is asked about itself rather than about the world: its build,
//! its log, what it costs, its settings and its screen. The tool arguments, and
//! the [`DeviceRequest`] that carries them down the channel; the answer is an
//! [`AgentReply`](crate::args::AgentReply), as the account's tools' is.
//!
//! **Apart from [`AgentRequest`](crate::args::AgentRequest) because none of it
//! needs an avatar.** Every one of these answers offline, and the client serves
//! them from its own `mcp::device` and `mcp::screen`, not from the agent's
//! module, which is already the largest in the client.

use rmcp::schemars;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AppLogArgs {
    #[schemars(
        description = "Only lines at this level or worse: 'error', 'warn', 'info' (default) \
                       or 'debug'."
    )]
    pub level: Option<String>,
    #[schemars(description = "Only lines containing this text (a module name, a word).")]
    pub contains: Option<String>,
    #[schemars(
        description = "Only lines after this number, as an earlier answer gave it: the way to \
                       read what was logged since you last looked."
    )]
    pub after: Option<u64>,
    #[schemars(description = "At most this many lines, newest kept. Default 80, at most 400.")]
    pub limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PerfArgs {
    #[schemars(
        description = "Measure over this many seconds from now, 1 to 30, and answer when it \
                       ends. Omit for the figures as they stand."
    )]
    pub seconds: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetSettingArgs {
    #[schemars(description = "The setting's name, as `settings` lists it.")]
    pub name: String,
    #[schemars(description = "Its new value: true/false, a number, or one of the listed words.")]
    pub value: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetLayerArgs {
    #[schemars(description = "The layer's name, as `layers` lists it.")]
    pub name: String,
    #[schemars(description = "Show it (true) or hide it (false).")]
    pub on: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RouteArgs {
    #[schemars(description = "Where to, latitude (`my_places` and `find_place` give them).")]
    pub lat: f64,
    #[schemars(description = "Where to, longitude.")]
    pub lng: f64,
    #[schemars(
        description = "Where from, latitude; with `from_lng`. Default: where your avatar \
                       stands, or the middle of the view offline."
    )]
    pub from_lat: Option<f64>,
    pub from_lng: Option<f64>,
    #[schemars(description = "'foot' (default), 'bike' or 'car'.")]
    pub mode: Option<String>,
    #[schemars(
        description = "Also put the route on the screen, as the Route panel shows one. \
                       Default false: planning it changes nothing anybody sees."
    )]
    pub show: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FlyToArgs {
    #[schemars(description = "Latitude to fly to.")]
    pub lat: f64,
    #[schemars(description = "Longitude to fly to.")]
    pub lng: f64,
    #[schemars(
        description = "Metres above the ground to arrive at. Default: the height the camera \
                       has now."
    )]
    pub altitude_m: Option<f64>,
    #[schemars(
        description = "Wait up to this many seconds (at most 60) for the flight to land and \
                       answer then. Default: answer at once while it flies."
    )]
    pub wait_s: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct UiTreeArgs {
    #[schemars(description = "Only controls whose label contains this text.")]
    pub contains: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PressArgs {
    #[schemars(description = "The control's `ref`, as `ui_tree` gave it.")]
    pub r#ref: Option<u64>,
    #[schemars(
        description = "Or its label: the first control on screen whose label is this, \
                       ignoring case."
    )]
    pub label: Option<String>,
    #[schemars(description = "Hold it for a long press (a context menu). Default false.")]
    pub long: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AvatarArgs {
    #[schemars(
        description = "Also open the Appearance panel, which turns the camera onto the avatar \
                       (its Face tab comes close to the face), for a picture of the screen. \
                       Default false."
    )]
    pub show: Option<bool>,
}

/// A change to the saved look: every field left out stays as it is.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct SetAvatarArgs {
    #[schemars(
        description = "Start from one of `avatar`'s starters, by label, before the rest of \
                       these apply."
    )]
    pub starter: Option<String>,
    #[schemars(description = "0 feminine … 100 masculine; continuous.")]
    pub body_shape: Option<u8>,
    #[schemars(description = "Years, 18–90.")]
    pub age: Option<u8>,
    #[schemars(description = "0–100, 50 average.")]
    pub height: Option<u8>,
    #[schemars(description = "0–100, 50 average.")]
    pub weight: Option<u8>,
    #[schemars(description = "0–100, 50 average.")]
    pub muscle: Option<u8>,
    #[schemars(description = "0–100, 50 average: the body's proportions, ideal at 100.")]
    pub proportions: Option<u8>,
    #[schemars(description = "0–100, 50 average.")]
    pub chest: Option<u8>,
    #[schemars(description = "A skin family, as `avatar` lists them.")]
    pub skin: Option<String>,
    #[schemars(description = "-100 darker … 100 lighter, over the skin family's colour.")]
    pub skin_tone: Option<i8>,
    #[schemars(description = "An eye colour, as `avatar` lists them.")]
    pub eyes: Option<String>,
    #[schemars(description = "A hair style, as `avatar` lists them, or \"none\" for none.")]
    pub hair: Option<String>,
    #[schemars(description = "Hair colour [r, g, b], 0–255 each; [] for the style's own colour.")]
    pub hair_colour: Option<Vec<u8>>,
    #[schemars(description = "Eyebrows, as `avatar` lists them.")]
    pub eyebrows: Option<String>,
    #[schemars(
        description = "An outfit or a dress, as `avatar` lists them, or \"none\" to wear a top \
                       and a bottom from `extras` instead."
    )]
    pub outfit: Option<String>,
    #[schemars(
        description = "The outfit's upper half (or the top) [r, g, b]; [] for its own colour."
    )]
    pub top_colour: Option<Vec<u8>>,
    #[schemars(
        description = "The outfit's lower half (or the bottom) [r, g, b]; [] for its own colour."
    )]
    pub bottom_colour: Option<Vec<u8>>,
    #[schemars(description = "Shoes, as `avatar` lists them.")]
    pub shoes: Option<String>,
    #[schemars(description = "A hat, as `avatar` lists them, or \"none\".")]
    pub hat: Option<String>,
    #[schemars(
        description = "Everything worn beyond the outfit — a beard, glasses, a top and a \
                       bottom — by name, at most one of each kind. Replaces the whole list; \
                       [] wears none."
    )]
    pub extras: Option<Vec<String>>,
    #[schemars(
        description = "Face and body shape controls by modifier, as `avatar` lists them, \
                       -100 … 100 (0 … 100 for a one-sided one). Merged into the ones set: 0 \
                       resets one."
    )]
    pub shape: Option<std::collections::BTreeMap<String, i32>>,
    #[schemars(description = "Reset every face and body shape control before `shape` applies.")]
    pub reset_shape: Option<bool>,
    #[schemars(description = "Also open the Appearance panel, as `avatar`'s `show` does.")]
    pub show: Option<bool>,
}

#[derive(Debug)]
pub enum DeviceRequest {
    AppInfo,
    AppLog(AppLogArgs),
    LastCrash,
    Perf(PerfArgs),
    Diagnostics,
    Settings,
    SetSetting(SetSettingArgs),
    Layers,
    SetLayer(SetLayerArgs),
    Route(RouteArgs),
    FlyTo(FlyToArgs),
    UiTree(UiTreeArgs),
    Press(PressArgs),
    Avatar(AvatarArgs),
    SetAvatar(Box<SetAvatarArgs>),
}

impl DeviceRequest {
    /// The activity overlay's label for it.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AppInfo => "app info",
            Self::AppLog(_) => "app log",
            Self::LastCrash => "last crash",
            Self::Perf(_) => "perf",
            Self::Diagnostics => "diagnostics",
            Self::Settings => "settings",
            Self::SetSetting(_) => "set setting",
            Self::Layers => "layers",
            Self::SetLayer(_) => "set layer",
            Self::Route(_) => "route",
            Self::FlyTo(_) => "fly to",
            Self::UiTree(_) => "ui tree",
            Self::Press(_) => "press",
            Self::Avatar(_) => "avatar",
            Self::SetAvatar(_) => "set avatar",
        }
    }

    /// Answered by the client's `mcp::screen` (the egui side), not `mcp::device`.
    pub fn is_screen(&self) -> bool {
        matches!(self, Self::UiTree(_) | Self::Press(_))
    }
}
