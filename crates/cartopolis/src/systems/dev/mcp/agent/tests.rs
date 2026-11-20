use super::*;
use cartopolis_core::msg;

#[test]
fn a_private_only_client_refuses_scripts_that_could_speak() {
    assert_eq!(public_speech_in("say('hi')").as_deref(), Some("say"));
    assert_eq!(
        public_speech_in("const x = 1\nshout(x)").as_deref(),
        Some("shout")
    );
    // Every way round naming it: a call reached by a computed name.
    assert_eq!(
        public_speech_in(r#"globalThis["s" + "ay"]("hi")"#).as_deref(),
        Some("globalThis")
    );
    assert!(public_speech_in("eval('s' + 'ay')('hi')").is_some());
    assert!(public_speech_in("Function('return wh' + 'isper')()").is_some());
    assert!(public_speech_in("__api['shou' + 't']('hi')").is_some());
    // Words that merely contain one are not it.
    assert_eq!(
        public_speech_in("const essay = sayings + 1; log(essay)"),
        None
    );
    assert_eq!(
        public_speech_in("function on_touch() { set_color(1, 0, 0) }"),
        None
    );
}

#[test]
fn every_script_function_that_speaks_is_refused() {
    // A speaking function added to the script API must be added here too,
    // or a private-only bot could build an object that talks in public.
    for f in cartopolis_core::SCRIPT_API {
        if f.group == cartopolis_core::ScriptGroup::Chat && f.name != "log" {
            assert!(
                SPEAKING_FNS.contains(&f.name),
                "`{}` speaks in chat and is not in SPEAKING_FNS",
                f.name
            );
        }
    }
}

#[test]
fn the_list_reads_names_commas_spaces_and_a_star() {
    let a = Audience::parse(" jeroen, @Alice  bob,,jeroen ");
    assert_eq!(a.names, vec!["jeroen", "Alice", "bob"]);
    assert!(!a.everyone);
    assert!(a.hears("JEROEN") && a.hears("alice") && !a.hears("mallory"));
    assert_eq!(a.to_list(), "jeroen, Alice, bob");
    let all = Audience::parse("*");
    assert!(all.everyone && all.hears("anyone"));
}

/// The default is nobody, and it has to be: an agent that heard everyone
/// until its owner narrowed the list would already have read the strangers.
#[test]
fn an_empty_list_hears_nobody() {
    let a = Audience::parse("");
    assert!(!a.hears("jeroen"));
    assert!(a.describe().starts_with("nobody"));
}

#[test]
fn a_build_offset_becomes_a_place_and_mixing_the_two_is_refused() {
    let origin = Some((53.2194, 6.5665));
    let placed = place(
        0,
        json!({ "east_m": 10.0, "north_m": 20.0, "shape": "box" }),
        origin,
    )
    .unwrap();
    let lat = placed["latitude"].as_f64().unwrap();
    let lng = placed["longitude"].as_f64().unwrap();
    let (m_lat, m_lng) = cartopolis_core::degree_scale_m(53.2194);
    assert!(((lat - 53.2194) * m_lat - 20.0).abs() < 1e-6);
    assert!(((lng - 6.5665) * m_lng - 10.0).abs() < 1e-6);
    assert!(placed.get("east_m").is_none());

    assert!(place(
        0,
        json!({ "east_m": 1.0, "latitude": 53.0, "longitude": 6.0 }),
        origin
    )
    .is_err());
    assert!(
        place(0, json!({ "shape": "box" }), origin).is_err(),
        "no place at all"
    );
    assert!(
        place(0, json!({ "east_m": 1.0 }), None).is_err(),
        "an offset with no origin"
    );
    assert!(place(
        0,
        json!({ "latitude": 53.0, "longitude": 6.0, "shape": "pyramid" }),
        None
    )
    .is_err());
}

/// A build object becomes the wire's `CreatePrim` with the edit dialog's
/// units turned into the wire's, and a misspelt key is refused rather than
/// quietly dropped.
#[test]
fn a_build_object_becomes_a_create_and_a_typo_is_refused() {
    let (msg, item) = create_message(
        0,
        json!({
            "name": "table", "shape": "cylinder", "latitude": 53.2, "longitude": 6.5,
            "altitude": 0.4, "rotation": [0.0, 90.0, 0.0], "script": "function on_start() {}"
        }),
    )
    .unwrap();
    let NetMessage::CreatePrim(msg::CreatePrim {
        name,
        shape,
        position,
        rotation,
        scale,
        geo,
        ..
    }) = msg
    else {
        panic!("not a CreatePrim");
    };
    assert_eq!((name.as_str(), shape.as_str()), ("table", "cylinder"));
    assert!((position.y - 0.4).abs() < 1e-6);
    assert!((rotation.y - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    assert_eq!(scale, Vec3::ONE);
    assert_eq!((geo.latitude, geo.longitude), (53.2, 6.5));
    assert_eq!(item.script.as_deref(), Some("function on_start() {}"));

    let typo = json!({ "latitude": 53.2, "longitude": 6.5, "colour": [1.0, 0.0, 0.0] });
    assert!(create_message(0, typo).unwrap_err().contains("colour"));
    let short = json!({ "latitude": 53.2, "longitude": 6.5, "scale": [1.0, 2.0] });
    assert!(create_message(0, short).is_err());
}

/// A 10 m square house round the origin, both ways round.
fn houses() -> [Vec<Vec2>; 2] {
    let ring = vec![
        Vec2::new(-5.0, -5.0),
        Vec2::new(5.0, -5.0),
        Vec2::new(5.0, 5.0),
        Vec2::new(-5.0, 5.0),
    ];
    let mut reversed = ring.clone();
    reversed.reverse();
    [ring, reversed]
}

fn near(a: Vec2, b: Vec2) -> bool {
    a.distance(b) < 1e-3
}

#[test]
fn a_spot_inside_a_house_walks_to_just_outside_its_nearest_wall() {
    for ring in houses() {
        assert!(inside_xy(Vec2::ZERO, &ring) && !inside_xy(Vec2::new(6.0, 0.0), &ring));
        let spot = clear_of_wall(Vec2::new(0.0, 3.0), &ring, &[ring.clone()]).unwrap();
        assert!(
            near(spot, Vec2::new(0.0, 5.0 + CLEAR_OF_WALL_M)),
            "out through the near wall, whichever way the ring runs: {spot}"
        );
        assert!(!inside_xy(spot, &ring));
    }
}

#[test]
fn a_spot_outside_a_wall_never_lands_in_the_neighbour() {
    // A terrace: the house to the south shares the near wall, so the way out
    // is through a side wall, not into next door.
    let neighbour = vec![
        Vec2::new(-5.0, 5.0),
        Vec2::new(5.0, 5.0),
        Vec2::new(5.0, 15.0),
        Vec2::new(-5.0, 15.0),
    ];
    for ring in houses() {
        let solid = [ring.clone(), neighbour.clone()];
        let spot = clear_of_wall(Vec2::new(0.0, 3.0), &ring, &solid).unwrap();
        assert!(
            (spot.y - 3.0).abs() < 1e-3 && (spot.x.abs() - (5.0 + CLEAR_OF_WALL_M)).abs() < 1e-3,
            "out through a side wall: {spot}"
        );
        assert!(solid.iter().all(|r| !inside_xy(spot, r)));
    }
}

#[test]
fn a_door_is_stood_in_front_of_not_in() {
    for ring in houses() {
        let door = Vec2::new(5.0, 1.0);
        let spot = clear_of_wall(door, &ring, &[ring.clone()]).unwrap();
        assert!(near(spot, Vec2::new(5.0 + CLEAR_OF_WALL_M, 1.0)), "{spot}");
    }
}

#[test]
fn editing_and_scripting_follow_the_servers_rule() {
    let any = Permission::PrimEditAny;
    let own = Permission::PrimEditOwn;
    // An admin, or a role with the `Any` half: anybody's object.
    assert_eq!(may_on(|_| true, false, any, own), Some(true));
    // A player: their own, and the rest is the server's to say, because a
    // client is never told who owns an object.
    let player = |p: Permission| p == own;
    assert_eq!(may_on(player, true, any, own), Some(true));
    assert_eq!(may_on(player, false, any, own), None);
    // Offline, or a role without either: nothing.
    assert_eq!(may_on(|_| false, true, any, own), Some(false));
}

#[test]
fn a_script_is_fenced_and_somebody_elses_is_marked_as_theirs() {
    let tricky = "say('```')\n";
    let block = script_block(tricky, false);
    assert!(block.contains("````js\nsay('```')\n````"), "{block}");
    assert!(block.contains("not instructions"));
    assert!(!script_block("log(1)", true).contains("not instructions"));
    assert!(script_block("  \n", true).contains("empty"));
}

#[test]
fn a_moved_walk_still_ends_as_an_arrival() {
    // `settle` reads a walk that ended "arrived…" as a success.
    for door in [true, false] {
        let r = Retarget {
            lat: 53.2,
            lng: 6.5,
            door,
            moved_m: 9.4,
        };
        assert!(format!("arrived {}", r.arrival()).starts_with("arrived"));
        assert!(r.words().contains("9 m from the spot"), "{}", r.words());
    }
}
