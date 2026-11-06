//! Questions about the map rather than the app or the account: what is at a
//! point, how far apart two are, whether a build stands on the ground, what a
//! building is, what leaves the stop round the corner, what the municipality
//! published, and the sky the world is lit by. The tool arguments, and the
//! [`WorldRequest`] that carries them down the channel; the answer is an
//! [`AgentReply`](crate::args::AgentReply), as the device tools' is.
//!
//! **Apart from [`DeviceRequest`](crate::device::DeviceRequest) because these
//! read the map's own resources** — the terrain field, the register, the
//! transit feeds — and the client's `mcp::world` holds the dozen of them, which
//! would push `mcp::device::serve` past Bevy's parameter cap. None needs an
//! avatar: each answers offline, about whatever the map has loaded.

use rmcp::schemars;

/// A point: latitude and longitude, both optional where the tool has a default.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProbeArgs {
    #[schemars(
        description = "Latitude. Default: where your avatar stands, or the middle of the \
                       view offline."
    )]
    pub lat: Option<f64>,
    #[schemars(description = "Longitude; with `lat`.")]
    pub lng: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MeasureArgs {
    #[schemars(
        description = "Two or more points as [lat, lng] pairs, in order. Two give a distance \
                       and a bearing; more give each leg and the total, and with three or \
                       more the area of the shape they close."
    )]
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CheckGeometryArgs {
    #[schemars(
        description = "The objects to check, by id (`build` and `my_builds` give them). \
                       Default: every object within `radius_m` of the point."
    )]
    pub ids: Option<Vec<i64>>,
    #[schemars(description = "Centre to search round, latitude. Default: as `probe`.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Search radius in metres, default 40, at most 300.")]
    pub radius_m: Option<f32>,
    #[schemars(
        description = "How far off the ground still counts as standing on it, in metres. \
                       Default 0.05."
    )]
    pub tolerance_m: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BuildingArgs {
    #[schemars(
        description = "A point inside the building, latitude. Default: the building \
                       selected on the screen, else the one at your avatar or the middle of \
                       the view."
    )]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(
        description = "Also select it on the screen, as a double tap does: the glass \
                       outline and its card. Default false."
    )]
    pub select: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LookInsideArgs {
    #[schemars(
        description = "A point inside the building, latitude. Default: the building \
                       selected on the screen."
    )]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(
        description = "The storey to show: 0 the ground floor, 1 the first, and so on. \
                       Default: the ground floor."
    )]
    pub floor: Option<u32>,
    #[schemars(description = "Close it again: the walls back and the floor picker gone.")]
    pub stop: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetSkyArgs {
    #[schemars(
        description = "The clock time to light the world for, \"HH:MM\" in the place's own \
                       time zone (\"17:30\")."
    )]
    pub time: Option<String>,
    #[schemars(
        description = "The day, \"YYYY-MM-DD\" or \"MM-DD\": it decides how high the sun \
                       climbs and when it sets."
    )]
    pub date: Option<String>,
    #[schemars(description = "Cloud cover, 0 (clear) to 1 (overcast).")]
    pub clouds: Option<f32>,
    #[schemars(description = "Fog, 0 to 1.")]
    pub fog: Option<f32>,
    #[schemars(description = "Rain, 0 to 1.")]
    pub rain: Option<f32>,
    #[schemars(
        description = "Back to the real clock and the live weather, undoing every \
                       change above. Do this when you are done."
    )]
    pub live: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DeparturesArgs {
    #[schemars(description = "Near this point, latitude. Default: as `probe`.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Stops within this many metres, default 300, at most 800.")]
    pub radius_m: Option<f32>,
    #[schemars(description = "Only this line (\"4\", \"N93\").")]
    pub line: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct VehiclesArgs {
    #[schemars(description = "Only this line (\"4\", \"N93\").")]
    pub line: Option<String>,
    #[schemars(description = "Near this point, latitude. Default: as `probe`.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Within this many metres, default 3000.")]
    pub radius_m: Option<f32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoticesArgs {
    #[schemars(description = "Near this point, latitude. Default: as `probe`.")]
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    #[schemars(description = "Within this many metres, default 300.")]
    pub radius_m: Option<f32>,
    #[schemars(
        description = "Only notices still open to respond to (default), or false for \
                       everything published in the feed."
    )]
    pub open_only: Option<bool>,
}

#[derive(Debug)]
pub enum WorldRequest {
    Probe(ProbeArgs),
    Measure(MeasureArgs),
    CheckGeometry(CheckGeometryArgs),
    Building(BuildingArgs),
    LookInside(LookInsideArgs),
    SetSky(SetSkyArgs),
    Departures(DeparturesArgs),
    Vehicles(VehiclesArgs),
    Notices(NoticesArgs),
}

impl WorldRequest {
    /// The activity overlay's label for it.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Probe(_) => "probe",
            Self::Measure(_) => "measure",
            Self::CheckGeometry(_) => "check geometry",
            Self::Building(_) => "building",
            Self::LookInside(_) => "look inside",
            Self::SetSky(_) => "set sky",
            Self::Departures(_) => "departures",
            Self::Vehicles(_) => "vehicles",
            Self::Notices(_) => "notices",
        }
    }
}
