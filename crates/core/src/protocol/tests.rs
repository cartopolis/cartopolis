use glam::Vec3;
use uuid::Uuid;

use super::*;
use crate::error::ProtocolError;

/// The changelog is the source of truth for the wire version, so it has to
/// stay a gapless ascending run — a duplicated or skipped number would let
/// two different schemas answer to one version.
#[test]
fn protocol_history_is_contiguous_and_current() {
    assert!(!PROTOCOL_HISTORY.is_empty());
    for pair in PROTOCOL_HISTORY.windows(2) {
        assert_eq!(
            pair[1].version,
            pair[0].version + 1,
            "gap or duplicate between v{} and v{}",
            pair[0].version,
            pair[1].version
        );
    }
    for rev in PROTOCOL_HISTORY {
        assert!(!rev.summary.is_empty(), "v{} has no summary", rev.version);
    }
    assert_eq!(
        PROTOCOL_VERSION,
        PROTOCOL_HISTORY[PROTOCOL_HISTORY.len() - 1].version
    );
}

#[test]
fn roundtrip_hello_app_frame() {
    let m = NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Password {
            username: "dev".into(),
            password: "dev".into(),
        },
    };
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

/// The frame is the header then exactly `encode_message`'s bytes: encoding
/// straight after the header must not change a byte on the wire.
#[test]
fn an_app_frame_is_the_header_then_the_message() {
    let m = NetMessage::ClientHello {
        protocol_version: PROTOCOL_VERSION,
        auth: AuthCredential::Password {
            username: "dev".into(),
            password: "x".repeat(300),
        },
    };
    let b = encode_app_frame(&m).unwrap();
    assert_eq!(b.len(), APP_HEADER_LEN + encode_message(&m).unwrap().len());
    assert_eq!(&b[..2], &PROTOCOL_VERSION.to_le_bytes());
    assert_eq!(&b[APP_HEADER_LEN..], &encode_message(&m).unwrap()[..]);
}

#[test]
fn roundtrip_hello_ack_app_frame() {
    let m = NetMessage::ServerHelloAck {
        session_id: Uuid::nil(),
        tick_hz: 20.0,
        your_avatar_id: 7,
        principal_id: Uuid::nil(),
        username: "dev".into(),
        session_token: "deadbeef".into(),
        osm_tile_url_template: String::new(),
        api_base_url: String::new(),
        server_name: "cartopolis.org".into(),
        last_place: Some(GeoAnchor {
            latitude: 53.2194,
            longitude: 6.5665,
        }),
    };
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

#[test]
fn roundtrip_snapshot_app_frame() {
    let m = NetMessage::WorldSnapshot {
        tick: 42,
        avatars: vec![],
    };
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

fn an_avatar(id: u64) -> AvatarStateDto {
    AvatarStateDto {
        id,
        geo: GeoAnchor {
            latitude: 53.2194 + id as f64 * 1e-4,
            longitude: 6.5665 - id as f64 * 1e-4,
        },
        alt_m: id as f32 * 0.5,
        yaw: 1.25,
        emote: (id % 3 == 0).then_some(EmoteState {
            emote: Emote::Wave,
            sequence: id as u32,
        }),
        expression: None,
        seated: (id % 5 == 0).then_some([0.4, -1.0]),
        velocity: Vec3::new(1.8, 0.0, -0.5),
        holding: (id % 7 == 0).then_some(id as i64 * 300),
        driving: None,
    }
}

/// A snapshot assembled from states encoded one by one is the frame
/// `encode_app_frame` would have written for the same list — including the
/// counts whose varint takes two bytes, where a hand-rolled length would
/// first go wrong. This is what lets the sim encode each avatar once a tick
/// and still send every connection a snapshot of its own.
#[test]
fn snapshot_frame_matches_encode_app_frame() {
    for count in [0usize, 1, 2, 127, 128, 300] {
        let avatars: Vec<AvatarStateDto> = (0..count as u64).map(an_avatar).collect();
        let parts: Vec<Vec<u8>> = avatars
            .iter()
            .map(|a| encode_avatar_state(a).unwrap())
            .collect();
        let assembled = snapshot_frame(9_001, parts.iter().map(Vec::as_slice)).unwrap();
        let whole = NetMessage::WorldSnapshot {
            tick: 9_001,
            avatars,
        };
        assert_eq!(
            assembled,
            encode_app_frame(&whole).unwrap(),
            "{count} avatars: the assembled frame differs from the encoded one"
        );
        assert_eq!(decode_app_frame(&assembled).unwrap(), whole);
    }
}

/// The seat travels both ways (protocol 19), and it survives the encoding as
/// an *absent* value too — `None` is the state almost every intent and almost
/// every replicated avatar is in, so it is the one that must not cost
/// anything or go missing.
#[test]
fn roundtrip_seated_app_frames() {
    for seated in [None, Some([0.42, -1.03])] {
        let m = NetMessage::ClientIntent(msg::ClientIntent {
            request_id: 0,
            move_x: 0.0,
            move_z: 0.0,
            display_yaw: 1.25,
            flying: false,
            fly_up: false,
            fly_down: false,
            jump: false,
            emote: None,
            expression: None,
            seated,
            geo: None,
            driving: None,
        });
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);

        let m = NetMessage::WorldSnapshot {
            tick: 7,
            avatars: vec![AvatarStateDto {
                id: 3,
                geo: GeoAnchor {
                    latitude: 53.2194,
                    longitude: 6.5665,
                },
                alt_m: 0.05,
                yaw: 1.25,
                emote: None,
                expression: None,
                seated,
                velocity: Vec3::new(1.8, 0.0, -0.5),
                holding: None,
                driving: None,
            }],
        };
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
}

/// Picking up and letting go (protocol 48) travel as their own kinds, and a
/// held prim survives a snapshot.
/// Greetings (protocol 49) travel as their own kinds, a withdrawn offer
/// included, and the new emotes survive a snapshot.
#[test]
fn roundtrip_greeting_app_frames() {
    use crate::greet::Greeting;
    for m in [
        NetMessage::OfferGreeting(msg::OfferGreeting {
            request_id: 3,
            to: 7,
            greeting: Greeting::HighFive,
        }),
        NetMessage::GreetingOffered {
            from: 2,
            name: "ada".into(),
            greeting: Some(Greeting::Handshake),
        },
        NetMessage::GreetingOffered {
            from: 2,
            name: "ada".into(),
            greeting: None,
        },
        NetMessage::AcceptGreeting(msg::AcceptGreeting {
            request_id: 4,
            from: 2,
        }),
        NetMessage::GreetingStarted {
            greeting: Greeting::Handshake,
            partner: 7,
            yaw: -1.25,
        },
        NetMessage::FollowPlans(msg::FollowPlans {
            places: vec![[53.2166, 6.5672], [53.2190, 6.5650]],
        }),
        NetMessage::WorldSnapshot {
            tick: 9,
            avatars: [Emote::Handshake, Emote::HighFive, Emote::Throw]
                .into_iter()
                .enumerate()
                .map(|(i, emote)| AvatarStateDto {
                    emote: Some(EmoteState {
                        emote,
                        sequence: i as u32,
                    }),
                    ..an_avatar(i as u64 + 1)
                })
                .collect(),
        },
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
    assert!(Emote::Handshake.is_paired() && Emote::HighFive.is_paired());
    assert!(!Emote::Throw.is_paired() && !Emote::Wave.is_paired());
}

#[test]
fn roundtrip_grab_and_release_app_frames() {
    for m in [
        NetMessage::GrabPrim(msg::GrabPrim {
            request_id: 9,
            prim_id: 4_242,
        }),
        NetMessage::ReleasePrim(msg::ReleasePrim {
            request_id: 10,
            throw: true,
        }),
        NetMessage::ReleasePrim(msg::ReleasePrim {
            request_id: 11,
            throw: false,
        }),
        NetMessage::WorldSnapshot {
            tick: 3,
            avatars: vec![AvatarStateDto {
                holding: Some(4_242),
                ..an_avatar(1)
            }],
        },
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
    assert_eq!(
        message_kind(&NetMessage::GrabPrim(msg::GrabPrim {
            request_id: 0,
            prim_id: 1
        })),
        MessageKind::GrabPrim
    );
}

#[test]
fn roundtrip_create_prim_app_frame() {
    let m = NetMessage::CreatePrim(msg::CreatePrim {
        request_id: 123,
        name: "New Prim".into(),
        position: Vec3::new(10.0, 5.0, 20.0),
        rotation: Vec3::ZERO,
        scale: Vec3::ONE,
        color: [0.5, 0.5, 0.5],
        texture_id: None,
        model: None,
        draft: false,
        shape: "box".into(),
        surface: PrimSurface::default(),
        geometry: PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: 53.2194,
            longitude: 6.5665,
        },
        particles: Some(crate::particles::ParticleEffect::fire()),
    });
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

#[test]
fn roundtrip_update_prim_app_frame() {
    let m = NetMessage::UpdatePrim(msg::UpdatePrim {
        request_id: 124,
        prim_id: 42,
        position: Vec3::new(10.0, 5.0, 20.0),
        rotation: Vec3::new(0.0, 90.0, 0.0),
        scale: Vec3::new(2.0, 2.0, 2.0),
        color: [1.0, 0.0, 0.0],
        texture_id: Some("brick".into()),
        model: Some("2f8a1c".into()),
        name: "My Prim".into(),
        surface: PrimSurface::default(),
        geometry: PrimGeometry::default(),
        geo: GeoAnchor {
            latitude: 53.2194,
            longitude: 6.5665,
        },
        particles: None,
    });
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

/// A look survives the wire, travels small, and the server's check refuses
/// what it should.
#[test]
fn roundtrip_avatar_look_app_frames() {
    let mut look = AvatarLook::default_for(true);
    look.set_face("nose/nose-scale-horiz-decr-incr", 80);
    look.hair_colour = Some([230, 200, 150]);
    for m in [
        NetMessage::SetAvatarLook(msg::SetAvatarLook { look: look.clone() }),
        NetMessage::AvatarLookOf {
            avatar_id: 9,
            look: look.clone(),
        },
    ] {
        let bytes = encode_app_frame(&m).unwrap();
        assert!(bytes.len() < 256, "a look is small: {} bytes", bytes.len());
        let m2 = decode_app_frame(&bytes).unwrap();
        assert_eq!(m, m2);
    }
    assert!(look.is_reasonable());
    let mut bad = look.clone();
    bad.age = 12;
    assert!(!bad.is_reasonable(), "no children's bodies");
    let mut bad = look.clone();
    bad.outfit = "x".repeat(MAX_LOOK_NAME + 1);
    assert!(!bad.is_reasonable());
    let mut bad = look;
    bad.extras = vec!["glasses".into(); MAX_LOOK_EXTRAS + 1];
    assert!(!bad.is_reasonable(), "a bounded list of extras");
}

/// A look saved before protocol 45 (the server keeps them as postcard
/// bytes, `looks.rs`) still reads: the fields added since default.
#[test]
fn a_look_saved_before_skin_tone_still_reads() {
    let look = AvatarLook::default_for(false);
    let bytes = postcard::to_allocvec(&look).unwrap();
    // The two fields added last are a zero byte each (an i8 0 and an
    // empty list); a look written before them simply ends there.
    let old = &bytes[..bytes.len() - 2];
    assert!(
        postcard::from_bytes::<AvatarLook>(old).is_err(),
        "postcard gives no defaults at the end — which is why from_stored exists"
    );
    assert_eq!(AvatarLook::from_stored(old), Some(look.clone()));
    assert_eq!(AvatarLook::from_stored(&bytes), Some(look));
}

#[test]
fn roundtrip_chat_app_frames() {
    for m in [
        NetMessage::ChatSend(msg::ChatSend {
            request_id: 7,
            scope: ChatScope::Local { shout: true },
            body: "hi".into(),
        }),
        NetMessage::ChatSend(msg::ChatSend {
            request_id: 8,
            scope: ChatScope::Global,
            body: "yo".into(),
        }),
        NetMessage::ChatMessage {
            channel: "geo:".into(),
            sender_id: 3,
            sender_name: "dev".into(),
            sent_at_ms: 1234,
            body: "hello".into(),
            distance_m: Some(12.5),
        },
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
}

#[test]
fn roundtrip_moderation_app_frames() {
    for m in [
        NetMessage::SetBlocked(msg::SetBlocked {
            request_id: 3,
            username: "loud".into(),
            blocked: true,
        }),
        NetMessage::BlockedList {
            usernames: vec!["loud".into(), "louder".into()],
        },
        NetMessage::BlockedList { usernames: vec![] },
        NetMessage::ReportUser(msg::ReportUser {
            request_id: 4,
            username: "loud".into(),
            reason: ReportReason::Harassment,
            note: "followed me home and would not leave".into(),
            message: Some("the line they sent".into()),
            asset: None,
        }),
        NetMessage::ReportUser(msg::ReportUser {
            request_id: 5,
            username: "loud".into(),
            reason: ReportReason::Other,
            note: String::new(),
            message: None,
            // The other shape a report has since protocol 22: raised from an
            // object rather than a chat line.
            asset: Some("d0".repeat(32)),
        }),
        NetMessage::ReportAck {
            request_id: 4,
            accepted: true,
        },
        NetMessage::DeleteAccount(msg::DeleteAccount {
            request_id: 6,
            password: "hunter2".into(),
        }),
        NetMessage::AccountDeleted,
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
}

/// `as_str` is what lands in SQLite and in the admin API, so a reason that
/// does not survive the trip silently retypes every stored report as `Other`.
#[test]
fn every_report_reason_survives_its_stored_form() {
    for r in ReportReason::ALL {
        assert_eq!(ReportReason::from_wire(r.as_str()), *r);
        assert!(!r.label().is_empty());
    }
    assert_eq!(
        ReportReason::from_wire("something we stopped using"),
        ReportReason::Other
    );
}

#[test]
fn roundtrip_voice_app_frames() {
    for m in [
        NetMessage::VoiceFrame(msg::VoiceFrame {
            scope: ChatScope::Local { shout: false },
            seq: 42,
            payload: vec![0x00, 0xff, 0x7f],
        }),
        NetMessage::VoiceFrame(msg::VoiceFrame {
            scope: ChatScope::Global,
            seq: u32::MAX,
            payload: vec![1; 1500],
        }),
        NetMessage::VoiceData {
            channel: "voice:local".into(),
            sender_id: 9,
            seq: 43,
            payload: vec![0xde, 0xad],
            distance_m: Some(31.25),
        },
        NetMessage::VoiceData {
            channel: "voice:global".into(),
            sender_id: 9,
            seq: 44,
            payload: vec![],
            distance_m: None,
        },
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
}

#[test]
fn roundtrip_call_app_frames() {
    let group = vec![7u8; 16];
    for m in [
        NetMessage::CallJoin(msg::CallJoin {
            request_id: 3,
            group: group.clone(),
            offer: "v=0\r\n".into(),
            screen: Some("2".into()),
        }),
        NetMessage::CallJoined {
            request_id: 3,
            group: group.clone(),
            participant: 12,
            answer: "v=0\r\n".into(),
        },
        NetMessage::CallOffer {
            group: group.clone(),
            sdp: "v=0\r\n".into(),
            tracks: vec![CallTrack {
                mid: "0".into(),
                participant: 11,
                kind: CallTrackKind::Screen,
            }],
        },
        NetMessage::CallAnswer(msg::CallAnswer {
            group: group.clone(),
            sdp: "v=0\r\n".into(),
        }),
        NetMessage::CallLeave(msg::CallLeave {
            group: group.clone(),
        }),
        NetMessage::CallRoster {
            group: group.clone(),
            members: vec![CallMember {
                participant: 11,
                username: "alice".into(),
                device: Uuid::from_u128(1),
                role: CallRole::Host,
                hand: Some(2),
                held: true,
                sharing: false,
                muted: true,
                recording: false,
                guest: true,
                room: 2,
                agent: true,
            }],
            settings: CallSettings::default(),
            waiting: vec![CallWaiter {
                participant: 13,
                username: "guest".into(),
                guest: true,
                since_ms: 5,
            }],
            spotlight: Some(11),
            rooms: Some(CallRooms {
                names: vec!["Stage".into()],
                until_ms: Some(9),
                may_move: true,
            }),
        },
        NetMessage::CallControl(msg::CallControl {
            group: group.clone(),
            action: CallAction::Role {
                participant: 4,
                role: CallRole::CoHost,
            },
        }),
        NetMessage::CallEnded {
            group: group.clone(),
            reason: CallEndReason::Removed { by: "bob".into() },
        },
        NetMessage::CallReact(msg::CallReact {
            group: group.clone(),
            epoch: 9,
            sealed: vec![1, 2, 3],
        }),
        NetMessage::CallReacted {
            group: group.clone(),
            participant: 4,
            epoch: 9,
            sealed: vec![1, 2, 3],
        },
        NetMessage::CallWatch(msg::CallWatch {
            group: group.clone(),
            cameras: Some(vec![4, 5]),
        }),
        NetMessage::CallWaiting {
            request_id: 3,
            group: group.clone(),
            participant: 13,
        },
        NetMessage::CallInvite(msg::CallInvite {
            group: group.clone(),
        }),
        NetMessage::CallInvited {
            group: group.clone(),
            code: "k7q2m9x4".into(),
            expires_ms: 1_792_000_800_000,
        },
        NetMessage::CallKeyDrop(msg::CallKeyDrop {
            group: group.clone(),
            participant: 13,
            sealed: vec![9; 40],
        }),
        NetMessage::CallKeyDropped {
            group: group.clone(),
            sealed: vec![9; 40],
        },
        NetMessage::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            auth: AuthCredential::Guest {
                code: "k7q2m9x4".into(),
                name: "Sanne".into(),
            },
        },
        NetMessage::CallControl(msg::CallControl {
            group: group.clone(),
            action: CallAction::OpenRooms {
                names: vec!["A".into()],
                assign: vec![(4, 1)],
                minutes: 10,
                may_move: false,
            },
        }),
    ] {
        let b = encode_app_frame(&m).unwrap();
        assert_eq!(decode_app_frame(&b).unwrap(), m);
    }
}

#[test]
fn roundtrip_delete_prim_app_frame() {
    let m = NetMessage::DeletePrim(msg::DeletePrim {
        request_id: 125,
        prim_id: 42,
    });
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

#[test]
fn roundtrip_prim_upsert_app_frame() {
    let prim = PrimDto {
        id: 42,
        name: "Updated Prim".into(),
        shape: "sphere".into(),
        position: Vec3::new(15.0, 8.0, 25.0),
        rotation: Vec3::new(45.0, 45.0, 0.0),
        scale: Vec3::new(1.5, 1.5, 1.5),
        color: [0.0, 1.0, 0.0],
        texture_id: Some("grass".into()),
        model: None,
        path_cut_begin: 0.0,
        path_cut_end: 1.0,
        hollow: 0.0,
        twist_begin: 0.0,
        twist_end: 0.0,
        taper_x: 0.0,
        taper_y: 0.0,
        top_shear_x: 0.0,
        top_shear_y: 0.0,
        slice_begin: 0.0,
        slice_end: 1.0,
        surface: PrimSurface::default(),
        geo: GeoAnchor {
            latitude: 53.2194,
            longitude: 6.5665,
        },
        particles: Some(crate::particles::ParticleEffect::blast()),
        scripted: true,
        video: Some(PrimVideo {
            asset: "ab".repeat(32),
            started_at_ms: 1_790_000_000_000,
            looped: true,
        }),
        sound_loop: Some(crate::sounds::PrimSound {
            sound: crate::sounds::SoundName::Bell,
            volume: 0.5,
            range_m: 80.0,
        }),
        physical: true,
        hand_range: 2.0,
        tone: Some(crate::tone::ToneSpec::default()),
        draft: true,
        groups: vec![GroupLevel {
            id: 7,
            name: "Market stall".into(),
        }],
    };
    let m = NetMessage::PrimUpsert { prim: prim.clone() };
    let b = encode_app_frame(&m).unwrap();
    let m2 = decode_app_frame(&b).unwrap();
    assert_eq!(m, m2);
}

#[test]
fn prim_motion_round_trip_and_size() {
    let body = PrimBody {
        id: 42,
        geo: GeoAnchor {
            latitude: 53.2066,
            longitude: 6.5918,
        },
        altitude: 0.11,
        rotation: [0.0, 0.38, 0.0, 0.92],
        velocity: Vec3::new(4.0, 1.5, -2.0),
        resting: false,
    };
    let m = NetMessage::PrimMotion {
        tick: 9_000,
        bodies: vec![body; 3],
    };
    let b = encode_app_frame(&m).unwrap();
    assert_eq!(decode_app_frame(&b).unwrap(), m);
    // The point of the message is that it is small: a ball in play is one of
    // these twenty times a second to every client.
    assert!(b.len() < 3 * 64, "{} bytes for three bodies", b.len());
}

/// Every number `from_wire` answers names the kind with that discriminant,
/// and the retired numbers answer nothing. `message_kinds!` writes both from
/// one table, so a forgotten arm cannot happen; what this guards is the holes.
#[test]
fn message_kind_wire_is_a_bijection() {
    for v in 0..=u16::MAX {
        if let Some(k) = MessageKind::from_wire(v) {
            assert_eq!(k as u16, v, "{k:?} answers to {v}");
        }
    }
    assert_eq!(MessageKind::from_wire(0), None);
    // 5 is the hole `ObserverUpdate` left when regions went (protocol 23). A
    // retired number stays retired: reusing it would make a stale client's
    // observer update decode as whatever took the slot, and the run is
    // therefore no longer contiguous on purpose.
    assert_eq!(MessageKind::from_wire(5), None);
    // 19 (`ChatSubscribe`) and 75–81 (workspaces and channels) went with
    // rooms and workspaces in protocol 39, and stay retired for the same reason.
    for retired in [19, 75, 76, 77, 78, 79, 80, 81] {
        assert_eq!(MessageKind::from_wire(retired), None);
    }
}

/// A variant with a `request_id` field has to be marked `#[request_id]` in
/// `message_kinds!`, or its header carries 0 and a client waiting on the
/// reply never matches it — the one mistake there that still compiles. The
/// pinned wire shape names every variant's fields, so it is the reference.
#[test]
fn every_request_id_reaches_the_header() {
    let kinds: Vec<MessageKind> = (0..=u16::MAX).filter_map(MessageKind::from_wire).collect();
    let shape = include_str!("wire_shape.txt");
    let (_, variants) = shape
        .split_once("\nNetMessage = enum\n")
        .expect("wire_shape.txt lists NetMessage");
    let mut seen = 0;
    for line in variants.lines().take_while(|l| l.starts_with("  #")) {
        // "  #2 ServerError { request_id: U32, code: U32, message: Str }"
        let name = line
            .split_whitespace()
            .nth(1)
            .and_then(|n| n.split(['{', '(']).next())
            .expect("a variant line names its variant");
        // "  #8 CreatePrim(CreatePrim)": the field is the `msg` struct's.
        let inner = line
            .split_once('(')
            .map(|(_, t)| t.trim_end_matches(')'))
            .filter(|t| shape.contains(&format!("\n{t} = struct\n")));
        let has_field = match inner {
            Some(t) => shape
                .split(&format!("\n{t} = struct\n"))
                .nth(1)
                .into_iter()
                .flat_map(|rest| rest.lines().take_while(|l| l.starts_with("  .")))
                .any(|l| l.ends_with(" request_id: U32")),
            None => line.contains("{ request_id: U32") || line.contains(", request_id: U32"),
        };
        let kind = kinds
            .iter()
            .find(|k| format!("{k:?}") == name)
            .unwrap_or_else(|| panic!("NetMessage::{name} has no MessageKind"));
        assert_eq!(
            kind.has_request_id(),
            has_field,
            "NetMessage::{name}: mark its row #[request_id] exactly when it has the field"
        );
        seen += 1;
    }
    assert_eq!(seen, kinds.len(), "one NetMessage variant per kind");
}

/// The header guards in `decode_app_frame` — the checks that keep a
/// version-skewed or corrupt frame from being misread as a valid message.
/// Untested by the round-trips, and exactly the paths a mismatched client hits.
#[test]
fn decode_app_frame_rejects_bad_headers() {
    let good = encode_app_frame(&NetMessage::PrimRemoved { id: 1 }).unwrap();

    // Truncated below the 8-byte app header.
    assert!(matches!(
        decode_app_frame(&good[..4]),
        Err(ProtocolError::FrameTooShort(4))
    ));

    // Wrong protocol version (bytes 0..2).
    let mut wrong_ver = good.clone();
    wrong_ver[0] = wrong_ver[0].wrapping_add(1);
    assert!(matches!(
        decode_app_frame(&wrong_ver),
        Err(ProtocolError::UnsupportedVersion(_))
    ));

    // Unknown message kind (bytes 2..4); 0 is never assigned.
    let mut bad_kind = good.clone();
    bad_kind[2] = 0;
    bad_kind[3] = 0;
    assert!(matches!(
        decode_app_frame(&bad_kind),
        Err(ProtocolError::UnknownMessageKind(0))
    ));
}
