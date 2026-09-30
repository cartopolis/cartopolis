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
