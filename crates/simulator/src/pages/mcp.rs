//! Content pages over MCP, at `/mcp` (`docs/mockups/cms`): what an agent makes
//! and publishes pages with.
//!
//! Streamable HTTP in its plainest form: every message is a POST answered with
//! one JSON body, no session and no server-sent stream, which the transport
//! allows and which is all a request/answer tool surface needs. Hand-written
//! rather than `rmcp`'s server, because that is four JSON-RPC methods and a
//! table, and the transport would have pulled a session layer into the
//! simulator for nothing.
//!
//! Signed in with the bearer every other route takes — in practice an access
//! token from `/tokens` — and every tool needs `pages_edit`; the ones that change
//! what readers see need `pages_publish` as well.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cartopolis_core::permissions::Permission;
use serde_json::{json, Value};

use super::check::{Children, REGISTRY};
use super::{PageError, Texts};
use crate::http::{require, HttpState};

/// The protocol revisions this answers in; the newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Content pages for this Cartopolis server's website, at /{lang}/{slug}. \
A page is JSX built only from the components `components` lists; every word a reader sees is a \
{$t('key')} whose text you give per language, so the page can be shown in every language the server has. \
Read `components` first. Then `put_page` with the JSX and the texts in the page's source language, \
`preview` to read it back, `set_texts` for each other language, and `publish`.";

pub async fn no_stream() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "this endpoint answers POST only: no stream, no session",
    )
        .into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        .into_response()
}

pub async fn handle(State(state): State<HttpState>, headers: HeaderMap, body: Bytes) -> Response {
    let who = match require(&state, &headers, Permission::PagesEdit) {
        Ok((_, who)) => who,
        Err(e) => return e.into_response(),
    };
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(Value::Null, -32700, "not JSON");
    };
    if msg.is_array() {
        return rpc_error(
            Value::Null,
            -32600,
            "one message per request: batches are not supported",
        );
    }
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = msg.get("id").cloned() else {
        // A notification (`notifications/initialized`) or a response: nothing to answer.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "cartopolis-pages", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (text, is_error) = match call(&state, &headers, &who, name, &args) {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => return rpc_error(id, -32601, &format!("no method {method}")),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

fn tools() -> Vec<Value> {
    let slug = json!({"type": "string", "description": "The page's address: lower-case letters, digits and hyphens (over, download)."});
    let lang =
        json!({"type": "string", "description": "A language code from `languages` (nl, en)."});
    let texts_by_lang = json!({
        "type": "object",
        "description": "{lang: {key: text}}. A text may use **bold**, *italic*, [link](/path) and a blank line between paragraphs. An empty text removes it.",
        "additionalProperties": {"type": "object", "additionalProperties": {"type": "string"}},
    });
    vec![
        tool("components", "Every component a page may use: its props and their types, what children it takes, and an example. Read this before writing a page.", json!({}), &[]),
        tool("languages", "The languages pages are served in; the first is the default.", json!({}), &[]),
        tool("list_pages", "Every page: title, source language, live revision, whether the draft has changed since, and per language the texts missing or stale.", json!({}), &[]),
        tool("get_page", "A page's draft: its JSX, its text keys and every text in every language, and per language what is missing or stale.", json!({"slug": slug}), &["slug"]),
        tool(
            "put_page",
            "Create a page or replace its draft: the JSX and texts in one or more languages. Every $t key needs a text in the page's source language, given now or before. Refused as a whole, with every problem and its line, when anything is wrong. Readers see nothing until `publish`.",
            json!({
                "slug": slug,
                "jsx": {"type": "string", "description": "One <Page> element built from the components, every visible text a {$t('key')}."},
                "source_lang": {"type": "string", "description": "The language the page is written in; only when creating it. The server's default when left out."},
                "texts": texts_by_lang,
            }),
            &["slug", "jsx"],
        ),
        tool(
            "set_texts",
            "Texts for one language of a page's draft: a translation, or changes to the source. A translation of a source text that changes later shows as stale until translated again.",
            json!({"slug": slug, "lang": lang, "texts": {"type": "object", "additionalProperties": {"type": "string"}, "description": "{key: text}; an empty text removes a translation."}}),
            &["slug", "lang", "texts"],
        ),
        tool("preview", "The draft in one language as readable text — each component with its texts, and where a text falls back to another language — and the address to see it in a browser.", json!({"slug": slug, "lang": lang}), &["slug"]),
        tool("publish", "Make the draft the next revision and put it live. Needs pages_publish.", json!({"slug": slug}), &["slug"]),
        tool("unpublish", "Take a page down; its draft and revisions stay. Needs pages_publish.", json!({"slug": slug}), &["slug"]),
        tool("revisions", "A page's published revisions, newest first, and which one is live.", json!({"slug": slug}), &["slug"]),
        tool("restore", "Put a published revision back into the draft, to change or publish again.", json!({"slug": slug, "revision": {"type": "integer"}}), &["slug", "revision"]),
        tool("delete_page", "Delete a page that is not live, with every revision. Needs pages_publish.", json!({"slug": slug}), &["slug"]),
    ]
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{name} is required"))
}

fn said(e: PageError) -> String {
    match e {
        PageError::NotFound => "no such page".into(),
        PageError::Invalid(problems) => {
            let mut out = String::from("Refused; nothing was saved.\n");
            for p in problems {
                if p.line == 0 {
                    let _ = writeln!(out, "- {}", p.message);
                } else {
                    let _ = writeln!(out, "- {p}");
                }
            }
            out
        }
        PageError::Conflict(why) => why,
        PageError::Internal(e) => {
            tracing::warn!("pages mcp: {e:#}");
            "the server could not complete that".into()
        }
    }
}

fn pretty<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn call(
    state: &HttpState,
    headers: &HeaderMap,
    who: &str,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let pages = &state.pages;
    let publishing = matches!(name, "publish" | "unpublish" | "delete_page");
    if publishing {
        require(state, headers, Permission::PagesPublish).map_err(|e| e.1)?;
    }
    match name {
        "components" => Ok(components()),
        "languages" => Ok(pretty(
            &json!({"languages": pages.languages(), "default": pages.languages()[0]}),
        )),
        "list_pages" => pages.list().map(|l| pretty(&l)).map_err(said),
        "get_page" => pages
            .get(arg(args, "slug")?)
            .map(|d| pretty(&d))
            .map_err(said),
        "put_page" => {
            let texts: Texts = match args.get("texts") {
                None | Some(Value::Null) => Texts::new(),
                Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("texts: {e}"))?,
            };
            let slug = arg(args, "slug")?;
            let source = args.get("source_lang").and_then(Value::as_str);
            pages
                .put(slug, arg(args, "jsx")?, source, &texts, who)
                .map(|d| saved(&d))
                .map_err(said)
        }
        "set_texts" => {
            let texts: BTreeMap<String, String> =
                serde_json::from_value(args.get("texts").cloned().unwrap_or_default())
                    .map_err(|e| format!("texts: {e}"))?;
            pages
                .set_texts(arg(args, "slug")?, arg(args, "lang")?, &texts, who)
                .map(|d| saved(&d))
                .map_err(said)
        }
        "preview" => {
            let slug = arg(args, "slug")?;
            let lang = args
                .get("lang")
                .and_then(Value::as_str)
                .unwrap_or(&pages.languages()[0]);
            let data = pages.draft_data(slug, lang).map_err(said)?;
            let base = state.config.public_base_url.trim_end_matches('/');
            Ok(format!(
                "{}\nIn a browser, signed in: {base}/{lang}/{slug}?draft",
                pages.outline(&data)
            ))
        }
        "publish" => {
            let slug = arg(args, "slug")?;
            let r = pages.publish(slug, who).map_err(said)?;
            let base = state.config.public_base_url.trim_end_matches('/');
            let at: Vec<String> = pages
                .languages()
                .iter()
                .map(|l| format!("{base}/{l}/{slug}"))
                .collect();
            Ok(format!(
                "Revision {} is live at {}",
                r.revision,
                at.join(" and ")
            ))
        }
        "unpublish" => pages
            .unpublish(arg(args, "slug")?)
            .map(|()| "Taken down; the draft stays.".into())
            .map_err(said),
        "revisions" => pages
            .revisions(arg(args, "slug")?)
            .map(|r| pretty(&r))
            .map_err(said),
        "restore" => {
            let revision = args
                .get("revision")
                .and_then(Value::as_i64)
                .ok_or("revision is required")?;
            pages
                .restore(arg(args, "slug")?, revision, who)
                .map(|d| saved(&d))
                .map_err(said)
        }
        "delete_page" => pages
            .delete(arg(args, "slug")?)
            .map(|()| "Deleted.".into())
            .map_err(said),
        _ => Err(format!("no tool {name}")),
    }
}

/// What a write answers: the state of every language, so the next step is plain.
fn saved(d: &super::Draft) -> String {
    let mut out = format!(
        "Saved the draft of {} ({} texts, written in {}). ",
        d.summary.slug,
        d.keys.len(),
        d.summary.source_lang
    );
    match d.summary.live_revision {
        Some(r) => {
            let _ = write!(
                out,
                "Revision {r} is live; readers see this draft only once it is published."
            );
        }
        None => out.push_str("Not live yet."),
    }
    for (lang, status) in &d.summary.languages {
        if status.missing.is_empty() && status.stale.is_empty() {
            let _ = write!(out, "\n{lang}: complete");
        } else {
            let _ = write!(out, "\n{lang}: ");
            if !status.missing.is_empty() {
                let _ = write!(out, "missing {}", status.missing.join(", "));
            }
            if !status.stale.is_empty() {
                let sep = if status.missing.is_empty() { "" } else { "; " };
                let _ = write!(
                    out,
                    "{sep}stale (the source changed since) {}",
                    status.stale.join(", ")
                );
            }
        }
    }
    out
}

/// The registry as an agent reads it.
fn components() -> String {
    let mut out = format!(
        "A page is one <{root}> holding the components below. Props are name=\"text\", name={{3}}, name={{true}} (or just name), or name={{$t('key')}}. \
Every prop of type text, and every text child, is {{$t('key')}}: keys are lower-case words joined by dots (hero.heading), \
scoped to the page; never type words into the JSX. Text may use **bold**, *italic*, [link](/path) and blank lines between paragraphs.\n",
        root = REGISTRY.root
    );
    for (name, spec) in &REGISTRY.components {
        let _ = write!(out, "\n<{name}> — {}\n", spec.doc);
        for (prop, p) in &spec.props {
            let kind = match p.kind {
                super::check::PropKind::Text => "text: {$t('key')}".to_string(),
                super::check::PropKind::Url => "url: a site path, https: or mailto:".to_string(),
                super::check::PropKind::Image => "image: a site path or https:".to_string(),
                super::check::PropKind::Enum => format!("one of {}", p.values.join(", ")),
                super::check::PropKind::Number => "number".to_string(),
                super::check::PropKind::Bool => "true or false".to_string(),
                super::check::PropKind::Place => "place: lat,lon[,120m,pitch,heading]".to_string(),
            };
            let _ = writeln!(
                out,
                "  {prop}{} — {kind}",
                if p.required { " (required)" } else { "" }
            );
        }
        let children = match spec.children {
            Children::None => "no children: close it with />".to_string(),
            Children::Text => "exactly one {$t('key')} child".to_string(),
            Children::Blocks => match &spec.allow {
                Some(allow) => format!("components as children: {}", allow.join(", ")),
                None => "components as children".to_string(),
            },
        };
        let _ = writeln!(out, "  children: {children}");
        let _ = writeln!(
            out,
            "  example:\n{}",
            spec.example
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let site: Vec<&str> = REGISTRY.site_texts.keys().map(String::as_str).collect();
    let _ = write!(
        out,
        "\nThe site's own texts (keys starting site.) are filled in by the server: {}.",
        site.join(", ")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_an_object_schema() {
        for t in tools() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            for r in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(
                    t["inputSchema"]["properties"]
                        .get(r.as_str().unwrap())
                        .is_some(),
                    "{}",
                    t["name"]
                );
            }
        }
    }

    #[test]
    fn the_components_read_as_text() {
        let text = components();
        assert!(text.contains("<Hero> — "));
        assert!(text.contains("heading (required) — text: {$t('key')}"));
        assert!(text.contains("site.download.android"));
    }
}
