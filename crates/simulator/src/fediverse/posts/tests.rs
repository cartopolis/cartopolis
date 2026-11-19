use super::*;
use cartopolis_core::{CommunitySettings, JoinRule};

const BASE: &str = "https://cartopolis.test";
const SERVER: &str = "cartopolis.test";

struct World {
    conn: Connection,
    owner: String,
    member: String,
    outsider: String,
    community: Uuid,
}

fn world() -> World {
    let conn = crate::db::open_and_migrate(":memory:").unwrap();
    let owner = crate::auth::create_account(&conn, "karim", "pw")
        .unwrap()
        .to_string();
    let member = crate::auth::create_account(&conn, "Sanne", "pw")
        .unwrap()
        .to_string();
    let outsider = crate::auth::create_account(&conn, "tom", "pw")
        .unwrap()
        .to_string();
    let settings = CommunitySettings {
        name: "Buurt Oosterpoort".into(),
        about: String::new(),
        join: JoinRule::Open,
        listed: true,
        members_start_topics: true,
        picture: None,
        place: None,
    };
    let community = communities::create(&conn, &owner, &settings, 1).unwrap();
    communities::join(&conn, &member, community, "", 2).unwrap();
    World {
        conn,
        owner,
        member,
        outsider,
        community,
    }
}

fn on(w: &World, handle: &str) {
    set_public(
        &w.conn,
        &w.owner,
        w.community,
        &PublicSettings {
            enabled: true,
            handle: Some(handle.into()),
            who_posts: WhoPosts::Members,
        },
        10,
    )
    .unwrap();
}

fn words(text: &str) -> Content {
    Content {
        text: text.into(),
        ..Content::default()
    }
}

fn post(text: &str) -> NewPost {
    NewPost {
        text: text.into(),
        picture: None,
        snapshot: None,
        place: PostPlace {
            latitude: 53.21,
            longitude: 6.58,
            name: Some("Meeuwerderweg".into()),
            view: Some(PointView {
                altitude: 90.0,
                pitch: -30.0,
                yaw: 15.0,
            }),
        },
        warning: None,
        sensitive: false,
        picture_alt: None,
    }
}

#[test]
fn only_a_moderator_switches_posts_on_and_the_address_is_chosen_once() {
    let w = world();
    let settings = PublicSettings {
        enabled: true,
        handle: Some("oosterpoort".into()),
        who_posts: WhoPosts::Members,
    };
    assert_eq!(
        set_public(&w.conn, &w.member, w.community, &settings, 1),
        Err(Refusal::NotAModerator)
    );
    assert_eq!(
        set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                handle: None,
                ..settings.clone()
            },
            1
        ),
        Err(Refusal::NeedsHandle)
    );
    assert_eq!(
        set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                handle: Some("Ooster Poort".into()),
                ..settings.clone()
            },
            1
        ),
        Err(Refusal::BadHandle)
    );
    set_public(&w.conn, &w.owner, w.community, &settings, 1).unwrap();
    assert_eq!(
        set_public(
            &w.conn,
            &w.owner,
            w.community,
            &PublicSettings {
                handle: Some("other".into()),
                ..settings.clone()
            },
            2
        ),
        Err(Refusal::HandleFixed)
    );
    // Switching off and on again keeps the address without naming it.
    let off = set_public(
        &w.conn,
        &w.owner,
        w.community,
        &PublicSettings {
            enabled: false,
            handle: None,
            who_posts: WhoPosts::Moderators,
        },
        3,
    )
    .unwrap();
    assert_eq!(off.handle, "oosterpoort");
    assert_eq!(community_by_handle(&w.conn, "oosterpoort").unwrap(), None);
}

#[test]
fn a_handle_is_not_anybodys_username_and_is_never_reused() {
    let w = world();
    // `Sanne` has a capital; her address is still taken folded.
    let mut settings = PublicSettings {
        enabled: true,
        handle: Some("sanne".into()),
        who_posts: WhoPosts::Members,
    };
    assert_eq!(
        set_public(&w.conn, &w.owner, w.community, &settings, 1),
        Err(Refusal::HandleTaken)
    );
    settings.handle = Some("oosterpoort".into());
    set_public(&w.conn, &w.owner, w.community, &settings, 1).unwrap();
    forget_community(&w.conn, w.community, 5).unwrap();
    assert!(!handle_free(&w.conn, "oosterpoort").unwrap());
}

#[test]
fn members_post_with_a_place_and_outsiders_do_not() {
    let w = world();
    assert_eq!(
        create_post(&w.conn, &w.member, w.community, &post("hi"), BASE, 1),
        Err(Refusal::PostsOff)
    );
    on(&w, "oosterpoort");
    assert_eq!(
        create_post(&w.conn, &w.outsider, w.community, &post("hi"), BASE, 1),
        Err(Refusal::NotAMember)
    );
    assert_eq!(
        create_post(&w.conn, &w.member, w.community, &post("   "), BASE, 1),
        Err(Refusal::Empty)
    );
    let mut far = post("hi");
    far.place.latitude = 200.0;
    assert_eq!(
        create_post(&w.conn, &w.member, w.community, &far, BASE, 1),
        Err(Refusal::BadPlace)
    );
    let id = create_post(
        &w.conn,
        &w.member,
        w.community,
        &post("Plant swap"),
        BASE,
        2,
    )
    .unwrap();
    let feed = feed(&w.conn, w.community, None, None, SERVER).unwrap();
    assert_eq!(feed.len(), 1);
    assert_eq!(feed[0].id, id);
    assert_eq!(feed[0].author.address, "Sanne@cartopolis.test");
    assert_eq!(
        feed[0].place.as_ref().unwrap().name.as_deref(),
        Some("Meeuwerderweg")
    );
    assert_eq!(feed[0].place.as_ref().unwrap().view.unwrap().pitch, -30.0);
    assert!(!feed[0].mine);
    let r = row(&w.conn, id).unwrap().unwrap();
    assert_eq!(r.ap_id, format!("{BASE}/ap/p/{id}"));
}

#[test]
fn a_notice_board_takes_posts_from_moderators_only() {
    let w = world();
    set_public(
        &w.conn,
        &w.owner,
        w.community,
        &PublicSettings {
            enabled: true,
            handle: Some("board".into()),
            who_posts: WhoPosts::Moderators,
        },
        1,
    )
    .unwrap();
    assert_eq!(
        create_post(&w.conn, &w.member, w.community, &post("hi"), BASE, 1),
        Err(Refusal::ModeratorsOnly)
    );
    create_post(&w.conn, &w.owner, w.community, &post("notice"), BASE, 1).unwrap();
    let info = info(&w.conn, Some(&w.member), w.community, SERVER, true).unwrap();
    assert!(!info.may_post);
    assert_eq!(info.address.as_deref(), Some("board@cartopolis.test"));
}

#[test]
fn somebody_here_likes_once_and_sees_it_and_can_take_it_back() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(
        &w.conn,
        &w.member,
        w.community,
        &post("Plant swap"),
        BASE,
        1,
    )
    .unwrap();
    let seen = |who: &str| {
        let r = row(&w.conn, p).unwrap().unwrap();
        let d = dto(&w.conn, &r, Some(who), SERVER).unwrap();
        (d.likes, d.liked)
    };
    assert!(like(&w.conn, &w.outsider, p, true, BASE, 2).unwrap().1);
    assert!(
        !like(&w.conn, &w.outsider, p, true, BASE, 3).unwrap().1,
        "once"
    );
    assert_eq!(seen(&w.outsider), (1, true));
    assert_eq!(seen(&w.owner), (1, false));
    assert!(like(&w.conn, &w.outsider, p, false, BASE, 4).unwrap().1);
    assert_eq!(seen(&w.outsider), (0, false));
}

#[test]
fn replies_are_flat_and_anybody_signed_in_may_reply() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(
        &w.conn,
        &w.member,
        w.community,
        &post("Plant swap"),
        BASE,
        1,
    )
    .unwrap();
    let (r1, _) = create_reply(&w.conn, &w.outsider, p, "I'll come", BASE, 2).unwrap();
    // A reply to a reply hangs under the post.
    let (r2, _) = create_reply(&w.conn, &w.owner, r1, "great", BASE, 3).unwrap();
    let t = thread(&w.conn, r2, Some(&w.owner), SERVER).unwrap();
    assert_eq!(t.post.id, p);
    assert_eq!(t.post.replies, 2);
    assert_eq!(
        t.replies.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![r1, r2]
    );
    assert!(t.replies[1].mine);
    // …but remembers whom it answers, which other servers thread it by.
    let answers = |id| row(&w.conn, id).unwrap().unwrap().in_reply_to;
    assert_eq!(answers(r1), Some(super::super::post_url(BASE, p)));
    assert_eq!(answers(r2), Some(super::super::post_url(BASE, r1)));
}

#[test]
fn the_author_deletes_with_replies_and_a_moderator_hides() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("one"), BASE, 1).unwrap();
    let (r, _) = create_reply(&w.conn, &w.outsider, p, "reply", BASE, 2).unwrap();
    assert_eq!(delete(&w.conn, &w.owner, p), Err(Refusal::NotYours));
    let gone = delete(&w.conn, &w.member, p).unwrap();
    assert_eq!(gone.iter().map(|g| g.id).collect::<Vec<_>>(), vec![p, r]);
    assert!(row(&w.conn, r).unwrap().is_none());

    let p = create_post(&w.conn, &w.member, w.community, &post("two"), BASE, 3).unwrap();
    assert_eq!(hide(&w.conn, &w.member, p), Err(Refusal::NotAModerator));
    hide(&w.conn, &w.owner, p).unwrap();
    assert!(feed(&w.conn, w.community, None, None, SERVER)
        .unwrap()
        .is_empty());
    assert_eq!(thread(&w.conn, p, None, SERVER), Err(Refusal::NoSuchPost));
}

#[test]
fn a_remote_post_is_stored_once_and_its_author_alone_changes_it() {
    let w = world();
    on(&w, "oosterpoort");
    let remote = Remote {
        ap_id: "https://mastodon.example/statuses/1".into(),
        author_actor: "https://mastodon.example/users/anna".into(),
        via_actor: None,
        text: "Heron back on the quay".into(),
        picture: None,
        place: None,
        url: Some("https://mastodon.example/@anna/1".into()),
        created_ms: 5,
        parent: None,
        in_reply_to: None,
        warning: None,
        sensitive: false,
        picture_alt: None,
        more_pictures: Vec::new(),
    };
    let id = store_remote(&w.conn, w.community, &remote)
        .unwrap()
        .unwrap();
    assert_eq!(store_remote(&w.conn, w.community, &remote).unwrap(), None);
    assert_eq!(
        update_remote(
            &w.conn,
            &remote.ap_id,
            "https://elsewhere/users/x",
            &words("spam"),
            6
        )
        .unwrap(),
        0
    );
    update_remote(
        &w.conn,
        &remote.ap_id,
        &remote.author_actor,
        &words("Heron!"),
        6,
    )
    .unwrap();
    let r = row(&w.conn, id).unwrap().unwrap();
    assert_eq!((r.text.as_str(), r.edited_ms), ("Heron!", Some(6)));
    let shown = dto(&w.conn, &r, None, SERVER).unwrap();
    assert!(!shown.author.local);
    assert_eq!(
        delete_remote(&w.conn, &remote.ap_id, &remote.author_actor).unwrap(),
        1
    );
    assert!(row(&w.conn, id).unwrap().is_none());
}

#[test]
fn a_blocked_account_is_hidden_and_stays_out() {
    let w = world();
    on(&w, "oosterpoort");
    let actor = "https://spam.example/users/deals";
    let remote = Remote {
        ap_id: "https://spam.example/1".into(),
        author_actor: actor.into(),
        via_actor: None,
        text: "best phones".into(),
        picture: None,
        place: None,
        url: None,
        created_ms: 1,
        parent: None,
        in_reply_to: None,
        warning: None,
        sensitive: false,
        picture_alt: None,
        more_pictures: Vec::new(),
    };
    store_remote(&w.conn, w.community, &remote).unwrap();
    assert_eq!(
        block_actor(&w.conn, &w.member, w.community, actor, 2),
        Err(Refusal::NotAModerator)
    );
    block_actor(&w.conn, &w.owner, w.community, actor, 2).unwrap();
    assert!(actor_blocked(&w.conn, w.community, actor).unwrap());
    assert!(feed(&w.conn, w.community, None, None, SERVER)
        .unwrap()
        .is_empty());
}

#[test]
fn only_a_public_posts_picture_is_public() {
    let w = world();
    on(&w, "oosterpoort");
    let hash = crate::assets::content_hash(b"jpeg");
    assert!(!picture_is_public(&w.conn, &hash).unwrap());
    let mut p = post("pic");
    p.picture = Some(hash.clone());
    let id = create_post(&w.conn, &w.member, w.community, &p, BASE, 1).unwrap();
    assert!(picture_is_public(&w.conn, &hash).unwrap());
    hide(&w.conn, &w.owner, id).unwrap();
    assert!(!picture_is_public(&w.conn, &hash).unwrap());
    p.picture = Some("../etc".into());
    assert_eq!(
        create_post(&w.conn, &w.member, w.community, &p, BASE, 1),
        Err(Refusal::BadPicture)
    );
}

#[test]
fn a_deleted_account_takes_its_posts_with_it() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("mine"), BASE, 1).unwrap();
    let (r, _) = create_reply(&w.conn, &w.outsider, p, "yours", BASE, 2).unwrap();
    let gone = forget_account(&w.conn, &w.member, 3).unwrap();
    assert_eq!(gone.len(), 2);
    assert!(row(&w.conn, r).unwrap().is_none());
    // Nobody elsewhere knew the account (it has no key): nothing to tell.
    let queued: i64 = w
        .conn
        .query_row("SELECT COUNT(*) FROM fedi_gone_accounts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queued, 0);
}

/// A remote actor, stored as an inbox would store it.
fn remote(w: &World, id: &str) {
    w.conn
            .execute(
                "INSERT INTO fedi_actors (id, kind, username, host, name, inbox, shared_inbox, key_id, public_pem, fetched_ms)
                 VALUES (?1, 'Person', 'anna', 'm.test', 'Anna', ?1 || '/inbox', 'https://m.test/inbox', 'k', 'p', 0)",
                params![id],
            )
            .unwrap();
}

#[test]
fn a_known_account_leaves_word_for_the_servers_that_knew_it() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("mine"), BASE, 1).unwrap();
    let ap = row(&w.conn, p).unwrap().unwrap().ap_id;
    crate::fediverse::signing::public_pem(&w.conn, &person_local(&w.member), 1).unwrap();
    remote(&w, "https://m.test/users/anna");
    w.conn
            .execute(
                "INSERT INTO fedi_followers (local, actor, followed_ms) VALUES (?1, 'https://m.test/users/anna', 0)",
                params![group_local(w.community)],
            )
            .unwrap();
    forget_account(&w.conn, &w.member, 3).unwrap();
    let (inboxes, posts): (String, String) = w
        .conn
        .query_row(
            "SELECT inboxes, posts FROM fedi_gone_accounts WHERE account = ?1",
            params![w.member],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(inboxes, r#"["https://m.test/inbox"]"#);
    assert_eq!(posts, format!("[\"{ap}\"]"));
    // The key stays to sign the deletion with; the actor is gone, not unknown.
    w.conn
        .execute("DELETE FROM accounts WHERE id = ?1", params![w.member])
        .unwrap();
    assert!(account_gone(&w.conn, &w.member).unwrap());
    assert!(!account_gone(&w.conn, &w.owner).unwrap());
}

#[test]
fn only_its_author_edits_a_post() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("old"), BASE, 1).unwrap();
    assert_eq!(edit(&w.conn, &w.owner, p, "new", 2), Err(Refusal::NotYours));
    assert_eq!(edit(&w.conn, &w.member, p, "  ", 2), Err(Refusal::Empty));
    let r = edit(&w.conn, &w.member, p, " new ", 5).unwrap();
    assert_eq!((r.text.as_str(), r.edited_ms), ("new", Some(5)));
}

#[test]
fn likes_count_once_per_account_and_go_when_taken_back() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("x"), BASE, 1).unwrap();
    let ap = row(&w.conn, p).unwrap().unwrap().ap_id;
    assert!(vote(
        &w.conn,
        &ap,
        "https://m.test/users/a",
        "https://m.test/l/1",
        1,
        2
    )
    .unwrap());
    assert!(vote(
        &w.conn,
        &ap,
        "https://m.test/users/a",
        "https://m.test/l/2",
        1,
        3
    )
    .unwrap());
    assert!(vote(
        &w.conn,
        &ap,
        "https://l.test/u/b",
        "https://l.test/d/1",
        -1,
        3
    )
    .unwrap());
    assert!(!vote(
        &w.conn,
        "https://x.test/p/9",
        "https://l.test/u/b",
        "a",
        1,
        3
    )
    .unwrap());
    let d = dto(&w.conn, &row(&w.conn, p).unwrap().unwrap(), None, SERVER).unwrap();
    assert_eq!((d.likes, d.dislikes), (1, 1));
    // Taken back by the like's id, as Mastodon does, and by its object.
    unvote(
        &w.conn,
        "https://m.test/users/a",
        Some("https://m.test/l/2"),
        None,
    )
    .unwrap();
    unvote(&w.conn, "https://l.test/u/b", None, Some(&ap)).unwrap();
    let d = dto(&w.conn, &row(&w.conn, p).unwrap().unwrap(), None, SERVER).unwrap();
    assert_eq!((d.likes, d.dislikes), (0, 0));
}

#[test]
fn a_report_from_elsewhere_waits_for_a_moderator() {
    let w = world();
    on(&w, "oosterpoort");
    let p = create_post(&w.conn, &w.member, w.community, &post("x"), BASE, 1).unwrap();
    let ap = row(&w.conn, p).unwrap().unwrap().ap_id;
    remote(&w, "https://m.test/users/anna");
    assert_eq!(
        report(&w.conn, &ap, "https://m.test/users/anna", "spam", 2).unwrap(),
        1
    );
    // The same report twice is one report.
    assert_eq!(
        report(&w.conn, &ap, "https://m.test/users/anna", "spam", 3).unwrap(),
        0
    );
    assert_eq!(
        report(&w.conn, &ap, "https://m.test/actor", "", 3).unwrap(),
        1
    );
    assert_eq!(
        open_reports(&w.conn, &w.member, w.community, SERVER),
        Err(Refusal::NotAModerator)
    );
    let open = open_reports(&w.conn, &w.owner, w.community, SERVER).unwrap();
    assert_eq!(open.len(), 2);
    assert_eq!(open[0].reporter, "anna@m.test");
    assert_eq!(open[1].reporter, "m.test");
    assert_eq!(
        info(&w.conn, Some(&w.owner), w.community, SERVER, true)
            .unwrap()
            .open_reports,
        2
    );
    assert_eq!(
        resolve_report(
            &w.conn,
            &w.owner,
            w.community,
            open[0].id,
            ReportAction::Dismiss,
            4
        )
        .unwrap(),
        None
    );
    let hidden = resolve_report(
        &w.conn,
        &w.owner,
        w.community,
        open[1].id,
        ReportAction::Hide,
        5,
    )
    .unwrap();
    assert_eq!(hidden.map(|r| r.id), Some(p));
    assert!(open_reports(&w.conn, &w.owner, w.community, SERVER)
        .unwrap()
        .is_empty());
    assert!(row(&w.conn, p).unwrap().unwrap().hidden);
}

#[test]
fn a_person_page_lists_public_posts_only_for_somebody_who_wrote_one() {
    let w = world();
    on(&w, "oosterpoort");
    assert_eq!(
        person_posts(&w.conn, "sanne", None, SERVER),
        Err(Refusal::NoSuchPerson)
    );
    // The owner moderates a public community: findable, so a page, empty.
    assert!(person_posts(&w.conn, "karim", None, SERVER)
        .unwrap()
        .posts
        .is_empty());
    create_post(&w.conn, &w.member, w.community, &post("hello"), BASE, 1).unwrap();
    let page = person_posts(&w.conn, "SANNE", None, SERVER).unwrap();
    assert_eq!(page.address, "Sanne@cartopolis.test");
    assert_eq!(page.posts[0].text, "hello");
}

#[test]
fn a_public_community_shows_itself_to_anybody() {
    let w = world();
    assert!(info(&w.conn, None, w.community, SERVER, true)
        .unwrap()
        .community
        .is_none());
    on(&w, "oosterpoort");
    let card = info(&w.conn, None, w.community, SERVER, true)
        .unwrap()
        .community
        .unwrap();
    assert_eq!(
        (card.id, card.name.as_str()),
        (w.community, "Buurt Oosterpoort")
    );
}
