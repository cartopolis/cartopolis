//! Region states: a place on a board coloured by a state with a time — threat
//! now, soon, later, passed; a weather code; a flood warning; a band of a
//! statistic (v1-engine).
//!
//! A layer of region states either **lists** them (a published alert, a
//! study's table, a CBS figure per municipality: place, time, state) or
//! **derives** them from the scenario's tracks: each area takes the state of
//! the soonest track to enter it. That second kind is what an air-alert map is,
//! and it is a model, so a Live scenario may not have one (v1-drones: "it must
//! never colour a region in Live"). Or it **reads a figure** the areas carry
//! (CBS's share of people aged 65 and over, per buurt) and gives each area the
//! state of the band its value falls in.
//!
//! A layer can stand on several boards, one per rung of the ladder (buurt,
//! wijk, gemeente, provincie, country): the one read follows the camera.
//!
//! A figure can be a **series**, one value per year (`AreaSet::field_at`): on
//! a clock of years the year the clock stands in picks the value, so a
//! forecast plays to 2050 and a measured series back to 2014. Such a layer can
//! read its figure as the change since a year (`change_since`), and name the
//! two fields that bound it (`range`: a forecast's interval, a score's 95 %).

use crate::board::{AreaSet, Board};
use crate::time::{Span, Stamp, Time};
use crate::track::Track;
use serde::Deserialize;

/// Tracks are sampled this often along their path to find where they enter and
/// leave an area, km: ~40 s at a drone's 170 km/h, and under the width of the
/// smallest area a continental set holds.
const SAMPLE_KM: f64 = 2.0;

/// When a derived state applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum When {
    /// A track is in the area now.
    Inside,
    /// A track has been and left.
    Passed,
}

/// One state a layer can give a place.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    pub id: String,
    /// What it means, in words a person reads ("Expected within an hour").
    pub label: String,
    /// `#rrggbb`.
    pub colour: String,
    /// For a derived layer: `inside`, `passed`, or …
    #[serde(default)]
    pub when: Option<When>,
    /// … a track enters within this long (`"1:00"`).
    #[serde(default)]
    pub within: Option<Span>,
    /// For a layer reading a figure: the value is under this. The last state
    /// has none and takes the rest.
    #[serde(default)]
    pub below: Option<f64>,
}

/// One listed state: from `t` on, `place` is in `state` (empty clears it).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetSpec {
    pub place: String,
    pub t: Stamp,
    pub state: String,
}

/// What a layer's states come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Derive {
    /// The soonest track to enter an area.
    Tracks,
    /// The band a figure of the area falls in.
    Value,
}

/// A layer of region states as the file writes it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionStateSpec {
    pub id: String,
    /// What the layer shows, in words a person reads ("Aged 65 and over").
    #[serde(default)]
    pub label: Option<String>,
    /// Which heading the lens lists it under ("People", "Homes"); the
    /// figures without one come first, under none.
    #[serde(default)]
    pub group: Option<String>,
    /// One board, or …
    #[serde(default)]
    pub board: Option<Board>,
    /// … the same layer on several rungs, any order.
    #[serde(default)]
    pub boards: Vec<Board>,
    /// For `derive = "value"`: the area field read.
    #[serde(default)]
    pub field: Option<String>,
    /// Read the figure as its change, in %, since this year's value of it
    /// (`"2023"`): growth and decline rather than size.
    #[serde(default)]
    pub change_since: Option<Stamp>,
    /// The fields that bound the figure, low then high: how sure it is. Shown
    /// beside the value wherever a set carries them; never coloured.
    #[serde(default)]
    pub range: Option<[String; 2]>,
    /// Only areas in these countries (ISO alpha-3) take part; all when empty.
    #[serde(default)]
    pub within: Vec<String>,
    #[serde(default)]
    pub derive: Option<Derive>,
    #[serde(rename = "state")]
    pub states: Vec<StateSpec>,
    #[serde(default)]
    pub set: Vec<SetSpec>,
}

/// A state's colour, parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub [u8; 3]);

impl Rgb {
    pub fn parse(s: &str) -> Result<Rgb, String> {
        let h = s
            .strip_prefix('#')
            .filter(|h| h.len() == 6)
            .ok_or_else(|| format!("colour {s:?}: expected #rrggbb"))?;
        let c =
            |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| format!("colour {s:?}"));
        Ok(Rgb([c(0)?, c(2)?, c(4)?]))
    }
}

/// One state of a layer, ready to draw.
#[derive(Clone, Debug)]
pub struct State {
    pub id: String,
    pub label: String,
    pub colour: Rgb,
    pub when: Option<When>,
    pub within: Option<Time>,
    pub below: Option<f64>,
}

/// A layer of region states on its scenario's clock.
#[derive(Clone, Debug)]
pub struct RegionStates {
    pub id: String,
    pub label: String,
    pub group: Option<String>,
    /// The boards it stands on, as written.
    pub boards: Vec<Board>,
    pub field: Option<String>,
    /// The year a figure is read as a change since.
    pub change_since: Option<i32>,
    /// The fields bounding the figure, low and high.
    pub range: Option<[String; 2]>,
    pub within: Vec<String>,
    pub derive: Option<Derive>,
    pub states: Vec<State>,
    /// Listed states per place, each sorted by time: `(t, state)`, `None`
    /// clearing it.
    pub listed: Vec<(String, Vec<(Time, Option<usize>)>)>,
}

impl RegionStates {
    pub(crate) fn new(
        spec: RegionStateSpec,
        resolve: impl Fn(Stamp) -> Time,
    ) -> Result<Self, String> {
        let id = spec.id;
        if spec.states.is_empty() {
            return Err(format!("region_state {id:?}: no states"));
        }
        let boards: Vec<Board> = spec.board.into_iter().chain(spec.boards).collect();
        if boards.is_empty() {
            return Err(format!("region_state {id:?}: no board"));
        }
        if spec.derive == Some(Derive::Value) && spec.field.is_none() {
            return Err(format!(
                "region_state {id:?}: a figure's layer names its `field`"
            ));
        }
        let change_since = match spec.change_since {
            None => None,
            Some(Stamp::Year(y)) => Some(y),
            Some(_) => {
                return Err(format!(
                    "region_state {id:?}: `change_since` is a year (\"2023\")"
                ))
            }
        };
        if (change_since.is_some() || spec.range.is_some()) && spec.derive != Some(Derive::Value) {
            return Err(format!(
                "region_state {id:?}: `change_since` and `range` belong to a figure's layer"
            ));
        }
        let n_states = spec.states.len();
        let mut last_below = f64::MIN;
        let mut states = Vec::new();
        for (i, s) in spec.states.into_iter().enumerate() {
            if spec.derive == Some(Derive::Value) {
                match s.below {
                    Some(b) if b > last_below => last_below = b,
                    None if i + 1 == n_states => {}
                    _ => {
                        return Err(format!(
                            "region_state {id:?}: state {:?} needs a `below` above the last; only the final state has none",
                            s.id
                        ))
                    }
                }
            }
            if spec.derive == Some(Derive::Tracks) && s.when.is_none() && s.within.is_none() {
                return Err(format!(
                    "region_state {id:?}: state {:?} needs `when` or `within` to be derived",
                    s.id
                ));
            }
            states.push(State {
                colour: Rgb::parse(&s.colour)?,
                id: s.id,
                label: s.label,
                when: s.when,
                within: s.within.map(|w| w.0),
                below: s.below,
            });
        }
        let mut listed: Vec<(String, Vec<(Time, Option<usize>)>)> = Vec::new();
        for s in spec.set {
            let state = if s.state.is_empty() {
                None
            } else {
                Some(
                    states
                        .iter()
                        .position(|x| x.id == s.state)
                        .ok_or_else(|| format!("region_state {id:?}: no state {:?}", s.state))?,
                )
            };
            let t = resolve(s.t);
            match listed.iter_mut().find(|(p, _)| *p == s.place) {
                Some((_, v)) => v.push((t, state)),
                None => listed.push((s.place, vec![(t, state)])),
            }
        }
        for (_, v) in &mut listed {
            v.sort_by_key(|e| e.0);
        }
        Ok(RegionStates {
            label: spec.label.unwrap_or_else(|| id.clone()),
            group: spec.group,
            id,
            boards,
            field: spec.field,
            change_since,
            range: spec.range,
            within: spec.within,
            derive: spec.derive,
            states,
            listed,
        })
    }

    /// The state of the band a value falls in.
    #[must_use]
    pub fn state_for_value(&self, v: f32) -> Option<usize> {
        let v = f64::from(v);
        self.states
            .iter()
            .position(|s| s.below.map_or(true, |b| v < b))
    }

    /// The figure this layer shows for an area of `set` in `year`: the
    /// field's value that year, or its change in % since `change_since`.
    /// `None` where either is not published.
    #[must_use]
    pub fn figure(&self, set: &AreaSet, area: usize, year: Option<i32>) -> Option<f32> {
        self.read(set, area, self.field.as_deref()?, year)
    }

    /// The bounds of [`RegionStates::figure`], low and high, read the same
    /// way, where the set carries them.
    #[must_use]
    pub fn figure_range(
        &self,
        set: &AreaSet,
        area: usize,
        year: Option<i32>,
    ) -> Option<(f32, f32)> {
        let [lo, hi] = self.range.as_ref()?;
        Some((
            self.read(set, area, lo, year)?,
            self.read(set, area, hi, year)?,
        ))
    }

    fn read(&self, set: &AreaSet, area: usize, key: &str, year: Option<i32>) -> Option<f32> {
        let v = set.value(area, set.field_at(key, year)?)?;
        let Some(since) = self.change_since else {
            return Some(v);
        };
        // The base is the figure itself that year, also for a bound.
        let base_key = self.field.as_deref()?;
        let base = set.value(area, set.field_at(base_key, Some(since))?)?;
        (base != 0.0).then(|| (v / base - 1.0) * 100.0)
    }

    fn takes_part(&self, country: &str) -> bool {
        self.within.is_empty() || self.within.iter().any(|c| c == country)
    }
}

/// One track's stay in one area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Visit {
    pub track: usize,
    pub t_in: Time,
    pub t_out: Time,
}

/// Every track's stays per area of a derived layer, worked out once so that a
/// state at any time is a lookup.
#[derive(Clone, Debug, Default)]
pub struct Visits {
    /// Per area of the set (indexed as the set): its visits, by entry time.
    pub per_area: Vec<Vec<Visit>>,
}

impl Visits {
    pub(crate) fn new(layer: &RegionStates, areas: &AreaSet, tracks: &[Track]) -> Visits {
        let mut per_area: Vec<Vec<Visit>> = vec![Vec::new(); areas.areas.len()];
        let part: Vec<bool> = areas
            .areas
            .iter()
            .map(|a| layer.takes_part(&a.country))
            .collect();
        for (ti, track) in tracks.iter().enumerate() {
            let n = (track.length_km() / SAMPLE_KM).ceil().max(1.0) as usize;
            let mut open: Option<(usize, Time)> = None;
            let mut last_t = track.t0;
            for i in 0..=n {
                let km = (i as f64 * SAMPLE_KM).min(track.length_km());
                let t = track
                    .t0
                    .plus(Time((km / track.speed_kmh * 3600.0).round() as i64));
                let here = areas.area_at(track.point_at_km(km)).filter(|&a| part[a]);
                if open.map(|o| o.0) != here {
                    if let Some((a, t_in)) = open.take() {
                        per_area[a].push(Visit {
                            track: ti,
                            t_in,
                            t_out: last_t,
                        });
                    }
                    open = here.map(|a| (a, t));
                }
                last_t = t;
            }
            if let Some((a, t_in)) = open {
                per_area[a].push(Visit {
                    track: ti,
                    t_in,
                    t_out: last_t,
                });
            }
        }
        for v in &mut per_area {
            v.sort_by_key(|x| x.t_in);
        }
        Visits { per_area }
    }

    /// An area's derived state at `t`: the state, when the soonest track
    /// enters (the time itself; `t` when one is inside), and which track.
    #[must_use]
    pub fn state_at(
        &self,
        layer: &RegionStates,
        area: usize,
        t: Time,
    ) -> Option<(usize, Time, usize)> {
        let visits = self.per_area.get(area)?;
        let pick = |w: When| layer.states.iter().position(|s| s.when == Some(w));
        if let Some(v) = visits.iter().find(|v| v.t_in <= t && t <= v.t_out) {
            return pick(When::Inside).map(|s| (s, t, v.track));
        }
        if let Some(v) = visits.iter().find(|v| v.t_in > t) {
            let lead = v.t_in.minus(t);
            let soonest = layer
                .states
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.within.filter(|&w| lead <= w).map(|w| (w, i)))
                .min();
            if let Some((_, s)) = soonest {
                return Some((s, v.t_in, v.track));
            }
        }
        visits
            .iter()
            .rev()
            .find(|v| v.t_out < t)
            .and_then(|v| pick(When::Passed).map(|s| (s, v.t_out, v.track)))
    }
}
