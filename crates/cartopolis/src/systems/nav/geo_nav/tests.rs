use super::*;
use crate::grid::WorldFrame;
use crate::systems::egui_manager::UnpaintedPasses as _;
use crate::systems::globe::{sub_point, GlobeState};

const ANCHOR: (f64, f64, u32) = (53.2194, 6.5665, 16);

fn suggestion(label: &str) -> GeoSuggestion {
    GeoSuggestion::plain(label, 53.0, 6.0)
}

/// A teleport that had to pick an altitude arrives high enough to see where
/// it landed — that is what [`MIN_TELEPORT_ALTITUDE`] is for.
#[test]
fn a_derived_teleport_altitude_keeps_the_destination_in_view() {
    assert_eq!(
        free_teleport_altitude(None, || 5.0),
        MIN_TELEPORT_ALTITUDE,
        "a jump from near the ground must still show the destination"
    );
    assert_eq!(
        free_teleport_altitude(None, || 4_000.0),
        4_000.0,
        "and an altitude already above the floor is kept, not lowered"
    );
}

/// …but a **stated** altitude is an instruction, not a suggestion. This is
/// the one that regressed: the floor was applied to both, so a bookmark at
/// 30 m, a `?place=…,25m,…` link and `--shot --at lat,lng,35` all silently
/// became 60 m — and the shot harness then reported the 60 as if it had been
/// asked for, which is how it went unnoticed.
#[test]
fn a_stated_teleport_altitude_is_honoured_below_the_view_floor() {
    for stated in [35.0, 30.0, 25.0, MIN_CAMERA_ALTITUDE] {
        assert_eq!(
            free_teleport_altitude(Some(stated), || 1_000.0),
            stated,
            "an explicitly requested {stated} m must not be raised"
        );
    }
}

/// The floor a stated altitude *does* take is the one that keeps the near
/// plane out of the ground — a different limit with a different job.
#[test]
fn a_stated_teleport_altitude_still_clears_the_ground() {
    assert_eq!(
        free_teleport_altitude(Some(0.0), || 1_000.0),
        MIN_CAMERA_ALTITUDE
    );
    assert_eq!(
        free_teleport_altitude(Some(-500.0), || 1_000.0),
        MIN_CAMERA_ALTITUDE
    );
    assert!(
        MIN_CAMERA_ALTITUDE < MIN_TELEPORT_ALTITUDE,
        "the keep-out-of-the-ground floor must stay under the see-it floor, \
             or stating an altitude gains nothing"
    );
}

/// The suggestion rows show a name, a kind and an address context, and all
/// three come out of one flattened `display_name` plus two sibling fields.
#[test]
fn a_nominatim_hit_splits_into_name_kind_and_context() {
    let hit = serde_json::json!({
        "display_name": "Amsterdam, Noord-Holland, Nederland",
        "name": "Amsterdam",
        "addresstype": "city",
        "lat": "52.3730796",
        "lon": "4.8924534",
    });
    let s = GeoSuggestion::from_nominatim(&hit).expect("well-formed hit");
    assert_eq!(s.name, "Amsterdam");
    assert_eq!(s.context, "Noord-Holland, Nederland");
    assert_eq!(s.kind.as_deref(), Some("city"));
    // The full line survives — it is what a picked suggestion leaves behind
    // in the search field.
    assert_eq!(s.label, "Amsterdam, Noord-Holland, Nederland");
}

/// Plenty of hits carry no `name` at all (an unnamed way, a house number).
/// Splitting the display line at its first comma is the same convention.
#[test]
fn a_hit_without_a_name_falls_back_to_the_first_segment() {
    let hit = serde_json::json!({
        "display_name": "12, Oosterstraat, Groningen, Nederland",
        "addresstype": "house_number",
        "lat": "53.2194",
        "lon": "6.5665",
    });
    let s = GeoSuggestion::from_nominatim(&hit).expect("well-formed hit");
    assert_eq!(s.name, "12");
    assert_eq!(s.context, "Oosterstraat, Groningen, Nederland");
    // Underscores are a database detail, not a word.
    assert_eq!(s.kind.as_deref(), Some("house number"));
}

/// The distance is the whole reason the rows got richer: it is what tells the
/// Amsterdam next door from the one in Missouri.
#[test]
fn distance_scales_its_unit_and_names_the_direction() {
    let groningen = (53.2194, 6.5665);
    // Same spot: no useful direction to give.
    assert_eq!(distance_label(groningen, groningen), "here");
    // A few hundred metres north stays in metres.
    let near = (53.2224, 6.5665);
    assert!(
        distance_label(groningen, near).ends_with(" m N"),
        "got {}",
        distance_label(groningen, near)
    );
    // Amsterdam is ~147 km west-south-west; the thousands group with a thin
    // space only once there are four digits, so this one has none.
    let amsterdam = (52.3731, 4.8925);
    assert_eq!(distance_label(groningen, amsterdam), "147 km SW");
    // Amsterdam, Missouri — the hit this whole line exists to disambiguate.
    // North-west, not west: the bearing is the great-circle one, which leaves
    // the Netherlands over the pole side rather than along the parallel.
    let missouri = (39.4028, -93.9666);
    let far = distance_label(groningen, missouri);
    assert!(far.ends_with(" km NW"), "got {far}");
    assert!(far.contains('\u{202F}'), "four digits should group: {far}");
}

/// Haversine, not the flat approximation the driver uses: a quarter of the
/// way round the planet is where the two disagree by thousands of km.
#[test]
fn great_circle_matches_a_known_quarter_meridian() {
    let quarter = great_circle_m((0.0, 0.0), (90.0, 0.0));
    assert!((quarter - 10_007_543.0).abs() < 1_000.0, "got {quarter}");
}

#[test]
fn cache_answers_a_repeated_query_without_a_lookup() {
    let mut nav = GeoNavState::default();
    assert!(nav.cached("Groningen").is_none());

    nav.remember("Groningen".into(), &[suggestion("Groningen, Nederland")]);
    let hit = nav.cached("Groningen").expect("just remembered");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].label, "Groningen, Nederland");

    // Backspacing to a prefix we asked about earlier is a hit too — that is
    // the case the debounce cannot help with.
    nav.remember("Groninge".into(), &[suggestion("Groningen, Nederland")]);
    assert!(nav.cached("Groninge").is_some());
    // An empty answer is still an answer; re-asking would just return [].
    nav.remember("Gron".into(), &[]);
    assert_eq!(nav.cached("Gron").map(|l| l.len()), Some(0));
}

#[test]
fn cache_evicts_oldest_first_and_refreshes_on_reuse() {
    let mut nav = GeoNavState::default();
    for i in 0..CACHE_CAP {
        nav.remember(format!("q{i}"), &[suggestion("x")]);
    }
    assert_eq!(nav.cache.len(), CACHE_CAP);

    // Touching the oldest entry moves it to the newest end, so the *next*
    // eviction takes q1 rather than the entry we just used.
    assert!(nav.cached("q0").is_some());
    nav.remember("q0".into(), &[suggestion("x")]);
    nav.remember("fresh".into(), &[suggestion("x")]);

    assert_eq!(nav.cache.len(), CACHE_CAP);
    assert!(nav.cached("q0").is_some(), "reused entry must survive");
    assert!(nav.cached("q1").is_none(), "oldest must be evicted");
    assert!(nav.cached("fresh").is_some());
}

/// The mitigation `docs/notes/geocoding-latency.md` asks for: Nominatim does
/// no prefix matching, so a half-typed name answers `[]` as a matter of
/// course. Clearing the list on that made the dropdown blink empty between
/// keystrokes and announce "No results" for a query nobody had finished
/// typing — the search read as broken exactly while it was working.
#[test]
fn an_empty_answer_keeps_the_last_list_and_holds_its_verdict() {
    let mut nav = GeoNavState::default();
    nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
    assert!(!nav.stale);
    assert_eq!(nav.status(), None);

    // "Groninge" → "Groningen…" mid-word: nothing comes back.
    nav.apply_suggestions(Vec::new());
    assert_eq!(nav.suggestions.len(), 1, "the last real list must stay up");
    assert!(nav.stale, "…but marked, so the rows can be dimmed");
    assert_eq!(nav.status(), None, "and no verdict yet");

    // Once the empty answer has stood for the grace period, say so — under the
    // rows, which are still real places worth clicking.
    nav.empty_since = Some(
        Instant::now()
            .checked_sub(NO_RESULTS_GRACE * 2)
            .expect("the monotonic clock is older than the grace period"),
    );
    assert_eq!(nav.status().as_deref(), Some("No results"));

    // A real answer clears both.
    nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
    assert!(!nav.stale);
    assert_eq!(nav.status(), None);
}

/// A failure is not a "no results", and does not wait: the network being down
/// is a fact about now, not a verdict on the query.
#[test]
fn a_failure_is_reported_at_once() {
    let mut nav = GeoNavState::default();
    nav.error = Some("Search failed: timeout".to_string());
    assert_eq!(nav.status().as_deref(), Some("Search failed: timeout"));
}

/// Emptying the field drops the answers with it — a list of places under a
/// blank query is not "the last thing you searched", it is a list with nothing
/// tying it to what the bar says.
#[test]
fn emptying_the_field_forgets_the_answers() {
    let mut nav = GeoNavState::default();
    nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
    nav.apply_suggestions(Vec::new());
    assert!(nav.stale);
    nav.forget_suggestions();
    assert!(nav.suggestions.is_empty() && !nav.stale && nav.empty_since.is_none());
}

/// "Route here" from a row is the sentence this whole path exists for, and it
/// must behave exactly as the map's own "Route to here" does — including
/// dropping the route that was drawn for the previous destination.
#[test]
fn routing_to_a_place_sets_the_destination_and_opens_the_panel() {
    // `set_endpoint_at` fires the reverse geocode on the `IoTaskPool`.
    bevy::tasks::IoTaskPool::get_or_init(Default::default);

    let mut nav = GeoNavState::default();
    let mut route = RouteState::default();
    let mut panel = RightPanel::None;

    nav.route_to("Vismarkt", 53.2194, 6.5665);
    apply_route_request(&mut nav, &mut route, &mut panel);

    assert!(nav.route_request.is_none(), "the request is spent");
    assert_eq!(route.to.coord, Some((53.2194, 6.5665)));
    // The name the user just read on the row, not the coordinates the reverse
    // lookup is only now going out to fetch.
    assert_eq!(route.to.label, "Vismarkt");
    assert!(panel.is(RightPanel::Route), "and the planner is on screen");

    // Routing somewhere is not flying there.
    assert!(nav.pending_teleport.is_none());
}

/// A nameless place still routes; it just wears its coordinates, which is what
/// `set_endpoint_at` puts there for a map pick.
#[test]
fn routing_without_a_name_keeps_the_coordinate_label() {
    bevy::tasks::IoTaskPool::get_or_init(Default::default);
    let mut nav = GeoNavState::default();
    let mut route = RouteState::default();
    let mut panel = RightPanel::None;

    nav.route_to("  ", 53.0, 6.0);
    apply_route_request(&mut nav, &mut route, &mut panel);
    assert!(
        !route.to.label.is_empty() && !route.to.label.contains('📍'),
        "an unnamed pick keeps the coordinate label, and that label must not \
             carry a glyph the font cannot draw"
    );
}

/// The recents list: newest first, without the place we are standing on (that
/// one is not somewhere to go), capped so the dropdown stays a dropdown.
#[test]
fn recents_read_newest_first_and_skip_where_we_are() {
    let mut nav = GeoNavState::default();
    for i in 0..(RECENT_SUGGESTIONS + 3) {
        nav.record_visit(NavEntry {
            lat: i as f64,
            lng: 0.0,
            altitude: 100.0,
            pose: CameraPose::default(),
            label: Some(format!("place {i}")),
        });
    }
    let names: Vec<String> = nav.recent_entries().map(|(_, e)| e.title()).collect();
    assert_eq!(names.len(), RECENT_SUGGESTIONS);
    let here = nav.history[nav.history_pos].title();
    assert!(
        !names.contains(&here),
        "where we already are is not a destination"
    );
    assert_eq!(names[0], format!("place {}", RECENT_SUGGESTIONS + 1));

    // A visit that arrived without a name still lists — as its coordinates,
    // which is all a startup restore can honestly say about itself.
    nav.record_visit(NavEntry {
        lat: 53.2194,
        lng: 6.5665,
        altitude: 100.0,
        pose: CameraPose::default(),
        label: None,
    });
    nav.record_visit(NavEntry {
        lat: 1.0,
        lng: 1.0,
        altitude: 100.0,
        pose: CameraPose::default(),
        label: None,
    });
    let names: Vec<String> = nav.recent_entries().map(|(_, e)| e.title()).collect();
    assert_eq!(names[0], "53.21940, 6.56650");
}

/// Enter must not fly to the top of a *stale* list: those hits answer a query
/// the field no longer holds, and Enter is the one gesture that acts without
/// the user having read the row.
#[test]
fn an_empty_answer_spends_a_pending_enter_without_jumping() {
    let mut nav = GeoNavState::default();
    nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);
    nav.teleport_on_result = true;
    nav.apply_suggestions(Vec::new());
    assert!(!nav.teleport_on_result, "the flag must be spent");
    assert!(
        nav.pending_teleport.is_none(),
        "and it must not jump to a hit for an older query"
    );
}

/// Drive one row through egui and click at `aim` (computed from the row's own
/// rect, since the layout decides where things land).
fn click_row(aim: impl Fn(egui::Rect) -> egui::Pos2) -> (bool, bool) {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    let s = GeoSuggestion::plain("Vismarkt, Groningen", 53.2194, 6.5665);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };

    let mut target = egui::Pos2::ZERO;
    let (mut clicked, mut routed) = (false, false);
    for frame in 0..5 {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(screen);
        match frame {
            1 => raw.events.push(egui::Event::PointerMoved(target)),
            2 => raw.events.push(button(target, true)),
            3 => raw.events.push(button(target, false)),
            _ => {}
        }
        let _ = ctx.run_ui_unpainted(raw, |ui| {
            let pick = destination_row(ui, &s, Some((53.0, 6.0)), 360.0, false);
            target = aim(pick.response.rect);
            clicked |= pick.clicked();
            routed |= pick.routed();
        });
    }
    (clicked, routed)
}

/// The ⚑ sits *inside* a widget that senses clicks over the whole row, which is
/// the arrangement that eats a button's press when it goes the other way round
/// (see `dnd_drag_source` in `docs/notes/egui-shared-root-ui.md`). It works here
/// because egui registers a `UiBuilder::sense` scope's own widget *before* its
/// contents and only updates it in place afterwards, so the button is on top of
/// the row rather than under it — a fact worth a test rather than a comment,
/// since nothing about the code says it and the failure is a dead button.
#[test]
fn the_route_button_is_clickable_inside_the_row() {
    // The button is on the row's first line, hard against the right edge.
    let hit = [6.0_f32, 10.0, 14.0].iter().any(|&dy| {
        let (clicked, routed) = click_row(|r| egui::pos2(r.right() - 10.0, r.top() + dy));
        routed && !clicked
    });
    assert!(hit, "the ⚑ never received a click");
}

/// …and the rest of the row still goes to the row, or picking a search result
/// would have stopped working.
#[test]
fn clicking_the_name_still_picks_the_row() {
    let (clicked, routed) = click_row(|r| egui::pos2(r.left() + 24.0, r.top() + 10.0));
    assert!(clicked, "the row itself must still take a click");
    assert!(!routed, "and must not route");
}

#[test]
fn a_cache_hit_teleports_just_like_a_reply() {
    // Enter pressed before the results landed, then an answer we already
    // hold: the jump must still happen, or Enter would silently do nothing
    // on exactly the queries we serve fastest.
    let mut nav = GeoNavState::default();
    nav.teleport_on_result = true;
    nav.apply_suggestions(vec![suggestion("Groningen, Nederland")]);

    assert!(!nav.teleport_on_result, "flag must be consumed");
    assert!(nav.pending_teleport.is_some(), "cache hit must still jump");
    assert!(!nav.show_suggestions);
    assert_eq!(nav.query, "Groningen, Nederland");
}

/// A globe built around [`ANCHOR`], the way `setup_globe` leaves it: Earth's
/// centre one radius below the local ground.
fn test_globe() -> GlobeState {
    let mut globe = GlobeState::default();
    globe.radius = 6_378_137.0;
    globe.center_local = Vec3::new(0.0, -globe.radius, 0.0);
    globe.anchor_lat = ANCHOR.0;
    globe.anchor_lng = ANCHOR.1;
    globe.ready = true;
    globe
}

/// The separability [`WorldFrame`] is built on, asserted rather than assumed:
/// longitude may not depend on the northing, nor latitude on the easting.
///
/// This is what lets a grid conversion cost `n + n` instead of `n²`, and it is
/// exactly the kind of property that is true of the maths, easy to break in the
/// code, and invisible afterwards — a `lat_at` that quietly read `wx` would
/// still return plausible coordinates everywhere near the anchor.
#[test]
fn the_mercator_frame_is_separable() {
    let frame = WorldFrame::new(ANCHOR);
    for w in [-40_000.0, -1_200.0, 0.0, 850.0, 40_000.0] {
        for other in [-40_000.0, 0.0, 40_000.0] {
            let (_, lng) = world_to_lat_lng(ANCHOR, w, other);
            assert!((lng - frame.lng_at(w)).abs() < 1e-12, "lng moved with wz");
            let (lat, _) = world_to_lat_lng(ANCHOR, other, w);
            assert!((lat - frame.lat_at(w)).abs() < 1e-12, "lat moved with wx");
        }
    }
}

/// The frame is an optimisation, so it has to agree with the round trip it
/// replaced — not merely with itself.
#[test]
fn the_frame_round_trips_against_the_forward_mapping() {
    for (lat, lng) in [(53.2194, 6.5665), (53.5, 6.0), (52.9, 7.1)] {
        let (wx, wz) = lat_lng_to_world(ANCHOR, lat, lng);
        let frame = WorldFrame::new(ANCHOR);
        assert!((frame.lat_at(wz) - lat).abs() < 1e-9);
        assert!((frame.lng_at(wx) - lng).abs() < 1e-9);
    }
}

/// Near the ground the anchored Mercator frame is the ground truth (the
/// streamed tiles are keyed with it), so the fix stays on the flat mapping.
#[test]
fn camera_geo_uses_the_flat_frame_near_the_ground() {
    let tf = Transform::from_xyz(1_200.0, 400.0, -800.0);
    let got = camera_geo(ANCHOR, &test_globe(), &GridCell::default(), &tf);
    let flat = world_to_lat_lng(ANCHOR, 1_200.0, -800.0);
    assert!((got.0 - flat.0).abs() < 1e-9 && (got.1 - flat.1).abs() < 1e-9);
}

/// A Fly pose has to arrive *placed*, not merely written: `camera_controls`
/// reseeds the look from the live camera transform on the first frame it sees
/// an uninitialized Fly camera, which threw away the angle the pose had just
/// set. Every bookmark, `?place=` link and `--shot --look` that named a pitch
/// was quietly overruled the first time it was applied.
#[test]
fn a_fly_pose_arrives_placed_rather_than_reseeded() {
    let mut cs = CameraState::default();
    // Entering Fly clears the flag on purpose — that is what makes a plain
    // mode switch restore the last fly spot.
    crate::systems::free_camera::enter_mode(&mut cs, CameraMode::Free);
    assert!(!cs.free_initialized);

    CameraPose::free_look(-12.0, 180.0).apply(&mut cs);
    assert!(
        cs.free_initialized,
        "a pose that names a look angle must not be reseeded from the transform"
    );
    assert!((cs.free_camera_rotation.x - (-12.0_f32).to_radians()).abs() < 1e-6);
    assert!((cs.free_camera_rotation.y - 180.0_f32.to_radians()).abs() < 1e-6);

    // The other modes are untouched: Orbit still sets its own flag, and a
    // pose with no mode still leaves the current view alone.
    let mut orbit = CameraState::default();
    CameraPose {
        mode: "earth".to_string(),
        orbit_distance: 1_500.0,
        ..Default::default()
    }
    .apply(&mut orbit);
    assert!(orbit.orbit_initialized);
    assert!(!orbit.free_initialized);
}

/// In space the flat frame projects a multi-thousand-kilometre offset onto an
/// arbitrary point of the map — the bug that saved a mid-ocean last location
/// on exit from orbit. The sub-point is used instead.
#[test]
fn camera_geo_uses_the_sub_point_in_space() {
    let globe = test_globe();
    let tf = Transform::from_xyz(3_000_000.0, 1_000_000.0, 0.0);
    let got = camera_geo(ANCHOR, &globe, &GridCell::default(), &tf);

    let dir = (tf.translation.as_dvec3() - globe.center_local.as_dvec3())
        .normalize()
        .as_vec3();
    let expected = sub_point(globe.anchor_lat, globe.anchor_lng, dir);
    assert!((got.0 - expected.0).abs() < 1e-9 && (got.1 - expected.1).abs() < 1e-9);

    // …and that is nowhere near what the flat frame claimed.
    let flat = world_to_lat_lng(ANCHOR, 3_000_000.0, 0.0);
    assert!(
        (flat.1 - got.1).abs() > 5.0,
        "flat frame {flat:?} should disagree wildly with the sub-point {got:?}"
    );
}

#[test]
fn nav_history_back_forward() {
    let mk = |lat: f64| NavEntry {
        lat,
        lng: 0.0,
        altitude: 100.0,
        pose: CameraPose::default(),
        label: None,
    };
    let mut nav = GeoNavState::default();
    assert!(!nav.can_go_back() && !nav.can_go_forward());

    nav.record_visit(mk(1.0));
    assert!(!nav.can_go_back(), "one entry: nowhere to go back");
    nav.record_visit(mk(2.0));
    nav.record_visit(mk(3.0));
    assert_eq!(nav.history_pos, 2);
    assert!(nav.can_go_back() && !nav.can_go_forward());

    // Back to 2.0 queues a flagged teleport (so it isn't re-recorded).
    nav.go_back();
    assert_eq!(nav.history_pos, 1);
    assert!(nav.from_history);
    assert_eq!(nav.pending_teleport.as_ref().unwrap().lat, 2.0);
    assert!(nav.can_go_forward());

    // Forward again to 3.0 (simulate apply_teleport clearing the flag).
    nav.from_history = false;
    nav.pending_teleport = None;
    nav.go_forward();
    assert_eq!(nav.history_pos, 2);
    assert_eq!(nav.pending_teleport.as_ref().unwrap().lat, 3.0);
    assert!(!nav.can_go_forward());

    // Go back, then a fresh (non-history) visit truncates the forward tail
    // (3.0) and appends — browser semantics.
    nav.from_history = false;
    nav.go_back(); // pos 1 → 2.0
    nav.from_history = false; // a real teleport, not a history move
    nav.record_visit(mk(9.0));
    assert_eq!(
        nav.history.iter().map(|e| e.lat).collect::<Vec<_>>(),
        vec![1.0, 2.0, 9.0]
    );
    assert_eq!(nav.history_pos, 2);
    assert!(!nav.can_go_forward());

    // Re-visiting the current spot is a no-op (no duplicate stacking).
    nav.record_visit(mk(9.0));
    assert_eq!(nav.history.len(), 3);
}

#[test]
fn parse_location_variants() {
    let fix = |lat, lng, altitude, look| {
        Some(LocationFix {
            lat,
            lng,
            altitude,
            look,
        })
    };
    assert_eq!(
        parse_location("53.2194, 6.5665"),
        fix(53.2194, 6.5665, None, None)
    );
    assert_eq!(
        parse_location("53.21940, 6.56650, 120"),
        fix(53.2194, 6.5665, Some(120.0), None)
    );
    assert_eq!(
        parse_location("53.2194, 6.5665, 120m"),
        fix(53.2194, 6.5665, Some(120.0), None)
    );
    assert_eq!(
        parse_location("53.2194N 6.5665E"),
        fix(53.2194, 6.5665, None, None)
    );
    assert_eq!(parse_location("Groningen"), None);
    assert_eq!(parse_location("300, 4"), None); // latitude out of range
    assert_eq!(parse_location("1, 2, 3, 4"), None); // a lone pitch is malformed
}

#[test]
fn parse_location_with_camera_rotation() {
    // What the HUD copy button writes, decoration and all.
    assert_eq!(
        parse_location("53.21940, 6.56650, 120m, -12.5°, 45.0°"),
        Some(LocationFix {
            lat: 53.2194,
            lng: 6.5665,
            altitude: Some(120.0),
            look: Some((-12.5, 45.0)),
        })
    );
    // And the same string as it survives a URL, without the units.
    assert_eq!(
        parse_location("53.2194,6.5665,120,-12.5,45"),
        parse_location("53.21940, 6.56650, 120m, -12.5°, 45.0°")
    );
    assert_eq!(parse_location("53.2194, 6.5665, 120, x, 45"), None);
}

#[test]
fn location_fix_teleport_restores_the_view() {
    let fix = parse_location("53.2194, 6.5665, 120, -12.5, 45").unwrap();
    let tp = Teleport::fix(fix);
    assert_eq!(tp.altitude, Some(120.0));
    let mut cs = CameraState::default();
    tp.pose.unwrap().apply(&mut cs);
    assert!(matches!(cs.mode, CameraMode::Free));
    assert!((cs.free_camera_rotation.x - (-12.5f32).to_radians()).abs() < 1e-6);
    assert!((cs.free_camera_rotation.y - 45f32.to_radians()).abs() < 1e-6);
    // A fix without angles leaves the camera alone.
    assert!(Teleport::fix(parse_location("53.2194, 6.5665").unwrap())
        .pose
        .is_none());
}

#[test]
fn camera_pose_round_trips() {
    let cs = CameraState {
        mode: CameraMode::Free,
        free_camera_rotation: Vec2::new(0.3, -1.2),
        azimuth: 1.1,
        pitch: 0.4,
        distance: 42.0,
        ..Default::default()
    };
    let pose = CameraPose::capture(&cs);
    let mut restored = CameraState::default();
    pose.apply(&mut restored);
    assert!(matches!(restored.mode, CameraMode::Free));
    assert_eq!(restored.free_camera_rotation, Vec2::new(0.3, -1.2));
    assert_eq!(restored.azimuth, 1.1);
    assert_eq!(restored.pitch, 0.4);
    assert_eq!(restored.distance, 42.0);
}

#[test]
fn earth_pose_round_trips() {
    let cs = CameraState {
        mode: CameraMode::Orbit,
        orbit_distance: 8200.0,
        orbit_yaw: 1.3,
        orbit_pitch: 0.9,
        ..Default::default()
    };
    let pose = CameraPose::capture(&cs);
    let mut restored = CameraState::default();
    pose.apply(&mut restored);
    assert!(matches!(restored.mode, CameraMode::Orbit));
    assert_eq!(restored.orbit_distance, 8200.0);
    assert_eq!(restored.orbit_yaw, 1.3);
    assert_eq!(restored.orbit_pitch, 0.9);
    // The focus is re-derived from the jump target, so the pose leaves it for
    // apply_teleport to set; entering marks the state seeded.
    assert!(restored.orbit_initialized);
}

#[test]
fn empty_pose_keeps_current_view() {
    // Old bookmarks (saved before poses) deserialize to an empty mode and
    // must not disturb the current camera.
    let mut cs = CameraState {
        mode: CameraMode::Avatar,
        azimuth: 0.7,
        ..Default::default()
    };
    CameraPose::default().apply(&mut cs);
    assert!(matches!(cs.mode, CameraMode::Avatar));
    assert_eq!(cs.azimuth, 0.7);
}

/// Run [`apply_teleport`] once on `tp`, signed in or not, from Follow mode
/// with the avatar standing at the anchor. Returns the avatar's position and
/// the camera mode afterwards.
fn teleport_once(tp: Teleport, signed_in: bool) -> (Vec3, CameraMode, bool) {
    let mut app = App::new();
    let mut nav = GeoNavState::default();
    nav.pending_teleport = Some(tp);
    app.insert_resource(nav)
        .insert_resource(MapStream::anchored_at(ANCHOR))
        .insert_resource(CameraState {
            mode: CameraMode::Avatar,
            ..Default::default()
        })
        .insert_resource(AvatarState::default())
        .insert_resource(GlobeState::default());
    let (tx, _rx) = futures_channel::mpsc::unbounded();
    if signed_in {
        app.insert_resource(crate::resources::OnlineSession { intent_tx: tx });
    }
    app.world_mut()
        .spawn((FreeCamera, GridCell::ZERO, Transform::default()));
    let avatar = app
        .world_mut()
        .spawn((Avatar, GridCell::ZERO, Transform::default()))
        .id();
    app.add_systems(Update, apply_teleport);
    app.update();
    let at = app.world().get::<Transform>(avatar).unwrap().translation;
    let reanchors = app.world().resource::<MapStream>().reanchor_pending();
    (at, app.world().resource::<CameraState>().mode, reanchors)
}

/// A search pick, signed in, moves the view and leaves the body: the route
/// planner's "My position" is the avatar, and a pick that brought it along
/// made every "To" on the place card a route from the place to itself.
#[test]
fn a_signed_in_look_leaves_the_avatar_standing() {
    // ~700 m east of the anchor: a short jump, in the same frame.
    let near = Teleport::geo(ANCHOR.0, ANCHOR.1 + 0.01, None).looking();
    let (at, mode, reanchors) = teleport_once(near, true);
    assert!(!reanchors);
    assert!(at.length() < 1e-3, "the avatar moved to {at}");
    assert!(
        matches!(mode, CameraMode::Orbit),
        "Follow would pull the view back"
    );

    // The same jump that is not a look still brings the avatar.
    let beam = Teleport::geo(ANCHOR.0, ANCHOR.1 + 0.01, None);
    let (at, ..) = teleport_once(beam, true);
    assert!(at.x > 600.0, "a beam must move the avatar, it is at {at}");

    // Offline there is no body apart from the view, so a look is a jump.
    let offline = Teleport::geo(ANCHOR.0, ANCHOR.1 + 0.01, None).looking();
    let (at, ..) = teleport_once(offline, false);
    assert!(
        at.x > 600.0,
        "offline the avatar rides along, it is at {at}"
    );
}

/// A far look re-anchors the map under the place and carries the avatar
/// into the new frame at the latitude and longitude it stood on.
#[test]
fn a_far_look_keeps_the_avatar_where_it_was_on_the_earth() {
    // Winsum, ~15 km north-west: past `SHORT_JUMP_M`.
    let far = Teleport::geo(53.3284, 6.5164, None).looking();
    let (at, _, reanchors) = teleport_once(far, true);
    assert!(reanchors);
    let next = crate::systems::map_stream::snap_anchor(53.3284, 6.5164, ANCHOR.2);
    let (wx, wz) = lat_lng_to_world((next.0, next.1, ANCHOR.2), ANCHOR.0, ANCHOR.1);
    let grid = Grid::new(WORLD_CELL_EDGE, WORLD_SWITCH_THRESHOLD);
    let (_, local) = grid.translation_to_grid(DVec3::new(wx, 0.0, wz));
    assert!(
        (at.x - local.x).abs() < 0.5 && (at.z - local.z).abs() < 0.5,
        "the avatar is at {at}, its spot on the earth is {local}"
    );
}
