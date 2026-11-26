//! What the Layers panel (`lib_layers`) reads beside [`DataLayers`], which
//! lives in `data_layers` with the streaming it drives. A module of its own so
//! the layer state the map reads does not depend on everything the panel shows.
//!
//! [`DataLayers`]: crate::systems::data_layers::DataLayers

use crate::grid::AnchoredCamera;
use bevy::prelude::*;

/// What the Layers panel needs to know about the view: whether the camera is in
/// space, which decides which tiles can be seen from here.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct LayerView<'w, 's> {
    globe: Res<'w, crate::systems::globe::GlobeState>,
    camera: AnchoredCamera<'w, 's>,
}

impl LayerView<'_, '_> {
    pub(crate) fn in_space(&self) -> bool {
        self.camera
            .get()
            .is_some_and(|(c, t)| self.globe.in_space(c, t))
    }
}

/// The Plans tile's label, which is also how its ⋯ is told open.
pub(crate) const PLANS_TILE: &str = "Plans";
