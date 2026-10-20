//! The city's sound: what is around the avatar, which recorded background
//! loops that calls for, and what its feet are landing on.
//!
//! # Why it is recordings now
//!
//! Every sound the soundscape made was synthesised, and every complaint about
//! it was a synthesis artefact: walking "sounded like a wind tunnel", the steps
//! were "cheap", then a metronome, and the road hum "did not sound like
//! traffic" (`docs/notes/city-soundscape.md`). These are CC0 recordings
//! (`assets/sounds/LICENSE.md`), and the code's job is only to choose and mix
//! them from what the map already knows.
//!
//! # What the mix is made of, and what it is not
//!
//! Every input is read from data the client already holds for drawing: the
//! roads and their speeds (`traffic::DriveNetwork`), shops and cafés
//! (`WorldPlaces`), lawns (`GrassAreas`), registered water (`TerrainField`),
//! the walls (`BuildingColliders`), the interiors, the live weather and the
//! sun. A background loop is a texture over that, like the stone pattern drawn
//! over a pavement — it places nothing. Single sounds tied to one spot (a
//! church bell, a gull) would be placements, and are only for a source the
//! data names.
//!
//! The decisions are pure functions here, tested without an audio device —
//! this container has none, and the playback half has never been heard in it.
//! The sampling and playback systems are behind the `audio` feature.

// Without `audio` nothing outside the tests calls in; see `soundscape`.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use cartopolis_geo::paving::{PavingKind, PavingMaterial};

/// What the sampler found around the avatar, every input `0..=1`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Surroundings {
    /// The loudest road in earshot, by its speed and distance
    /// ([`road_loudness`]).
    pub traffic: f32,
    /// How many shops, cafés and bars are near, saturating at a shopping street.
    pub shops: f32,
    /// Share of the probe points that land on grass.
    pub green: f32,
    /// Share of the probe points that land on or beside water.
    pub water: f32,
    /// Share of the probe rays stopped by a wall: 0 on an open square or
    /// field, 1 in a narrow street.
    pub enclosure: f32,
    pub indoors: bool,
    /// 0 at night, 1 by mid-morning (`utils::sun::daylight`).
    pub daylight: f32,
    /// The rain in effect, live or from the slider.
    pub rain: f32,
}

/// The recorded background loops. The order is the asset order and the mix's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bed {
    /// Traffic on a through road.
    Traffic,
    /// A pedestrian shopping street: footsteps and murmur, no words.
    Shopping,
    /// A quiet street with houses: distant traffic, a few birds.
    Residential,
    /// A park: birds over a distant city.
    Park,
    /// A canal side: water lapping at the quay.
    Water,
    /// An open square.
    Square,
    /// The city at night.
    Night,
    /// The quiet of a room.
    Indoor,
    /// Rain on a street.
    Rain,
}

impl Bed {
    pub const ALL: [Bed; 9] = [
        Bed::Traffic,
        Bed::Shopping,
        Bed::Residential,
        Bed::Park,
        Bed::Water,
        Bed::Square,
        Bed::Night,
        Bed::Indoor,
        Bed::Rain,
    ];

    /// The file under `assets/sounds/ambience/`.
    #[must_use]
    pub fn file(self) -> &'static str {
        match self {
            Bed::Traffic => "traffic.ogg",
            Bed::Shopping => "shopping.ogg",
            Bed::Residential => "residential.ogg",
            Bed::Park => "park.ogg",
            Bed::Water => "water.ogg",
            Bed::Square => "square.ogg",
            Bed::Night => "night.ogg",
            Bed::Indoor => "indoor.ogg",
            Bed::Rain => "rain.ogg",
        }
    }
}

/// How loud each outdoor loop is heard from inside. A level, not a filter:
/// bevy's mixer has no low-pass, so a room makes the street quieter rather
/// than duller. The room tone on top is what says "inside".
const INDOOR_LEAK: f32 = 0.18;

/// The loudest the outdoor beds may sum to. The mix is a set of shares, and
/// without a ceiling a canal beside a busy road beside a park would be three
/// full beds at once.
const OUTDOOR_CEILING: f32 = 1.0;

/// The level of each [`Bed`], in [`Bed::ALL`] order, before the user's volume.
#[must_use]
pub fn bed_mix(s: &Surroundings) -> [f32; 9] {
    let c = |v: f32| v.clamp(0.0, 1.0);
    let day = c(s.daylight);
    let night = 1.0 - day;
    let (traffic, shops, green, water, enclosure, rain) = (
        c(s.traffic),
        c(s.shops),
        c(s.green),
        c(s.water),
        c(s.enclosure),
        c(s.rain),
    );

    // Each place's share. Traffic thins at night but never stops; shops and
    // birds stop; rain keeps people off the street and silences the birds.
    let mut beds = [0.0f32; 9];
    beds[0] = traffic * (0.45 + 0.55 * day);
    beds[1] = shops * day * (1.0 - 0.7 * rain);
    beds[3] = green * day * (1.0 - 0.7 * rain);
    beds[4] = water * 0.8;
    // A square is hard ground open on every side: what is left when the walls,
    // the lawns and the water are taken out.
    beds[5] = (1.0 - enclosure) * (1.0 - green) * (1.0 - water) * 0.5 * day;
    // Residential is the floor under everything else by day, and night the
    // floor after dark, so nowhere outdoors is silent.
    let taken: f32 = beds.iter().sum();
    let floor = (1.0 - taken).max(0.25);
    beds[2] = floor * day;
    beds[6] = floor.max(0.5) * night;

    let outdoor: f32 = beds.iter().sum();
    if outdoor > OUTDOOR_CEILING {
        let k = OUTDOOR_CEILING / outdoor;
        beds.iter_mut().for_each(|b| *b *= k);
    }
    beds[8] = rain;

    if s.indoors {
        beds.iter_mut().for_each(|b| *b *= INDOOR_LEAK);
        beds[7] = 1.0;
    }
    beds
}

/// How loud a road of `speed` m/s is from `distance` metres, `0..=1`.
///
/// Level falls off with distance from a line source (3 dB per doubling) and
/// faster past the first row of houses, which this does not know about, so it
/// is cut off at [`ROAD_REACH`]. A main road at 15 m/s heard from its
/// pavement is about full; a 30 km/h back street is a third of that.
#[must_use]
pub fn road_loudness(speed: f32, distance: f32) -> f32 {
    let source = (speed / 15.0).clamp(0.0, 1.4);
    let near = 8.0;
    let d = distance.max(near);
    let falloff = (near / d).sqrt();
    let fade = 1.0 - (d / ROAD_REACH).clamp(0.0, 1.0);
    (source * falloff * fade).clamp(0.0, 1.0)
}

/// How far a road is listened for, in metres.
pub const ROAD_REACH: f32 = 120.0;

/// How many shops, cafés and bars within [`SHOP_REACH`] make a shopping
/// street. The Herestraat has well over this within 40 m of any point.
pub const SHOPS_FOR_A_STREET: f32 = 12.0;
pub const SHOP_REACH: f32 = 40.0;

/// What a footstep lands on — one set of recorded steps each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepSurface {
    /// Fired clay or concrete block pavers: the Dutch street.
    Brick,
    Asphalt,
    /// Slabs, tiles, poured concrete, natural stone.
    Stone,
    /// Loose stone, shell, sand.
    Gravel,
    /// Lawn, a planted bed, bare earth.
    Grass,
    /// A wooden floor indoors.
    Indoor,
}

impl StepSurface {
    pub const ALL: [StepSurface; 6] = [
        StepSurface::Brick,
        StepSurface::Asphalt,
        StepSurface::Stone,
        StepSurface::Gravel,
        StepSurface::Grass,
        StepSurface::Indoor,
    ];

    /// File prefix under `assets/sounds/steps/`; the takes are `-1.ogg` up.
    #[must_use]
    pub fn stem(self) -> &'static str {
        match self {
            StepSurface::Brick => "brick",
            StepSurface::Asphalt => "asphalt",
            StepSurface::Stone => "stone",
            StepSurface::Gravel => "gravel",
            StepSurface::Grass => "grass",
            StepSurface::Indoor => "indoor",
        }
    }

    /// Hard ground, which sounds different wet.
    #[must_use]
    pub fn is_hard(self) -> bool {
        matches!(
            self,
            StepSurface::Brick | StepSurface::Asphalt | StepSurface::Stone
        )
    }

    /// Level of this surface's takes against the others. Soft ground is
    /// quieter under the same foot.
    #[must_use]
    pub fn level(self) -> f32 {
        match self {
            StepSurface::Brick | StepSurface::Stone => 1.0,
            StepSurface::Asphalt => 0.85,
            StepSurface::Gravel => 0.9,
            StepSurface::Grass => 0.6,
            StepSurface::Indoor => 0.8,
        }
    }
}

/// What the avatar is stepping on, from the survey where it says, the lawns
/// where it does not, and asphalt otherwise: off the survey is nearly always an
/// OpenStreetMap carriageway, which the BGT layer leaves to OSM.
#[must_use]
pub fn step_surface(
    paving: Option<(PavingKind, Option<PavingMaterial>)>,
    on_grass: bool,
    indoors: bool,
) -> StepSurface {
    if indoors {
        return StepSurface::Indoor;
    }
    let Some((kind, material)) = paving else {
        return if on_grass {
            StepSurface::Grass
        } else {
            StepSurface::Asphalt
        };
    };
    if let Some(m) = material {
        return match m {
            PavingMaterial::Asphalt => StepSurface::Asphalt,
            PavingMaterial::Brick | PavingMaterial::ConcretePaver => StepSurface::Brick,
            PavingMaterial::Concrete | PavingMaterial::Tiles | PavingMaterial::Stone => {
                StepSurface::Stone
            }
            PavingMaterial::Gravel | PavingMaterial::Sand => StepSurface::Gravel,
            PavingMaterial::GrassPaver
            | PavingMaterial::Grass
            | PavingMaterial::Planting
            | PavingMaterial::Unpaved => StepSurface::Grass,
        };
    }
    // The kind alone: what a Dutch street of that kind is usually laid in.
    match kind {
        PavingKind::Verge => StepSurface::Grass,
        PavingKind::Garden if on_grass => StepSurface::Grass,
        PavingKind::Road | PavingKind::Street | PavingKind::Parking | PavingKind::Cycleway => {
            StepSurface::Asphalt
        }
        _ => StepSurface::Brick,
    }
}

/// The echo off the walls either side, for a footstep: `(delay s, level)`, or
/// `None` where there is nothing close enough to answer.
///
/// One reflection, not a reverb — bevy's mixer has none. A narrow street's
/// facades send a step back as a short slap; past ~20 m the echo is too late
/// and too faint to read as the same step, and indoors the room tone carries
/// the room.
#[must_use]
pub fn step_echo(left: Option<f32>, right: Option<f32>) -> Option<(f32, f32)> {
    const SPEED_OF_SOUND: f32 = 343.0;
    const NEAREST_HEARD: f32 = 2.5;
    const FARTHEST_HEARD: f32 = 20.0;
    let walls: Vec<f32> = [left, right]
        .into_iter()
        .flatten()
        .filter(|d| (NEAREST_HEARD..FARTHEST_HEARD).contains(d))
        .collect();
    if walls.is_empty() {
        return None;
    }
    let d = walls.iter().sum::<f32>() / walls.len() as f32;
    // Both sides answering is a street; one is a wall beside a square.
    let both = if walls.len() == 2 { 1.0 } else { 0.6 };
    let level = 0.32 * both * (1.0 - d / FARTHEST_HEARD);
    Some((2.0 * d / SPEED_OF_SOUND, level))
}

#[cfg(feature = "audio")]
pub use playback::*;

/// The ECS half: load the recordings, sample the surroundings a few times a
/// second, and ease each loop toward its level.
#[cfg(feature = "audio")]
mod playback {
    use super::*;
    use bevy::audio::{AudioPlayer, AudioSink, AudioSource, PlaybackMode, PlaybackSettings, Volume};
    use bevy::prelude::*;
    use std::collections::HashMap;

    use crate::resources::{AppPrefs, AvatarState, CameraMode, CameraState, DevPanelState};
    use crate::systems::collision::BuildingColliders;
    use crate::systems::grass::GrassAreas;
    use crate::systems::interiors::InteriorFloors;
    use crate::systems::places::PoiCategory;
    use crate::systems::terrain::TerrainField;
    use crate::systems::traffic::DriveNetwork;
    use crate::systems::world_places::{InteractKind, WorldPlaces};

    /// How many takes each surface has on disk (`assets/sounds/steps/`).
    pub const STEP_TAKES: usize = 6;
    /// Wet takes, used for every hard surface in the rain.
    pub const WET_TAKES: usize = 6;
    /// Above this much rain, hard ground sounds wet.
    const WET_ABOVE: f32 = 0.15;

    /// How often the surroundings are sampled, in seconds. The answer moves at
    /// walking pace; the cost is a scan of the loaded roads and walls.
    const SAMPLE_EVERY: f32 = 0.25;
    /// How fast a loop's level follows its target, per second. Slow: walking
    /// round a corner off a main road takes seconds, and so should the sound.
    const BED_RATE: f32 = 0.8;
    /// Overall level of the loops under the footsteps.
    const BED_GAIN: f32 = 0.55;
    /// The ring the green and water are probed on, and the length of the rays
    /// that find the walls.
    const PROBE_RADIUS: f32 = 14.0;
    const RAY_LENGTH: f32 = 30.0;
    /// Head height, for the wall rays: a wall only counts at its own height.
    const EAR_HEIGHT: f32 = 1.6;

    #[derive(Resource)]
    pub struct AmbienceSounds {
        steps: HashMap<StepSurface, Vec<Handle<AudioSource>>>,
        wet: Vec<Handle<AudioSource>>,
    }

    /// What the avatar is standing on and how the walls answer it, refreshed by
    /// [`sample_surroundings`]; the footsteps read it.
    #[derive(Resource, Default)]
    pub struct Underfoot {
        pub surface: Option<StepSurface>,
        pub wet: bool,
        pub echo: Option<(f32, f32)>,
        target: [f32; 9],
        current: [f32; 9],
    }

    impl AmbienceSounds {
        /// A recorded take for this surface, if it has loaded; `None` sends the
        /// caller to the synthesised step. `pick` chooses among the takes.
        pub fn step(
            &self,
            surface: StepSurface,
            wet: bool,
            pick: usize,
            sources: &Assets<AudioSource>,
        ) -> Option<Handle<AudioSource>> {
            let takes = if wet && surface.is_hard() && !self.wet.is_empty() {
                &self.wet
            } else {
                self.steps.get(&surface)?
            };
            let h = takes.get(pick % takes.len().max(1))?;
            sources.contains(h).then(|| h.clone())
        }
    }

    #[derive(Component)]
    pub struct BedSound(usize);

    /// Load the recordings and start every loop, silent. Started up front for
    /// the wind's reason: a loop started mid-walk has an attack, and a bed
    /// should arrive without ever being noticed.
    pub fn setup_ambience(mut commands: Commands, assets: Res<AssetServer>) {
        let beds: Vec<Handle<AudioSource>> = Bed::ALL
            .iter()
            .map(|b| assets.load(format!("sounds/ambience/{}", b.file())))
            .collect();
        let steps = StepSurface::ALL
            .iter()
            .map(|s| {
                let takes = (1..=STEP_TAKES)
                    .map(|n| assets.load(format!("sounds/steps/{}-{n}.ogg", s.stem())))
                    .collect();
                (*s, takes)
            })
            .collect();
        let wet = (1..=WET_TAKES)
            .map(|n| assets.load(format!("sounds/steps/wet-{n}.ogg")))
            .collect();
        for (i, handle) in beds.iter().enumerate() {
            commands.spawn((
                AudioPlayer(handle.clone()),
                PlaybackSettings {
                    mode: PlaybackMode::Loop,
                    volume: Volume::Linear(0.0),
                    ..default()
                },
                BedSound(i),
            ));
        }
        commands.insert_resource(AmbienceSounds { steps, wet });
    }

    /// The map's answers, bundled: the sampler is otherwise over Bevy's
    /// parameter limit.
    #[derive(bevy::ecs::system::SystemParam)]
    pub struct CityData<'w> {
        places: Res<'w, WorldPlaces>,
        roads: Res<'w, DriveNetwork>,
        grass: Res<'w, GrassAreas>,
        terrain: Res<'w, TerrainField>,
        walls: Res<'w, BuildingColliders>,
        interiors: Res<'w, InteriorFloors>,
    }

    /// Work out what is around the avatar and what it stands on.
    pub fn sample_surroundings(
        time: Res<Time>,
        avatar: Res<AvatarState>,
        dev: Res<DevPanelState>,
        city: CityData,
        mut underfoot: ResMut<Underfoot>,
        mut since: Local<f32>,
    ) {
        *since += time.delta_secs();
        if *since < SAMPLE_EVERY {
            return;
        }
        *since = 0.0;

        let p = Vec2::new(avatar.position.x, avatar.position.z);
        let rain = dev.precipitation.clamp(0.0, 1.0);
        let indoors = city.interiors.building_at(p).is_some();
        let on_grass = |q: Vec2| {
            city.grass.grass_at(q)
                || matches!(
                    city.places.paving_at(q),
                    Some((_, Some(PavingMaterial::Grass | PavingMaterial::Planting)))
                        | Some((PavingKind::Verge, _))
                )
        };

        let surface = step_surface(city.places.paving_at(p), on_grass(p), indoors);

        let traffic = city
            .roads
            .drivable_near(p, ROAD_REACH)
            .map(|(speed, d)| road_loudness(speed, d))
            .fold(0.0f32, f32::max);
        let shops = city
            .places
            .within(p, SHOP_REACH)
            .filter(|(i, _)| {
                matches!(
                    i.kind,
                    InteractKind::Place {
                        category: PoiCategory::Shop | PoiCategory::Food | PoiCategory::Drink,
                        ..
                    }
                )
            })
            .count() as f32
            / SHOPS_FOR_A_STREET;

        // Eight probes on a ring, and the same eight directions as rays.
        let dirs: Vec<Vec2> = (0..8)
            .map(|i| Vec2::from_angle(i as f32 * std::f32::consts::FRAC_PI_4))
            .collect();
        let probes = || dirs.iter().map(|d| p + *d * PROBE_RADIUS);
        let share = |n: usize| n as f32 / dirs.len() as f32;
        let green = share(probes().filter(|q| on_grass(*q)).count());
        let water = share(
            probes()
                .filter(|q| city.terrain.water_surface_at(q.x, q.y).is_some())
                .count(),
        );
        let y = avatar.display_position.y + EAR_HEIGHT;
        let wall = |d: Vec2| {
            city.walls
                .segment_fraction(p, y, p + d * RAY_LENGTH, y)
                .map(|f| f * RAY_LENGTH)
        };
        let enclosure = share(dirs.iter().filter(|d| wall(**d).is_some()).count());
        // The echo is off the walls either side of the way the avatar faces.
        let side3 = Quat::from_rotation_y(avatar.facing_yaw) * Vec3::X;
        let side = Vec2::new(side3.x, side3.z);
        let echo = if indoors {
            None
        } else {
            step_echo(wall(side), wall(-side))
        };

        let s = Surroundings {
            traffic,
            shops: shops.min(1.0),
            green,
            water,
            enclosure,
            indoors,
            daylight: crate::utils::sun::daylight(dev.time_of_day, dev.sun_declination_rad),
            rain,
        };
        underfoot.surface = Some(surface);
        underfoot.wet = rain > WET_ABOVE && !indoors;
        underfoot.echo = echo;
        underfoot.target = bed_mix(&s);
        // At debug, one line a sample: what a recorded walk's soundtrack is
        // rebuilt from (`RUST_LOG=cartopolis::ambience=debug`).
        tracing::debug!(
            target: "cartopolis::ambience",
            x = p.x,
            z = p.y,
            surface = surface.stem(),
            wet = underfoot.wet,
            echo = ?echo,
            beds = ?underfoot.target,
            surroundings = ?s,
            "ambience sample"
        );
    }

    /// Ease each loop toward its level. Only while following the avatar: the
    /// other camera modes look at the map from above, and a street's murmur
    /// over a view of the province is a poltergeist.
    pub fn update_bed_volumes(
        time: Res<Time>,
        camera: Res<CameraState>,
        prefs: Res<AppPrefs>,
        mut underfoot: ResMut<Underfoot>,
        mut sinks: Query<(&BedSound, &mut AudioSink)>,
    ) {
        let follow = camera.mode == CameraMode::Avatar;
        let k = 1.0 - (-BED_RATE * time.delta_secs()).exp();
        let Underfoot {
            target, current, ..
        } = &mut *underfoot;
        for (c, t) in current.iter_mut().zip(target.iter()) {
            let t = if follow { *t } else { 0.0 };
            *c += (t - *c) * k;
        }
        let gain = BED_GAIN * prefs.effects_gain();
        for (bed, mut sink) in &mut sinks {
            let v = current.get(bed.0).copied().unwrap_or(0.0) * gain;
            sink.set_volume(Volume::Linear(v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> Surroundings {
        Surroundings {
            daylight: 1.0,
            enclosure: 0.8,
            ..Default::default()
        }
    }

    fn level(s: &Surroundings, bed: Bed) -> f32 {
        let i = Bed::ALL.iter().position(|b| *b == bed).unwrap();
        bed_mix(s)[i]
    }

    #[test]
    fn nowhere_outdoors_is_silent() {
        for daylight in [0.0, 0.5, 1.0] {
            let mix = bed_mix(&Surroundings {
                daylight,
                ..Default::default()
            });
            assert!(mix.iter().sum::<f32>() > 0.2, "silent at daylight {daylight}");
        }
    }

    #[test]
    fn a_quiet_street_is_residential_by_day_and_night_after_dark() {
        let s = day();
        assert!(level(&s, Bed::Residential) > 0.5);
        assert_eq!(level(&s, Bed::Night), 0.0);
        let dark = Surroundings {
            daylight: 0.0,
            ..s
        };
        assert_eq!(level(&dark, Bed::Residential), 0.0);
        assert!(level(&dark, Bed::Night) >= 0.5);
    }

    #[test]
    fn the_outdoor_beds_never_stack_past_the_ceiling() {
        let busy = Surroundings {
            traffic: 1.0,
            shops: 1.0,
            green: 1.0,
            water: 1.0,
            enclosure: 0.0,
            daylight: 1.0,
            ..Default::default()
        };
        let mix = bed_mix(&busy);
        let outdoor: f32 = mix[..8].iter().sum();
        assert!(outdoor <= OUTDOOR_CEILING + 1e-4, "{outdoor}");
    }

    #[test]
    fn shops_and_birds_fall_quiet_at_night_and_in_rain() {
        // One place at a time: with two, the ceiling rescales them both, and
        // the one that fell would take back what the other gave up.
        for (bed, place) in [
            (Bed::Park, Surroundings { green: 1.0, ..day() }),
            (Bed::Shopping, Surroundings { shops: 1.0, ..day() }),
        ] {
            let wet = Surroundings { rain: 1.0, ..place };
            let dark = Surroundings {
                daylight: 0.0,
                ..place
            };
            assert!(level(&wet, bed) < level(&place, bed) * 0.5, "{bed:?}");
            assert_eq!(level(&dark, bed), 0.0, "{bed:?}");
            assert_eq!(level(&wet, Bed::Rain), 1.0);
        }
    }

    #[test]
    fn indoors_the_street_leaks_in_under_the_room() {
        let out = Surroundings {
            traffic: 1.0,
            rain: 1.0,
            ..day()
        };
        let inside = Surroundings {
            indoors: true,
            ..out
        };
        assert_eq!(level(&inside, Bed::Indoor), 1.0);
        assert_eq!(level(&out, Bed::Indoor), 0.0);
        assert!(level(&inside, Bed::Traffic) < level(&out, Bed::Traffic) * 0.25);
        assert!(level(&inside, Bed::Rain) < 0.25);
    }

    #[test]
    fn a_main_road_is_louder_than_a_back_street_and_fades_with_distance() {
        assert!(road_loudness(15.0, 8.0) > 0.9);
        assert!(road_loudness(4.5, 8.0) < road_loudness(15.0, 8.0) * 0.5);
        assert!(road_loudness(15.0, 60.0) < road_loudness(15.0, 15.0));
        assert_eq!(road_loudness(15.0, ROAD_REACH), 0.0);
        assert_eq!(road_loudness(0.0, 8.0), 0.0);
    }

    #[test]
    fn the_survey_decides_the_footstep() {
        use PavingKind as K;
        use PavingMaterial as M;
        let s = |p, g, i| step_surface(p, g, i);
        assert_eq!(s(Some((K::Footway, Some(M::Brick))), false, false), StepSurface::Brick);
        assert_eq!(s(Some((K::Footway, Some(M::Tiles))), false, false), StepSurface::Stone);
        assert_eq!(s(Some((K::Yard, Some(M::Gravel))), false, false), StepSurface::Gravel);
        assert_eq!(s(Some((K::Verge, None)), false, false), StepSurface::Grass);
        assert_eq!(s(Some((K::Street, None)), false, false), StepSurface::Asphalt);
        assert_eq!(s(Some((K::Footway, None)), false, false), StepSurface::Brick);
        // Off the survey: the lawns, else the OSM carriageway.
        assert_eq!(s(None, true, false), StepSurface::Grass);
        assert_eq!(s(None, false, false), StepSurface::Asphalt);
        // Indoors wins over whatever the ground outside was surveyed as.
        assert_eq!(s(Some((K::Footway, Some(M::Brick))), false, true), StepSurface::Indoor);
    }

    #[test]
    fn a_narrow_street_answers_a_step_and_an_open_square_does_not() {
        let (delay, street) = step_echo(Some(4.0), Some(5.0)).expect("walls both sides");
        assert!((delay - 2.0 * 4.5 / 343.0).abs() < 1e-4);
        let (_, one_side) = step_echo(Some(4.5), None).expect("one wall");
        assert!(one_side < street);
        assert!(step_echo(None, None).is_none());
        assert!(step_echo(Some(40.0), Some(35.0)).is_none());
        // Inside a wall's thickness is a doorway, not an echo.
        assert!(step_echo(Some(1.0), None).is_none());
    }
}
