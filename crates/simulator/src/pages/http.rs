//! Content pages over HTTP: what readers fetch, the editors' routes, and the
//! HTML shell with each page's head filled in (`docs/mockups/cms`).
//!
//! Readers get `/api/pages/{slug}` — the live revision, never the draft — with
//! no sign-in. Everything under `/api/admin/pages` needs `pages_edit`, and what
//! changes what readers see (`publish`, `unpublish`, `DELETE`) `pages_publish`.

use std::collections::BTreeMap;
use std::path::Path as FsPath;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use cartopolis_core::permissions::Permission;
use serde::Deserialize;
use serde_json::json;

use super::{PageError, PageService, Texts};
use crate::http::{internal, require, HttpState};

pub fn routes() -> Router<HttpState> {
    Router::new()
        .route("/api/pages/{slug}", get(live))
        .route("/api/admin/pages", get(list))
        .route(
            "/api/admin/pages/{slug}",
            get(draft).put(put_page).delete(delete_page),
        )
        .route("/api/admin/pages/{slug}/draft", get(preview))
        .route("/api/admin/pages/{slug}/texts/{lang}", put(put_texts))
        .route("/api/admin/pages/{slug}/publish", post(publish))
        .route("/api/admin/pages/{slug}/unpublish", post(unpublish))
        .route("/api/admin/pages/{slug}/revisions", get(revisions))
        .route("/api/admin/pages/{slug}/restore", post(restore))
        .route(
            "/mcp",
            post(super::mcp::handle)
                .get(super::mcp::no_stream)
                .delete(super::mcp::no_stream),
        )
}

/// A refusal as the API reports it. An invalid page also carries each problem
/// with its line and column, for an editor to point at.
pub(crate) fn refused(e: PageError) -> Response {
    match e {
        PageError::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such page"})),
        )
            .into_response(),
        PageError::Invalid(problems) => {
            let error = problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error, "problems": problems})),
            )
                .into_response()
        }
        PageError::Conflict(why) => {
            (StatusCode::CONFLICT, Json(json!({"error": why}))).into_response()
        }
        PageError::Internal(e) => internal(e).into_response(),
    }
}

fn answer<T: serde::Serialize>(r: Result<T, PageError>) -> Response {
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(e),
    }
}

#[derive(Deserialize)]
struct LangQuery {
    lang: Option<String>,
}

impl LangQuery {
    fn lang<'a>(&'a self, pages: &'a PageService) -> &'a str {
        self.lang.as_deref().unwrap_or(&pages.languages()[0])
    }
}

async fn live(
    State(state): State<HttpState>,
    Path(slug): Path<String>,
    Query(q): Query<LangQuery>,
) -> Response {
    match state.pages.live_data(&slug, q.lang(&state.pages)) {
        Some(data) => Json(data).into_response(),
        None => refused(PageError::NotFound),
    }
}

async fn list(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesEdit) {
        return e.into_response();
    }
    answer(
        state
            .pages
            .list()
            .map(|pages| json!({"pages": pages, "languages": state.pages.languages()})),
    )
}

async fn draft(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesEdit) {
        return e.into_response();
    }
    answer(state.pages.get(&slug))
}

async fn preview(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Query(q): Query<LangQuery>,
) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesEdit) {
        return e.into_response();
    }
    answer(state.pages.draft_data(&slug, q.lang(&state.pages)))
}

#[derive(Deserialize)]
struct PutPage {
    jsx: String,
    source_lang: Option<String>,
    #[serde(default)]
    texts: Texts,
}

async fn put_page(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<PutPage>,
) -> Response {
    let who = match require(&state, &headers, Permission::PagesEdit) {
        Ok((_, who)) => who,
        Err(e) => return e.into_response(),
    };
    answer(state.pages.put(
        &slug,
        &body.jsx,
        body.source_lang.as_deref(),
        &body.texts,
        &who,
    ))
}

#[derive(Deserialize)]
struct PutTexts {
    texts: BTreeMap<String, String>,
}

async fn put_texts(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path((slug, lang)): Path<(String, String)>,
    Json(body): Json<PutTexts>,
) -> Response {
    let who = match require(&state, &headers, Permission::PagesEdit) {
        Ok((_, who)) => who,
        Err(e) => return e.into_response(),
    };
    answer(state.pages.set_texts(&slug, &lang, &body.texts, &who))
}

async fn publish(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let who = match require(&state, &headers, Permission::PagesPublish) {
        Ok((_, who)) => who,
        Err(e) => return e.into_response(),
    };
    answer(state.pages.publish(&slug, &who))
}

async fn unpublish(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesPublish) {
        return e.into_response();
    }
    answer(state.pages.unpublish(&slug).map(|()| json!({"ok": true})))
}

async fn revisions(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesEdit) {
        return e.into_response();
    }
    answer(
        state
            .pages
            .revisions(&slug)
            .map(|r| json!({"revisions": r})),
    )
}

#[derive(Deserialize)]
struct Restore {
    revision: i64,
}

async fn restore(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<Restore>,
) -> Response {
    let who = match require(&state, &headers, Permission::PagesEdit) {
        Ok((_, who)) => who,
        Err(e) => return e.into_response(),
    };
    answer(state.pages.restore(&slug, body.revision, &who))
}

async fn delete_page(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = require(&state, &headers, Permission::PagesPublish) {
        return e.into_response();
    }
    answer(state.pages.delete(&slug).map(|()| json!({"ok": true})))
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// `/{lang}/{slug}` → the language and the page, and `/{lang}/` the home page,
/// when `lang` is one this server serves; any other path is not a content page
/// (`/c/{handle}`, `/fonts/…`).
fn page_path<'a>(path: &'a str, languages: &[String]) -> Option<(&'a str, &'a str)> {
    let rest = path.strip_prefix('/')?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (lang, slug) = rest.split_once('/').unwrap_or((rest, super::HOME));
    (languages.iter().any(|l| l == lang) && super::valid_slug(slug)).then_some((lang, slug))
}

/// Where a page lives in one language: the home page at `/{lang}/`.
fn page_url(base: &str, lang: &str, slug: &str) -> String {
    if slug == super::HOME {
        format!("{base}/{lang}/")
    } else {
        format!("{base}/{lang}/{slug}")
    }
}

/// The language a `/p/{slug}` reader asked for: the first of theirs this
/// server has, else its default.
fn preferred<'a>(accept: Option<&str>, languages: &'a [String]) -> &'a str {
    let mut asked: Vec<(f32, &str)> = accept
        .unwrap_or("")
        .split(',')
        .filter_map(|part| {
            let mut bits = part.trim().split(';');
            let tag = bits.next()?.trim();
            let q = bits
                .find_map(|b| b.trim().strip_prefix("q="))
                .and_then(|q| q.parse::<f32>().ok())
                .unwrap_or(1.0);
            Some((q, tag))
        })
        .collect();
    asked.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, tag) in asked {
        let primary = tag.split('-').next().unwrap_or(tag).to_ascii_lowercase();
        if let Some(l) = languages.iter().find(|l| **l == primary) {
            return l;
        }
    }
    &languages[0]
}

fn escape(s: &str) -> String {
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

/// The head a page's shell carries: its title and description, the link
/// preview's tags, and where it is in every other language.
fn head(pages: &PageService, base: &str, lang: &str, slug: &str, draft: bool) -> (String, bool) {
    let base = base.trim_end_matches('/');
    let found = pages.head(slug, lang);
    let mut h = String::new();
    match &found {
        Some((title, description)) => {
            let (title, description) = (escape(title), escape(description));
            h.push_str(&format!("<title>{title} · Cartopolis</title>\n"));
            h.push_str(&format!(
                "    <meta name=\"description\" content=\"{description}\" />\n"
            ));
            h.push_str(&format!(
                "    <meta property=\"og:title\" content=\"{title}\" />\n"
            ));
            h.push_str(&format!(
                "    <meta property=\"og:description\" content=\"{description}\" />\n"
            ));
            h.push_str("    <meta property=\"og:type\" content=\"website\" />\n");
            h.push_str("    <meta property=\"og:site_name\" content=\"Cartopolis\" />\n");
            let url = page_url(base, lang, slug);
            h.push_str(&format!(
                "    <meta property=\"og:url\" content=\"{url}\" />\n"
            ));
            h.push_str(&format!("    <link rel=\"canonical\" href=\"{url}\" />\n"));
            for l in pages.languages() {
                let url = page_url(base, l, slug);
                h.push_str(&format!(
                    "    <link rel=\"alternate\" hreflang=\"{l}\" href=\"{url}\" />\n"
                ));
            }
            // The home page's own address in no language is the site's root.
            let any = if slug == super::HOME {
                format!("{base}/")
            } else {
                format!("{base}/p/{slug}")
            };
            h.push_str(&format!(
                "    <link rel=\"alternate\" hreflang=\"x-default\" href=\"{any}\" />"
            ));
        }
        None => h.push_str("<title>Cartopolis</title>"),
    }
    // A draft preview, or a page that is not there, is nothing to index.
    if draft || found.is_none() {
        h.push_str("\n    <meta name=\"robots\" content=\"noindex\" />");
    }
    (h, found.is_some())
}

/// What the files' fallback asks first (`static_files`): the shell for a content
/// page, the redirect for `/p/{slug}`, or `None` for any other path.
pub async fn shell(
    pages: &PageService,
    shell_file: &FsPath,
    base: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Option<Response> {
    let path = uri.path();
    // The front door: the home page in the reader's language once one is
    // published, else the web UI's `/` (the chat) as before. An address with a
    // query is the chat's own (`/?show=…` from before Community moved to
    // `/chat`), so it is left to the web UI.
    if path == "/" {
        let accept = headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok());
        let lang = preferred(accept, pages.languages());
        if uri.query().is_some() || pages.head(super::HOME, lang).is_none() {
            return None;
        }
        return Some(Redirect::to(&format!("/{lang}/")).into_response());
    }
    if let Some(slug) = path.strip_prefix("/p/").map(|s| s.trim_end_matches('/')) {
        if !super::valid_slug(slug) {
            return None;
        }
        let accept = headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok());
        let lang = preferred(accept, pages.languages());
        return Some(Redirect::to(&format!("/{lang}/{slug}")).into_response());
    }
    let (lang, slug) = page_path(path, pages.languages())?;
    let draft = uri.query().is_some_and(|q| {
        q.split('&')
            .any(|p| p == "draft" || p.starts_with("draft="))
    });
    let template = match tokio::fs::read_to_string(shell_file).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("content pages: no shell at {}: {e}", shell_file.display());
            return Some(
                (
                    StatusCode::NOT_FOUND,
                    "content pages are not installed on this server",
                )
                    .into_response(),
            );
        }
    };
    let (head, live) = head(pages, base, lang, slug, draft);
    let html = match (
        template.find("<!--cms-head-->"),
        template.find("<!--/cms-head-->"),
    ) {
        (Some(a), Some(b)) if a < b => format!(
            "{}{head}{}",
            &template[..a],
            &template[b + "<!--/cms-head-->".len()..]
        ),
        _ => template,
    };
    let html = html.replacen("<html lang=\"en\">", &format!("<html lang=\"{lang}\">"), 1);
    // A draft is drawn by the page's script with the editor's session, so its
    // shell is a 200 even though nothing is live at the address.
    let status = if live || draft {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    Some(
        (
            status,
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            html,
        )
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs() -> Vec<String> {
        vec!["nl".into(), "en".into()]
    }

    #[test]
    fn which_paths_are_pages() {
        assert_eq!(page_path("/nl/over", &langs()), Some(("nl", "over")));
        assert_eq!(page_path("/en/over/", &langs()), Some(("en", "over")));
        assert_eq!(page_path("/de/over", &langs()), None);
        assert_eq!(page_path("/c/groningen", &langs()), None);
        assert_eq!(page_path("/nl/over/meer", &langs()), None);
        // The home page is the language alone.
        assert_eq!(page_path("/nl", &langs()), Some(("nl", "home")));
        assert_eq!(page_path("/en/", &langs()), Some(("en", "home")));
        assert_eq!(page_path("/", &langs()), None);
        assert_eq!(page_path("/nl/Over", &langs()), None);
    }

    #[test]
    fn the_readers_language() {
        assert_eq!(preferred(Some("en-GB,en;q=0.9,nl;q=0.8"), &langs()), "en");
        assert_eq!(
            preferred(Some("de-DE,de;q=0.9,nl;q=0.5,en;q=0.7"), &langs()),
            "en"
        );
        assert_eq!(preferred(Some("fr"), &langs()), "nl");
        assert_eq!(preferred(None, &langs()), "nl");
    }

    #[test]
    fn the_head_is_escaped() {
        assert_eq!(
            escape(r#"a "b" <c> & 'd'"#),
            "a &quot;b&quot; &lt;c&gt; &amp; &#39;d&#39;"
        );
    }
}
