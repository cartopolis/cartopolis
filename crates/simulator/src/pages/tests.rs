use super::*;

fn service() -> PageService {
    let mut conn = Connection::open_in_memory().unwrap();
    crate::db::migrate(&mut conn).unwrap();
    PageService::new(Arc::new(Mutex::new(conn)), vec!["nl".into(), "en".into()]).unwrap()
}

const JSX: &str = r#"<Page title={$t('title')} description={$t('description')}>
  <Hero heading={$t('hero.heading')}>
    <Text lead>{$t('hero.lead')}</Text>
  </Hero>
  <Section><DownloadButtons /></Section>
</Page>"#;

fn texts(lang: &str, pairs: &[(&str, &str)]) -> Texts {
    let mut t = Texts::new();
    t.insert(
        lang.into(),
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    );
    t
}

fn dutch() -> Texts {
    texts(
        "nl",
        &[
            ("title", "Over **Cartopolis**"),
            ("description", "Een plek voor buren."),
            ("hero.heading", "Jouw stad, gedeeld"),
            ("hero.lead", "Praat en bouw samen."),
        ],
    )
}

fn messages(e: PageError) -> Vec<String> {
    match e {
        PageError::Invalid(p) => p.into_iter().map(|p| p.message).collect(),
        other => panic!("not invalid: {other:?}"),
    }
}

#[test]
fn a_page_is_written_published_and_served() {
    let s = service();
    let draft = s.put("over", JSX, None, &dutch(), "midas").unwrap();
    assert_eq!(draft.summary.source_lang, "nl");
    assert_eq!(draft.summary.title, "Over Cartopolis");
    assert_eq!(draft.summary.live_revision, None);
    assert_eq!(draft.summary.languages["en"].missing.len(), 4);
    assert!(draft.summary.languages["nl"].missing.is_empty());

    // Not live yet: readers get nothing, an editor the draft.
    assert!(s.live_data("over", "nl").is_none());
    let preview = s.draft_data("over", "en").unwrap();
    assert_eq!(preview.revision, None);
    assert_eq!(
        preview.texts["hero.heading"],
        Resolved {
            text: "Jouw stad, gedeeld".into(),
            lang: "nl".into()
        }
    );

    let rev = s.publish("over", "midas").unwrap();
    assert_eq!(rev.revision, 1);
    let live = s.live_data("over", "en").unwrap();
    assert_eq!(live.revision, Some(1));
    assert_eq!(live.languages, ["nl", "en"]);
    // The English falls back to the Dutch, and says so; the site's own texts
    // come in English.
    assert_eq!(live.texts["title"].lang, "nl");
    assert_eq!(
        live.texts["site.download.android"],
        Resolved {
            text: "Android app".into(),
            lang: "en".into()
        }
    );
    assert_eq!(live.tree["c"], "Page");
    assert_eq!(
        s.head("over", "nl").unwrap(),
        ("Over Cartopolis".into(), "Een plek voor buren.".into())
    );
    assert!(s.live_data("over", "de").is_none());
}

#[test]
fn readers_see_the_revision_until_the_next_publish() {
    let s = service();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    s.publish("over", "a").unwrap();
    s.set_texts(
        "over",
        "nl",
        &[("hero.heading".to_string(), "Nieuw".to_string())].into(),
        "a",
    )
    .unwrap();
    assert_eq!(
        s.live_data("over", "nl").unwrap().texts["hero.heading"].text,
        "Jouw stad, gedeeld"
    );
    assert!(s.get("over").unwrap().summary.draft_changed);
    s.publish("over", "a").unwrap();
    assert_eq!(
        s.live_data("over", "nl").unwrap().texts["hero.heading"].text,
        "Nieuw"
    );
    assert!(!s.get("over").unwrap().summary.draft_changed);
    assert_eq!(
        s.revisions("over")
            .unwrap()
            .iter()
            .map(|r| (r.revision, r.live))
            .collect::<Vec<_>>(),
        [(2, true), (1, false)]
    );
}

#[test]
fn a_translation_goes_stale_when_its_source_changes() {
    let s = service();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    let en = [("hero.heading".to_string(), "Your city, shared".to_string())].into();
    let d = s.set_texts("over", "en", &en, "a").unwrap();
    assert!(d.summary.languages["en"].stale.is_empty());
    assert_eq!(d.summary.languages["en"].missing.len(), 3);

    let d = s
        .set_texts(
            "over",
            "nl",
            &[("hero.heading".to_string(), "Jouw stad".to_string())].into(),
            "a",
        )
        .unwrap();
    assert_eq!(d.summary.languages["en"].stale, ["hero.heading"]);

    // Translating it again makes it fresh.
    let d = s
        .set_texts(
            "over",
            "en",
            &[("hero.heading".to_string(), "Your city".to_string())].into(),
            "a",
        )
        .unwrap();
    assert!(d.summary.languages["en"].stale.is_empty());

    // An empty text removes a translation; a source text cannot be removed.
    let d = s
        .set_texts(
            "over",
            "en",
            &[("hero.heading".to_string(), String::new())].into(),
            "a",
        )
        .unwrap();
    assert_eq!(d.summary.languages["en"].missing.len(), 4);
    let e = s
        .set_texts(
            "over",
            "nl",
            &[("hero.heading".to_string(), String::new())].into(),
            "a",
        )
        .unwrap_err();
    assert!(messages(e)[0].contains("cannot be empty"));
}

#[test]
fn what_a_save_refuses() {
    let s = service();
    let missing = texts("nl", &[("title", "Over")]);
    let got = messages(s.put("over", JSX, None, &missing, "a").unwrap_err());
    assert!(
        got.iter()
            .any(|m| m.contains("no nl text for $t('description')")),
        "{got:?}"
    );

    let mut extra = dutch();
    extra
        .get_mut("nl")
        .unwrap()
        .insert("nobody.uses".into(), "x".into());
    let got = messages(s.put("over", JSX, None, &extra, "a").unwrap_err());
    assert!(
        got.iter().any(|m| m.contains("uses no $t('nobody.uses')")),
        "{got:?}"
    );

    let got = messages(
        s.put("over", "<Page><Text>Hoi</Text></Page>", None, &dutch(), "a")
            .unwrap_err(),
    );
    assert!(got[0].contains("bare text"), "{got:?}");

    assert!(
        messages(s.put("Over!", JSX, None, &dutch(), "a").unwrap_err())[0]
            .contains("not a page address")
    );
    assert!(
        messages(s.put("over", JSX, Some("de"), &dutch(), "a").unwrap_err())[0]
            .contains("not one of")
    );
    // Nothing was written by any of it.
    assert!(matches!(s.get("over"), Err(PageError::NotFound)));

    s.put("over", JSX, None, &dutch(), "a").unwrap();
    assert!(messages(
        s.put("over", JSX, Some("en"), &Texts::new(), "a")
            .unwrap_err()
    )[0]
    .contains("does not change"));
}

#[test]
fn texts_follow_the_jsx() {
    let s = service();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    s.set_texts(
        "over",
        "en",
        &[("hero.lead".to_string(), "Talk".to_string())].into(),
        "a",
    )
    .unwrap();
    // The lead is gone from the page: its texts go with it, and a later save
    // needs no text for it.
    let shorter = JSX.replace("    <Text lead>{$t('hero.lead')}</Text>\n", "");
    let d = s.put("over", &shorter, None, &Texts::new(), "a").unwrap();
    assert!(!d.keys.contains(&"hero.lead".to_string()));
    assert!(d.texts.values().all(|t| !t.contains_key("hero.lead")));
}

#[test]
fn restore_and_delete() {
    let s = service();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    s.set_texts(
        "over",
        "en",
        &[("title".to_string(), "About".to_string())].into(),
        "a",
    )
    .unwrap();
    s.publish("over", "a").unwrap();
    s.set_texts(
        "over",
        "nl",
        &[("title".to_string(), "Anders".to_string())].into(),
        "a",
    )
    .unwrap();
    let d = s.restore("over", 1, "a").unwrap();
    assert_eq!(d.texts["nl"]["title"], "Over **Cartopolis**");
    assert!(d.summary.languages["en"].stale.is_empty());

    assert!(matches!(s.delete("over"), Err(PageError::Conflict(_))));
    s.unpublish("over").unwrap();
    assert!(s.live_data("over", "nl").is_none());
    s.delete("over").unwrap();
    assert!(matches!(s.get("over"), Err(PageError::NotFound)));
    assert!(s.revisions("over").is_err());
}

#[test]
fn live_pages_survive_a_restart() {
    let mut conn = Connection::open_in_memory().unwrap();
    crate::db::migrate(&mut conn).unwrap();
    let conn = Arc::new(Mutex::new(conn));
    let s = PageService::new(conn.clone(), vec!["nl".into(), "en".into()]).unwrap();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    s.publish("over", "a").unwrap();
    let again = PageService::new(conn, vec!["nl".into(), "en".into()]).unwrap();
    assert_eq!(again.live_data("over", "nl").unwrap().revision, Some(1));
}

#[test]
fn plain_drops_the_markup() {
    assert_eq!(
        plain("Over **Cartopolis**, *echt* [hier](/x)\n\nmeer \\*ster"),
        "Over Cartopolis, echt hier meer *ster"
    );
}

#[test]
fn an_outline_an_agent_can_read() {
    let s = service();
    s.put("over", JSX, None, &dutch(), "a").unwrap();
    let out = s.outline(&s.draft_data("over", "en").unwrap());
    assert!(out.starts_with("Page "), "{out}");
    assert!(
        out.contains("  Hero heading=\"Jouw stad, gedeeld\" [nl, no en yet]"),
        "{out}"
    );
    assert!(
        out.contains("      \"Praat en bouw samen.\" [nl, no en yet]"),
        "{out}"
    );
}

/// `/` is the chat until a home page is published, then that page in the
/// reader's language; a link with a query stays the chat's.
#[tokio::test]
async fn the_front_door() {
    let s = service();
    let dir = tempfile::tempdir().unwrap();
    let shell_file = dir.path().join("pages.html");
    std::fs::write(
        &shell_file,
        "<html lang=\"en\"><head><!--cms-head--><!--/cms-head--></head></html>",
    )
    .unwrap();
    let ask = |path: &'static str, accept: &'static str| {
        let (s, shell_file) = (s.clone(), shell_file.clone());
        async move {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(axum::http::header::ACCEPT_LANGUAGE, accept.parse().unwrap());
            let uri: axum::http::Uri = path.parse().unwrap();
            http::shell(
                &s,
                &render::Renderer::off(),
                &shell_file,
                "https://c.test",
                &uri,
                &headers,
            )
            .await
        }
    };
    assert!(ask("/", "en").await.is_none(), "no home page: the chat");

    s.put(HOME, JSX, None, &dutch(), "a").unwrap();
    assert!(
        ask("/", "en").await.is_none(),
        "a draft is not the front door"
    );
    s.publish(HOME, "a").unwrap();

    let to = ask("/", "en-GB,en;q=0.9").await.unwrap();
    assert_eq!(to.headers()[axum::http::header::LOCATION], "/en/");
    let to = ask("/", "fr").await.unwrap();
    assert_eq!(to.headers()[axum::http::header::LOCATION], "/nl/");
    assert!(
        ask("/?show=3", "en").await.is_none(),
        "the chat's old links"
    );

    let page = ask("/en/", "en").await.unwrap();
    assert_eq!(page.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(page.into_body(), 1 << 20)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body.contains("<link rel=\"canonical\" href=\"https://c.test/en/\" />"),
        "{body}"
    );
    assert!(
        body.contains("hreflang=\"x-default\" href=\"https://c.test/\""),
        "{body}"
    );
}

/// A live page goes out drawn, with the data it was drawn from beside it; a
/// draft, and any page the renderer cannot draw, goes out for the browser.
#[tokio::test]
async fn a_live_page_is_drawn_for_the_visit() {
    let s = service();
    let dir = tempfile::tempdir().unwrap();
    let shell_file = dir.path().join("pages.html");
    std::fs::write(
        &shell_file,
        "<html lang=\"en\"><head><!--cms-head--><!--/cms-head--></head><body data-cms></body></html>",
    )
    .unwrap();
    let bundle = dir.path().join(render::BUNDLE);
    std::fs::write(
        &bundle,
        "export async function renderPage(d) { return `<main>${d.texts['hero.heading'].text}</main>` }",
    )
    .unwrap();
    let drawn = render::Renderer::start(crate::scripting::BunEngine::development(), bundle);
    let ask = |path: &'static str, renderer: render::Renderer| {
        let (s, shell_file) = (s.clone(), shell_file.clone());
        async move {
            let uri: axum::http::Uri = path.parse().unwrap();
            let page = http::shell(
                &s,
                &renderer,
                &shell_file,
                "https://c.test",
                &uri,
                &axum::http::HeaderMap::new(),
            )
            .await
            .unwrap();
            let body = axum::body::to_bytes(page.into_body(), 1 << 20)
                .await
                .unwrap();
            String::from_utf8(body.to_vec()).unwrap()
        }
    };

    let mut words = dutch();
    words
        .get_mut("nl")
        .unwrap()
        .insert("hero.heading".into(), "</script><b>x</b>".into());
    s.put("over", JSX, None, &words, "a").unwrap();
    s.publish("over", "a").unwrap();

    let page = ask("/nl/over", drawn.clone()).await;
    assert!(
        page.contains("<body data-cms><main></script><b>x</b></main></body>"),
        "{page}"
    );
    // The data cannot end its own script element.
    let data = page
        .split("id=\"cms-data\">")
        .nth(1)
        .unwrap()
        .split("</script>")
        .next()
        .unwrap();
    assert!(
        data.contains("\\u003c/script>\\u003cb>x\\u003c/b>"),
        "{data}"
    );
    assert!(!data.contains('<'), "{data}");

    let draft = ask("/nl/over?draft", drawn).await;
    assert!(
        draft.contains("<body data-cms></body>"),
        "a draft is the browser's: {draft}"
    );
    assert!(!draft.contains("cms-data"), "{draft}");

    let unrendered = ask("/nl/over", render::Renderer::off()).await;
    assert!(
        unrendered.contains("<body data-cms></body>"),
        "{unrendered}"
    );
    assert!(
        unrendered.contains("id=\"cms-data\""),
        "the browser draws it from the data: {unrendered}"
    );
}
