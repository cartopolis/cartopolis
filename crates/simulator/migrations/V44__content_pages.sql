-- Content pages (docs/mockups/cms, crate::pages): JSX blocks whose every word
-- is a $t text, kept per page and language.

-- A page and its one draft. The tree is the JSX read into JSON, which is what
-- the web UI draws; the JSX is kept as written, for an editor to read back.
CREATE TABLE IF NOT EXISTS pages (
    slug           TEXT PRIMARY KEY,
    source_lang    TEXT NOT NULL,          -- the language it was written in
    draft_jsx      TEXT NOT NULL,
    draft_tree     TEXT NOT NULL,
    live_revision  INTEGER,                -- page_revisions.revision readers see; NULL = not live
    draft_changed  INTEGER NOT NULL DEFAULT 1,
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,       -- unix seconds
    updated_by     TEXT NOT NULL,
    updated_at     INTEGER NOT NULL
);

-- The draft's texts. A translation keeps a hash of the source text it stands
-- beside; when the source changes, the two no longer match and it shows stale.
CREATE TABLE IF NOT EXISTS page_texts (
    slug         TEXT NOT NULL,
    key          TEXT NOT NULL,
    lang         TEXT NOT NULL,
    text         TEXT NOT NULL,
    source_hash  TEXT,                     -- NULL in the source language
    PRIMARY KEY (slug, key, lang)
);

-- What was published, never changed: going back is restoring one into the draft.
CREATE TABLE IF NOT EXISTS page_revisions (
    slug          TEXT NOT NULL,
    revision      INTEGER NOT NULL,
    source_lang   TEXT NOT NULL,
    jsx           TEXT NOT NULL,
    tree          TEXT NOT NULL,
    texts         TEXT NOT NULL,           -- JSON {lang: {key: text}}
    published_by  TEXT NOT NULL,
    published_at  INTEGER NOT NULL,
    PRIMARY KEY (slug, revision)
);
