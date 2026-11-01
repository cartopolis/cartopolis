use super::*;
use board::Area;

/// A row of square "provinces" a degree wide along the equator, in two
/// countries, and one off to the north in a third.
fn strip() -> AreaSet {
    let sq = |id: &str, country: &str, w: f64, s: f64| {
        Area::new(
            id.into(),
            id.into(),
            country.into(),
            vec![vec![vec![
                [w, s],
                [w + 1.0, s],
                [w + 1.0, s + 1.0],
                [w, s + 1.0],
            ]]],
        )
    };
    let mut areas: Vec<Area> = (0..6)
        .map(|i| {
            sq(
                &format!("P{i}"),
                if i < 3 { "AAA" } else { "BBB" },
                f64::from(i),
                -0.5,
            )
        })
        .collect();
    areas.push(sq("N", "CCC", 2.0, 3.0));
    AreaSet::new("strip".into(), "test".into(), areas)
}

const WAVE: &str = r##"
[scenario]
name = "A test wave"
mode = "exercise"
sources = ["made up"]
clock = { start = "22:00", end = "06:00", opens = "23:00" }

[[track]]
id = "G"
count = 10
speed_kmh = 111.195
t0 = "22:00"
path = [[0.5, 0.0], [5.5, 0.0]]
down = [{ t = "23:00", n = 4 }]

[[region_state]]
id = "threat"
board = "areas:strip"
derive = "tracks"
within = ["AAA", "BBB"]
state = [
  { id = "now", label = "Over the region", colour = "#d63a2f", when = "inside" },
  { id = "soon", label = "Within an hour", colour = "#ef8a2e", within = "1:00" },
  { id = "later", label = "Within three hours", colour = "#d9c25a", within = "3:00" },
  { id = "passed", label = "Passed", colour = "#5a7f99", when = "passed" },
]
"##;

fn state_of(frame: &Frame, play: &Play, place: &str) -> Option<String> {
    let layer = &play.scenario.region_states[0];
    frame.regions[0]
        .iter()
        .find(|p| p.place == place)
        .map(|p| layer.states[p.state].id.clone())
}

#[test]
fn regions_take_the_state_of_the_soonest_track() {
    let play = Play::new(Scenario::parse(WAVE).unwrap(), vec![strip()]).unwrap();
    assert_eq!(play.scenario.opens, Time(3600));
    // At 111 km/h on the equator the group crosses a degree an hour; it set off
    // in P0's middle at 22:00, so at 23:00 it is in P1's middle.
    let f = play.at(Time(3600));
    assert_eq!(state_of(&f, &play, "P0").as_deref(), Some("passed"));
    assert_eq!(state_of(&f, &play, "P1").as_deref(), Some("now"));
    assert_eq!(state_of(&f, &play, "P2").as_deref(), Some("soon"));
    assert_eq!(state_of(&f, &play, "P3").as_deref(), Some("later"));
    assert_eq!(state_of(&f, &play, "P4").as_deref(), Some("later"));
    // Over three hours ahead.
    assert_eq!(state_of(&f, &play, "P5"), None);
    // Never on its path.
    assert_eq!(state_of(&f, &play, "N"), None);
    let p2 = f.regions[0].iter().find(|p| p.place == "P2").unwrap();
    // Entry times are found on a 2 km sampling, so to within a minute.
    let eta = p2.eta.unwrap().minutes();
    assert!((eta - 90.0).abs() < 1.5, "{eta}");
}

#[test]
fn a_track_at_a_time_is_where_it_is_and_how_many_fly() {
    let play = Play::new(Scenario::parse(WAVE).unwrap(), vec![strip()]).unwrap();
    let f = play.at(Time(3600 + 60));
    let g = &f.tracks[0];
    assert_eq!(g.moving, Some(6));
    assert_eq!(g.downed, 4);
    assert!((g.heading - 90.0).abs() < 1e-6);
    let head = g.head.unwrap();
    assert!((head[0] - 1.5167).abs() < 1e-3, "{head:?}");
    // Before it sets off and after it lands, it is nowhere.
    let after = play.at(Time(6 * 3600));
    assert_eq!(after.tracks[0].head, None);
    assert_eq!(after.tracks[0].moving, None);
}

#[test]
fn the_path_behind_and_ahead_meet_at_the_head() {
    let play = Play::new(Scenario::parse(WAVE).unwrap(), vec![strip()]).unwrap();
    let t = Time(3600);
    let behind = play.behind(0, t);
    let ahead = play.ahead(0, t, Time(2 * 3600), Time(1800));
    assert_eq!(behind.last().copied(), Some(ahead[0].at));
    assert_eq!(ahead.len(), 5);
    assert!(ahead[4].cone_km >= ahead[0].cone_km);
    let reach = play.cone_reaches(0, 0, t, Time(3 * 3600));
    let names: Vec<&str> = reach
        .iter()
        .map(|(a, _)| play.areas[0].areas[*a].id.as_str())
        .collect();
    assert_eq!(names, ["P2", "P3", "P4"]);
}

#[test]
fn a_listed_state_plays_on_any_board() {
    let text = r##"
[scenario]
name = "Warnings"
mode = "replay"
sources = ["a feed"]
clock = { start = "T+0:00", end = "+48:00" }

[[region_state]]
id = "code"
board = "h3:5"
state = [
  { id = "yellow", label = "Code yellow", colour = "#e8d44d" },
  { id = "orange", label = "Code orange", colour = "#ef8a2e" },
]
set = [
  { place = "851f9a3bfffffff", t = "+1:00", state = "yellow" },
  { place = "851f9a3bfffffff", t = "+3:00", state = "orange" },
  { place = "851f9a3bfffffff", t = "+6:00", state = "" },
]
"##;
    let play = Play::new(Scenario::parse(text).unwrap(), Vec::new()).unwrap();
    let at = |h: i64| {
        play.at(Time(h * 3600)).regions[0]
            .first()
            .map(|p| play.scenario.region_states[0].states[p.state].id.clone())
    };
    assert_eq!(at(0), None);
    assert_eq!(at(2).as_deref(), Some("yellow"));
    assert_eq!(at(4).as_deref(), Some("orange"));
    assert_eq!(at(7), None);
    assert_eq!(play.scenario.clock.label(Time(3 * 3600)), "T+3:00");
}

#[test]
fn live_never_derives_and_boards_must_be_loaded() {
    let live = WAVE.replace("mode = \"exercise\"", "mode = \"live\"");
    let err = Scenario::parse(&live).unwrap_err().to_string();
    assert!(err.contains("Live"), "{err}");
    let missing = Play::new(Scenario::parse(WAVE).unwrap(), Vec::new());
    assert!(matches!(missing, Err(ScenarioError::MissingBoard(_))));
    let unsourced = WAVE.replace("sources = [\"made up\"]", "sources = []");
    assert!(Scenario::parse(&unsourced).is_err());
}

#[test]
fn the_shipped_drone_exercise_reads() {
    let s = Scenario::parse(include_str!("../testdata/night-wave.toml")).unwrap();
    assert_eq!(s.meta.mode, Mode::Exercise);
    assert_eq!(s.tracks.len(), 4);
    assert_eq!(s.clock.label(s.opens), "02:10");
    assert_eq!(s.boards(), [Board::Areas("ne-admin1-europe".into())]);
    // As the sheet has them at 02:10: A has 46 of 64 still flying.
    assert_eq!(s.tracks[0].moving(s.opens), Some(46));
}

/// The drone exercise on the shipped provinces, at the time the sheet
/// (v1-drones) draws it.
#[test]
fn the_drone_exercise_at_0210_matches_the_sheet() {
    let areas = AreaSet::decode(include_bytes!("../testdata/ne-admin1-europe.bin")).unwrap();
    let s = Scenario::parse(include_str!("../testdata/night-wave.toml")).unwrap();
    let opens = s.opens;
    let play = Play::new(s, vec![areas]).unwrap();
    let f = play.at(opens);
    let layer = &play.scenario.region_states[0];
    let set = play.layer_areas(0).unwrap();
    let mut counts = vec![0; layer.states.len()];
    for p in &f.regions[0] {
        counts[p.state] += 1;
    }
    let mut next: Vec<(i64, &str)> = f.regions[0]
        .iter()
        .filter(|p| layer.states[p.state].within.is_some())
        .map(|p| {
            (
                p.eta.unwrap().minus(opens).0 / 60,
                set.areas[p.area.unwrap()].name.as_str(),
            )
        })
        .collect();
    next.sort();
    // The sheet names the same three next, 2, 6 and 9 minutes out; it sampled
    // its paths every four minutes, so its entries come up to four late, and
    // its "now" counted a region left in the last eight (5 now, 6 within the
    // hour, 13 within three, 14 passed). Here an entry is found to the 2 km.
    let names: Vec<&str> = next.iter().take(3).map(|n| n.1).collect();
    assert_eq!(
        names,
        [
            "Świętokrzyskie Voivodeship",
            "Lesser Poland Voivodeship",
            "Marijampolė"
        ],
        "{next:?}"
    );
    assert_eq!(counts, [3, 7, 13, 18]);
    // Group A over Masovia, 46 of 64 still flying.
    let a = &f.tracks[0];
    assert_eq!(a.moving, Some(46));
    assert_eq!(
        set.areas[set.area_at(a.head.unwrap()).unwrap()].name,
        "Masovian Voivodeship"
    );
}

const FIGURES: &str = r##"
[scenario]
name = "Older people"
mode = "replay"
confidence = "documented"
sources = ["a statistics office"]
clock = { start = "2024", end = "2024" }

[[region_state]]
id = "older"
label = "Aged 65 and over"
boards = ["areas:wijk", "areas:buurt"]
derive = "value"
field = "old"
state = [
  { id = "low", label = "under 15 %", colour = "#cfe3ea", below = 15.0 },
  { id = "mid", label = "15 to 25 %", colour = "#8fb8c8", below = 25.0 },
  { id = "high", label = "25 % and over", colour = "#3f6f86" },
]
"##;

/// Two buurten in one wijk, the wijk's figure rolled up from them.
fn figure_sets() -> Vec<AreaSet> {
    let field = |key: &str, roll: board::Roll| board::Field {
        key: key.into(),
        label: key.into(),
        unit: String::new(),
        roll,
        year: Some(2024),
    };
    let fields = vec![
        field("people", board::Roll::Sum),
        field(
            "old",
            board::Roll::Mean {
                weight: "people".into(),
            },
        ),
    ];
    let sq = |id: &str, w: f64, size: f64| {
        Area::new(
            id.into(),
            id.into(),
            "NLD".into(),
            vec![vec![vec![
                [w, 0.0],
                [w + size, 0.0],
                [w + size, size],
                [w, size],
            ]]],
        )
    };
    let mut a = sq("BU1", 0.0, 1.0);
    a.values = vec![100.0, 10.0];
    a.parent = Some("WK1".into());
    let mut b = sq("BU2", 1.0, 1.0);
    b.values = vec![100.0, 30.0];
    b.parent = Some("WK1".into());
    let buurt = AreaSet::new("buurt".into(), "t".into(), vec![a, b])
        .with_rung(Rung::Neighbourhood)
        .with_fields(fields);
    let mut wijk = AreaSet::new("wijk".into(), "t".into(), vec![sq("WK1", 0.0, 2.0)])
        .with_rung(Rung::District);
    wijk.roll_up(&buurt);
    vec![wijk, buurt]
}

#[test]
fn a_figure_is_read_on_the_rung_the_camera_is_at() {
    let mut play = Play::new(Scenario::parse(FIGURES).unwrap(), figure_sets()).unwrap();
    assert_eq!(play.scenario.clock.label(play.scenario.opens), "2024");
    let layer = |play: &Play| {
        let f = play.at(Time::ZERO);
        f.regions[0]
            .iter()
            .map(|p| {
                (
                    p.place.clone(),
                    play.scenario.region_states[0].states[p.state].id.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    // From a buurt's height, each buurt in its own band.
    assert!(play.set_rung(Rung::Neighbourhood));
    assert_eq!(
        layer(&play),
        [("BU1".into(), "low".into()), ("BU2".into(), "high".into())]
    );
    // Higher up the wijk, at the weighted mean of its buurten: 20 %.
    assert!(play.set_rung(Rung::Municipality));
    assert_eq!(layer(&play), [("WK1".to_string(), "mid".to_string())]);
    assert_eq!(play.at(Time::ZERO).regions[0][0].value, Some(20.0));
    assert!(!play.set_rung(Rung::Province));
    // Far beyond the ladder's reach the layer says nothing.
    play.set_rung(Rung::SolarSystem);
    assert!(play.layer_areas(0).is_none());
    assert!(play.at(Time::ZERO).regions[0].is_empty());
}

#[test]
fn a_figure_layer_must_name_a_field_its_areas_have() {
    let no_field = FIGURES.replace("field = \"old\"\n", "");
    assert!(Scenario::parse(&no_field).is_err());
    let wrong = FIGURES.replace("field = \"old\"", "field = \"young\"");
    assert!(Play::new(Scenario::parse(&wrong).unwrap(), figure_sets()).is_err());
    let unordered = FIGURES.replace("below = 25.0", "below = 5.0");
    assert!(Scenario::parse(&unordered).is_err());
}

const FORECAST: &str = r##"
[scenario]
name = "A forecast"
mode = "replay"
confidence = "modelled"
sources = ["test"]
clock = { start = "2023", end = "2050", forecast = "2024" }

[[region_state]]
id = "growth"
board = "areas:gemeente"
derive = "value"
field = "people"
change_since = "2023"
range = ["people_low", "people_high"]
state = [
  { id = "shrinks", label = "shrinks", colour = "#b35806", below = 0.0 },
  { id = "grows", label = "grows", colour = "#542788" },
]
"##;

/// A gemeente whose population is a series with an interval: 1,000 in 2023,
/// 900 (800–1,000) in 2030, 1,200 (1,000–1,400) in 2050.
fn forecast_set() -> AreaSet {
    let field = |key: &str, year: i32| board::Field {
        key: key.into(),
        label: key.into(),
        unit: "people".into(),
        roll: board::Roll::Sum,
        year: Some(year),
    };
    let mut fields = Vec::new();
    let mut values = Vec::new();
    for (year, mid, lo, hi) in [
        (2023, 1000.0, 1000.0, 1000.0),
        (2030, 900.0, 800.0, 1000.0),
        (2050, 1200.0, 1000.0, 1400.0),
    ] {
        for (key, v) in [("people", mid), ("people_low", lo), ("people_high", hi)] {
            fields.push(field(key, year));
            values.push(v);
        }
    }
    let mut a = Area::new(
        "GM1".into(),
        "Somewhere".into(),
        "NLD".into(),
        vec![vec![vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]],
    );
    a.values = values;
    AreaSet::new("gemeente".into(), "t".into(), vec![a])
        .with_rung(Rung::Municipality)
        .with_fields(fields)
}

#[test]
fn a_series_plays_on_a_clock_of_years() {
    let mut play = Play::new(Scenario::parse(FORECAST).unwrap(), vec![forecast_set()]).unwrap();
    play.set_rung(Rung::Municipality);
    let clock = play.scenario.clock;
    assert_eq!(play.scenario.forecast, Some(time::YEAR));
    let at = |play: &Play, year: &str| {
        let t = clock.resolve(time::Stamp::parse(year).unwrap());
        let f = play.at(t);
        let p = &f.regions[0][0];
        let layer = &play.scenario.region_states[0];
        let set = play.layer_areas(0).unwrap();
        (
            layer.states[p.state].id.clone(),
            p.value.unwrap().round(),
            layer
                .figure_range(set, 0, clock.year_at(t))
                .map(|(l, h)| (l.round(), h.round())),
        )
    };
    assert_eq!(at(&play, "2023"), ("grows".into(), 0.0, Some((0.0, 0.0))));
    // Between published years the last one before holds: 2035 reads 2030.
    assert_eq!(
        at(&play, "2035"),
        ("shrinks".into(), -10.0, Some((-20.0, 0.0)))
    );
    assert_eq!(at(&play, "2050"), ("grows".into(), 20.0, Some((0.0, 40.0))));
}

#[test]
fn change_since_is_a_year_on_a_figure() {
    let bad = FORECAST.replace("change_since = \"2023\"", "change_since = \"22:00\"");
    assert!(Scenario::parse(&bad).is_err());
    let not_a_figure = FORECAST.replace("derive = \"value\"\n", "");
    assert!(Scenario::parse(&not_a_figure).is_err());
}

#[test]
fn a_figure_is_read_on_the_finest_board_wide_enough_to_see() {
    let mut play = Play::new(Scenario::parse(FIGURES).unwrap(), figure_sets()).unwrap();
    // The wijk is a 2° square at the equator (~222 km), each buurt 1°.
    let name = |play: &Play| play.layer_areas(0).map(|s| s.name.clone());
    // Close in, the buurten are wide: read them.
    play.set_scale(1.0);
    assert_eq!(name(&play).as_deref(), Some("buurt"));
    // Further out a buurt is under 0.8 of 12 points (9), where the board
    // shown is let go; the wijk is still over (19).
    assert!(play.set_scale(12.0));
    assert_eq!(name(&play).as_deref(), Some("wijk"));
    // Back in a little: the wijk holds until a buurt is wide enough itself.
    assert!(!play.set_scale(9.5));
    assert!(play.set_scale(7.0));
    assert_eq!(name(&play).as_deref(), Some("buurt"));
    // From far enough nothing is readable.
    play.set_scale(1_000.0);
    assert!(play.layer_areas(0).is_none());
}
