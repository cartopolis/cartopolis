use super::*;

pub(super) fn walk(
    state: &mut AgentState,
    audience: &Audience,
    w: &mut AgentWorld,
    args: &WalkArgs,
    now: f64,
) -> AgentReply {
    if w.bodies.is_empty() {
        return AgentReply::refuse(
            "There is no avatar to walk: this client is offline. Sign it in to a server first.",
        );
    }
    let Some(anchor) = w.anchor.geo else {
        return AgentReply::refuse("The map has not started yet; try again in a moment.");
    };
    let gait = match args.pace.as_deref().unwrap_or("walk") {
        "walk" => Gait::Walk,
        "run" => Gait::Run,
        "stroll" => Gait::Stroll,
        other => {
            return AgentReply::refuse(format!("`{other}` is not a pace; use walk, run or stroll"))
        }
    };
    let here = Vec2::new(w.avatar.position.x, w.avatar.position.z);
    let coords = match (&args.person, args.lat, args.lng, &args.place) {
        (Some(_), None, None, None) => None,
        (Some(_), ..) => {
            return AgentReply::refuse("Give a person, or a place — not both.");
        }
        (None, ..) => match resolve_place(state, args.lat, args.lng, args.place.as_deref()) {
            Ok(p) => Some(p),
            Err(why) => return AgentReply::refuse(why),
        },
    };
    // A spot inside a building is not somewhere to walk to: the route ends on
    // the footpath nearest it and the last straight stretch runs into the wall
    // (s584: 12 m short, nose to the brick). Walk to its door, or to the nearest
    // spot outside its walls, instead — unless the avatar is in there already.
    let from = w.avatar_geo();
    let mut checked = true;
    let mut moved = None;
    let coords = coords.map(|(lat, lng, named)| {
        let Some(r) = walkable_goal(&w.around.facts, (lat, lng), from) else {
            // Undecided while the register around it has not loaded; the
            // route landing asks again (`steer`).
            checked = !w.around.facts.near(lat, lng, 300.0).is_empty();
            return (lat, lng);
        };
        let at = (r.lat, r.lng);
        moved = Some((r, named));
        at
    });
    let (goal, target) = match (&args.person, coords) {
        (Some(name), None) => {
            if !audience.hears(name) {
                return AgentReply::refuse(format!(
                    "{name} is not somebody you are allowed to hear. You can hear: {}.",
                    audience.describe()
                ));
            }
            let Some(sim_id) = w.avatar_of(name) else {
                return AgentReply::refuse(format!(
                    "I do not know which avatar is {name}: avatars carry no names, and they \
                     have not said anything in public chat since this client arrived. Ask \
                     them to say something, or walk to a place instead."
                ));
            };
            let Some(p) = w.remote_position(sim_id) else {
                return AgentReply::refuse(format!("{name} is not in view any more."));
            };
            (
                Goal::Person {
                    name: name.clone(),
                    sim_id,
                },
                Vec2::new(p.x, p.z),
            )
        }
        (None, Some((lat, lng))) => {
            let (x, z) = crate::grid::lat_lng_to_world(anchor, lat, lng);
            (Goal::Place { lat, lng }, Vec2::new(x as f32, z as f32))
        }
        _ => {
            return AgentReply::refuse(
                "Give `lat` and `lng`, a saved `place`, or a `person` — one of them.",
            );
        }
    };
    let distance = here.distance(target);
    if f64::from(distance) > MAX_WALK_M {
        return AgentReply::refuse(format!(
            "That is {:.1} km away, further than a walk. Teleport near it first, then walk \
             the last stretch.",
            distance / 1000.0
        ));
    }
    let eta = distance / gait.speed();
    // A place gets a walking route along the streets; a person is near enough
    // and moves, so they are walked at directly.
    let (what, planning) = match &goal {
        Goal::Place { lat, lng } => {
            let template = w.tile_url.0.lock().map(|g| g.clone()).unwrap_or_default();
            let here = w.avatar_geo();
            (
                "there".to_string(),
                here.map(|from| {
                    crate::systems::navigation::solve_detached(
                        from,
                        (*lat, *lng),
                        crate::systems::routing::TravelMode::Foot,
                        template,
                    )
                }),
            )
        }
        Goal::Person { name, .. } => (format!("to {name}"), None),
    };
    let routed = planning.is_some();
    let here = Vec2::new(w.avatar.position.x, w.avatar.position.z);
    state.walk = Some(Walk {
        goal,
        gait,
        planning,
        path: Vec::new(),
        next: 0,
        plan: None,
        started: now,
        moved_from: here,
        moved_at: now,
        unstick: 0,
        sidestep: None,
        follow: None,
        holding: false,
        then: None,
        from,
        checked,
        arrive_as: moved.as_ref().map(|(r, _)| r.arrival().to_string()),
    });
    state.walk_ended = None;
    w.avatar.stand_up();
    w.camera.mode = CameraMode::Avatar;
    let moved_words = moved.as_ref().map_or(String::new(), |(r, named)| {
        format!(
            "{} is inside a building, so the walk ends {}. ",
            named
                .as_ref()
                .map_or("That spot".to_string(), |n| format!("\"{n}\"")),
            r.words()
        )
    });
    AgentReply::ok(
        format!(
            "{moved_words}Walking {what}: {distance:.0} m as the crow flies, about {eta:.0} s \
             at that pace. {} It stops if something is in the way — `me` says how it is going.",
            if routed {
                "It plans a route along the streets first, which takes a few seconds."
            } else {
                "It walks straight at them."
            }
        ),
        json!({
            "distance_m": distance,
            "eta_s": eta,
            "routed": routed,
            "moved_to": moved.as_ref().map(|(r, _)| json!({
                "lat": r.lat,
                "lng": r.lng,
                "door": r.door,
                "moved_m": r.moved_m,
            })),
        }),
    )
}

/// A walk's goal, moved off a spot nobody can stand on to the nearest one
/// somebody can.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Retarget {
    pub(super) lat: f64,
    pub(super) lng: f64,
    /// To the building's door, rather than to the nearest bit of pavement
    /// outside a building the register has no door for.
    pub(super) door: bool,
    /// How far from the spot asked for.
    pub(super) moved_m: f32,
}

impl Retarget {
    /// "at the building's nearest door, 9 m from the spot".
    pub(super) fn words(&self) -> String {
        if self.door {
            format!(
                "at the building's nearest door, {:.0} m from the spot",
                self.moved_m
            )
        } else {
            format!(
                "at the nearest spot outside its walls, {:.0} m from the spot (the map has \
                 no door for it)",
                self.moved_m
            )
        }
    }

    /// How arriving there is worded.
    pub(super) fn arrival(&self) -> &'static str {
        if self.door {
            "at the building's door"
        } else {
            "outside the building's wall"
        }
    }
}

/// Where a walk to `goal` should end instead, when `goal` is inside a building
/// and the walk does not start inside that same building: the building's door
/// nearest the goal, stood just clear of its wall; or, with no door on the map,
/// the nearest spot clear of its walls that is in no other building. `None`
/// leaves the goal alone — outside every loaded building, or nothing better to
/// offer.
pub(super) fn walkable_goal(
    facts: &BuildingFacts,
    goal: (f64, f64),
    from: Option<(f64, f64)>,
) -> Option<Retarget> {
    let near = facts.near(goal.0, goal.1, 300.0);
    let (_, home) = near
        .iter()
        .find(|(_, b)| around::inside_ring(goal, &b.footprint))?;
    if from.is_some_and(|f| around::inside_ring(f, &home.footprint)) {
        return None;
    }
    // Metres east and south of the goal, the world frame's axes.
    let local = |ring: &[(f64, f64)]| -> Vec<Vec2> {
        ring.iter().map(|&p| around::geo_step(goal, p)).collect()
    };
    let own = local(&home.footprint);
    let mut solid: Vec<Vec<Vec2>> = near
        .iter()
        .filter(|(_, b)| {
            b.footprint
                .first()
                .is_some_and(|&p| around::geo_step(goal, p).length() <= NEIGHBOURS_M)
        })
        .map(|(_, b)| local(&b.footprint))
        .collect();
    solid.push(own.clone());
    let door = home
        .doors
        .iter()
        .map(|&d| around::geo_step(goal, d))
        .min_by(|a, b| a.length().total_cmp(&b.length()));
    let spot = clear_of_wall(door.unwrap_or(Vec2::ZERO), &own, &solid).or(door)?;
    let (lat, lng) = around::geo_offset(goal, spot);
    Some(Retarget {
        lat,
        lng,
        door: door.is_some(),
        moved_m: spot.length(),
    })
}

/// The point nearest `at` that stands [`CLEAR_OF_WALL_M`] outside one of
/// `ring`'s walls and inside none of the `solid` rings (which should include
/// `ring` itself). Each wall's outside is found by probing [`WALL_PROBE_M`] off
/// its middle — never from the ring's winding. All in one flat metric frame.
pub(super) fn clear_of_wall(at: Vec2, ring: &[Vec2], solid: &[Vec<Vec2>]) -> Option<Vec2> {
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let mut best: Option<(f32, Vec2)> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let ab = b - a;
        let len2 = ab.length_squared();
        if len2 < 1e-6 {
            continue;
        }
        let mid = (a + b) * 0.5;
        let mut out = Vec2::new(ab.y, -ab.x) / len2.sqrt();
        if inside_xy(mid + out * WALL_PROBE_M, ring) {
            out = -out;
        }
        if inside_xy(mid + out * WALL_PROBE_M, ring) {
            // Inside on both sides: a sliver, not a wall with an outside.
            continue;
        }
        let t = ((at - a).dot(ab) / len2).clamp(0.0, 1.0);
        let spot = a + ab * t + out * CLEAR_OF_WALL_M;
        if solid.iter().any(|r| inside_xy(spot, r)) {
            continue;
        }
        let d = spot.distance(at);
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, spot));
        }
    }
    best.map(|(_, s)| s)
}

/// Whether a point is inside a ring, both in the same flat frame (even-odd;
/// either winding, open or closed).
pub(super) fn inside_xy(p: Vec2, ring: &[Vec2]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// `design_room`: open it at a spot, close it, or place its drafts.
pub(super) fn design_room(
    state: &mut AgentState,
    w: &mut AgentWorld,
    args: DesignRoomArgs,
    now: f64,
    reply: oneshot::Sender<Reply>,
) -> Option<(oneshot::Sender<Reply>, AgentReply)> {
    if w.online.is_none() {
        return Some((reply, AgentReply::refuse(NOT_ONLINE)));
    }
    let answer = match args.action.as_str() {
        "open" => {
            let (Some(lat), Some(lng)) = (args.latitude, args.longitude) else {
                return Some((
                    reply,
                    AgentReply::refuse("Give `latitude` and `longitude` for the spot."),
                ));
            };
            crate::systems::design_room::enter(&mut w.around.room, &mut w.nav, lat, lng);
            AgentReply::ok(
                format!(
                    "The design room is open at {lat:.6}, {lng:.6} on the person's screen. \
                     Build there with `build` (give `lat`/`lng` of the spot as the origin and \
                     place parts with `east_m`/`north_m`, or leave the origin out: it is the \
                     spot while the room is open). What you build is a draft only this \
                     account sees, until `design_room` `place`."
                ),
                json!({ "latitude": lat, "longitude": lng }),
            )
        }
        "close" => {
            w.around.room.close();
            AgentReply::ok(
                "The design room is closed; its drafts wait in it for next time.",
                Value::Null,
            )
        }
        "place" => {
            let drafts = w.around.room.drafts.clone();
            if w.around.room.spot.is_none() {
                AgentReply::refuse("The design room is not open: `open` it first.")
            } else if drafts.is_empty() {
                AgentReply::refuse("There are no drafts in the room to place.")
            } else {
                let count = drafts.len();
                let body = json!({ "prim_ids": drafts }).to_string();
                let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                    let answer = crate::systems::model_assets::api_post(
                        "/api/inventory/drafts/place",
                        Some(body),
                    )
                    .await;
                    HttpDone::Room(
                        answer,
                        format!("Placed {count} objects: they stand in the street for everybody."),
                    )
                });
                state.pending.push(Pending::Http {
                    label: "design room",
                    about: "Placing".into(),
                    task,
                    until: now + HTTP_WAIT_S,
                    reply,
                });
                return None;
            }
        }
        other => AgentReply::refuse(format!(
            "`{other}` is not something the design room does: `open`, `close` or `place`."
        )),
    };
    Some((reply, answer))
}

pub(super) fn teleport(state: &mut AgentState, w: &mut AgentWorld, args: &PlaceArgs) -> AgentReply {
    let (lat, lng, named) = match resolve_place(state, args.lat, args.lng, args.place.as_deref()) {
        Ok(p) => p,
        Err(why) => return AgentReply::refuse(why),
    };
    state.walk = None;
    w.nav.pending_teleport = Some(Teleport::geo(lat, lng, None));
    if !w.bodies.is_empty() {
        w.camera.mode = CameraMode::Avatar;
    }
    AgentReply::ok(
        format!(
            "Teleporting to {}. The map around it takes a few seconds to arrive.",
            named.map_or(format!("{lat:.6}, {lng:.6}"), |n| format!("\"{n}\""))
        ),
        json!({ "lat": lat, "lng": lng }),
    )
}

pub(super) fn emote(w: &mut AgentWorld, args: &EmoteArgs) -> AgentReply {
    let emote = match args.name.trim().to_ascii_lowercase().as_str() {
        "wave" => Emote::Wave,
        "interact" => Emote::Interact,
        "roll" => Emote::Roll,
        "punch" => Emote::Punch,
        "kick" => Emote::Kick,
        "faint" => Emote::Faint,
        "throw" => Emote::Throw,
        // Made with somebody, so only the sim starts them (protocol 49).
        "handshake" | "highfive" => {
            return AgentReply::refuse(
                "A greeting is made with somebody: offer it from their person card.",
            )
        }
        other => {
            return AgentReply::refuse(format!(
                "`{other}` is not an emote; use wave, interact, roll, punch, kick, faint or throw"
            ))
        }
    };
    if w.bodies.is_empty() {
        return AgentReply::refuse("There is no avatar: this client is offline.");
    }
    w.avatar.pending_emote = Some(emote);
    AgentReply::ok(
        format!("{}.", args.name.trim()),
        json!({ "emote": args.name.trim() }),
    )
}
