//! Owning a building: resolving a typed address against the Dutch address
//! register to the building it is in, and recording who owns which building.
//!
//! An account may own several buildings and a building has one owner
//! (`V25__building_owners.sql` says why it is the building and not the
//! address). Ownership is what atlas asks about before it lets anybody save a
//! building design (`/api/buildings/{pand}/access`): the design is atlas's, the
//! answer to "may this person change it" is this server's.
//!
//! # Two registers, because neither one answers the whole question
//!
//! What a person types is an address, and what the client draws is a building.
//! Joining them takes two keyless PDOK services and there is no way around it:
//!
//!  * **Locatieserver** turns free text into addresses. It is the only one of the
//!    two that is any good at "grote markt 1" — it is a search index, with
//!    tolerance for spelling, ordering and a missing postcode — and it hands back
//!    a `nummeraanduiding_id` (the address) and an `adresseerbaarobject_id` (the
//!    unit behind it). It does **not** carry the building.
//!  * **The BAG WFS** does. One `bag:verblijfsobject` row per unit, carrying
//!    `pandidentificatie`, which is the same `identificatie` atlas's `pipeline/lod22.rs`
//!    writes into the tiles and `systems::building_facts` reads back. That is the
//!    join, and it is an exact string match rather than a geometry test.
//!
//! So a search is one request and a claim is two. The second one is on the claim
//! rather than the search on purpose: it is a request per *candidate* if it
//! happens during the search, and the person is going to pick exactly one.
//!
//! # Why the server does the lookups and not the browser
//!
//! Three reasons, in order of how much they matter. The availability of an
//! address is a fact only this server holds, so the results have to come past it
//! anyway. The municipality limit is a rule, and a rule enforced by the page that
//! draws the form is not one. And a claim must re-resolve the address itself
//! rather than trust the ids the browser sends back — otherwise the id is simply
//! a field an attacker fills in, and "Groningen only" holds for everyone who did
//! not read this comment.

use anyhow::{anyhow, Context};
use rusqlite::{Connection, OptionalExtension};
use std::time::Duration;
use uuid::Uuid;

/// Free-text address search. `q` is what the person typed.
const LOCATIESERVER: &str = "https://api.pdok.nl/bzk/locatieserver/search/v3_1/free";
/// One row per addressable unit, carrying the building it belongs to.
const BAG_WFS: &str = "https://service.pdok.nl/lv/bag/wfs/v2_0";

/// Candidates returned for one query. Enough to choose from on a phone, few
/// enough that the list is a list rather than a scroll.
const MAX_RESULTS: usize = 8;

/// Both services are somebody else's, reached over the public internet, and are
/// being waited on inside a request. Short, and a failure is reported as such
/// rather than retried — the person is looking at a form and can press the button
/// again, which is a better retry than one this code invents.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(HTTP_TIMEOUT)
        .timeout_read(HTTP_TIMEOUT)
        .user_agent(concat!(
            "cartopolis-simulator/",
            env!("CARGO_PKG_VERSION"),
            " (+https://cartopolis.org)"
        ))
        .build()
}

/// One address a person could claim.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Address {
    /// The BAG nummeraanduiding: the address itself, and the primary key of a
    /// claim.
    pub nummeraanduiding_id: String,
    /// The BAG verblijfsobject: the unit at that address, and the key the
    /// building is looked up by.
    pub adresseerbaarobject_id: String,
    /// As the register renders it, which is the form the person recognises.
    pub address: String,
    pub postcode: Option<String>,
    pub municipality: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// `POINT(6.56719613 53.21856902)` — longitude first, which is the trap.
fn parse_point(wkt: &str) -> Option<(f64, f64)> {
    let inner = wkt.trim().strip_prefix("POINT(")?.strip_suffix(')')?;
    let mut parts = inner.split_whitespace();
    let lng: f64 = parts.next()?.parse().ok()?;
    let lat: f64 = parts.next()?.parse().ok()?;
    Some((lat, lng))
}

fn field(doc: &serde_json::Value, key: &str) -> Option<String> {
    doc.get(key)?.as_str().map(str::to_string)
}

/// Ask Locatieserver for addresses matching `query`, restricted to one
/// municipality.
///
/// **Blocking.** Called from `spawn_blocking`; `ureq` is what the rest of this
/// workspace already fetches with, and an async client for two requests would be
/// a second TLS stack in the tree to save two threads.
pub fn search(query: &str, municipality: &str) -> anyhow::Result<Vec<Address>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let response = agent()
        .get(LOCATIESERVER)
        .query("q", query)
        .query("fq", "type:adres")
        // A *narrowing* filter and not the rule. `gemeentenaam` is a tokenised
        // Solr field, so `gemeentenaam:"Groningen"` also matches
        // **Midden-Groningen** — a different municipality forty kilometres away,
        // whose addresses came back looking exactly like local ones. The rule is
        // the exact comparison below; this only keeps the query cheap. The quotes
        // still matter for a name with a space in it ("Bergen op Zoom" parses as
        // three clauses without them).
        .query("fq", &format!("gemeentenaam:\"{municipality}\""))
        // Over-fetch, because that filter lets neighbours through and they are
        // dropped afterwards. Asking for eight would return two.
        .query("rows", &(MAX_RESULTS * 4).to_string())
        .query(
            "fl",
            "id,weergavenaam,postcode,gemeentenaam,centroide_ll,\
             nummeraanduiding_id,adresseerbaarobject_id",
        )
        .call()
        .context("locatieserver")?
        .into_string()
        .context("locatieserver body")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&response).context("locatieserver json")?;
    let docs = parsed
        .get("response")
        .and_then(|r| r.get("docs"))
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("locatieserver returned no docs array"))?;

    let mut out = Vec::new();
    for doc in docs {
        // The municipality rule, applied to every candidate by exact name. See
        // the `fq` above for why the query cannot be trusted to have done it.
        if !field(doc, "gemeentenaam").is_some_and(|name| name.eq_ignore_ascii_case(municipality)) {
            continue;
        }
        // Every other field here is required, and a doc missing one is skipped
        // rather than defaulted: a candidate with no address id is one that
        // cannot be claimed, and offering it would be offering a button that
        // fails.
        let (Some(nummeraanduiding_id), Some(adresseerbaarobject_id), Some(address)) = (
            field(doc, "nummeraanduiding_id"),
            field(doc, "adresseerbaarobject_id"),
            field(doc, "weergavenaam"),
        ) else {
            continue;
        };
        let Some((latitude, longitude)) =
            field(doc, "centroide_ll").as_deref().and_then(parse_point)
        else {
            continue;
        };
        out.push(Address {
            nummeraanduiding_id,
            adresseerbaarobject_id,
            address,
            postcode: field(doc, "postcode"),
            municipality: field(doc, "gemeentenaam").unwrap_or_default(),
            latitude,
            longitude,
        });
        if out.len() == MAX_RESULTS {
            break;
        }
    }
    Ok(out)
}

/// The one address with this id, straight from the register.
///
/// The claim path's re-resolution: a claim names an id, and this is what turns
/// that id back into an address and a municipality the server has checked itself.
pub fn lookup(nummeraanduiding_id: &str, municipality: &str) -> anyhow::Result<Option<Address>> {
    // Locatieserver's `lookup` is keyed by its own `id`, not by a BAG id, so the
    // way to ask "which address is this nummeraanduiding?" is a search over the
    // indexed field. Exactly one row can match — it is a primary key in the
    // register.
    let found = search_field("nummeraanduiding_id", nummeraanduiding_id)?;
    Ok(found.into_iter().find(|a| {
        a.nummeraanduiding_id == nummeraanduiding_id
            && a.municipality.eq_ignore_ascii_case(municipality)
    }))
}

fn search_field(field_name: &str, value: &str) -> anyhow::Result<Vec<Address>> {
    // The value is a register id; refuse anything that is not, rather than
    // interpolating a caller's string into a Solr query.
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Vec::new());
    }
    let response = agent()
        .get(LOCATIESERVER)
        .query("q", "*")
        .query("fq", "type:adres")
        .query("fq", &format!("{field_name}:{value}"))
        .query("rows", "1")
        .query(
            "fl",
            "id,weergavenaam,postcode,gemeentenaam,centroide_ll,\
             nummeraanduiding_id,adresseerbaarobject_id",
        )
        .call()
        .context("locatieserver lookup")?
        .into_string()
        .context("locatieserver lookup body")?;
    let parsed: serde_json::Value = serde_json::from_str(&response)?;
    let docs = parsed
        .get("response")
        .and_then(|r| r.get("docs"))
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for doc in &docs {
        let (Some(na), Some(ao), Some(address)) = (
            field(doc, "nummeraanduiding_id"),
            field(doc, "adresseerbaarobject_id"),
            field(doc, "weergavenaam"),
        ) else {
            continue;
        };
        let Some((latitude, longitude)) =
            field(doc, "centroide_ll").as_deref().and_then(parse_point)
        else {
            continue;
        };
        out.push(Address {
            nummeraanduiding_id: na,
            adresseerbaarobject_id: ao,
            address,
            postcode: field(doc, "postcode"),
            municipality: field(doc, "gemeentenaam").unwrap_or_default(),
            latitude,
            longitude,
        });
    }
    Ok(out)
}

/// The BAG building an addressable unit belongs to.
///
/// `None` means the register knows the address and it has no building: a berth
/// (`ligplaats`) or a pitch (`standplaats`). Those are real addresses with real
/// people at them and no 3DBAG solid, so there is nothing on the map to claim —
/// the caller reports that rather than storing a claim against a building that
/// does not exist.
pub fn pand_for(adresseerbaarobject_id: &str) -> anyhow::Result<Option<String>> {
    if !adresseerbaarobject_id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    // FES 2.0, not `CQL_FILTER`: PDOK's WFS accepts the CQL parameter and
    // **ignores it**, answering with the first page of the whole layer. A filter
    // that is quietly dropped is worse than one that errors — the first version
    // of this claimed a house in Appingedam for an address in Groningen.
    let filter = format!(
        "<fes:Filter xmlns:fes=\"http://www.opengis.net/fes/2.0\">\
           <fes:PropertyIsEqualTo>\
             <fes:ValueReference>identificatie</fes:ValueReference>\
             <fes:Literal>{adresseerbaarobject_id}</fes:Literal>\
           </fes:PropertyIsEqualTo>\
         </fes:Filter>"
    );
    let response = agent()
        .get(BAG_WFS)
        .query("service", "WFS")
        .query("version", "2.0.0")
        .query("request", "GetFeature")
        .query("typeNames", "bag:verblijfsobject")
        .query("outputFormat", "application/json")
        .query("count", "1")
        .query("srsName", "EPSG:4326")
        .query("filter", &filter)
        .call()
        .context("bag wfs")?
        .into_string()
        .context("bag wfs body")?;
    let parsed: serde_json::Value = serde_json::from_str(&response).context("bag wfs json")?;
    let pand = parsed
        .get("features")
        .and_then(|f| f.as_array())
        .and_then(|f| f.first())
        .and_then(|f| f.get("properties"))
        .and_then(|p| p.get("pandidentificatie"))
        .and_then(|p| p.as_str())
        .map(str::to_string);
    Ok(pand)
}
// ---------------------------------------------------------------------------
// Owning a building
// ---------------------------------------------------------------------------

/// One building somebody owns, as the API reports it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Ownership {
    /// The 3DBAG/BAG building id: the key.
    pub pand_id: String,
    /// The address the building was claimed through, and the one shown for it.
    pub nummeraanduiding_id: String,
    pub address: String,
    pub postcode: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    pub claimed_at: String,
    /// Who owns it. Present on the admin listing, where the operator is
    /// resolving a dispute and needs to know; an owner's own view has no use
    /// for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// What [`claim`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Claimed {
    Ok,
    /// Somebody else owns that building.
    Taken,
    /// The caller already owns it.
    AlreadyYours,
    /// The caller already owns as many as an account may.
    AtLimit {
        limit: usize,
    },
}

/// Record that `principal` owns the building at `address`.
///
/// "Somebody else has it" is the database's rule — the building is a primary
/// key, so two simultaneous claims cannot both win. The per-account limit is
/// checked here instead, inside the same connection the insert uses; a race
/// between one account's own two requests can exceed it by one, which is a
/// cosmetic overrun of a fairness setting rather than a security property.
pub fn claim(
    conn: &Connection,
    principal: Uuid,
    address: &Address,
    pand_id: &str,
    limit: usize,
) -> anyhow::Result<Claimed> {
    if let Some(owner) = owner_of(conn, pand_id)? {
        return Ok(if owner == principal.to_string() {
            Claimed::AlreadyYours
        } else {
            Claimed::Taken
        });
    }
    if owned_by(conn, principal)?.len() >= limit {
        return Ok(Claimed::AtLimit { limit });
    }
    let n = conn
        .execute(
            "INSERT OR IGNORE INTO building_owners
         (pand_id, principal, nummeraanduiding_id, address, postcode, latitude, longitude)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                pand_id,
                principal.to_string(),
                address.nummeraanduiding_id,
                address.address,
                address.postcode,
                address.latitude,
                address.longitude,
            ],
        )
        .context("insert building owner")?;
    Ok(if n > 0 { Claimed::Ok } else { Claimed::Taken })
}

const OWNER_COLUMNS: &str =
    "pand_id, nummeraanduiding_id, address, postcode, latitude, longitude, claimed_at";

fn row_to_ownership(r: &rusqlite::Row<'_>) -> rusqlite::Result<Ownership> {
    Ok(Ownership {
        pand_id: r.get(0)?,
        nummeraanduiding_id: r.get(1)?,
        address: r.get(2)?,
        postcode: r.get(3)?,
        latitude: r.get(4)?,
        longitude: r.get(5)?,
        claimed_at: r.get(6)?,
        username: None,
    })
}

/// The principal that owns a building, if anybody does.
pub fn owner_of(conn: &Connection, pand_id: &str) -> anyhow::Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT principal FROM building_owners WHERE pand_id = ?1",
            [pand_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// One building's ownership row, with the owner's name.
pub fn ownership(conn: &Connection, pand_id: &str) -> anyhow::Result<Option<Ownership>> {
    let sql = format!(
        "SELECT {}, accounts.username FROM building_owners
         LEFT JOIN accounts ON accounts.id = building_owners.principal
         WHERE building_owners.pand_id = ?1",
        qualified()
    );
    Ok(conn
        .query_row(&sql, [pand_id], |r| {
            let mut o = row_to_ownership(r)?;
            o.username = r.get(7)?;
            Ok(o)
        })
        .optional()?)
}

/// The buildings this account owns, oldest claim first.
pub fn owned_by(conn: &Connection, principal: Uuid) -> anyhow::Result<Vec<Ownership>> {
    let sql = format!(
        "SELECT {OWNER_COLUMNS} FROM building_owners WHERE principal = ?1 ORDER BY claimed_at, pand_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([principal.to_string()], row_to_ownership)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Which of these addresses are in a building somebody already owns.
///
/// Only as good as the address a building was claimed *through*: the search
/// happens before the building behind each address is looked up (that is a
/// request per candidate), so another flat in an owned block reads as free here
/// and is refused at the claim, which looks the building up.
pub fn taken(
    conn: &Connection,
    ids: &[String],
) -> anyhow::Result<std::collections::HashSet<String>> {
    if ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT nummeraanduiding_id FROM building_owners WHERE nummeraanduiding_id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(ids), |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
    Ok(rows)
}

/// Give a building up. Only its owner can — `principal` is part of the match.
/// Returns whether there was one to give up.
pub fn release(conn: &Connection, principal: Uuid, pand_id: &str) -> anyhow::Result<bool> {
    let n = conn.execute(
        "DELETE FROM building_owners WHERE principal = ?1 AND pand_id = ?2",
        rusqlite::params![principal.to_string(), pand_id],
    )?;
    Ok(n > 0)
}

/// Release by building, whoever owns it: the operator's handle.
pub fn release_building(conn: &Connection, pand_id: &str) -> anyhow::Result<bool> {
    let n = conn.execute("DELETE FROM building_owners WHERE pand_id = ?1", [pand_id])?;
    Ok(n > 0)
}

fn qualified() -> String {
    OWNER_COLUMNS
        .split(", ")
        .map(|c| format!("building_owners.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every building with an owner, newest claim first, with the owner's name.
pub fn list(conn: &Connection) -> anyhow::Result<Vec<Ownership>> {
    let sql = format!(
        "SELECT {}, accounts.username FROM building_owners
         LEFT JOIN accounts ON accounts.id = building_owners.principal
         ORDER BY claimed_at DESC",
        qualified()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            let mut o = row_to_ownership(r)?;
            o.username = r.get(7)?;
            Ok(o)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Free every building an account owned, for the account-erasure paths. A
/// building owned by an account that no longer exists is one nobody can edit
/// and nobody can claim.
pub fn erase_for(conn: &Connection, principal: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM building_owners WHERE principal = ?1",
        [principal],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate(&mut conn).unwrap();
        conn
    }

    fn address(id: &str) -> Address {
        Address {
            nummeraanduiding_id: id.into(),
            adresseerbaarobject_id: "0014010011065378".into(),
            address: format!("Grote Markt {id}, 9712HN Groningen"),
            postcode: Some("9712HN".into()),
            municipality: "Groningen".into(),
            latitude: 53.218_569,
            longitude: 6.567_196,
        }
    }

    #[test]
    fn a_point_is_longitude_first() {
        assert_eq!(
            parse_point("POINT(6.56719613 53.21856902)"),
            Some((53.218_569_02, 6.567_196_13))
        );
        assert_eq!(parse_point("53.2, 6.5"), None);
        assert_eq!(parse_point("POINT(6.5)"), None);
    }

    #[test]
    fn one_building_has_one_owner_and_an_owner_has_several() {
        let conn = db();
        let ada = Uuid::new_v4();
        let grace = Uuid::new_v4();

        assert_eq!(
            claim(&conn, ada, &address("1"), "pand-a", 3).unwrap(),
            Claimed::Ok
        );
        // The same building through another of its addresses: a block of flats
        // has one owner now.
        assert_eq!(
            claim(&conn, grace, &address("2"), "pand-a", 3).unwrap(),
            Claimed::Taken
        );
        assert_eq!(
            claim(&conn, ada, &address("2"), "pand-a", 3).unwrap(),
            Claimed::AlreadyYours
        );
        // A second and a third building for the same person, and not a fourth.
        assert_eq!(
            claim(&conn, ada, &address("3"), "pand-b", 3).unwrap(),
            Claimed::Ok
        );
        assert_eq!(
            claim(&conn, ada, &address("4"), "pand-c", 3).unwrap(),
            Claimed::Ok
        );
        assert_eq!(
            claim(&conn, ada, &address("5"), "pand-d", 3).unwrap(),
            Claimed::AtLimit { limit: 3 }
        );

        let held: Vec<String> = owned_by(&conn, ada)
            .unwrap()
            .into_iter()
            .map(|o| o.pand_id)
            .collect();
        assert_eq!(held, ["pand-a", "pand-b", "pand-c"]);
        assert_eq!(owner_of(&conn, "pand-a").unwrap(), Some(ada.to_string()));
        assert_eq!(
            taken(&conn, &["1".to_string(), "2".to_string(), "3".to_string()]).unwrap(),
            ["1".to_string(), "3".to_string()].into_iter().collect()
        );

        // Only the owner can give one up, and only that one.
        assert!(!release(&conn, grace, "pand-a").unwrap());
        assert!(release(&conn, ada, "pand-a").unwrap());
        assert!(!release(&conn, ada, "pand-a").unwrap());
        assert_eq!(owned_by(&conn, ada).unwrap().len(), 2);
        assert_eq!(
            claim(&conn, grace, &address("2"), "pand-a", 3).unwrap(),
            Claimed::Ok
        );
    }

    #[test]
    fn the_admin_listing_names_the_owner() {
        let conn = db();
        let ada =
            crate::auth::create_account_with_role(&conn, "ada", "hunter2long", false).unwrap();
        claim(&conn, ada, &address("1"), "pand-a", 10).unwrap();
        let listed = list(&conn).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].username.as_deref(), Some("ada"));
        assert_eq!(
            ownership(&conn, "pand-a")
                .unwrap()
                .unwrap()
                .username
                .as_deref(),
            Some("ada")
        );

        assert!(release_building(&conn, "pand-a").unwrap());
        assert!(list(&conn).unwrap().is_empty());
    }

    #[test]
    fn deleting_an_account_frees_its_buildings() {
        let conn = db();
        let ada =
            crate::auth::create_account_with_role(&conn, "ada", "hunter2long", false).unwrap();
        claim(&conn, ada, &address("1"), "pand-a", 10).unwrap();
        claim(&conn, ada, &address("2"), "pand-b", 10).unwrap();
        crate::db::erase_account_traces(&conn, &ada.to_string(), "ada").unwrap();
        assert!(list(&conn).unwrap().is_empty());
    }

    /// The V12 → V25 migration: every house claim becomes its building, and
    /// where two people held flats in one building the earlier claim keeps it.
    #[test]
    fn house_claims_become_building_owners() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::migrate_to(&mut conn, 24).unwrap();
        conn.execute_batch(
            "INSERT INTO house_claims (nummeraanduiding_id, principal, pand_id, address, latitude, longitude, claimed_at)
             VALUES ('1', 'p-ada', 'pand-a', 'A 1', 53.2, 6.5, '2026-08-01 10:00:00'),
                    ('2', 'p-grace', 'pand-a', 'A 2', 53.2, 6.5, '2026-08-02 10:00:00'),
                    ('3', 'p-hopper', 'pand-b', 'B 1', 53.2, 6.5, '2026-08-03 10:00:00');",
        )
        .unwrap();
        crate::db::migrate(&mut conn).unwrap();
        assert_eq!(owner_of(&conn, "pand-a").unwrap().as_deref(), Some("p-ada"));
        assert_eq!(
            owner_of(&conn, "pand-b").unwrap().as_deref(),
            Some("p-hopper")
        );
    }

    /// Both services are somebody else's and this reaches them, so it is
    /// `#[ignore]` — run it by hand when the join stops working:
    /// `cargo test -p cartopolis_simulator groningen -- --ignored --nocapture`.
    #[test]
    #[ignore = "reaches PDOK over the network"]
    fn a_groningen_address_resolves_to_a_3dbag_building() {
        let found = search("Grote Markt 1", "Groningen").unwrap();
        let first = found.first().expect("Grote Markt 1 exists");
        assert!(first.address.starts_with("Grote Markt 1"));
        assert_eq!(first.municipality, "Groningen");
        let pand = pand_for(&first.adresseerbaarobject_id).unwrap();
        assert_eq!(pand.as_deref(), Some("0014100010934570"), "the stadhuis");

        // The municipality limit holds against an address outside it.
        assert!(search("Dam 1", "Groningen")
            .unwrap()
            .iter()
            .all(|a| a.municipality == "Groningen"));
        // And a re-resolution of an id outside the municipality finds nothing.
        assert!(lookup(&first.nummeraanduiding_id, "Amsterdam")
            .unwrap()
            .is_none());
        assert!(lookup(&first.nummeraanduiding_id, "Groningen")
            .unwrap()
            .is_some());
    }
}
