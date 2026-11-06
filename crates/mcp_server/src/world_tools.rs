//! The map's tools: what is at a point, distances, whether a build stands on
//! the ground, buildings and their floors, the sky, transit and the
//! municipality's notices. Each is a [`WorldRequest`] answered by the client's
//! `mcp::world`; see [`crate::world`] for why they are apart.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, ErrorData};

use super::Tools;
use crate::protocol::Request;
use crate::world::{
    BuildingArgs, CheckGeometryArgs, DeparturesArgs, LookInsideArgs, MeasureArgs, NoticesArgs,
    ProbeArgs, SetSkyArgs, VehiclesArgs, WorldRequest,
};

#[tool_router(router = world_router, vis = "pub(super)")]
impl Tools {
    #[tool(
        description = "What is at one point on the map, without a picture: the height of the \
                       ground, water and its level, the paving or grass, a bridge deck or a \
                       tunnel, the building it is inside (with its address), and any objects \
                       standing there. The cheap way to check where something would stand \
                       before you build it."
    )]
    async fn probe(
        &self,
        Parameters(args): Parameters<ProbeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Probe(args)).await
    }

    #[tool(
        description = "Distances on the ground between points: two give the distance and the \
                       compass bearing from the first to the second; more give each leg and \
                       the total, and three or more also the area they enclose."
    )]
    async fn measure(
        &self,
        Parameters(args): Parameters<MeasureArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Measure(args)).await
    }

    #[tool(
        description = "Check objects for the mistakes a picture hides: floating above the \
                       ground, sunk into it, overlapping each other, or reaching into a \
                       building. Measured from the objects' shapes and the ground under \
                       them, with nothing drawn — run it after every `build`. Shapes are \
                       judged by their boxes, so a sphere counts as the box round it; a \
                       model is judged by its `scale` box, not its own size, so check one \
                       with a picture."
    )]
    async fn check_geometry(
        &self,
        Parameters(args): Parameters<CheckGeometryArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::CheckGeometry(args)).await
    }

    #[tool(
        description = "What the register says about a building (the Netherlands only): its \
                       addresses, what it is used for, when it was built, its height and \
                       floors, floor area, whether it is a listed monument, and its outline. \
                       With `select` it is also picked out on the screen with its card."
    )]
    async fn building(
        &self,
        Parameters(args): Parameters<BuildingArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Building(args)).await
    }

    #[tool(
        description = "Show the inside of a building on the screen: its walls fade, one \
                       storey is drawn with its rooms, and the floor picker opens. Answers \
                       which storeys there are. `stop` closes it again."
    )]
    async fn look_inside(
        &self,
        Parameters(args): Parameters<LookInsideArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::LookInside(args)).await
    }

    #[tool(
        description = "Set the time, the day and the weather the world is drawn with: \
                       evening light, a winter sun, clouds, fog or rain. It holds until \
                       changed, and the person sees it too, so put it back with `live` when \
                       you are done. Use it to compare pictures under one light."
    )]
    async fn set_sky(
        &self,
        Parameters(args): Parameters<SetSkyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::SetSky(args)).await
    }

    #[tool(
        description = "The next buses, trams and trains from the stops near a point, live \
                       (the Netherlands): line, destination, when it leaves and how late it \
                       is. Only for stops near what the map has loaded — `fly_to` a place \
                       first if it is far away."
    )]
    async fn departures(
        &self,
        Parameters(args): Parameters<DeparturesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Departures(args)).await
    }

    #[tool(
        description = "Where buses, trams and trains are right now (the Netherlands), from \
                       the live vehicle feed: by line, nearest first, with how far away each \
                       is."
    )]
    async fn vehicles(
        &self,
        Parameters(args): Parameters<VehiclesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Vehicles(args)).await
    }

    #[tool(
        description = "What the municipality has published about places near a point \
                       (Groningen for now): permit applications and decisions, demolitions, \
                       events, traffic orders — each with its date, the last day to respond, \
                       and a link to the official text."
    )]
    async fn notices(
        &self,
        Parameters(args): Parameters<NoticesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.world(WorldRequest::Notices(args)).await
    }
}

impl Tools {
    async fn world(&self, request: WorldRequest) -> Result<CallToolResult, ErrorData> {
        self.answer(Request::World(request)).await
    }
}
