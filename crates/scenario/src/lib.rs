//! **The scenario engine** (`docs/mockups/scenarios/`, `docs/notes/scenarios.md`).
//!
//! A scenario is a lens with a clock. It plays a timeline of **primitives** —
//! a point, rings, a plume, a [`track`], a [`region`] state, an arc, a field, a
//! counter — placed on a [`board`] (official areas, H3 cells, a network), each
//! with a source and how sure it is. A nuclear exchange, a night of drones, a
//! municipality's figures and a solar storm are scenario files; nothing here
//! knows about any of them.
//!
//! So far: tracks, and region states on official areas — listed, derived from
//! tracks, or read off a figure the areas carry — on the [`ladder`] of rungs
//! from the stars to the street, and [`Play::at`]: the state of every
//! primitive at a time. No `bevy`: atlas, the simulator and the client share it.

pub mod board;
pub mod ladder;
pub mod region;
pub mod time;
pub mod track;

use board::{AreaSet, Board, LonLat};
use ladder::Rung;
use region::{RegionStateSpec, RegionStates, Visits};
use serde::Deserialize;
use time::{Clock, Stamp, Time};
use track::{Track, TrackSpec};

/// How sure a primitive is; drawn, never hidden (v1-engine: "uncertainty is
/// drawn").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// From a record of what happened.
    Documented,
    /// A model's output.
    #[default]
    Modelled,
    /// Made up for an exercise; marked so on every screen.
    Invented,
}

/// How real what a scenario shows is. Each wears its own frame on every screen
/// and every rung (v4-whole).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Official feeds only; the only mode that may look like a warning.
    Live,
    /// A published study or a past event, as written.
    Replay,
    /// What if, here, today? Striped, never sent to anyone as an alert.
    Exercise,
    /// Fiction, kept off Earth.
    Sandbox,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClockSpec {
    start: Stamp,
    end: Stamp,
    /// Where the clock stands when the scenario opens; its start when absent.
    #[serde(default)]
    opens: Option<Stamp>,
    /// From here on the figures are a model's, not a measurement's: a
    /// forecast's first year. Drawn hatched on the clock.
    #[serde(default)]
    forecast: Option<Stamp>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetaSpec {
    name: String,
    mode: Mode,
    #[serde(default)]
    summary: String,
    /// The question a person brings to it ("Who lives here?"), which is what
    /// the shelf of lenses lists it under.
    #[serde(default)]
    question: Option<String>,
    sources: Vec<String>,
    #[serde(default)]
    confidence: Confidence,
    clock: ClockSpec,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSpec {
    scenario: MetaSpec,
    #[serde(default)]
    track: Vec<TrackSpec>,
    #[serde(default)]
    region_state: Vec<RegionStateSpec>,
}

/// What a scenario says about itself.
#[derive(Clone, Debug)]
pub struct Meta {
    pub name: String,
    pub mode: Mode,
    pub summary: String,
    pub question: Option<String>,
    pub sources: Vec<String>,
    pub confidence: Confidence,
}

/// Why a scenario could not be read or played.
#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("scenario file: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("scenario: {0}")]
    Invalid(String),
    #[error("scenario needs board {0}, which is not loaded")]
    MissingBoard(Board),
}

/// A scenario, read and on its clock.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub meta: Meta,
    pub clock: Clock,
    /// Where the clock stands when it opens.
    pub opens: Time,
    /// Where a model takes over from measurement, when it does.
    pub forecast: Option<Time>,
    pub tracks: Vec<Track>,
    pub region_states: Vec<RegionStates>,
}

impl Scenario {
    /// Read a scenario file.
    pub fn parse(text: &str) -> Result<Scenario, ScenarioError> {
        let spec: FileSpec = toml::from_str(text)?;
        let bad = ScenarioError::Invalid;
        let meta = spec.scenario;
        if meta.sources.is_empty() {
            return Err(bad(format!("{:?} names no sources", meta.name)));
        }
        let (start_wall, start_year) = match meta.clock.start {
            Stamp::Wall(m) => (Some(m), None),
            Stamp::After(0) => (None, None),
            Stamp::Year(y) => (None, Some(y)),
            Stamp::After(_) => {
                return Err(bad(
                    "the clock starts at a time of day, a year or T+0:00".into()
                ))
            }
        };
        let mut clock = Clock {
            start_wall,
            start_year,
            end: Time::ZERO,
        };
        clock.end = clock.resolve(meta.clock.end);
        // A clock of years may be a single year: figures that describe one.
        if clock.end < Time::ZERO || (clock.end == Time::ZERO && start_year.is_none()) {
            return Err(bad("the clock ends where it starts".into()));
        }
        let resolve = |s: Stamp| clock.resolve(s);
        let tracks = spec
            .track
            .into_iter()
            .map(|t| Track::new(t, resolve))
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        let region_states = spec
            .region_state
            .into_iter()
            .map(|r| RegionStates::new(r, resolve))
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        for r in &region_states {
            if r.derive.is_some() && meta.mode == Mode::Live {
                return Err(bad(format!(
                    "region_state {:?}: a Live scenario shows what a feed says, never a model's derivation",
                    r.id
                )));
            }
            if r.derive.is_some() && !r.boards.iter().all(|b| matches!(b, Board::Areas(_))) {
                return Err(bad(format!(
                    "region_state {:?}: only official areas can be derived so far",
                    r.id
                )));
            }
        }
        let opens = meta.clock.opens.map_or(Time::ZERO, resolve);
        let forecast = meta.clock.forecast.map(resolve);
        Ok(Scenario {
            meta: Meta {
                name: meta.name,
                mode: meta.mode,
                summary: meta.summary,
                question: meta.question,
                sources: meta.sources,
                confidence: meta.confidence,
            },
            clock,
            opens,
            forecast,
            tracks,
            region_states,
        })
    }

    /// The boards it places primitives on, each once.
    #[must_use]
    pub fn boards(&self) -> Vec<Board> {
        let mut out: Vec<Board> = Vec::new();
        for b in self.region_states.iter().flat_map(|r| &r.boards) {
            if !out.contains(b) {
                out.push(b.clone());
            }
        }
        out
    }
}

/// One region-state layer joined to its area sets.
struct Join {
    /// Indices into [`Play::areas`], one per official-area board.
    sets: Vec<usize>,
    /// Which of `sets` is read at the camera's rung; `None` when none is near.
    active: Option<usize>,
    /// The tracks' stays per area of `sets[0]`, when derived from tracks.
    visits: Option<Visits>,
}

/// A scenario with its boards, ready to answer what is true at any time.
pub struct Play {
    pub scenario: Scenario,
    pub areas: Vec<AreaSet>,
    layers: Vec<Join>,
}

/// One track at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackNow {
    pub track: usize,
    /// Where it is, while it is on its path.
    pub head: Option<LonLat>,
    /// Compass bearing it travels on, degrees.
    pub heading: f64,
    /// How many are still moving; `None` before it sets off or after its end.
    pub moving: Option<u32>,
    pub downed: u32,
}

/// One place's state at a time.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceNow {
    /// The place's name on its board (an area's code, a cell, a node).
    pub place: String,
    /// The area, when the board is official areas and the place is in the set.
    pub area: Option<usize>,
    /// Index into the layer's states.
    pub state: usize,
    /// For a derived state: when the soonest track enters (or left, for a
    /// passed one).
    pub eta: Option<Time>,
    /// For a derived state: which track.
    pub by: Option<usize>,
    /// For a state read off a figure: the value.
    pub value: Option<f32>,
}

/// Every primitive at one time.
#[derive(Clone, Debug)]
pub struct Frame {
    pub t: Time,
    pub tracks: Vec<TrackNow>,
    /// Per region-state layer, the places that have a state.
    pub regions: Vec<Vec<PlaceNow>>,
}

/// A point ahead on a track: when, where, how wide the cone is there, and the
/// heading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ahead {
    pub t: Time,
    pub at: LonLat,
    pub cone_km: f64,
    pub heading: f64,
}

impl Play {
    /// Join a scenario to the boards it needs. Every official-area board must be
    /// among `areas`; cells and networks carry no geometry yet.
    pub fn new(scenario: Scenario, areas: Vec<AreaSet>) -> Result<Play, ScenarioError> {
        let mut layers = Vec::new();
        for r in &scenario.region_states {
            let mut sets = Vec::new();
            for b in &r.boards {
                if let Board::Areas(name) = b {
                    sets.push(
                        areas
                            .iter()
                            .position(|a| &a.name == name)
                            .ok_or_else(|| ScenarioError::MissingBoard(b.clone()))?,
                    );
                }
            }
            if r.derive == Some(region::Derive::Value) {
                let key = r.field.as_deref().unwrap_or_default();
                if let Some(&s) = sets.iter().find(|&&s| areas[s].field(key).is_none()) {
                    return Err(ScenarioError::Invalid(format!(
                        "region_state {:?}: area set {:?} has no field {key:?}",
                        r.id, areas[s].name
                    )));
                }
            }
            let visits = match (r.derive, sets.first()) {
                (Some(region::Derive::Tracks), Some(&s)) => {
                    Some(Visits::new(r, &areas[s], &scenario.tracks))
                }
                _ => None,
            };
            layers.push(Join {
                active: (!sets.is_empty()).then_some(0),
                sets,
                visits,
            });
        }
        Ok(Play {
            scenario,
            areas,
            layers,
        })
    }

    /// Read each layer on its board nearest `rung` (`ladder::Rung::nearest_of`);
    /// true when any layer moved to another board.
    pub fn set_rung(&mut self, rung: Rung) -> bool {
        let mut moved = false;
        for j in &mut self.layers {
            let rungs: Vec<Rung> = j.sets.iter().map(|&s| self.areas[s].rung).collect();
            let active = rung.nearest_of(&rungs);
            moved |= active != j.active;
            j.active = active;
        }
        moved
    }

    /// The area set a region-state layer is read on now.
    #[must_use]
    pub fn layer_areas(&self, layer: usize) -> Option<&AreaSet> {
        let j = self.layers.get(layer)?;
        j.active.map(|a| &self.areas[j.sets[a]])
    }

    /// The state of everything at `t`.
    #[must_use]
    pub fn at(&self, t: Time) -> Frame {
        let tracks = self
            .scenario
            .tracks
            .iter()
            .enumerate()
            .map(|(i, tr)| {
                let km = tr.km_at(t);
                TrackNow {
                    track: i,
                    head: km.map(|k| tr.point_at_km(k)),
                    heading: km.map_or(0.0, |k| tr.heading_at_km(k)),
                    moving: tr.moving(t),
                    downed: tr.downed(t),
                }
            })
            .collect();
        let regions = self
            .scenario
            .region_states
            .iter()
            .zip(&self.layers)
            .map(|(layer, j)| {
                let set = j.active.map(|a| &self.areas[j.sets[a]]);
                let mut out = Vec::new();
                // Track stays were worked out on the layer's first board.
                if let (Some(visits), Some(set), Some(0)) = (&j.visits, set, j.active) {
                    for (a, area) in set.areas.iter().enumerate() {
                        if let Some((state, eta, by)) = visits.state_at(layer, a, t) {
                            out.push(PlaceNow {
                                place: area.id.clone(),
                                area: Some(a),
                                state,
                                eta: Some(eta),
                                by: Some(by),
                                value: None,
                            });
                        }
                    }
                }
                if let (Some(region::Derive::Value), Some(set)) = (layer.derive, set) {
                    let year = self.scenario.clock.year_at(t);
                    for (a, area) in set.areas.iter().enumerate() {
                        let Some(v) = layer.figure(set, a, year) else {
                            continue;
                        };
                        if let Some(state) = layer.state_for_value(v) {
                            out.push(PlaceNow {
                                place: area.id.clone(),
                                area: Some(a),
                                state,
                                eta: None,
                                by: None,
                                value: Some(v),
                            });
                        }
                    }
                }
                for (place, steps) in &layer.listed {
                    let Some(&(_, Some(state))) = steps.iter().rev().find(|s| s.0 <= t) else {
                        continue;
                    };
                    let area = set.and_then(|s| s.index_of(place));
                    // A listed state overrides a derived one for the same place.
                    out.retain(|p| &p.place != place);
                    out.push(PlaceNow {
                        place: place.clone(),
                        area,
                        state,
                        eta: None,
                        by: None,
                        value: None,
                    });
                }
                out
            })
            .collect();
        Frame { t, tracks, regions }
    }

    /// A track's path behind it at `t`, from where it set off to its head.
    #[must_use]
    pub fn behind(&self, track: usize, t: Time) -> Vec<LonLat> {
        let tr = &self.scenario.tracks[track];
        let Some(now_km) = tr.km_at(t) else {
            return Vec::new();
        };
        let mut out = vec![tr.path[0]];
        for (p, &d) in tr.path.iter().zip(tr.along()) {
            if d > 0.0 && d < now_km {
                out.push(*p);
            }
        }
        out.push(tr.point_at_km(now_km));
        out
    }

    /// A track's path ahead of it at `t`, every `step`, to `horizon` or its end.
    #[must_use]
    pub fn ahead(&self, track: usize, t: Time, horizon: Time, step: Time) -> Vec<Ahead> {
        let tr = &self.scenario.tracks[track];
        let mut out = Vec::new();
        if tr.km_at(t).is_none() || step.0 <= 0 {
            return out;
        }
        let mut s = t;
        while s <= t.plus(horizon) {
            let Some(km) = tr.km_at(s) else { break };
            out.push(Ahead {
                t: s,
                at: tr.point_at_km(km),
                cone_km: tr.cone_km(s.minus(t)),
                heading: tr.heading_at_km(km),
            });
            s = s.plus(step);
        }
        out
    }

    /// The areas of a layer a track's cone reaches ahead of `t`, and when it
    /// first does, soonest first; areas it is in now are left out.
    #[must_use]
    pub fn cone_reaches(
        &self,
        track: usize,
        layer: usize,
        t: Time,
        horizon: Time,
    ) -> Vec<(usize, Time)> {
        let Some(set) = self.layer_areas(layer) else {
            return Vec::new();
        };
        let lr = &self.scenario.region_states[layer];
        let mut out: Vec<(usize, Time)> = Vec::new();
        let here = self
            .scenario
            .tracks
            .get(track)
            .and_then(|tr| tr.position(t))
            .and_then(|p| set.area_at(p));
        for a in self.ahead(track, t, horizon, Time(120)) {
            for side in [0.0, -90.0, 90.0] {
                let km = if side == 0.0 { 0.0 } else { a.cone_km };
                let p = track::offset(a.at, a.heading + side, km);
                let Some(area) = set.area_at(p) else { continue };
                let area_ok = lr.within.is_empty() || lr.within.contains(&set.areas[area].country);
                if area_ok && Some(area) != here && !out.iter().any(|o| o.0 == area) {
                    out.push((area, a.t));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
