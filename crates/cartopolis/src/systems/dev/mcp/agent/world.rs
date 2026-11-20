use super::*;
use cartopolis_core::msg;

// ---------------------------------------------------------------------------
// Being comfortable in the world
// ---------------------------------------------------------------------------

/// The people near, as `me` and `look_around` name them: by the name they
/// spoke under in public if the agent may hear them, "someone" otherwise.
pub(super) fn people_near(audience: &Audience, w: &AgentWorld, radius: f32) -> Vec<(f32, String)> {
    if !w.session.is_online() {
        return Vec::new();
    }
    let names = public_names(w);
    let here = w.here();
    let mut people: Vec<(f32, String)> = w
        .remotes
        .iter()
        .map(|r| {
            let p = r.net_position + r.net_velocity * r.net_age;
            let d = Vec2::new(p.x - here.x, p.z - here.y).length();
            let name = names
                .iter()
                .find(|(id, _)| *id == r.sim_id)
                .map(|(_, n)| n.clone())
                .filter(|n| audience.hears(n))
                .unwrap_or_else(|| "someone".into());
            (d, name)
        })
        .filter(|(d, _)| *d <= radius)
        .collect();
    people.sort_by(|a, b| a.0.total_cmp(&b.0));
    people
}

pub(super) fn look_around(
    state: &AgentState,
    audience: &Audience,
    w: &AgentWorld,
    args: &LookAroundArgs,
) -> AgentReply {
    let Some(geo) = w.avatar_geo() else {
        return AgentReply::refuse(
            "There is no avatar to look around from: this client is offline, or the map has \
             not started. `view` can still photograph a place.",
        );
    };
    let radius = args
        .radius_m
        .unwrap_or(around::DEFAULT_LOOK_M)
        .clamp(5.0, around::MAX_LOOK_M);
    let prims = w
        .prims
        .iter()
        .filter_map(|(p, g)| {
            let g = g?;
            Some(around::SeenPrim {
                id: p.id,
                name: if p.name.trim().is_empty() {
                    "object".into()
                } else {
                    p.name.clone()
                },
                at: w.to_world(g.latitude, g.longitude)?,
                scripted: p.scripted,
                mine: state.built.contains(&p.id),
            })
        })
        .collect();
    let scene = around::Scene {
        here: w.here(),
        geo,
        radius,
        places: &w.around.places,
        facts: &w.around.facts,
        prims,
        people: people_near(audience, w, radius),
    };
    let (mut text, mut data) = around::describe(&scene);
    if w.avatar.is_seated() {
        text.insert_str(0, "You are sitting down. ");
        data["seated"] = json!(true);
    }
    text.push_str(&format!(
        "\nYou are facing {}.",
        around::compass(facing_bearing(w))
    ));
    data["facing_deg"] = json!(facing_bearing(w));
    AgentReply::ok(text, data)
}

/// The avatar's facing as a compass bearing.
pub(super) fn facing_bearing(w: &AgentWorld) -> f32 {
    // `facing_yaw` is `atan2(x, z)` of the direction faced (`avatar::yaw_from_dir`),
    // and the world's +z is south.
    let yaw = w.avatar.facing_yaw;
    around::bearing_deg(yaw.sin(), yaw.cos())
}

/// A place by name or by coordinates, as the walking and teleporting tools
/// take it.
pub(super) fn resolve_place(
    state: &AgentState,
    lat: Option<f64>,
    lng: Option<f64>,
    place: Option<&str>,
) -> Result<(f64, f64, Option<String>), String> {
    match (lat, lng, place) {
        (Some(lat), Some(lng), None) => {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
                return Err(format!("{lat}, {lng} is not a place on Earth."));
            }
            Ok((lat, lng, None))
        }
        (None, None, Some(name)) => around::find_saved(&state.places, name)
            .map(|p| (p.lat, p.lng, Some(p.name.clone())))
            .ok_or_else(|| {
                let known: Vec<&str> = state.places.iter().map(|p| p.name.as_str()).collect();
                format!(
                    "You have no saved place called \"{name}\"{}. `find_place` looks a place \
                     up by name; `save_place` keeps one.",
                    if known.is_empty() {
                        String::new()
                    } else {
                        format!(" (you have: {})", known.join(", "))
                    }
                )
            }),
        _ => Err("Give `lat` and `lng`, or `place` — one or the other.".into()),
    }
}

pub(super) fn save_place(
    state: &mut AgentState,
    w: &AgentWorld,
    args: SavePlaceArgs,
) -> AgentReply {
    if state.keeps_key.is_none() {
        return AgentReply::refuse(NOT_ONLINE);
    }
    let name = args.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return AgentReply::refuse("Give the place a name of at most 80 characters.");
    }
    let (lat, lng) = match (args.lat, args.lng) {
        (Some(lat), Some(lng)) => (lat, lng),
        (None, None) => match w.avatar_geo() {
            Some(at) => at,
            None => return AgentReply::refuse("There is no avatar standing anywhere to save."),
        },
        _ => return AgentReply::refuse("Give both `lat` and `lng`, or neither."),
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
        return AgentReply::refuse(format!("{lat}, {lng} is not a place on Earth."));
    }
    let place = SavedPlace {
        name: name.clone(),
        lat,
        lng,
        note: args.note.filter(|n| !n.trim().is_empty()),
    };
    let replaced = match state
        .places
        .iter_mut()
        .find(|p| p.name.eq_ignore_ascii_case(&name))
    {
        Some(p) => {
            *p = place;
            true
        }
        None => {
            if state.places.len() >= around::MAX_PLACES {
                return AgentReply::refuse(format!(
                    "You already keep {} places; forget one first.",
                    around::MAX_PLACES
                ));
            }
            state.places.push(place);
            false
        }
    };
    save_places(state);
    AgentReply::ok(
        format!(
            "{} \"{name}\" at {lat:.6}, {lng:.6}. `walk_to` and `teleport` take it by name.",
            if replaced { "Moved" } else { "Saved" }
        ),
        json!({ "name": name, "lat": lat, "lng": lng }),
    )
}

pub(super) fn my_places(state: &AgentState, w: &AgentWorld) -> AgentReply {
    if state.places.is_empty() {
        return AgentReply::ok(
            "You have no saved places yet; `save_place` keeps one.",
            json!({ "places": [] }),
        );
    }
    let here = w.avatar_geo();
    let mut rows: Vec<(Option<f32>, &SavedPlace)> = state
        .places
        .iter()
        .map(|p| {
            (
                here.map(|h| around::geo_step(h, (p.lat, p.lng)).length()),
                p,
            )
        })
        .collect();
    rows.sort_by(|a, b| a.0.unwrap_or(f32::MAX).total_cmp(&b.0.unwrap_or(f32::MAX)));
    let lines: Vec<String> = rows
        .iter()
        .map(|(d, p)| {
            let mut s = p.name.clone();
            if let (Some(d), Some(h)) = (d, here) {
                let step = around::geo_step(h, (p.lat, p.lng));
                if *d < 3.0 {
                    s.push_str(" — here");
                } else {
                    s.push_str(&format!(
                        " — {} {}",
                        distance_words(*d),
                        around::compass(around::bearing_deg(step.x, step.y))
                    ));
                }
            }
            if let Some(n) = &p.note {
                s.push_str(&format!(" ({n})"));
            }
            s
        })
        .collect();
    AgentReply::ok(
        lines.join("\n"),
        json!({ "places": rows.iter().map(|(d, p)| json!({
            "name": p.name, "lat": p.lat, "lng": p.lng, "note": p.note, "distance_m": d,
        })).collect::<Vec<_>>() }),
    )
}

/// The geocoder's hits, with how far each is from the avatar.
pub(super) fn geocode_answer(
    w: &AgentWorld,
    hits: &[crate::systems::geo_nav::GeoSuggestion],
    limit: usize,
) -> AgentReply {
    let here = w.avatar_geo();
    let rows: Vec<(String, Value)> = hits
        .iter()
        .take(limit)
        .map(|h| {
            let mut line = h.name.clone();
            if let Some(k) = &h.kind {
                line.push_str(&format!(" ({k})"));
            }
            if !h.context.is_empty() {
                line.push_str(&format!(", {}", h.context));
            }
            line.push_str(&format!(" — {:.6}, {:.6}", h.lat, h.lng));
            let away = here.map(|at| around::geo_step(at, (h.lat, h.lng)));
            if let Some(step) = away {
                line.push_str(&format!(
                    ", {} {} of you",
                    distance_words(step.length()),
                    around::compass(around::bearing_deg(step.x, step.y))
                ));
            }
            (
                line,
                json!({
                    "name": h.name, "kind": h.kind, "context": h.context,
                    "lat": h.lat, "lng": h.lng,
                    "distance_m": away.map(|s| s.length()),
                }),
            )
        })
        .collect();
    let mut text = rows
        .iter()
        .map(|(l, _)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str(
        "\n`walk_to` goes there (up to 2 km), `teleport` anywhere; `save_place` keeps one by name.",
    );
    AgentReply::ok(
        text,
        json!({ "results": rows.into_iter().map(|(_, v)| v).collect::<Vec<_>>() }),
    )
}

/// What a `wait` woke for, if anything has happened since it began.
pub(super) fn what_happened(
    state: &AgentState,
    audience: &Audience,
    w: &AgentWorld,
    events: &[Event],
    base: &WaitBase,
    near_m: f32,
) -> Option<(String, Value)> {
    let mut said: Vec<String> = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();
    for e in events {
        match e {
            Event::Chat if anything_to_hear(audience, w, state.cursor) => {
                said.push("Somebody said something: `read_chat` has it.".into());
                kinds.push("chat");
            }
            Event::Arrival => {
                let names = public_names(w);
                let arrived: Vec<String> = near_ids(w, near_m)
                    .into_iter()
                    .filter(|id| !base.near.contains(id))
                    .map(|id| {
                        names
                            .iter()
                            .find(|(i, _)| *i == id)
                            .map(|(_, n)| n.clone())
                            .filter(|n| audience.hears(n))
                            .unwrap_or_else(|| "someone".into())
                    })
                    .collect();
                if !arrived.is_empty() {
                    said.push(format!("{} came within {near_m:.0} m.", arrived.join(", ")));
                    kinds.push("arrival");
                }
            }
            Event::Call => {
                let me = w.identity.username.clone().unwrap_or_default();
                if let Some(r) = w.calls.ringing(&me).into_iter().next() {
                    said.push(format!(
                        "A call is ringing from {}: `answer_call` or `decline_call`.",
                        r.who.join(", ")
                    ));
                    kinds.push("call");
                }
            }
            Event::Contact => {
                let new: Vec<&str> = w
                    .contacts
                    .incoming
                    .iter()
                    .map(|r| r.username.as_str())
                    .filter(|n| !base.incoming.iter().any(|b| b == n))
                    .collect();
                if !new.is_empty() {
                    said.push(format!(
                        "{} asked to be your contact: `answer_contact`.",
                        new.join(", ")
                    ));
                    kinds.push("contact");
                }
            }
            Event::Script if own_script_lines(state, w) > base.script_lines => {
                said.push("One of your objects printed something: `script_output` has it.".into());
                kinds.push("script");
            }
            Event::Location => {
                let (lines, _) = shared_locations(audience, w);
                let new: Vec<String> = lines
                    .into_iter()
                    .filter(|l| {
                        !base
                            .sharing
                            .iter()
                            .any(|b| l.starts_with(&format!("{b} is sharing")))
                    })
                    .collect();
                if !new.is_empty() {
                    said.push(new.join(" "));
                    kinds.push("location");
                }
            }
            Event::Walk if base.walking && state.walk.is_none() => {
                said.push(format!(
                    "Your walk ended: {}.",
                    state.walk_ended.as_deref().unwrap_or("stopped")
                ));
                kinds.push("walk");
            }
            Event::Object => {
                let mut seen = Vec::new();
                for (id, was) in &base.objects {
                    let Some(was) = was else { continue };
                    match object_look(w, *id) {
                        None => seen.push(format!(
                            "object {id} is gone — removed, or no longer in this client's view"
                        )),
                        Some(now) => {
                            let what = was.changes(&now);
                            if !what.is_empty() {
                                seen.push(format!("object {id} {}", what.join(", ")));
                            }
                        }
                    }
                }
                if !seen.is_empty() {
                    said.push(capitalise(&seen.join("; ")) + ".");
                    kinds.push("object");
                }
            }
            _ => {}
        }
    }
    (!said.is_empty()).then(|| (said.join(" "), json!({ "happened": kinds })))
}

/// Who is sharing their real location with this account right now — a live
/// share in a private chat — in words and as data. Only people the agent may
/// hear, like everything else it reads.
pub(super) fn shared_locations(audience: &Audience, w: &AgentWorld) -> (Vec<String>, Vec<Value>) {
    let now_ms = (around::now_unix() * 1000.0) as u64;
    let here = w.avatar_geo();
    w.chat
        .locations
        .iter()
        .filter(|l| l.until_ms > now_ms && audience.hears(&l.from))
        .map(|l| {
            let mins = (l.until_ms - now_ms) / 60_000 + 1;
            let mut s = format!(
                "{} is sharing their real location with you: {:.6}, {:.6} (±{:.0} m), for \
                 {mins} more min",
                l.from, l.latitude, l.longitude, l.accuracy_m
            );
            let away = here.map(|h| around::geo_step(h, (l.latitude, l.longitude)));
            if let Some(step) = away {
                s.push_str(&format!(
                    "; {} {} of you",
                    distance_words(step.length()),
                    around::compass(around::bearing_deg(step.x, step.y))
                ));
            }
            s.push('.');
            (
                s,
                json!({
                    "from": l.from, "lat": l.latitude, "lng": l.longitude,
                    "accuracy_m": l.accuracy_m, "until_ms": l.until_ms,
                    "distance_m": away.map(|s| s.length()),
                }),
            )
        })
        .unzip()
}

/// How many lines in a chat are pictures.
pub(super) fn picture_lines(w: &AgentWorld, key: &str) -> usize {
    w.chat
        .channels
        .iter()
        .find(|c| c.key == key)
        .map_or(0, |c| {
            c.lines.iter().filter(|l| l.picture.is_some()).count()
        })
}

/// `"350 m"` or `"4.2 km"`.
pub(super) fn distance_words(m: f32) -> String {
    if m < 1000.0 {
        format!("{m:.0} m")
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

/// The avatars within `radius`, by id.
pub(super) fn near_ids(w: &AgentWorld, radius: f32) -> Vec<u64> {
    let here = w.here();
    w.remotes
        .iter()
        .filter(|r| {
            let p = r.net_position + r.net_velocity * r.net_age;
            Vec2::new(p.x, p.z).distance(here) <= radius
        })
        .map(|r| r.sim_id)
        .collect()
}

/// How many console lines the agent's own objects have printed.
pub(super) fn own_script_lines(state: &AgentState, w: &AgentWorld) -> usize {
    w.script
        .console
        .iter()
        .filter(|l| state.built.contains(&l.prim_id))
        .count()
}

pub(super) fn follow(
    state: &mut AgentState,
    audience: &Audience,
    w: &mut AgentWorld,
    args: &FollowArgs,
    now: f64,
) -> AgentReply {
    let answer = walk(
        state,
        audience,
        w,
        &WalkArgs {
            lat: None,
            lng: None,
            place: None,
            person: Some(args.person.clone()),
            pace: None,
            wait_s: None,
        },
        now,
    );
    if answer.error {
        return answer;
    }
    let gap = args.distance_m.unwrap_or(FOLLOW_GAP_M).clamp(1.0, 30.0);
    if let Some(walk) = state.walk.as_mut() {
        walk.follow = Some(gap);
    }
    AgentReply::ok(
        format!(
            "Following {} at about {gap:.0} m, walking or running to keep up, until `stop` \
             or they go out of sight.",
            args.person
        ),
        json!({ "following": args.person, "gap_m": gap }),
    )
}

/// Start a walk to a thing near by, which does `then` on arrival — or do it now
/// if the avatar is already within reach.
pub(super) fn go_and(
    state: &mut AgentState,
    w: &mut AgentWorld,
    at: Vec2,
    then: Then,
    now: f64,
) -> bool {
    if w.here().distance(at) <= INTERACT_RADIUS {
        return false;
    }
    let Some(anchor) = w.anchor.geo else {
        return false;
    };
    let (lat, lng) = crate::grid::world_to_lat_lng(anchor, f64::from(at.x), f64::from(at.y));
    let here = w.here();
    state.walk = Some(Walk {
        goal: Goal::Place { lat, lng },
        gait: Gait::Walk,
        // A thing within forty metres is walked at directly: a route along the
        // street would bend away to the nearest path and back.
        planning: None,
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
        then: Some(then),
        // A thing near by is walked to where it is: a bench or a prim stood
        // inside a building is meant to be reached in there.
        from: None,
        checked: true,
        arrive_as: None,
    });
    state.walk_ended = None;
    w.avatar.stand_up();
    w.camera.mode = CameraMode::Avatar;
    true
}

/// Do a walk's `then`, now the avatar is there.
pub(super) fn arrive(then: &Then, w: &mut AgentWorld) -> String {
    match then {
        Then::Sit(thing) => sit_on(thing, w),
        Then::Use(target) => use_target(target, w),
    }
}

/// Sit on a seat that is within reach.
pub(super) fn sit_on(thing: &Interactable, w: &mut AgentWorld) -> String {
    let from = w.here();
    let Some(seat) = thing.seat_spot(from) else {
        return format!("{} is not something to sit on", thing.name.to_lowercase());
    };
    // Sitting lives in Follow: leaving it stands the avatar up
    // (`avatar::handle_avatar_movement`).
    w.camera.mode = CameraMode::Avatar;
    w.avatar.seated = Some(crate::resources::Seated {
        offset: seat.pos - from,
        yaw: seat.yaw,
        blend: 0.0,
        rising: false,
        lift: 0.0,
        dangle: false,
        edge: false,
    });
    format!(
        "sat down on the {}; `stand` gets up, and so does walking anywhere",
        thing.name.to_lowercase()
    )
}

/// Use something within reach, as the E key does.
pub(super) fn use_target(target: &Target, w: &mut AgentWorld) -> String {
    match target {
        Target::Thing(i) if i.seat_spot(w.here()).is_some() => sit_on(i, w),
        Target::Thing(i) => {
            w.avatar.pending_emote = Some(Emote::Interact);
            match i.kind.detail() {
                Some(d) => format!("{} {} ({d})", i.kind.verb().to_lowercase(), i.name),
                None => format!(
                    "{} the {}",
                    i.kind.verb().to_lowercase(),
                    i.name.to_lowercase()
                ),
            }
        }
        Target::Prim { id, name, .. } => {
            w.avatar.pending_emote = Some(Emote::Interact);
            if let Some(online) = &w.online {
                online.send(
                    "agent touch",
                    NetMessage::TouchPrim(msg::TouchPrim {
                        request_id: 0,
                        prim_id: *id,
                    }),
                );
            }
            format!(
                "pressed \"{name}\" (#{id}); its script hears it — `read_chat` or \
                 `script_output` may show what it did"
            )
        }
    }
}

/// The nearest thing matching `name` within [`REACH_FOR_M`]: places and props,
/// and — unless only seats will do — scripted prims.
pub(super) fn find_near(
    w: &AgentWorld,
    name: Option<&str>,
    seats_only: bool,
) -> Option<(Target, f32)> {
    let here = w.here();
    let wanted = name.map(|n| n.trim().to_lowercase());
    let matches = |s: &str, detail: Option<&str>| {
        wanted.as_deref().is_none_or(|n| {
            s.to_lowercase().contains(n) || detail.is_some_and(|d| d.to_lowercase().contains(n))
        })
    };
    let mut best: Option<(Target, f32)> = None;
    for (i, d) in w.around.places.within(here, REACH_FOR_M) {
        if seats_only && i.seat_spot(here).is_none() {
            continue;
        }
        if !matches(&i.name, i.kind.detail()) {
            continue;
        }
        if best.as_ref().is_none_or(|(_, b)| d < *b) {
            best = Some((Target::Thing(i.clone()), d));
        }
    }
    if !seats_only {
        for (p, g) in w.prims.iter() {
            let Some(g) = g else { continue };
            if !p.scripted || !matches(&p.name, None) {
                continue;
            }
            let Some(at) = w.to_world(g.latitude, g.longitude) else {
                continue;
            };
            let d = at.distance(here);
            if d <= REACH_FOR_M && best.as_ref().is_none_or(|(_, b)| d < *b) {
                best = Some((
                    Target::Prim {
                        id: p.id,
                        name: p.name.clone(),
                        at,
                    },
                    d,
                ));
            }
        }
    }
    best
}

pub(super) fn sit(
    state: &mut AgentState,
    w: &mut AgentWorld,
    name: Option<&str>,
    now: f64,
) -> AgentReply {
    if w.bodies.is_empty() {
        return AgentReply::refuse("There is no avatar: this client is offline.");
    }
    let Some((Target::Thing(thing), d)) = find_near(w, name, true) else {
        return AgentReply::refuse(format!(
            "There is nothing to sit on within {REACH_FOR_M:.0} m{}. `look_around` lists \
             what is near.",
            name.map_or(String::new(), |n| format!(" called \"{n}\""))
        ));
    };
    state.walk = None;
    if go_and(state, w, thing.pos, Then::Sit(thing.clone()), now) {
        return AgentReply::ok(
            format!(
                "Walking to the {} {d:.0} m away, to sit on it.",
                thing.name.to_lowercase()
            ),
            json!({ "walking_to": thing.name, "distance_m": d }),
        );
    }
    let said = sit_on(&thing, w);
    AgentReply::ok(capitalise(&said) + ".", json!({ "seated": true }))
}

pub(super) fn interact(
    state: &mut AgentState,
    w: &mut AgentWorld,
    name: Option<&str>,
    now: f64,
) -> AgentReply {
    if w.bodies.is_empty() {
        return AgentReply::refuse("There is no avatar: this client is offline.");
    }
    let Some((target, d)) = find_near(w, name, false) else {
        return AgentReply::refuse(format!(
            "There is nothing to use within {REACH_FOR_M:.0} m{}. `look_around` lists what \
             is near.",
            name.map_or(String::new(), |n| format!(" called \"{n}\""))
        ));
    };
    state.walk = None;
    if go_and(state, w, target.at(), Then::Use(target.clone()), now) {
        return AgentReply::ok(
            format!("Walking to {} {d:.0} m away, to use it.", target.name()),
            json!({ "walking_to": target.name(), "distance_m": d }),
        );
    }
    w.avatar.stand_up();
    let said = use_target(&target, w);
    AgentReply::ok(capitalise(&said) + ".", json!({ "used": target.name() }))
}

pub(super) fn face(audience: &Audience, w: &mut AgentWorld, args: &FaceArgs) -> AgentReply {
    let (yaw, what) = match face_yaw(audience, w, args) {
        Ok(aim) => aim,
        Err(refusal) => return refusal,
    };
    if w.avatar.is_seated() {
        return AgentReply::refuse(
            "You are sitting down and face out of the seat; `stand` first. (`look` can still \
             turn your head.)",
        );
    }
    w.avatar.facing_yaw = yaw;
    let bearing = facing_bearing(w);
    AgentReply::ok(
        format!("Facing {what} ({}).", around::compass(bearing)),
        json!({ "facing_deg": bearing }),
    )
}

/// Which way to turn to face what `args` names, as an avatar yaw, and what to
/// call it.
pub(super) fn face_yaw(
    audience: &Audience,
    w: &AgentWorld,
    args: &FaceArgs,
) -> Result<(f32, String), AgentReply> {
    if w.bodies.is_empty() {
        return Err(AgentReply::refuse(
            "There is no avatar: this client is offline.",
        ));
    }
    let here = w.here();
    let (toward, what) = if let Some(b) = args.bearing_deg {
        let r = b.to_radians();
        (
            Some(here + Vec2::new(r.sin(), -r.cos())),
            format!("{:.0}°", b.rem_euclid(360.0)),
        )
    } else if let Some(name) = &args.person {
        if !audience.hears(name) {
            return Err(AgentReply::refuse(format!(
                "{name} is not somebody you are allowed to hear."
            )));
        }
        let at = w
            .avatar_of(name)
            .and_then(|id| w.remote_position(id))
            .map(|p| Vec2::new(p.x, p.z));
        (at, name.clone())
    } else if let Some(id) = args.object {
        let at = w
            .prims
            .iter()
            .find(|(p, _)| p.id == id)
            .and_then(|(_, g)| g.and_then(|g| w.to_world(g.latitude, g.longitude)));
        (at, format!("object {id}"))
    } else if let (Some(lat), Some(lng)) = (args.lat, args.lng) {
        (w.to_world(lat, lng), format!("{lat:.5}, {lng:.5}"))
    } else {
        return Err(AgentReply::refuse(
            "Say what to face: `person`, `object`, `lat`+`lng` or `bearing_deg`.",
        ));
    };
    let Some(toward) = toward else {
        return Err(AgentReply::refuse(format!(
            "Cannot see where {what} is from here (a person shows only after saying something \
             in public chat)."
        )));
    };
    let dir = toward - here;
    // Standing on it already: keep facing the way it faces.
    let yaw = crate::systems::avatar::yaw_from_dir(Vec3::new(dir.x, 0.0, dir.y))
        .unwrap_or(w.avatar.facing_yaw);
    Ok((yaw, what))
}
