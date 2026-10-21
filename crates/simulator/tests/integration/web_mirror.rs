//! The atlas web UI's hand-written copies of this server's JSON, read against
//! what serde actually writes.
//!
//! `crates/atlas/web/src/types.ts` describes `auth::AccountRow` and
//! `library::PlaceholderModel` in TypeScript, and being in one repository does not
//! make those descriptions checked: a renamed or added field compiles on both
//! sides and arrives in the admin pages as a column that reads `undefined`. These
//! serialise a real value of each type and compare its keys with the interface's
//! fields, both ways round, through the shared reader in
//! `cartopolis_core::ts_mirror`.
//!
//! No server is booted: this is a statement about two files, not about a socket.

use cartopolis_core::ts_mirror;
use cartopolis_simulator::auth::AccountRow;
use cartopolis_simulator::library::PlaceholderModel;

const WEB_TYPES: &str = include_str!("../../../atlas/web/src/types.ts");

/// The keys serde writes for `value`, sorted.
fn keys<T: serde::Serialize>(value: &T) -> Vec<String> {
    let serde_json::Value::Object(map) = serde_json::to_value(value).unwrap() else {
        panic!("not serialised as an object");
    };
    let mut keys: Vec<String> = map.keys().cloned().collect();
    keys.sort();
    keys
}

/// The fields of an interface in `types.ts`, sorted.
fn web_fields(name: &str) -> Vec<String> {
    let mut fields = ts_mirror::type_fields(WEB_TYPES, name)
        .unwrap_or_else(|| panic!("`{name}` is not declared in atlas/web/src/types.ts"));
    fields.sort();
    fields
}

#[test]
fn account_row() {
    // `email` is `Some` so an optional field could not be skipped by serde and
    // hide from the comparison.
    let row = AccountRow {
        id: String::new(),
        username: String::new(),
        disabled: false,
        admin: false,
        created_at: String::new(),
        email: Some(String::new()),
        email_pending: false,
    };
    assert_eq!(
        web_fields("Account"),
        keys(&row),
        "atlas/web/src/types.ts's Account has drifted from auth::AccountRow"
    );
}

#[test]
fn placeholder_model() {
    let model = PlaceholderModel {
        kind: String::new(),
        asset: String::new(),
        scale: 1.0,
        yaw_offset: 0.0,
        set_by: String::new(),
    };
    assert_eq!(
        web_fields("PlaceholderModel"),
        keys(&model),
        "atlas/web/src/types.ts's PlaceholderModel has drifted from library::PlaceholderModel"
    );
}

#[test]
fn post_dto() {
    use cartopolis_core::point::PointView;
    use cartopolis_core::posts::{PostAuthor, PostDto, PostPlace};
    // Every optional field is `Some`, so serde writes every key.
    let place = PostPlace {
        latitude: 53.2,
        longitude: 6.5,
        name: Some(String::new()),
        view: Some(PointView {
            altitude: 1.0,
            pitch: 0.0,
            yaw: 0.0,
        }),
    };
    let author = PostAuthor {
        name: String::new(),
        address: String::new(),
        local: true,
    };
    let post = PostDto {
        id: uuid::Uuid::nil(),
        community: uuid::Uuid::nil(),
        parent: Some(uuid::Uuid::nil()),
        author: author.clone(),
        via: Some(String::new()),
        text: String::new(),
        picture: Some(String::new()),
        snapshot: Some(String::new()),
        place: Some(place.clone()),
        url: Some(String::new()),
        created_ms: 0,
        edited_ms: Some(0),
        replies: 0,
        mine: false,
        likes: 0,
        dislikes: 0,
    };
    let report = cartopolis_core::posts::PostReport {
        id: uuid::Uuid::nil(),
        post: post.clone(),
        reporter: String::new(),
        reason: String::new(),
        created_ms: 0,
    };
    let person = cartopolis_core::posts::PersonPosts {
        address: String::new(),
        picture: Some(String::new()),
        posts: vec![post.clone()],
    };
    let link = cartopolis_core::posts::FollowLink {
        url: String::new(),
        software: Some(String::new()),
    };
    let drift = "atlas/web/src/types.ts has drifted from cartopolis_core::posts";
    assert_eq!(web_fields("PostDto"), keys(&post), "{drift}");
    assert_eq!(web_fields("PostReport"), keys(&report), "{drift}");
    assert_eq!(web_fields("PersonPosts"), keys(&person), "{drift}");
    assert_eq!(web_fields("FollowLink"), keys(&link), "{drift}");
    assert_eq!(web_fields("PostPlace"), keys(&place), "{drift}");
    assert_eq!(web_fields("PostAuthor"), keys(&author), "{drift}");
    assert_eq!(
        web_fields("PostView"),
        keys(&place.view.unwrap()),
        "{drift}"
    );
}

#[test]
fn public_info() {
    use cartopolis_core::posts::{FollowDto, PublicInfo, PublicSettings, SourceKind, WhoPosts};
    let follow = FollowDto {
        address: String::new(),
        name: String::new(),
        kind: SourceKind::Account,
        accepted: false,
    };
    let settings = PublicSettings {
        enabled: true,
        handle: Some(String::new()),
        who_posts: WhoPosts::Members,
    };
    let info = PublicInfo {
        settings: settings.clone(),
        address: Some(String::new()),
        followers: 0,
        follows: vec![follow.clone()],
        may_post: false,
        may_moderate: false,
        federating: false,
        community: Some(cartopolis_core::posts::PublicCommunity {
            id: uuid::Uuid::nil(),
            name: String::new(),
            about: String::new(),
            picture: Some(String::new()),
        }),
        open_reports: 0,
    };
    let drift = "atlas/web/src/types.ts has drifted from cartopolis_core::posts";
    assert_eq!(web_fields("PublicInfo"), keys(&info), "{drift}");
    assert_eq!(
        web_fields("PublicCommunity"),
        keys(info.community.as_ref().unwrap()),
        "{drift}"
    );
    assert_eq!(web_fields("PublicSettings"), keys(&settings), "{drift}");
    assert_eq!(web_fields("FollowDto"), keys(&follow), "{drift}");
}

#[test]
fn access_token() {
    // Every optional field is `Some`, so serde writes every key.
    let token = cartopolis_simulator::auth::AccessTokenInfo {
        id: String::new(),
        label: String::new(),
        elevated: false,
        created_at: 0,
        last_used: Some(0),
        expires_at: Some(0),
    };
    assert_eq!(
        web_fields("AccessToken"),
        keys(&token),
        "atlas/web/src/types.ts's AccessToken has drifted from auth::AccessTokenInfo"
    );
}
