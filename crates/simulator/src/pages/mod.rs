//! Content pages: JSX blocks whose every word is a `$t` text, kept per page and
//! language, drawn by the web UI at `/{lang}/{slug}` (`docs/mockups/cms`).
//!
//! A page is written as JSX and read, never run ([`jsx`]), checked against the
//! components the web UI has ([`check`], `registry.json`), and kept as its source,
//! its tree and its texts. It has one **draft**, changed freely, and immutable
//! **revisions**: publishing copies the draft into the next one and makes it
//! live. Readers only ever see a revision; the live ones are held in memory, so
//! serving a page is a map lookup and never the database.
//!
//! [`PageService`] is the one place pages change. The admin routes ([`http`]) and
//! the MCP endpoint ([`mcp`]) both call it, as `PrimService` serves the wire and
//! `/api/admin/prims`, so their checks cannot disagree. Who may call it is the
//! callers' question: `pages_edit` for drafts, `pages_publish` for what readers
//! see.

pub mod check;
pub mod http;
pub mod jsx;
pub mod mcp;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Context as _;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};

use self::check::{keys, MAX_JSX_BYTES, MAX_TEXT_BYTES, REGISTRY};
use self::jsx::{Node, Pos, Problem, PropValue};

/// `{lang: {key: text}}`.
pub type Texts = BTreeMap<String, BTreeMap<String, String>>;

/// Why a page operation did not happen.
#[derive(Debug)]
pub enum PageError {
    NotFound,
    /// The page or its texts are not what may be saved; every problem at once.
    Invalid(Vec<Problem>),
    /// Allowed in itself, not in the page's state (deleting a live page).
    Conflict(String),
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for PageError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl From<rusqlite::Error> for PageError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.into())
    }
}

fn invalid(message: impl Into<String>) -> PageError {
    PageError::Invalid(vec![Problem::at(Pos { line: 0, col: 0 }, message)])
}

/// A text with the language it is actually in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resolved {
    pub text: String,
    pub lang: String,
}

/// What `/api/pages/{slug}` answers, and the draft route the same for an editor:
/// `PageData` in `src/cms/tree.ts`.
#[derive(Debug, Clone, Serialize)]
pub struct PageData {
    pub slug: String,
    pub lang: String,
    pub languages: Vec<String>,
    pub source_lang: String,
    pub revision: Option<i64>,
    pub tree: serde_json::Value,
    pub texts: BTreeMap<String, Resolved>,
}

/// A language's state against the source: keys with no text, and texts made
/// from a source sentence that has changed since.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LangStatus {
    pub missing: Vec<String>,
    pub stale: Vec<String>,
}

/// One row of the list.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub slug: String,
    /// The draft's title in its source language.
    pub title: String,
    pub source_lang: String,
    pub live_revision: Option<i64>,
    /// Whether the draft differs from what readers see.
    pub draft_changed: bool,
    pub languages: BTreeMap<String, LangStatus>,
    pub updated_by: String,
    pub updated_at: i64,
}

/// A page's draft, whole: what an editor reads back and changes.
#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    #[serde(flatten)]
    pub summary: Summary,
    pub jsx: String,
    pub keys: Vec<String>,
    pub texts: Texts,
}

#[derive(Debug, Clone, Serialize)]
pub struct Revision {
    pub revision: i64,
    pub published_by: String,
    pub published_at: i64,
    pub live: bool,
}

/// A live revision, as readers are served it.
struct Live {
    revision: i64,
    source_lang: String,
    tree: serde_json::Value,
    keys: Vec<String>,
    texts: Texts,
}

/// The one place pages change (module docs). Cheap to clone.
#[derive(Clone)]
pub struct PageService {
    conn: Arc<Mutex<Connection>>,
    live: Arc<RwLock<HashMap<String, Arc<Live>>>>,
    languages: Arc<Vec<String>>,
}

/// A page's address: lower-case letters, digits and hyphens.
#[must_use]
pub fn valid_slug(slug: &str) -> bool {
    (1..=64).contains(&slug.len())
        && slug.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// What a translation remembers of the source sentence it was made from.
fn source_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> i64 {
    i64::try_from(crate::auth::unix_now_s()).unwrap_or(i64::MAX)
}

/// The text a reader gets for `key` in `lang`: the page's own, else its source
/// language's, else the site's own text in either.
fn resolve(texts: &Texts, source: &str, lang: &str, key: &str) -> Option<Resolved> {
    let from = |l: &str| {
        texts.get(l).and_then(|t| t.get(key)).map(|text| Resolved {
            text: text.clone(),
            lang: l.to_string(),
        })
    };
    if let Some(r) = from(lang).or_else(|| from(source)) {
        return Some(r);
    }
    let site = REGISTRY.site_texts.get(key)?;
    for l in [lang, source] {
        if let Some(text) = site.get(l) {
            return Some(Resolved {
                text: text.clone(),
                lang: l.to_string(),
            });
        }
    }
    site.iter().next().map(|(l, text)| Resolved {
        text: text.clone(),
        lang: l.clone(),
    })
}

/// A text without its markup, for a title or a description in the HTML head.
#[must_use]
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            '*' | '[' => {}
            ']' if chars.peek() == Some(&'(') => {
                for c in chars.by_ref() {
                    if c == ')' {
                        break;
                    }
                }
            }
            '\n' => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn text_problems(
    keys: &[String],
    lang: &str,
    texts: &BTreeMap<String, String>,
    problems: &mut Vec<Problem>,
) {
    let none = Pos { line: 0, col: 0 };
    for (key, text) in texts {
        if !keys.contains(key) {
            problems.push(Problem::at(
                none,
                format!("texts.{lang}.{key}: the page uses no $t('{key}')"),
            ));
        } else if text.len() > MAX_TEXT_BYTES {
            problems.push(Problem::at(
                none,
                format!("texts.{lang}.{key}: longer than {MAX_TEXT_BYTES} bytes"),
            ));
        }
    }
}

impl PageService {
    /// The service over the server's database, with every live page loaded.
    pub fn new(conn: Arc<Mutex<Connection>>, languages: Vec<String>) -> anyhow::Result<Self> {
        // Two letters, because each is the first part of an address and the
        // web UI's `parsePath` reads exactly that.
        anyhow::ensure!(
            !languages.is_empty(),
            "page_languages is empty: content pages need at least one language"
        );
        for l in &languages {
            anyhow::ensure!(
                l.len() == 2 && l.chars().all(|c| c.is_ascii_lowercase()),
                "page_languages: {l:?} is not a two-letter language code (nl, en)"
            );
        }
        let service = Self {
            conn,
            live: Arc::default(),
            languages: Arc::new(languages),
        };
        let slugs: Vec<String> = {
            let conn = service.lock()?;
            let mut stmt =
                conn.prepare("SELECT slug FROM pages WHERE live_revision IS NOT NULL")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for slug in slugs {
            service.refresh_live(&slug)?;
        }
        Ok(service)
    }

    fn lock(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("database lock poisoned"))
    }

    #[must_use]
    pub fn languages(&self) -> &[String] {
        &self.languages
    }

    fn check_lang(&self, lang: &str) -> Result<(), PageError> {
        if self.languages.iter().any(|l| l == lang) {
            Ok(())
        } else {
            Err(invalid(format!(
                "{lang} is not one of this server's languages ({})",
                self.languages.join(", ")
            )))
        }
    }

    /// Re-read one page's live revision into memory, or drop it when none is.
    fn refresh_live(&self, slug: &str) -> anyhow::Result<()> {
        let row = {
            let conn = self.lock()?;
            conn.query_row(
                "SELECT r.revision, r.source_lang, r.tree, r.texts FROM pages p
                 JOIN page_revisions r ON r.slug = p.slug AND r.revision = p.live_revision
                 WHERE p.slug = ?1",
                params![slug],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        };
        let mut live = self
            .live
            .write()
            .map_err(|_| anyhow::anyhow!("page cache poisoned"))?;
        match row {
            None => {
                live.remove(slug);
            }
            Some((revision, source_lang, tree, texts)) => {
                let node: Node = serde_json::from_str(&tree).context("a revision's tree")?;
                let mut k = Vec::new();
                keys(&node, &mut k);
                live.insert(
                    slug.to_string(),
                    Arc::new(Live {
                        revision,
                        source_lang,
                        tree: serde_json::from_str(&tree)?,
                        keys: k,
                        texts: serde_json::from_str(&texts).context("a revision's texts")?,
                    }),
                );
            }
        }
        Ok(())
    }

    fn data(
        &self,
        slug: &str,
        lang: &str,
        source: &str,
        revision: Option<i64>,
        tree: serde_json::Value,
        page_keys: &[String],
        texts: &Texts,
    ) -> PageData {
        let mut resolved = BTreeMap::new();
        for key in page_keys.iter().chain(REGISTRY.site_texts.keys()) {
            if let Some(r) = resolve(texts, source, lang, key) {
                resolved.insert(key.clone(), r);
            }
        }
        PageData {
            slug: slug.to_string(),
            lang: lang.to_string(),
            languages: self.languages.to_vec(),
            source_lang: source.to_string(),
            revision,
            tree,
            texts: resolved,
        }
    }

    /// What a reader gets: the live revision in `lang`. `None` for a page that
    /// is not live, or a language the server does not serve.
    #[must_use]
    pub fn live_data(&self, slug: &str, lang: &str) -> Option<PageData> {
        if !self.languages.iter().any(|l| l == lang) {
            return None;
        }
        let live = self.live.read().ok()?.get(slug)?.clone();
        Some(self.data(
            slug,
            lang,
            &live.source_lang,
            Some(live.revision),
            live.tree.clone(),
            &live.keys,
            &live.texts,
        ))
    }

    /// The live page's title and description in `lang`, without markup, for
    /// the HTML head.
    #[must_use]
    pub fn head(&self, slug: &str, lang: &str) -> Option<(String, String)> {
        let data = self.live_data(slug, lang)?;
        let prop = |name: &str| {
            let key = data.tree.get("p")?.get(name)?.get("t")?.as_str()?;
            data.texts.get(key).map(|r| plain(&r.text))
        };
        Some((
            prop("title").unwrap_or_default(),
            prop("description").unwrap_or_default(),
        ))
    }

    /// The draft in `lang`, as a reader would get it once published.
    pub fn draft_data(&self, slug: &str, lang: &str) -> Result<PageData, PageError> {
        self.check_lang(lang)?;
        let draft = self.get(slug)?;
        let tree: serde_json::Value = {
            let conn = self.lock()?;
            let tree: String = conn.query_row(
                "SELECT draft_tree FROM pages WHERE slug = ?1",
                params![slug],
                |r| r.get(0),
            )?;
            serde_json::from_str(&tree).context("a draft's tree")?
        };
        Ok(self.data(
            slug,
            lang,
            &draft.summary.source_lang,
            None,
            tree,
            &draft.keys,
            &draft.texts,
        ))
    }

    fn load_texts(
        conn: &Connection,
        slug: &str,
    ) -> anyhow::Result<(Texts, BTreeMap<(String, String), Option<String>>)> {
        let mut texts = Texts::new();
        let mut hashes = BTreeMap::new();
        let mut stmt =
            conn.prepare("SELECT lang, key, text, source_hash FROM page_texts WHERE slug = ?1")?;
        let rows = stmt.query_map(params![slug], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (lang, key, text, hash) = row?;
            hashes.insert((lang.clone(), key.clone()), hash);
            texts.entry(lang).or_default().insert(key, text);
        }
        Ok((texts, hashes))
    }

    fn status(
        &self,
        source: &str,
        page_keys: &[String],
        texts: &Texts,
        hashes: &BTreeMap<(String, String), Option<String>>,
    ) -> BTreeMap<String, LangStatus> {
        let mut out = BTreeMap::new();
        let source_texts = texts.get(source);
        for lang in self.languages.iter() {
            let mut status = LangStatus::default();
            for key in page_keys.iter().filter(|k| !k.starts_with("site.")) {
                if !texts.get(lang).is_some_and(|t| t.contains_key(key)) {
                    status.missing.push(key.clone());
                    continue;
                }
                if lang != source {
                    let now = source_texts
                        .and_then(|t| t.get(key))
                        .map(|s| source_hash(s));
                    let then = hashes.get(&(lang.clone(), key.clone())).cloned().flatten();
                    if now.is_some() && now != then {
                        status.stale.push(key.clone());
                    }
                }
            }
            out.insert(lang.clone(), status);
        }
        out
    }

    /// A page's draft, whole.
    pub fn get(&self, slug: &str) -> Result<Draft, PageError> {
        let conn = self.lock()?;
        let row = conn
            .query_row(
                "SELECT source_lang, draft_jsx, draft_tree, live_revision, draft_changed, updated_by, updated_at
                 FROM pages WHERE slug = ?1",
                params![slug],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?,
                        r.get::<_, bool>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((source_lang, jsx, tree, live_revision, draft_changed, updated_by, updated_at)) =
            row
        else {
            return Err(PageError::NotFound);
        };
        let node: Node = serde_json::from_str(&tree).context("a draft's tree")?;
        let mut page_keys = Vec::new();
        keys(&node, &mut page_keys);
        let (texts, hashes) = Self::load_texts(&conn, slug)?;
        let title = match node.p.get("title") {
            Some(PropValue::Text(t)) => texts
                .get(&source_lang)
                .and_then(|s| s.get(&t.t))
                .map(|s| plain(s))
                .unwrap_or_default(),
            _ => String::new(),
        };
        let languages = self.status(&source_lang, &page_keys, &texts, &hashes);
        Ok(Draft {
            summary: Summary {
                slug: slug.to_string(),
                title,
                source_lang,
                live_revision,
                draft_changed,
                languages,
                updated_by,
                updated_at,
            },
            jsx,
            keys: page_keys,
            texts,
        })
    }

    /// Every page, newest change first.
    pub fn list(&self) -> Result<Vec<Summary>, PageError> {
        let slugs: Vec<String> = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare("SELECT slug FROM pages ORDER BY updated_at DESC, slug")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        slugs
            .iter()
            .map(|s| self.get(s).map(|d| d.summary))
            .collect()
    }

    /// Write a page's draft: its JSX and any texts with it. Creates the page
    /// when it does not exist; `source_lang` is fixed once it does. Refused as a
    /// whole, every problem at once, when anything is wrong — including a key
    /// left with no text in the source language.
    pub fn put(
        &self,
        slug: &str,
        jsx_src: &str,
        source_lang: Option<&str>,
        texts: &Texts,
        who: &str,
    ) -> Result<Draft, PageError> {
        if !valid_slug(slug) {
            return Err(invalid(format!(
                "{slug:?} is not a page address: lower-case letters, digits and hyphens"
            )));
        }
        if jsx_src.len() > MAX_JSX_BYTES {
            return Err(invalid(format!(
                "the JSX is longer than {MAX_JSX_BYTES} bytes"
            )));
        }
        let tree = jsx::parse(jsx_src).map_err(|p| PageError::Invalid(vec![p]))?;
        let mut problems = check::check(&tree);
        let mut page_keys = Vec::new();
        keys(&tree, &mut page_keys);

        let existing = match self.get(slug) {
            Ok(d) => Some(d),
            Err(PageError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let source = match (&existing, source_lang) {
            (Some(d), Some(asked)) if asked != d.summary.source_lang => {
                return Err(invalid(format!(
                    "the page is written in {}; its source language does not change",
                    d.summary.source_lang
                )));
            }
            (Some(d), _) => d.summary.source_lang.clone(),
            (None, Some(asked)) => asked.to_string(),
            (None, None) => self.languages[0].clone(),
        };
        self.check_lang(&source)?;
        for (lang, t) in texts {
            self.check_lang(lang)?;
            text_problems(&page_keys, lang, t, &mut problems);
        }
        let mut merged = existing
            .as_ref()
            .map(|d| d.texts.clone())
            .unwrap_or_default();
        for (lang, t) in texts {
            let into = merged.entry(lang.clone()).or_default();
            for (key, text) in t {
                if text.is_empty() {
                    into.remove(key);
                } else {
                    into.insert(key.clone(), text.clone());
                }
            }
        }
        let missing: Vec<&String> = page_keys
            .iter()
            .filter(|k| {
                !k.starts_with("site.") && !merged.get(&source).is_some_and(|t| t.contains_key(*k))
            })
            .collect();
        if !missing.is_empty() {
            problems.push(Problem::at(
                tree.at,
                format!(
                    "no {source} text for {}: give them in texts.{source}",
                    missing
                        .iter()
                        .map(|k| format!("$t('{k}')"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
        if !problems.is_empty() {
            return Err(PageError::Invalid(problems));
        }

        let tree_json = serde_json::to_string(&tree).context("a page's tree")?;
        let t = now();
        {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO pages (slug, source_lang, draft_jsx, draft_tree, draft_changed, created_by, created_at, updated_by, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?5, ?6)
                 ON CONFLICT(slug) DO UPDATE SET draft_jsx = ?3, draft_tree = ?4, draft_changed = 1, updated_by = ?5, updated_at = ?6",
                params![slug, source, jsx_src, tree_json, who, t],
            )?;
            // Texts for keys the page no longer uses go with them.
            let (_, old_hashes) = Self::load_texts(&tx, slug)?;
            for (lang, key) in old_hashes.keys() {
                if !page_keys.contains(key)
                    || !merged.get(lang).is_some_and(|m| m.contains_key(key))
                {
                    tx.execute(
                        "DELETE FROM page_texts WHERE slug = ?1 AND lang = ?2 AND key = ?3",
                        params![slug, lang, key],
                    )?;
                }
            }
            for (lang, t) in texts {
                for (key, text) in t.iter().filter(|(_, text)| !text.is_empty()) {
                    write_text(&tx, slug, &source, lang, key, text, &merged)?;
                }
            }
            tx.commit()?;
        }
        self.get(slug)
    }

    /// Texts for one language: a translation, or changes to the source. An
    /// empty text removes a translation; a source text cannot be removed while
    /// the page uses its key.
    pub fn set_texts(
        &self,
        slug: &str,
        lang: &str,
        texts: &BTreeMap<String, String>,
        who: &str,
    ) -> Result<Draft, PageError> {
        self.check_lang(lang)?;
        let draft = self.get(slug)?;
        let source = draft.summary.source_lang.clone();
        let mut problems = Vec::new();
        text_problems(&draft.keys, lang, texts, &mut problems);
        if lang == source {
            for (key, text) in texts {
                if text.is_empty() && !key.starts_with("site.") {
                    problems.push(Problem::at(Pos::default(), format!("texts.{lang}.{key}: the source text cannot be empty while the page uses it")));
                }
            }
        }
        if !problems.is_empty() {
            return Err(PageError::Invalid(problems));
        }
        let mut merged = draft.texts.clone();
        for (key, text) in texts {
            merged
                .entry(lang.to_string())
                .or_default()
                .insert(key.clone(), text.clone());
        }
        {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            for (key, text) in texts {
                if text.is_empty() {
                    tx.execute(
                        "DELETE FROM page_texts WHERE slug = ?1 AND lang = ?2 AND key = ?3",
                        params![slug, lang, key],
                    )?;
                } else {
                    write_text(&tx, slug, &source, lang, key, text, &merged)?;
                }
            }
            tx.execute("UPDATE pages SET draft_changed = 1, updated_by = ?2, updated_at = ?3 WHERE slug = ?1", params![slug, who, now()])?;
            tx.commit()?;
        }
        self.get(slug)
    }

    /// Make the draft the next revision, and that revision what readers see.
    pub fn publish(&self, slug: &str, who: &str) -> Result<Revision, PageError> {
        let draft = self.get(slug)?;
        let t = now();
        let revision = {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            let tree: String = tx.query_row(
                "SELECT draft_tree FROM pages WHERE slug = ?1",
                params![slug],
                |r| r.get(0),
            )?;
            let next: i64 = tx.query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM page_revisions WHERE slug = ?1",
                params![slug],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO page_revisions (slug, revision, source_lang, jsx, tree, texts, published_by, published_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![slug, next, draft.summary.source_lang, draft.jsx, tree, serde_json::to_string(&draft.texts).context("texts")?, who, t],
            )?;
            tx.execute(
                "UPDATE pages SET live_revision = ?2, draft_changed = 0 WHERE slug = ?1",
                params![slug, next],
            )?;
            tx.commit()?;
            next
        };
        self.refresh_live(slug)?;
        Ok(Revision {
            revision,
            published_by: who.to_string(),
            published_at: t,
            live: true,
        })
    }

    /// Take a page down: readers get a 404; the draft and the revisions stay.
    pub fn unpublish(&self, slug: &str) -> Result<(), PageError> {
        let changed = self.lock()?.execute(
            "UPDATE pages SET live_revision = NULL, draft_changed = 1 WHERE slug = ?1",
            params![slug],
        )?;
        if changed == 0 {
            return Err(PageError::NotFound);
        }
        self.refresh_live(slug)?;
        Ok(())
    }

    pub fn revisions(&self, slug: &str) -> Result<Vec<Revision>, PageError> {
        let draft = self.get(slug)?;
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT revision, published_by, published_at FROM page_revisions WHERE slug = ?1 ORDER BY revision DESC")?;
        let rows = stmt.query_map(params![slug], |r| {
            let revision: i64 = r.get(0)?;
            Ok(Revision {
                revision,
                published_by: r.get(1)?,
                published_at: r.get(2)?,
                live: draft.summary.live_revision == Some(revision),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Put an old revision back into the draft, to change or publish again.
    pub fn restore(&self, slug: &str, revision: i64, who: &str) -> Result<Draft, PageError> {
        let row = {
            let conn = self.lock()?;
            conn.query_row(
                "SELECT source_lang, jsx, tree, texts FROM page_revisions WHERE slug = ?1 AND revision = ?2",
                params![slug, revision],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?
        };
        let Some((source, jsx_src, tree, texts)) = row else {
            return Err(PageError::NotFound);
        };
        let texts: Texts = serde_json::from_str(&texts).context("a revision's texts")?;
        {
            let mut conn = self.lock()?;
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE pages SET draft_jsx = ?2, draft_tree = ?3, draft_changed = 1, updated_by = ?4, updated_at = ?5 WHERE slug = ?1",
                params![slug, jsx_src, tree, who, now()],
            )?;
            tx.execute("DELETE FROM page_texts WHERE slug = ?1", params![slug])?;
            // Restored as published: each translation fresh against the source
            // it was published beside.
            for (lang, t) in &texts {
                for (key, text) in t {
                    write_text(&tx, slug, &source, lang, key, text, &texts)?;
                }
            }
            tx.commit()?;
        }
        self.get(slug)
    }

    /// Delete a page that is not live, with its draft and every revision.
    pub fn delete(&self, slug: &str) -> Result<(), PageError> {
        let draft = self.get(slug)?;
        if draft.summary.live_revision.is_some() {
            return Err(PageError::Conflict(
                "the page is live: unpublish it first".into(),
            ));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        for table in ["page_texts", "page_revisions", "pages"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE slug = ?1"),
                params![slug],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A page's tree as text an agent can read: each component, its props and
    /// its texts in `lang`, indented.
    pub fn outline(&self, data: &PageData) -> String {
        fn walk(v: &serde_json::Value, data: &PageData, depth: usize, out: &mut String) {
            let text = |k: &str| {
                data.texts
                    .get(k)
                    .map(|r| {
                        if r.lang == data.lang {
                            format!("{:?}", r.text)
                        } else {
                            format!("{:?} [{}, no {} yet]", r.text, r.lang, data.lang)
                        }
                    })
                    .unwrap_or_else(|| format!("(no text for {k})"))
            };
            let pad = "  ".repeat(depth);
            if let Some(key) = v.get("t").and_then(|t| t.as_str()) {
                out.push_str(&format!("{pad}{}\n", text(key)));
                return;
            }
            let c = v.get("c").and_then(|c| c.as_str()).unwrap_or("?");
            out.push_str(&format!("{pad}{c}"));
            if let Some(props) = v.get("p").and_then(|p| p.as_object()) {
                for (name, value) in props {
                    match value.get("t").and_then(|t| t.as_str()) {
                        Some(key) => out.push_str(&format!(" {name}={}", text(key))),
                        None => out.push_str(&format!(" {name}={value}")),
                    }
                }
            }
            out.push('\n');
            for child in v.get("ch").and_then(|c| c.as_array()).into_iter().flatten() {
                walk(child, data, depth + 1, out);
            }
        }
        let mut out = String::new();
        walk(&data.tree, data, 0, &mut out);
        out
    }
}

/// One text row; a translation carries the hash of the source text it now
/// stands beside, so a later change to the source shows it as stale.
fn write_text(
    tx: &Connection,
    slug: &str,
    source: &str,
    lang: &str,
    key: &str,
    text: &str,
    all: &Texts,
) -> rusqlite::Result<usize> {
    let hash = if lang == source {
        None
    } else {
        all.get(source)
            .and_then(|t| t.get(key))
            .map(|s| source_hash(s))
    };
    tx.execute(
        "INSERT INTO page_texts (slug, key, lang, text, source_hash) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(slug, key, lang) DO UPDATE SET text = ?4, source_hash = ?5",
        params![slug, key, lang, text, hash],
    )
}

#[cfg(test)]
mod tests;
