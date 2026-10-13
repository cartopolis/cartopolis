//! The ActivityStreams JSON this server writes, and the little it needs to read
//! out of what other servers send.
//!
//! Written as `serde_json::Value`s rather than typed structs: the fediverse is
//! a family of dialects (Mastodon, Lemmy, Pleroma, Pixelfed, …) that agree on a
//! core and differ at the edges, and a reader here must take whatever shape an
//! object arrives in — a field as a string or an array, an object inline or as
//! its id — without refusing the whole document over one of them.
//!
//! A post is a `Note` with a `location` (`Place`), the same object a point in a
//! private chat already is (`cartopolis_core::point`), plus what makes it a post
//! in a group: `audience` and a `cc` naming the community, and a `name` so Lemmy
//! has a title for it. Mastodon ignores the `name` of a `Note` and shows its
//! `content`, which ends with a link that opens the map at the post's view.

use cartopolis_core::point::{rfc3339_utc, PointView};
use cartopolis_core::posts::{PostPlace, WhoPosts};
use serde_json::{json, Value};
use uuid::Uuid;

use super::posts::PostRow;
use super::{group_url, key_id, person_url, place_link, post_url};

/// Who may read something addressed to it: everybody.
pub const PUBLIC: &str = "https://www.w3.org/ns/activitystreams#Public";

/// The `@context` of everything this server writes: ActivityStreams, the
/// security vocabulary its keys are written in, and our one extension term.
#[must_use]
pub fn context() -> Value {
    json!([
        "https://www.w3.org/ns/activitystreams",
        "https://w3id.org/security/v1",
        {
            "cartopolis": "https://cartopolis.org/ns#",
            "Hashtag": "as:Hashtag",
            "sensitive": "as:sensitive",
            "postingRestrictedToMods": "https://join-lemmy.org/ns#postingRestrictedToMods"
        }
    ])
}

/// `object` with the context added, ready to serve or send.
#[must_use]
pub fn document(mut object: Value) -> Value {
    if let Some(map) = object.as_object_mut() {
        map.insert("@context".into(), context());
    }
    object
}

fn key(actor: &str, pem: &str) -> Value {
    json!({ "id": key_id(actor), "owner": actor, "publicKeyPem": pem })
}

/// An account here, as a `Person`. Its id carries the account's id, not its
/// name, so it means one account for good.
#[must_use]
pub fn person(base: &str, account: &str, username: &str, pem: &str) -> Value {
    let id = person_url(base, account);
    json!({
        "id": id,
        "type": "Person",
        "preferredUsername": username,
        "name": username,
        "inbox": format!("{id}/inbox"),
        "outbox": format!("{id}/outbox"),
        "followers": format!("{id}/followers"),
        "endpoints": { "sharedInbox": format!("{base}/ap/inbox") },
        "manuallyApprovesFollowers": false,
        "discoverable": true,
        "url": id,
        "publicKey": key(&id, pem),
    })
}

/// What a community's actor says about it.
pub struct GroupFacts<'a> {
    pub community: Uuid,
    pub handle: &'a str,
    pub name: &'a str,
    pub about: &'a str,
    pub picture: Option<&'a str>,
    pub place: Option<(f64, f64, Option<&'a str>)>,
    pub who_posts: WhoPosts,
    pub pem: &'a str,
    pub created_ms: i64,
}

/// A community here, as a `Group`: what Lemmy communities are, and what
/// Mastodon follows and shows boosts from.
#[must_use]
pub fn group(base: &str, g: &GroupFacts<'_>) -> Value {
    let id = group_url(base, g.community);
    let mut v = json!({
        "id": id,
        "type": "Group",
        "preferredUsername": g.handle,
        "name": g.name,
        "summary": escape_html(g.about),
        "inbox": format!("{id}/inbox"),
        "outbox": format!("{id}/outbox"),
        "followers": format!("{id}/followers"),
        "attributedTo": format!("{id}/moderators"),
        "endpoints": { "sharedInbox": format!("{base}/ap/inbox") },
        "manuallyApprovesFollowers": false,
        "discoverable": true,
        "postingRestrictedToMods": g.who_posts == WhoPosts::Moderators,
        "published": rfc3339_utc(u64::try_from(g.created_ms).unwrap_or(0)),
        "url": id,
        "publicKey": key(&id, g.pem),
    });
    if let Some(hash) = g.picture {
        v["icon"] = json!({
            "type": "Image",
            "mediaType": "image/jpeg",
            "url": format!("{base}/ap/media/{hash}"),
        });
    }
    if let Some((lat, lng, name)) = g.place {
        v["location"] = json!({
            "type": "Place",
            "name": name,
            "latitude": lat,
            "longitude": lng,
        });
    }
    v
}

/// This server itself, as an `Application`: the actor that signs a request
/// nobody in particular makes.
#[must_use]
pub fn instance(base: &str, server: &str, pem: &str) -> Value {
    let id = super::instance_url(base);
    json!({
        "id": id,
        "type": "Application",
        "preferredUsername": server,
        "name": "Cartopolis",
        "inbox": format!("{base}/ap/inbox"),
        "outbox": format!("{id}/outbox"),
        "manuallyApprovesFollowers": true,
        "url": base,
        "publicKey": key(&id, pem),
    })
}

/// `&`, `<`, `>`, `"` and `'` as entities.
#[must_use]
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Where a hashtag `#word` written at `i` ends, if one does: a `#` not glued
/// to the word before it, then letters, digits and `_`, at least one of them a
/// letter (`#1` is a number, not a tag) — Mastodon's own rule.
fn hashtag_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'#') {
        return None;
    }
    if i > 0 && (chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '&' | '#' | '/')) {
        return None;
    }
    let mut end = i + 1;
    while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
        end += 1;
    }
    (end > i + 1 && chars[i + 1..end].iter().any(|c| c.is_alphabetic())).then_some(end)
}

/// The hashtags in a text, without their `#`, each once, in order.
#[must_use]
pub fn hashtags(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = hashtag_end(&chars, i) {
            let tag: String = chars[i + 1..end].iter().collect();
            if !out.iter().any(|t| t.to_lowercase() == tag.to_lowercase()) {
                out.push(tag);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Where a hashtag links: this server's page for it. A Mastodon reader is sent
/// to their own server's timeline for the tag instead, which it finds by the
/// post's `Hashtag` tag.
fn tag_url(base: &str, tag: &str) -> String {
    format!("{base}/tags/{}", tag.to_lowercase())
}

/// Plain text as HTML paragraphs, the form every server's `content` takes, with
/// each hashtag a link as Mastodon writes one.
#[must_use]
pub fn text_to_html(text: &str, base: &str) -> String {
    let mut out = String::new();
    for p in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        out.push_str("<p>");
        let chars: Vec<char> = p.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if let Some(end) = hashtag_end(&chars, i) {
                let tag: String = chars[i + 1..end].iter().collect();
                out.push_str(&format!(
                    "<a href=\"{}\" class=\"mention hashtag\" rel=\"tag\">#<span>{}</span></a>",
                    escape_html(&tag_url(base, &tag)),
                    escape_html(&tag)
                ));
                i = end;
                continue;
            }
            match chars[i] {
                '\n' => out.push_str("<br>"),
                c => out.push_str(&escape_html(&c.to_string())),
            }
            i += 1;
        }
        out.push_str("</p>");
    }
    out
}

/// The thread a reply belongs to, as the reply's object names it.
pub struct ReplyTo<'a> {
    /// The fediverse id of the post or reply it answers.
    pub ap_id: &'a str,
    /// Who wrote that, so their server is told and they are mentioned.
    pub author: Option<&'a str>,
}

/// The fediverse id of the author of a row.
#[must_use]
pub fn author_url(base: &str, row: &PostRow) -> String {
    match &row.author_account {
        Some(a) => person_url(base, a),
        None => row.author_actor.clone().unwrap_or_default(),
    }
}

/// A post or reply written here, as a `Note`.
#[must_use]
pub fn note(
    base: &str,
    row: &PostRow,
    handle: &str,
    server: &str,
    reply_to: Option<&ReplyTo<'_>>,
) -> Value {
    let author = author_url(base, row);
    let group = group_url(base, row.community);
    let mut cc = vec![json!(group), json!(format!("{author}/followers"))];
    let mut tag = vec![json!({
        "type": "Mention",
        "href": group,
        "name": format!("@{handle}@{server}"),
    })];
    if let Some(parent_author) = reply_to.and_then(|r| r.author) {
        if parent_author != author {
            cc.push(json!(parent_author));
            tag.push(json!({ "type": "Mention", "href": parent_author }));
        }
    }
    // Hashtags: a link in the text and a `Hashtag` tag, both of which Mastodon
    // needs before it makes one clickable and files the post under it.
    for t in hashtags(&row.text) {
        tag.push(json!({
            "type": "Hashtag",
            "href": tag_url(base, &t),
            "name": format!("#{}", t.to_lowercase()),
        }));
    }
    let mut content = text_to_html(&row.text, base);
    let link = row.place.as_ref().map(|p| place_link(base, p));
    if let (Some(place), Some(link)) = (&row.place, &link) {
        let label = place.name.as_deref().unwrap_or("Open on the map");
        content.push_str(&format!(
            "<p>📍 <a href=\"{}\">{}</a></p>",
            escape_html(link),
            escape_html(label)
        ));
    }
    let mut v = json!({
        "id": row.ap_id,
        "type": "Note",
        "attributedTo": author,
        "to": [PUBLIC],
        "cc": cc,
        "audience": group,
        "content": content,
        "mediaType": "text/html",
        "source": { "content": row.text, "mediaType": "text/plain" },
        "published": rfc3339_utc(u64::try_from(row.created_ms).unwrap_or(0)),
        "tag": tag,
        "sensitive": false,
    });
    if let Some(edited) = row.edited_ms {
        v["updated"] = json!(rfc3339_utc(u64::try_from(edited).unwrap_or(0)));
    }
    match reply_to {
        Some(r) => v["inReplyTo"] = json!(r.ap_id),
        // A post's first line is its title on Lemmy, which has no posts
        // without one. Mastodon shows `content` and ignores this.
        None => v["name"] = json!(title(&row.text)),
    }
    if let Some(link) = link {
        v["url"] = json!(link);
    }
    if let Some(place) = &row.place {
        v["location"] = json!({
            "type": "Place",
            "name": place.name,
            "latitude": place.latitude,
            "longitude": place.longitude,
        });
        if let Some(view) = place.view {
            v["cartopolis:view"] = json!(view);
        }
    }
    // The snapshot first: it is what a post written here is about, and the
    // first attachment is the one a timeline shows largest.
    let mut attachments = Vec::new();
    if let Some(hash) = &row.snapshot {
        attachments.push(json!({
            "type": "Image",
            "mediaType": "image/jpeg",
            "url": format!("{base}/ap/media/{hash}"),
            "name": snapshot_alt(row.place.as_ref()),
        }));
    }
    if let Some(hash) = &row.picture {
        attachments.push(json!({
            "type": "Image",
            "mediaType": "image/jpeg",
            "url": format!("{base}/ap/media/{hash}"),
            "name": place_alt(row.place.as_ref()),
        }));
    }
    if !attachments.is_empty() {
        v["attachment"] = Value::Array(attachments);
    }
    v
}

/// What a snapshot shows, for somebody who cannot see it.
fn snapshot_alt(place: Option<&PostPlace>) -> String {
    match place.and_then(|p| p.name.as_deref()) {
        Some(n) => format!("A snapshot from Cartopolis: the 3D map at {n}"),
        None => "A snapshot from Cartopolis: the 3D map where this was written".into(),
    }
}

fn place_alt(place: Option<&PostPlace>) -> String {
    match place.and_then(|p| p.name.as_deref()) {
        Some(n) => format!("A picture at {n}"),
        None => "A picture".into(),
    }
}

/// The first line of a text, at most 200 characters: a post's title on Lemmy.
#[must_use]
pub fn title(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Post");
    let t: String = line.trim().chars().take(200).collect();
    if t.is_empty() {
        "Post".into()
    } else {
        t
    }
}

/// `actor` did `kind` to `object`, addressed like the object is.
#[must_use]
pub fn activity(kind: &str, id: &str, actor: &str, object: Value) -> Value {
    let to = object.get("to").cloned().unwrap_or(json!([PUBLIC]));
    let cc = object.get("cc").cloned().unwrap_or(json!([]));
    let mut v = json!({
        "id": id,
        "type": kind,
        "actor": actor,
        "to": to,
        "cc": cc,
        "object": object,
    });
    if let Some(audience) = v["object"].get("audience").cloned() {
        v["audience"] = audience;
    }
    v
}

/// A community boosts something to its followers: an activity, Lemmy's way
/// (FEP-1b12), or a bare object id, Mastodon's.
#[must_use]
pub fn announce(base: &str, community: Uuid, object: Value) -> Value {
    let group = group_url(base, community);
    json!({
        "id": format!("{group}/announce/{}", Uuid::new_v4()),
        "type": "Announce",
        "actor": group,
        "to": [PUBLIC],
        "cc": [format!("{group}/followers")],
        "audience": group,
        "object": object,
    })
}

/// `actor` follows `object`.
#[must_use]
pub fn follow(id: &str, actor: &str, object: &str) -> Value {
    json!({ "id": id, "type": "Follow", "actor": actor, "object": object })
}

/// `actor` takes back an earlier activity.
#[must_use]
pub fn undo(actor: &str, object: Value) -> Value {
    json!({
        "id": format!("{actor}#undo-{}", Uuid::new_v4()),
        "type": "Undo",
        "actor": actor,
        "object": object,
    })
}

/// `actor` accepts (or rejects) a follow.
#[must_use]
pub fn answer_follow(kind: &str, actor: &str, follow: Value) -> Value {
    json!({
        "id": format!("{actor}#{}-{}", kind.to_ascii_lowercase(), Uuid::new_v4()),
        "type": kind,
        "actor": actor,
        "object": follow,
    })
}

/// A post here is gone.
#[must_use]
pub fn delete(base: &str, row: &PostRow) -> Value {
    let author = author_url(base, row);
    json!({
        "id": format!("{}#delete", row.ap_id),
        "type": "Delete",
        "actor": author,
        "to": [PUBLIC],
        "cc": [group_url(base, row.community)],
        "audience": group_url(base, row.community),
        "object": { "id": row.ap_id, "type": "Tombstone" },
    })
}

/// The id of something given as an id or as an object carrying one.
#[must_use]
pub fn id_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(_) => v.get("id").and_then(Value::as_str).map(str::to_string),
        Value::Array(a) => a.first().and_then(id_of),
        _ => None,
    }
}

/// The ids in a field that may be one id, one object or a list of either.
#[must_use]
pub fn ids(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(id_of).collect(),
        Some(v) => id_of(v).into_iter().collect(),
        None => Vec::new(),
    }
}

/// The `type` of an object, the first one when there are several.
#[must_use]
pub fn kind(v: &Value) -> &str {
    match v.get("type") {
        Some(Value::String(s)) => s,
        Some(Value::Array(a)) => a.first().and_then(Value::as_str).unwrap_or(""),
        _ => "",
    }
}

/// Everybody an object or activity is addressed to or mentions: `to`, `cc`,
/// `audience` and the `Mention` tags.
#[must_use]
pub fn addressees(v: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for field in ["to", "cc", "audience", "bto", "bcc"] {
        out.extend(ids(v.get(field)));
    }
    if let Some(tags) = v.get("tag").and_then(Value::as_array) {
        for t in tags {
            if let Some(href) = t.get("href").and_then(Value::as_str) {
                out.push(href.to_string());
            }
        }
    }
    out
}

/// Whether an object is addressed to everybody: Mastodon's Public (`to`) and
/// Unlisted (`cc`) both name the public collection, while followers-only and
/// a private mention do not. Only such an object may be kept here, where a
/// community's posts are shown to anybody.
#[must_use]
pub fn is_public(v: &Value) -> bool {
    ["to", "cc"].iter().any(|field| {
        ids(v.get(*field))
            .iter()
            .any(|a| a == PUBLIC || a == "as:Public" || a == "Public")
    })
}

/// An account or community on another server, as much of it as is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub id: String,
    pub kind: String,
    pub username: String,
    pub host: String,
    pub name: String,
    pub inbox: String,
    pub shared_inbox: Option<String>,
    pub key_id: String,
    pub public_pem: String,
    pub url: Option<String>,
}

/// Read an actor document. `None` when it lacks what this server needs to
/// talk to it: an inbox and a key.
#[must_use]
pub fn read_actor(v: &Value) -> Option<Actor> {
    read_actor_key(v, None)
}

/// Read an actor document, keeping the key named `key_id` when it is given.
///
/// An actor may publish more than one key — `publicKey` as a list — and sign
/// with any of them: mastodon.social signs as `…#rsa-<fingerprint>` (seen
/// 2026-10-13). Taking the first key, and then finding its id did not match
/// the signature's, refused every follow from there.
#[must_use]
pub fn read_actor_key(v: &Value, key_id: Option<&str>) -> Option<Actor> {
    let id = v.get("id")?.as_str()?.to_string();
    let host = super::fetch::host_of(&id)?;
    let keys: Vec<&Value> = match v.get("publicKey")? {
        Value::Array(a) => a.iter().collect(),
        k => vec![k],
    };
    let key = match key_id {
        Some(want) => keys
            .into_iter()
            .find(|k| k.get("id").and_then(Value::as_str) == Some(want))?,
        None => keys.into_iter().next()?,
    };
    let username = v
        .get("preferredUsername")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(Actor {
        kind: kind(v).to_string(),
        name: v
            .get("name")
            .and_then(Value::as_str)
            .map(|n| html_to_text(n).trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| username.clone()),
        username,
        host,
        inbox: v.get("inbox")?.as_str()?.to_string(),
        shared_inbox: v
            .get("endpoints")
            .and_then(|e| e.get("sharedInbox"))
            .and_then(Value::as_str)
            .map(str::to_string),
        key_id: key.get("id")?.as_str()?.to_string(),
        public_pem: key.get("publicKeyPem")?.as_str()?.to_string(),
        url: v.get("url").and_then(id_of),
        id,
    })
}

/// HTML as plain text: tags dropped, breaks and paragraphs kept as newlines,
/// entities decoded. Nothing here ever renders HTML from another server, which
/// is the simplest way there is to make it safe.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..lt]));
        let Some(gt) = rest[lt..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim().to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "br" => out.push('\n'),
            "p" | "div" | "li" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "pre"
                if tag.starts_with('/') =>
            {
                out.push_str("\n\n");
            }
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out.push_str(&decode_entities(rest));
    // Collapse the runs of blank lines the closing tags leave behind.
    let mut lines: Vec<&str> = Vec::new();
    for line in out.lines() {
        let blank = line.trim().is_empty();
        if blank && lines.last().is_none_or(|l| l.trim().is_empty()) {
            continue;
        }
        lines.push(line.trim_end());
    }
    lines.join("\n").trim().to_string()
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&i| i <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of a post from elsewhere: Lemmy's title and body, or a note's
/// content, with a content warning kept in front of what it warns about.
#[must_use]
pub fn object_text(v: &Value) -> String {
    // The author's own words when the server sends them (`source`: this server,
    // Lemmy, Misskey), rather than the HTML made of them, which may carry
    // additions such as our own map link.
    let source = v
        .get("source")
        .filter(|s| {
            s.get("mediaType")
                .and_then(Value::as_str)
                .is_some_and(|m| m == "text/plain" || m == "text/markdown")
        })
        .and_then(|s| s.get("content"))
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string());
    let content = source.unwrap_or_else(|| {
        v.get("content")
            .and_then(Value::as_str)
            .map(html_to_text)
            .unwrap_or_default()
    });
    let mut text = String::new();
    if let Some(cw) = v.get("summary").and_then(Value::as_str) {
        let cw = html_to_text(cw);
        if !cw.is_empty() && kind(v) == "Note" {
            text.push_str(&format!("[{cw}]\n\n"));
        }
    }
    // A Lemmy post (`Page`) has a title as well as a body; a note's `name`, when
    // it has one, is usually its own first line again.
    if let Some(name) = v.get("name").and_then(Value::as_str) {
        let name = html_to_text(name);
        if !name.is_empty() && !content.starts_with(&name) {
            text.push_str(&name);
            if !content.is_empty() {
                text.push_str("\n\n");
            }
        }
    }
    text.push_str(&content);
    text.trim().to_string()
}

/// Where a post from elsewhere was written: its `location`, or a Cartopolis
/// link in it (Mastodon has no place of its own, but a link carries one).
#[must_use]
pub fn object_place(v: &Value) -> Option<PostPlace> {
    let location = match v.get("location") {
        Some(Value::Array(a)) => a.iter().find(|l| kind(l) == "Place"),
        Some(l) if kind(l) == "Place" => Some(l),
        _ => None,
    };
    if let Some(l) = location {
        let num = |f: &str| l.get(f).and_then(Value::as_f64);
        if let (Some(latitude), Some(longitude)) = (num("latitude"), num("longitude")) {
            let view: Option<PointView> = v
                .get("cartopolis:view")
                .and_then(|w| serde_json::from_value(w.clone()).ok());
            let place = PostPlace {
                latitude,
                longitude,
                name: l.get("name").and_then(Value::as_str).map(|n| {
                    n.chars()
                        .take(cartopolis_core::posts::MAX_PLACE_NAME)
                        .collect()
                }),
                view,
            };
            if place.in_range() {
                return Some(place);
            }
        }
    }
    let content = v.get("content").and_then(Value::as_str).unwrap_or_default();
    let mut links: Vec<String> = hrefs(content);
    links.extend(v.get("url").and_then(id_of));
    links.iter().find_map(|l| {
        let l = l.replace("&amp;", "&");
        super::place_from_link(&l)
    })
}

/// Every `href="…"` in a piece of HTML.
fn hrefs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("href=") {
        rest = &rest[i + 5..];
        let quote = rest.chars().next().unwrap_or(' ');
        if quote != '"' && quote != '\'' {
            continue;
        }
        if let Some(end) = rest[1..].find(quote) {
            out.push(rest[1..=end].to_string());
            rest = &rest[end + 1..];
        }
    }
    out
}

/// The first picture attached to an object, if any.
#[must_use]
pub fn image_url(v: &Value) -> Option<String> {
    let mut candidates: Vec<&Value> = Vec::new();
    match v.get("attachment") {
        Some(Value::Array(a)) => candidates.extend(a),
        Some(a) => candidates.push(a),
        None => {}
    }
    match v.get("image") {
        Some(Value::Array(a)) => candidates.extend(a),
        Some(a) => candidates.push(a),
        None => {}
    }
    candidates.into_iter().find_map(|a| {
        let media = a.get("mediaType").and_then(Value::as_str).unwrap_or("");
        let is_image = matches!(kind(a), "Image")
            || media.starts_with("image/")
            || (kind(a) == "Document" && media.is_empty());
        if !is_image {
            return None;
        }
        match a.get("url") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(urls)) => urls.iter().find_map(|u| {
                u.get("href")
                    .and_then(Value::as_str)
                    .or_else(|| u.as_str())
                    .map(str::to_string)
            }),
            Some(Value::Object(o)) => o.get("href").and_then(Value::as_str).map(str::to_string),
            _ => None,
        }
    })
}

/// When an object was published, as Unix milliseconds.
#[must_use]
pub fn published_ms(v: &Value) -> Option<i64> {
    v.get("published")
        .and_then(Value::as_str)
        .and_then(cartopolis_core::point::parse_rfc3339)
        .and_then(|ms| i64::try_from(ms).ok())
}

/// A post here, written as the `Create` its author sends. The `Create`'s id is
/// the post's own plus `#create`: nothing ever fetches it.
#[must_use]
pub fn create(base: &str, note: Value, row: &PostRow) -> Value {
    activity(
        "Create",
        &format!("{}#create", post_url(base, row.id)),
        &author_url(base, row),
        note,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> PostRow {
        PostRow {
            id: Uuid::nil(),
            community: Uuid::nil(),
            parent: None,
            ap_id: "https://c.test/ap/p/00000000-0000-0000-0000-000000000000".into(),
            author_account: Some("11111111-1111-1111-1111-111111111111".into()),
            author_actor: None,
            via_actor: None,
            text: text.into(),
            picture: Some("ab".repeat(32)),
            snapshot: Some("cd".repeat(32)),
            place: Some(PostPlace {
                latitude: 53.21,
                longitude: 6.58,
                name: Some("Meeuwerderweg".into()),
                view: Some(PointView {
                    altitude: 90.0,
                    pitch: -30.0,
                    yaw: 15.0,
                }),
            }),
            url: None,
            created_ms: 1_791_306_000_000,
            edited_ms: None,
            hidden: false,
        }
    }

    #[test]
    fn a_post_is_a_note_with_a_place_a_title_and_a_link() {
        let n = note(
            "https://c.test",
            &row("Plant swap <Saturday>\nBring cuttings"),
            "oost",
            "c.test",
            None,
        );
        assert_eq!(n["type"], "Note");
        assert_eq!(n["name"], "Plant swap <Saturday>");
        assert_eq!(n["audience"], group_url("https://c.test", Uuid::nil()));
        assert_eq!(n["location"]["latitude"], 53.21);
        assert_eq!(n["cartopolis:view"]["pitch"], -30.0);
        assert_eq!(n["published"], "2026-10-06T17:00:00Z");
        let content = n["content"].as_str().unwrap();
        assert!(
            content.starts_with("<p>Plant swap &lt;Saturday&gt;<br>Bring cuttings</p>"),
            "{content}"
        );
        assert!(
            content.contains("/go?place=53.21000,6.58000,90m,-30.0,15.0"),
            "{content}"
        );
        assert_eq!(
            image_url(&n).unwrap(),
            format!("https://c.test/ap/media/{}", "cd".repeat(32)),
            "the snapshot comes first"
        );
        assert_eq!(n["attachment"].as_array().unwrap().len(), 2);
        // What we write is what we read: text, place and all.
        assert_eq!(object_text(&n), "Plant swap <Saturday>\nBring cuttings");
        assert_eq!(object_place(&n), row("x").place);
    }

    #[test]
    fn a_reply_mentions_who_it_answers_and_has_no_title() {
        let r = ReplyTo {
            ap_id: "https://m.test/statuses/1",
            author: Some("https://m.test/users/anna"),
        };
        let n = note("https://c.test", &row("yes"), "oost", "c.test", Some(&r));
        assert_eq!(n["inReplyTo"], "https://m.test/statuses/1");
        assert!(n.get("name").is_none());
        assert!(addressees(&n).contains(&"https://m.test/users/anna".to_string()));
    }

    #[test]
    fn hashtags_are_links_and_tags_as_mastodon_writes_them() {
        assert_eq!(
            hashtags("Nou hier bankjes :D #groningen #TwinCity #groningen"),
            ["groningen", "TwinCity"]
        );
        // Glued to a word, a number, or an HTML entity: not hashtags.
        assert!(hashtags("a#b #1 &#39; c#").is_empty());
        let html = text_to_html("Bankjes <hier>\n#groningen!", "https://c.test");
        assert_eq!(
            html,
            "<p>Bankjes &lt;hier&gt;<br><a href=\"https://c.test/tags/groningen\" class=\"mention hashtag\" rel=\"tag\">#<span>groningen</span></a>!</p>"
        );
        let n = note(
            "https://c.test",
            &row("Bankjes #Groningen"),
            "oost",
            "c.test",
            None,
        );
        let tags = n["tag"].as_array().unwrap();
        assert!(tags.iter().any(|t| t["type"] == "Hashtag"
            && t["name"] == "#groningen"
            && t["href"] == "https://c.test/tags/groningen"));
        // What another server sends back reads as the words written.
        assert!(object_text(&json!({"type": "Note", "content": html})).contains("#groningen!"));
    }

    #[test]
    fn html_from_elsewhere_becomes_plain_text() {
        assert_eq!(
            html_to_text("<p>Heron back <span class=\"h-card\"><a href=\"x\">@<span>oost</span></a></span></p><p>on the quay &amp; the<br />bridge &#128038;</p>"),
            "Heron back @oost\n\non the quay & the\nbridge 🐦"
        );
        assert_eq!(html_to_text("<script>alert(1)</script>"), "alert(1)");
        assert_eq!(html_to_text("a &bogus; b & c"), "a &bogus; b & c");
    }

    #[test]
    fn a_mastodon_post_with_a_cartopolis_link_has_a_place() {
        let v = json!({
            "type": "Note",
            "content": "<p>Heron <a href=\"https://cartopolis.org/go?place=53.21940,6.56650,120m,-12.5,45.0\">here</a></p>",
        });
        let p = object_place(&v).unwrap();
        assert_eq!((p.latitude, p.view.unwrap().yaw), (53.2194, 45.0));
        assert_eq!(
            object_place(&json!({"type": "Note", "content": "<p>no</p>"})),
            None
        );
    }

    #[test]
    fn a_lemmy_page_keeps_its_title_and_body() {
        let v = json!({
            "type": "Page",
            "name": "Bike parking at the Oosterpoort",
            "content": "<p>The council plans 300 places.</p>",
            "attachment": [{ "type": "Link", "href": "https://x.test" }],
            "image": { "type": "Image", "url": "https://l.test/pic.jpg" },
        });
        assert_eq!(
            object_text(&v),
            "Bike parking at the Oosterpoort\n\nThe council plans 300 places."
        );
        assert_eq!(image_url(&v).as_deref(), Some("https://l.test/pic.jpg"));
    }

    #[test]
    fn only_public_and_unlisted_posts_are_public() {
        let public = json!({ "to": [PUBLIC], "cc": [] });
        let unlisted = json!({ "to": ["https://m.test/users/a/followers"], "cc": "as:Public" });
        let followers =
            json!({ "to": ["https://m.test/users/a/followers"], "cc": ["https://c.test/ap/c/x"] });
        let mention = json!({ "to": ["https://c.test/ap/c/x"] });
        assert!(is_public(&public));
        assert!(is_public(&unlisted));
        assert!(!is_public(&followers));
        assert!(!is_public(&mention));
    }

    #[test]
    fn the_key_a_request_was_signed_with_is_the_one_kept() {
        let v = json!({
            "id": "https://m.test/ap/users/1",
            "inbox": "https://m.test/ap/users/1/inbox",
            "publicKey": [
                { "id": "https://m.test/ap/users/1#main-key", "publicKeyPem": "OLD" },
                { "id": "https://m.test/ap/users/1#rsa-dc25", "publicKeyPem": "NEW" },
            ],
        });
        let a = read_actor_key(&v, Some("https://m.test/ap/users/1#rsa-dc25")).unwrap();
        assert_eq!(a.public_pem, "NEW");
        assert_eq!(read_actor(&v).unwrap().public_pem, "OLD");
        assert!(read_actor_key(&v, Some("https://m.test/ap/users/1#other")).is_none());
    }

    #[test]
    fn an_actor_reads_with_or_without_extras() {
        let v = json!({
            "id": "https://m.test/users/anna",
            "type": "Person",
            "preferredUsername": "anna",
            "inbox": "https://m.test/users/anna/inbox",
            "endpoints": { "sharedInbox": "https://m.test/inbox" },
            "publicKey": { "id": "https://m.test/users/anna#main-key", "publicKeyPem": "PEM" },
        });
        let a = read_actor(&v).unwrap();
        assert_eq!((a.host.as_str(), a.name.as_str()), ("m.test", "anna"));
        assert_eq!(a.shared_inbox.as_deref(), Some("https://m.test/inbox"));
        assert!(read_actor(&json!({"id": "https://m.test/x"})).is_none());
    }
}
