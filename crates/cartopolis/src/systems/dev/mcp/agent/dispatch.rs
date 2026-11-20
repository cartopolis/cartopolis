use super::*;
use cartopolis_core::msg;

/// Answer one request, or park it: `None` means the reply was kept and will be
/// sent by [`settle`].
#[allow(clippy::too_many_lines)]
pub(super) fn handle(
    request: AgentRequest,
    state: &mut AgentState,
    audience: &Audience,
    w: &mut AgentWorld,
    now: f64,
    reply: oneshot::Sender<Reply>,
) -> Option<(oneshot::Sender<Reply>, AgentReply)> {
    let answer = match request {
        AgentRequest::Me => me(state, audience, w),
        AgentRequest::ReadChat(args) => {
            let after = args.after.unwrap_or(state.cursor);
            let wait = f64::from(args.wait_s.unwrap_or(0).min(MAX_CHAT_WAIT_S));
            if wait > 0.0 && !anything_to_hear(audience, w, after) {
                // Park it: the answer is the next line worth reading, or the
                // timeout. A long poll rather than a loop of short ones, so a
                // model sitting in a conversation spends nothing while it waits.
                state.pending.push(Pending::Chat {
                    args: ReadChatArgs {
                        after: Some(after),
                        ..args
                    },
                    until: now + wait,
                    reply,
                });
                return None;
            }
            read_chat(state, audience, w, &args)
        }
        AgentRequest::Say(args) => {
            return say(state, audience, w, args, now, reply);
        }
        AgentRequest::Walk(args) => {
            let answer = walk(state, audience, w, &args, now);
            let wait = f64::from(args.wait_s.unwrap_or(0).min(MAX_CHAT_WAIT_S));
            if answer.error || wait <= 0.0 {
                answer
            } else {
                // Hold the answer until the walk has ended: an agent that only
                // hears "walking" polls `me` every second to find out when,
                // which the first bot did thirty times on one walk.
                state.pending.push(Pending::Walking {
                    started: now,
                    until: now + wait,
                    reply,
                });
                return None;
            }
        }
        AgentRequest::Stop => {
            let was = state.walk.take().is_some();
            state.walk_ended = was.then(|| "stopped when asked".to_string());
            AgentReply::ok(
                if was {
                    "Stopped."
                } else {
                    "Not walking anywhere."
                },
                json!({ "walking": false }),
            )
        }
        // The person paused the agent from the design room: building waits for
        // them. Looking, reading and talking do not.
        AgentRequest::Build(_)
        | AgentRequest::Edit(_)
        | AgentRequest::Delete(_)
        | AgentRequest::SetScript(_)
        | AgentRequest::Group(_)
        | AgentRequest::Ungroup(_)
        | AgentRequest::DesignRoom(_)
            if w.around.room.ai_paused =>
        {
            AgentReply::refuse(
                "The person paused you from the design room. Wait for them, or ask in \
                 chat what they want changed.",
            )
        }
        AgentRequest::DesignRoom(args) => return design_room(state, w, args, now, reply),
        AgentRequest::Group(args) => {
            let body = serde_json::json!({ "name": args.name, "prim_ids": args.ids }).to_string();
            let done = format!("Grouped as \u{201c}{}\u{201d}.", args.name);
            let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                let answer =
                    crate::systems::model_assets::api_post("/api/inventory/groups", Some(body))
                        .await;
                HttpDone::Room(answer, done)
            });
            state.pending.push(Pending::Http {
                label: "group",
                about: args.name,
                task,
                until: now + HTTP_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::Ungroup(args) => {
            let path = format!("/api/inventory/groups/{}", args.group_id);
            let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                let answer = crate::systems::model_assets::api_delete(&path)
                    .await
                    .map(|()| String::new());
                HttpDone::Room(answer, "Ungrouped.".to_string())
            });
            state.pending.push(Pending::Http {
                label: "ungroup",
                about: format!("Group {}", args.group_id),
                task,
                until: now + HTTP_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::Teleport(args) => teleport(state, w, &args),
        AgentRequest::Emote(args) => emote(w, &args),
        AgentRequest::Build(args)
            if audience.private_only
                && args.prims.iter().any(|p| {
                    p["script"]
                        .as_str()
                        .is_some_and(|s| public_speech_in(s).is_some())
                }) =>
        {
            let word = args
                .prims
                .iter()
                .find_map(|p| p["script"].as_str().and_then(public_speech_in))
                .unwrap_or_default();
            speech_refusal(&word)
        }
        AgentRequest::SetScript(args)
            if audience.private_only && public_speech_in(&args.source).is_some() =>
        {
            speech_refusal(&public_speech_in(&args.source).unwrap_or_default())
        }
        AgentRequest::Build(args) => match start_build(w, args) {
            Ok(items) => {
                let before = w.prims.iter().map(|(p, _)| p.id).collect();
                state.pending.push(Pending::Build {
                    items,
                    before,
                    restoring: None,
                    until: now + BUILD_WAIT_S,
                    reply,
                });
                return None;
            }
            Err(why) => AgentReply::refuse(why),
        },
        AgentRequest::SetScript(args) => {
            let Some(online) = &w.online else {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            };
            let seen = w
                .script
                .console
                .iter()
                .filter(|l| l.prim_id == args.id)
                .count();
            online.send(
                "agent script",
                NetMessage::SetPrimScript(msg::SetPrimScript {
                    request_id: 0,
                    prim_id: args.id,
                    source: args.source,
                }),
            );
            state.pending.push(Pending::Script {
                id: args.id,
                seen,
                until: now + SCRIPT_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::PlayVideo(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let upload = args.file.map(|path| {
                let slot = VideoUpload::default();
                let out = slot.clone();
                bevy::tasks::IoTaskPool::get()
                    .spawn(async move {
                        let got = match std::fs::read(&path) {
                            Ok(bytes) => crate::systems::model_assets::api_upload_video(bytes)
                                .await
                                .and_then(|body| {
                                    serde_json::from_str::<Value>(&body)
                                        .ok()
                                        .and_then(|j| j["hash"].as_str().map(str::to_string))
                                        .ok_or(body)
                                }),
                            Err(e) => Err(format!("could not read {path}: {e}")),
                        };
                        if let Ok(mut s) = out.lock() {
                            *s = Some(got);
                        }
                    })
                    .detach();
                slot
            });
            let looped = args.looped.unwrap_or(true);
            let upload_started = upload.is_some();
            if upload.is_none() {
                send_video(w, args.id, args.asset.clone(), looped);
            }
            state.pending.push(Pending::Video {
                id: args.id,
                upload,
                asset: args.asset,
                looped,
                until: now
                    + if upload_started {
                        VIDEO_WAIT_S
                    } else {
                        BUILD_WAIT_S
                    },
                reply,
            });
            return None;
        }
        AgentRequest::ReplaceBuilding(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            if !valid_pand(&args.pand) {
                return Some((reply, AgentReply::refuse(PAND_FORMAT)));
            }
            use base64::Engine as _;
            let glb = match base64::engine::general_purpose::STANDARD.decode(args.glb_base64.trim())
            {
                Ok(b) if b.starts_with(b"glTF") => b,
                Ok(_) => {
                    return Some((
                        reply,
                        AgentReply::refuse("That is not a GLB (binary glTF): it lacks the header."),
                    ))
                }
                Err(e) => {
                    return Some((
                        reply,
                        AgentReply::refuse(format!("glb_base64 is not base64: {e}")),
                    ))
                }
            };
            // The API's `Edit`: absent fields keep what the record holds.
            let mut placement = json!({
                "heading_deg": args.heading_deg.unwrap_or(0.0),
                "scale": 1.0,
                "base_m": 0.0,
            });
            if let (Some(lat), Some(lng)) = (args.lat, args.lng) {
                placement["lat"] = json!(lat);
                placement["lng"] = json!(lng);
            }
            let mut edit = json!({
                "placement": placement,
                "collision": args.collision.as_deref().unwrap_or("walls"),
                "interior": args.interior.as_deref().unwrap_or("none"),
            });
            if let Some(name) = &args.name {
                edit["name"] = json!(name);
            }
            let slot = VideoUpload::default();
            let out = slot.clone();
            let pand = args.pand.clone();
            let bytes = glb.len();
            bevy::tasks::IoTaskPool::get()
                .spawn(async move {
                    let got = crate::systems::model_assets::api_replace_building(
                        &pand,
                        glb,
                        edit.to_string(),
                    )
                    .await;
                    if let Ok(mut s) = out.lock() {
                        *s = Some(got);
                    }
                })
                .detach();
            state.pending.push(Pending::Building {
                pand: args.pand,
                restore: false,
                bytes,
                slot,
                until: now + BUILDING_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::RestoreBuilding(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            if !valid_pand(&args.pand) {
                return Some((reply, AgentReply::refuse(PAND_FORMAT)));
            }
            let slot = VideoUpload::default();
            let out = slot.clone();
            let pand = args.pand.clone();
            bevy::tasks::IoTaskPool::get()
                .spawn(async move {
                    let got = crate::systems::model_assets::api_restore_building(&pand)
                        .await
                        .map(|()| String::new());
                    if let Ok(mut s) = out.lock() {
                        *s = Some(got);
                    }
                })
                .detach();
            state.pending.push(Pending::Building {
                pand: args.pand,
                restore: true,
                bytes: 0,
                slot,
                until: now + BUILDING_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::Delete(args) => match &w.online {
            None => AgentReply::refuse(NOT_ONLINE),
            Some(online) => {
                // Copied before they go, for `undo`: the server keeps nothing
                // of a deleted object. One not in view cannot be copied.
                let mut made = Vec::new();
                let mut missing = Vec::new();
                for id in &args.ids {
                    match prim_messages(w, *id) {
                        Some((_, create, item)) => made.push((*id, create, item)),
                        None => missing.push(*id),
                    }
                }
                push_undo(state, Undo::Delete { made, missing });
                for id in &args.ids {
                    online.send(
                        "agent delete",
                        NetMessage::DeletePrim(msg::DeletePrim {
                            request_id: 0,
                            prim_id: *id,
                        }),
                    );
                }
                state.built.retain(|id| !args.ids.contains(id));
                save_builds(state);
                AgentReply::ok(
                    format!(
                        "Asked the server to delete {} object(s). It refuses any that are not \
                         yours; `my_builds` shows what is still standing.",
                        args.ids.len()
                    ),
                    json!({ "deleted": args.ids }),
                )
            }
        },
        AgentRequest::Touch(args) => match &w.online {
            None => AgentReply::refuse(NOT_ONLINE),
            Some(online) => {
                online.send(
                    "agent touch",
                    NetMessage::TouchPrim(msg::TouchPrim {
                        request_id: 0,
                        prim_id: args.id,
                    }),
                );
                AgentReply::ok(
                    format!("Touched object {}.", args.id),
                    json!({ "touched": args.id }),
                )
            }
        },
        AgentRequest::ScriptOutput(args) => script_output(state, w, &args),
        AgentRequest::MyBuilds => my_builds(state, w),
        AgentRequest::SignOut => {
            if !w.session.is_online() && !w.session.status.is_busy() {
                AgentReply::ok("Already signed out.", json!({ "online": false }))
            } else {
                state.walk = None;
                w.session
                    .request(crate::systems::session::SessionRequest::Offline);
                AgentReply::ok(
                    "Signed out: the avatar has left the world and nobody can see or \
                     message it. This client keeps running, and `sign_in` brings it back.",
                    json!({ "online": false }),
                )
            }
        }
        AgentRequest::Call(args) => return call(state, audience, w, args, now, reply),
        AgentRequest::AnswerCall => answer_call(audience, w),
        AgentRequest::DeclineCall => {
            let me = w.identity.username.clone().unwrap_or_default();
            match w.calls.ringing(&me).into_iter().next() {
                Some(r) => {
                    w.calls.request = Some(crate::systems::calls::CallRequest::Dismiss(r.group));
                    AgentReply::ok(
                        format!(
                            "Declined the call from {}. They are told.",
                            r.who.join(", ")
                        ),
                        json!({ "declined": r.who }),
                    )
                }
                None => AgentReply::refuse("Nothing is ringing."),
            }
        }
        AgentRequest::HangUp => {
            if w.calls.active_group().is_some() {
                w.calls.request = Some(crate::systems::calls::CallRequest::Leave);
                AgentReply::ok("Hung up.", json!({ "in_call": false }))
            } else {
                AgentReply::ok("Not in a call.", json!({ "in_call": false }))
            }
        }
        AgentRequest::CallStatus => call_status(w),
        AgentRequest::VoiceCall(args) => voice_call(audience, w, &args),
        AgentRequest::RequestContact(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let who = args.username.trim().to_string();
            if w.contacts.is_contact(&who) {
                AgentReply::ok(
                    format!("You and {who} are already contacts."),
                    json!({ "contact": who }),
                )
            } else {
                w.contacts.error = None;
                w.contacts
                    .say_hi(&who, args.message.as_deref().unwrap_or_default(), None);
                state.pending.push(Pending::Contact {
                    to: who,
                    until: now + CONTACT_WAIT_S,
                    reply,
                });
                return None;
            }
        }
        AgentRequest::AnswerContact(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let who = args.username.trim().to_string();
            if w.contacts.incoming_from(&who).is_none() {
                AgentReply::refuse(format!(
                    "There is no request from {who} waiting. `contacts` lists the ones there are."
                ))
            } else {
                w.contacts.answer(&who, args.accept);
                AgentReply::ok(
                    if args.accept {
                        format!("Accepted {who}'s request: you are contacts now.")
                    } else {
                        format!("Declined {who}'s request. They are not told.")
                    },
                    json!({ "username": who, "accepted": args.accept }),
                )
            }
        }
        AgentRequest::Contacts => contacts_list(w),
        AgentRequest::LookAround(args) => look_around(state, audience, w, &args),
        AgentRequest::FindPlace(args) => {
            let query = args.query.trim().to_string();
            if query.is_empty() {
                return Some((reply, AgentReply::refuse("Say what to look for.")));
            }
            let limit = args.limit.unwrap_or(5).clamp(1, 10);
            let slot = GeocodeSlot::default();
            let out = slot.clone();
            // Biased toward where the avatar is, not bounded to it: "the station"
            // should mean the nearby one, and "Amsterdam" should still work.
            let near = w.avatar_geo().map_or(String::new(), |(lat, lng)| {
                format!(
                    "&viewbox={},{},{},{}&bounded=0",
                    lng - 0.3,
                    lat + 0.2,
                    lng + 0.3,
                    lat - 0.2
                )
            });
            let url = format!(
                "{}/search?format=jsonv2&limit={limit}&q={}{near}",
                crate::systems::geo_nav::nominatim_base_url(),
                crate::systems::geo_nav::percent_encode(&query),
            );
            bevy::tasks::IoTaskPool::get()
                .spawn(async move {
                    let got = crate::systems::geo_nav::geocode_fetch(url).await;
                    if let Ok(mut s) = out.lock() {
                        *s = Some(got);
                    }
                })
                .detach();
            state.pending.push(Pending::Geocode {
                query,
                limit,
                slot,
                until: now + GEOCODE_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::SavePlace(args) => save_place(state, w, args),
        AgentRequest::MyPlaces => my_places(state, w),
        AgentRequest::ForgetPlace(args) => {
            let before = state.places.len();
            state
                .places
                .retain(|p| !p.name.eq_ignore_ascii_case(args.name.trim()));
            if state.places.len() == before {
                AgentReply::refuse(format!("No saved place is called \"{}\".", args.name))
            } else {
                save_places(state);
                AgentReply::ok(
                    format!("Forgot \"{}\".", args.name.trim()),
                    json!({ "forgot": args.name.trim() }),
                )
            }
        }
        AgentRequest::Wait(args) => {
            let mut events = Vec::new();
            for e in args.events.unwrap_or_default() {
                match Event::parse(&e) {
                    Some(e) if !events.contains(&e) => events.push(e),
                    Some(_) => {}
                    None => {
                        return Some((
                            reply,
                            AgentReply::refuse(format!(
                                "`{e}` is not something to wait for; use chat, arrival, call, \
                                 contact, script, walk, location or object"
                            )),
                        ))
                    }
                }
            }
            let ids = args.ids.unwrap_or_default();
            if events.is_empty() {
                events = Event::ALL.to_vec();
                if !ids.is_empty() {
                    events.push(Event::Object);
                }
            }
            if events.contains(&Event::Object) && ids.is_empty() {
                return Some((
                    reply,
                    AgentReply::refuse("Waiting for an object needs its id in `ids`."),
                ));
            }
            let objects: Vec<(i64, Option<ObjectLook>)> =
                ids.iter().map(|id| (*id, object_look(w, *id))).collect();
            if events.contains(&Event::Object) && objects.iter().all(|(_, o)| o.is_none()) {
                return Some((
                    reply,
                    AgentReply::refuse(
                        "None of those objects is in view, so there is nothing to watch. \
                         `look_around` lists the objects near you.",
                    ),
                ));
            }
            let near_m = args.near_m.unwrap_or(ARRIVAL_M).clamp(2.0, 300.0);
            let base = WaitBase {
                objects,
                near: near_ids(w, near_m),
                incoming: w
                    .contacts
                    .incoming
                    .iter()
                    .map(|r| r.username.clone())
                    .collect(),
                script_lines: own_script_lines(state, w),
                walking: state.walk.is_some(),
                sharing: w.chat.locations.iter().map(|l| l.from.clone()).collect(),
            };
            // Already-unread chat answers at once, as `read_chat` would.
            let timeout = f64::from(args.timeout_s.unwrap_or(300).min(MAX_CHAT_WAIT_S));
            state.pending.push(Pending::Wait {
                events,
                base,
                near_m,
                started: now,
                until: now + timeout,
                reply,
            });
            return None;
        }
        AgentRequest::Follow(args) => follow(state, audience, w, &args, now),
        AgentRequest::Sit(args) => sit(state, w, args.name.as_deref(), now),
        AgentRequest::Stand => {
            if w.avatar.is_seated() {
                w.avatar.stand_up();
                AgentReply::ok("Stood up.", json!({ "seated": false }))
            } else {
                AgentReply::ok("Already standing.", json!({ "seated": false }))
            }
        }
        AgentRequest::Interact(args) => interact(state, w, args.name.as_deref(), now),
        AgentRequest::Face(args) => face(audience, w, &args),
        AgentRequest::PrepareLook(prep) => {
            if w.bodies.is_empty() {
                return Some((
                    reply,
                    AgentReply::refuse(
                        "There is no avatar to look through: this client is offline.",
                    ),
                ));
            }
            let mut said = None;
            // Where the eyes look: along the body, or — sitting, when the body
            // faces out of the seat — a turn of the head alone.
            let mut look_yaw = w.avatar.facing_yaw;
            if let Some(f) = &prep.face {
                match face_yaw(audience, w, f) {
                    Err(refusal) => return Some((reply, refusal)),
                    Ok((yaw, what)) => {
                        if !w.avatar.is_seated() {
                            w.avatar.facing_yaw = yaw;
                        }
                        look_yaw = yaw;
                        said = Some(format!("Looking toward {what}."));
                    }
                }
            }
            // The camera's azimuth is measured the other way round from the
            // facing (`free_camera`'s first-person arm builds its rotation from
            // it), hence the half turn.
            w.camera.mode = CameraMode::Avatar;
            w.camera.first_person = true;
            w.camera.azimuth = look_yaw + std::f32::consts::PI;
            w.camera.pitch = (-prep.pitch_deg).to_radians();
            AgentReply::ok(
                said.unwrap_or_else(|| "Looking ahead.".into()),
                json!({ "facing_deg": facing_bearing(w) }),
            )
        }
        AgentRequest::EndLook => {
            w.camera.first_person = false;
            AgentReply::ok("", Value::Null)
        }
        AgentRequest::Edit(args) => match edit(w, &args) {
            Ok((want, before)) => {
                state.pending.push(Pending::Edit {
                    id: args.id,
                    want,
                    before,
                    until: now + EDIT_WAIT_S,
                    reply,
                });
                return None;
            }
            Err(why) => AgentReply::refuse(why),
        },
        AgentRequest::Inspect(args) => match inspect(state, w, args.id) {
            (answer, Fetch::Ask { mine }) => {
                // `PrimScript` lands only in the script editor's buffer, so the
                // idle editor is borrowed to receive it and put back afterwards
                // (`settle`). Only ever asked for a script this account may
                // write, which the server never refuses to read.
                let Some(online) = &w.online else {
                    return Some((reply, answer));
                };
                w.script.prim_id = Some(args.id);
                w.script.source.clear();
                w.script.saved.clear();
                w.script.fetched = false;
                w.script.needs_fetch = false;
                online.send(
                    "agent inspect script",
                    NetMessage::GetPrimScript(msg::GetPrimScript {
                        request_id: 0,
                        prim_id: args.id,
                    }),
                );
                state.pending.push(Pending::Inspect {
                    id: args.id,
                    mine,
                    answer,
                    until: now + INSPECT_WAIT_S,
                    reply,
                });
                return None;
            }
            (answer, Fetch::No) => answer,
        },
        AgentRequest::Remember(args) => remember(state, args),
        AgentRequest::Recall(args) => {
            let limit = args.limit.unwrap_or(30).clamp(1, 200);
            let found = around::recall(
                &state.notes,
                args.about.as_deref(),
                args.search.as_deref(),
                limit,
            );
            if found.is_empty() {
                AgentReply::ok(
                    if state.notes.is_empty() {
                        "You have no notes yet; `remember` writes one."
                    } else {
                        "No note matches."
                    },
                    json!({ "notes": [] }),
                )
            } else {
                let lines: Vec<String> = found
                    .iter()
                    .map(|n| match &n.about {
                        Some(a) => format!("#{} [{a}] {}", n.id, n.text),
                        None => format!("#{} {}", n.id, n.text),
                    })
                    .collect();
                AgentReply::ok(lines.join("\n"), json!({ "notes": found }))
            }
        }
        AgentRequest::SendPicture(send) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let to = send.to.trim().to_string();
            if !audience.hears(&to) {
                return Some((
                    reply,
                    AgentReply::refuse(format!(
                        "{to} is not on the list of people this client lets you hear, so you \
                         may not send them anything either. You can hear: {}.",
                        audience.describe()
                    )),
                ));
            }
            if w.identity
                .username
                .as_deref()
                .is_some_and(|me| me.eq_ignore_ascii_case(&to))
            {
                return Some((reply, AgentReply::refuse("That is you.")));
            }
            let channel = w
                .private
                .direct_with(&to)
                .map(|g| private_chat::channel_key(&g));
            if channel.is_none() {
                // No chat with them yet: start one, and send when it opens.
                w.chat.private_start.push(to.clone());
            }
            state.pending.push(Pending::Picture {
                to,
                channel: None,
                bytes: Some(send.bytes),
                caption: send.caption,
                before: 0,
                notice_before: w.chat.notice.as_ref().map(|n| n.text.clone()),
                until: now + PICTURE_SEND_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::SeePicture(args) => {
            let found = w.chat.channels.iter().find_map(|c| {
                c.lines
                    .iter()
                    .find(|l| l.seq == args.seq)
                    .map(|l| (c.key.clone(), l.clone()))
            });
            let Some((key, line)) = found else {
                return Some((
                    reply,
                    AgentReply::refuse(format!(
                        "There is no line {} (any more): `read_chat` lists what there is.",
                        args.seq
                    )),
                ));
            };
            let Some(picture) = line.picture.clone() else {
                return Some((
                    reply,
                    AgentReply::refuse(format!("Line {} is not a picture.", args.seq)),
                ));
            };
            // A picture is read as surely as a line is, so the same list
            // decides: one from anybody else is withheld, and so is its name.
            let own = w
                .identity
                .username
                .as_deref()
                .is_some_and(|me| line.sender.eq_ignore_ascii_case(me));
            if !own && !audible(audience, w, &key, &line) {
                return Some((
                    reply,
                    AgentReply::refuse(
                        "That picture is from somebody you are not allowed to hear, so it is \
                         withheld.",
                    ),
                ));
            }
            let what = format!(
                "{} from {}",
                line.body,
                if own { "you" } else { line.sender.as_str() }
            );
            state.pending.push(Pending::Seeing {
                picture,
                what,
                until: now + PICTURE_SEE_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::SayIn(line) => say_in(audience, w, line),
        AgentRequest::Undo => return undo(state, w, now, reply),
        AgentRequest::FindModels(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let query = args.query.unwrap_or_default().trim().to_string();
            let mut path = format!(
                "/api/library?q={}",
                crate::systems::geo_nav::percent_encode(&query)
            );
            if let Some(c) = args
                .category
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                path.push_str(&format!(
                    "&category={}",
                    crate::systems::geo_nav::percent_encode(c)
                ));
            }
            let mine = args.mine.unwrap_or(false);
            let limit = args.limit.unwrap_or(20).clamp(1, 100);
            let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                use crate::systems::model_assets::api_get;
                use crate::systems::model_library::{parse_entries, parse_items};
                let library = api_get(&path)
                    .await
                    .and_then(|body| parse_entries(&body))
                    .map(|mut found| {
                        found.truncate(limit);
                        found
                    });
                let inventory = if mine {
                    Some(
                        api_get("/api/inventory")
                            .await
                            .and_then(|body| parse_items(&body)),
                    )
                } else {
                    None
                };
                HttpDone::Models(library, inventory)
            });
            state.pending.push(Pending::Http {
                label: "find models",
                about: query,
                task,
                until: now + HTTP_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::Communities(args) => {
            return communities(audience, state, w, args, now, reply)
        }
        AgentRequest::JoinCommunity(args) => join_community(audience, w, args),
        AgentRequest::ReadPosts(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let (id, name) = match find_community(w, &args.community) {
                Ok(found) => found,
                Err(why) => return Some((reply, AgentReply::refuse(why))),
            };
            let path = match args.before {
                Some(ms) => format!("/api/communities/{id}/posts?before={ms}"),
                None => format!("/api/communities/{id}/posts"),
            };
            let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                HttpDone::Feed(json_of(crate::systems::model_assets::api_get(&path).await))
            });
            state.pending.push(Pending::Http {
                label: "read posts",
                about: name,
                task,
                until: now + HTTP_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::ReadPost(args) => {
            if w.online.is_none() {
                return Some((reply, AgentReply::refuse(NOT_ONLINE)));
            }
            let Ok(id) = uuid::Uuid::parse_str(args.id.trim()) else {
                return Some((reply, AgentReply::refuse(BAD_POST_ID)));
            };
            let task = bevy::tasks::IoTaskPool::get().spawn(async move {
                HttpDone::Thread(json_of(
                    crate::systems::model_assets::api_get(&format!("/api/posts/{id}")).await,
                ))
            });
            state.pending.push(Pending::Http {
                label: "read post",
                about: id.to_string(),
                task,
                until: now + HTTP_WAIT_S,
                reply,
            });
            return None;
        }
        AgentRequest::Calendar(args) => calendar(audience, w, &args),
        AgentRequest::AnswerMeeting(args) => answer_meeting(audience, w, &args),
        AgentRequest::Forget(args) => {
            let before = state.notes.len();
            state.notes.retain(|n| n.id != args.id);
            if state.notes.len() == before {
                AgentReply::refuse(format!("There is no note #{}.", args.id))
            } else {
                save_notes(state);
                AgentReply::ok(
                    format!("Forgot note #{}.", args.id),
                    json!({ "forgot": args.id }),
                )
            }
        }
        AgentRequest::SignIn => {
            if w.session.is_online() {
                AgentReply::ok("Already signed in.", json!({ "online": true }))
            } else if let Some(request) = state.sign_in.clone() {
                w.session.request(request);
                state.pending.push(Pending::SigningIn {
                    until: now + SIGN_IN_WAIT_S,
                    reply,
                });
                return None;
            } else {
                AgentReply::refuse(
                    "This client was not started with a server to sign in to, so there is \
                     nothing to sign back in with. Its owner signs it in from the menu.",
                )
            }
        }
    };
    Some((reply, answer))
}
