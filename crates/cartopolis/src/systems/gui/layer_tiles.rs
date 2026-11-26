//! The Layers panel's categories: their order, names, icons and subtitles.
//! The panel itself (the disc row, the layer rows, the base-map choice) is
//! drawn by the component library (`lib_layers`); which layers exist and what
//! they switch is `layers_panel` and `data_layers`.

use crate::systems::icons;

pub use crate::systems::layer_state::LayerCategory;

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
