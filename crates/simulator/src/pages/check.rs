//! A parsed page checked against the components that exist
//! (`crates/atlas/web/src/cms/registry.json`, the one description the web UI
//! draws with too, so the two cannot disagree about what a page may hold).

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

use super::jsx::{Child, Node, Pos, Problem, PropValue};

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub root: String,
    pub components: BTreeMap<String, ComponentSpec>,
    pub site_texts: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct ComponentSpec {
    pub doc: String,
    pub props: BTreeMap<String, PropSpec>,
    pub children: Children,
    #[serde(default)]
    pub allow: Option<Vec<String>>,
    pub example: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Children {
    None,
    Text,
    Blocks,
}

#[derive(Debug, Deserialize)]
pub struct PropSpec {
    #[serde(rename = "type")]
    pub kind: PropKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PropKind {
    Text,
    Url,
    Image,
    Enum,
    Number,
    Bool,
    Place,
}

pub const REGISTRY_JSON: &str = include_str!("../../../atlas/web/src/cms/registry.json");

pub static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    serde_json::from_str(REGISTRY_JSON).expect("registry.json is the web UI's and must parse")
});

/// More than a page needs; a bound on what one save may make the server hold.
pub const MAX_NODES: usize = 400;
pub const MAX_KEYS: usize = 400;
pub const MAX_TEXT_BYTES: usize = 8 * 1024;
pub const MAX_JSX_BYTES: usize = 64 * 1024;

/// A text key: lower-case words joined by dots, hyphens or underscores.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    (1..=100).contains(&key.len())
        && key.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
        && !key.contains("..")
        && !key.ends_with('.')
}

/// A link a page may carry: a site path, `https:` or `mailto:` — the same rule
/// as `safeHref` in `src/cms/tree.ts`.
#[must_use]
pub fn safe_href(href: &str) -> bool {
    if href.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    if let Some(rest) = href.strip_prefix('/') {
        return !rest.starts_with('/') && !rest.starts_with('\\');
    }
    if let Some(rest) = href.strip_prefix("https://") {
        return rest.chars().next().is_some_and(|c| c != '/');
    }
    href.strip_prefix("mailto:")
        .is_some_and(|rest| !rest.is_empty())
}

/// What follows `?place=` in a shared place link: `lat,lon`, then optionally the
/// height (`120m`), pitch and heading (`systems::place_link`).
#[must_use]
pub fn valid_place(place: &str) -> bool {
    let parts: Vec<&str> = place.split(',').collect();
    if !(2..=5).contains(&parts.len()) || place.len() > 80 {
        return false;
    }
    parts.iter().enumerate().all(|(i, part)| {
        let num = if i == 2 {
            part.strip_suffix('m').unwrap_or(part)
        } else {
            part
        };
        num.parse::<f64>().is_ok_and(f64::is_finite)
    })
}

/// The text keys a tree names, each once: an element's props (by name), then its children.
pub fn keys(node: &Node, into: &mut Vec<String>) {
    for v in node.p.values() {
        if let PropValue::Text(t) = v {
            if !into.contains(&t.t) {
                into.push(t.t.clone());
            }
        }
    }
    for c in &node.ch {
        match c {
            Child::Text(t) => {
                if !into.contains(&t.t) {
                    into.push(t.t.clone());
                }
            }
            Child::Node(n) => keys(n, into),
        }
    }
}

/// Everything wrong with a page's tree, or nothing. Every problem is reported at
/// once, so an agent fixes a page in one round rather than one error per try.
pub fn check(root: &Node) -> Vec<Problem> {
    let mut problems = Vec::new();
    let reg = &*REGISTRY;
    if root.c != reg.root {
        problems.push(Problem::at(
            root.at,
            format!("a page starts with <{}>, not <{}>", reg.root, root.c),
        ));
    }
    let mut count = 0;
    walk(root, true, &mut problems, &mut count);
    if count > MAX_NODES {
        problems.push(Problem::at(
            root.at,
            format!("{count} elements: a page holds at most {MAX_NODES}"),
        ));
    }
    let mut all = Vec::new();
    keys(root, &mut all);
    if all.len() > MAX_KEYS {
        problems.push(Problem::at(
            root.at,
            format!("{} texts: a page holds at most {MAX_KEYS}", all.len()),
        ));
    }
    problems
}

fn walk(node: &Node, is_root: bool, problems: &mut Vec<Problem>, count: &mut usize) {
    *count += 1;
    let reg = &*REGISTRY;
    let Some(spec) = reg.components.get(&node.c) else {
        let known: Vec<&str> = reg.components.keys().map(String::as_str).collect();
        problems.push(Problem::at(
            node.at,
            format!(
                "<{}> is not a component; there are {}",
                node.c,
                known.join(", ")
            ),
        ));
        return;
    };
    if !is_root && node.c == reg.root {
        problems.push(Problem::at(
            node.at,
            format!("<{}> stands only at the root, once", reg.root),
        ));
    }
    for (name, value) in &node.p {
        match spec.props.get(name) {
            None => {
                let known: Vec<&str> = spec.props.keys().map(String::as_str).collect();
                let has = if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                };
                problems.push(Problem::at(
                    node.at,
                    format!("<{}> has no prop {name}; its props are {has}", node.c),
                ));
            }
            Some(prop) => check_prop(node, name, prop, value, problems),
        }
    }
    for (name, prop) in &spec.props {
        if prop.required && !node.p.contains_key(name) {
            problems.push(Problem::at(node.at, format!("<{}> needs {name}", node.c)));
        }
    }
    match spec.children {
        Children::None => {
            if !node.ch.is_empty() {
                problems.push(Problem::at(
                    node.at,
                    format!("<{}> takes no children: close it with />", node.c),
                ));
            }
        }
        Children::Text => {
            if !matches!(node.ch.as_slice(), [Child::Text(_)]) {
                problems.push(Problem::at(
                    node.at,
                    format!(
                        "<{0}> holds exactly one {{$t('key')}}: <{0}>{{$t('…')}}</{0}>",
                        node.c
                    ),
                ));
            }
        }
        Children::Blocks => {
            for child in &node.ch {
                match child {
                    Child::Text(t) => problems.push(Problem::at(
                        node.at,
                        format!(
                            "<{}> holds components, not a text ($t('{}')); put it in a <Text>",
                            node.c, t.t
                        ),
                    )),
                    Child::Node(n) => {
                        if let Some(allow) = &spec.allow {
                            if reg.components.contains_key(&n.c) && !allow.contains(&n.c) {
                                problems.push(Problem::at(
                                    n.at,
                                    format!(
                                        "<{}> cannot stand in <{}>, which takes {}",
                                        n.c,
                                        node.c,
                                        allow.join(", ")
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    for child in &node.ch {
        if let Child::Text(t) = child {
            check_key(node.at, &t.t, problems);
        }
        if let Child::Node(n) = child {
            walk(n, false, problems, count);
        }
    }
}

fn check_key(at: Pos, key: &str, problems: &mut Vec<Problem>) {
    if !valid_key(key) {
        problems.push(Problem::at(
            at,
            format!(
                "{key:?} is not a text key: lower-case words joined by dots, like hero.heading"
            ),
        ));
    } else if key.starts_with("site.") && !REGISTRY.site_texts.contains_key(key) {
        let known: Vec<&str> = REGISTRY.site_texts.keys().map(String::as_str).collect();
        problems.push(Problem::at(at, format!("site. keys are the site's own texts ({}); a page's own keys do not start with site.", known.join(", "))));
    }
}

fn check_prop(
    node: &Node,
    name: &str,
    prop: &PropSpec,
    value: &PropValue,
    problems: &mut Vec<Problem>,
) {
    let at = node.at;
    let c = &node.c;
    let fail = |problems: &mut Vec<Problem>, what: String| {
        problems.push(Problem::at(at, format!("<{c}> {name}: {what}")))
    };
    match (prop.kind, value) {
        (PropKind::Text, PropValue::Text(t)) => check_key(at, &t.t, problems),
        (PropKind::Text, _) => fail(problems, "a reader sees it, so it is a {$t('key')}".into()),
        (_, PropValue::Text(_)) => fail(
            problems,
            "takes no text; it is not something a reader sees".into(),
        ),
        (PropKind::Url, PropValue::Str(s)) if safe_href(s) => {}
        (PropKind::Url, _) => fail(
            problems,
            "a link: a site path (/register), https: or mailto:".into(),
        ),
        (PropKind::Image, PropValue::Str(s)) if safe_href(s) && !s.starts_with("mailto:") => {}
        (PropKind::Image, _) => fail(
            problems,
            "a picture's address: a site path or https:".into(),
        ),
        (PropKind::Enum, PropValue::Str(s)) if prop.values.contains(s) => {}
        (PropKind::Enum, _) => fail(problems, format!("one of {}", prop.values.join(", "))),
        (PropKind::Number, PropValue::Number(_)) | (PropKind::Bool, PropValue::Bool(_)) => {}
        (PropKind::Number, _) => fail(problems, "a number, as {3}".into()),
        (PropKind::Bool, _) => fail(problems, format!("true or false; {name} alone means true")),
        (PropKind::Place, PropValue::Str(s)) if valid_place(s) => {}
        (PropKind::Place, _) => fail(
            problems,
            "a place: lat,lon, optionally then 120m,pitch,heading".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::jsx::parse;

    fn problems(src: &str) -> Vec<String> {
        check(&parse(src).unwrap())
            .into_iter()
            .map(|p| p.message)
            .collect()
    }

    const OK: &str = r#"<Page title={$t('title')} description={$t('description')}>
  <Hero heading={$t('hero.heading')} image="/img/groningen.svg">
    <Text lead>{$t('hero.lead')}</Text>
    <ButtonLink href="/register" variant="primary">{$t('hero.cta')}</ButtonLink>
  </Hero>
  <Section heading={$t('why.heading')}>
    <Columns>
      <Feature icon="lock" heading={$t('a.h')}>{$t('a.b')}</Feature>
    </Columns>
    <PlaceLink place="53.2165,6.5629,120m,-25,0">{$t('markt')}</PlaceLink>
    <Image src="https://example.org/a.jpg" alt={$t('img.alt')} />
    <DownloadButtons />
  </Section>
</Page>"#;

    #[test]
    fn the_registry_is_the_web_uis() {
        assert_eq!(REGISTRY.root, "Page");
        assert!(REGISTRY.components.contains_key("Hero"));
        for spec in REGISTRY.components.values() {
            for name in spec.allow.iter().flatten() {
                assert!(REGISTRY.components.contains_key(name), "{name}");
            }
            for prop in spec.props.values() {
                assert_eq!(prop.kind == PropKind::Enum, !prop.values.is_empty());
            }
        }
    }

    #[test]
    fn every_example_is_a_good_page() {
        for (name, spec) in &REGISTRY.components {
            let src = if name == "Page" {
                spec.example.clone()
            } else {
                format!(
                    "<Page title={{$t('t')}} description={{$t('d')}}>{}</Page>",
                    spec.example
                )
            };
            // An example may need a parent that allows it.
            let wrapped = match name.as_str() {
                "Feature" => src.replace(
                    &spec.example,
                    &format!("<Columns>{}</Columns>", spec.example),
                ),
                _ => src,
            };
            let tree = parse(&wrapped).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(check(&tree), vec![], "{name}");
        }
    }

    #[test]
    fn a_good_page_passes() {
        assert_eq!(problems(OK), Vec::<String>::new());
        let mut k = Vec::new();
        keys(&parse(OK).unwrap(), &mut k);
        assert_eq!(k[..3], ["description", "title", "hero.heading"]);
        assert_eq!(k.len(), 10);
    }

    #[test]
    fn what_is_refused() {
        let cases = [
            ("<Section />", "starts with <Page>"),
            ("<Page title={$t('t')} description={$t('d')}><Blink /></Page>", "not a component"),
            ("<Page title={$t('t')} />", "needs description"),
            ("<Page title='Over' description={$t('d')} />", "a reader sees it"),
            ("<Page title={$t('t')} description={$t('d')} colour='red' />", "no prop colour"),
            ("<Page title={$t('t')} description={$t('d')}><ButtonLink href='javascript:alert(1)'>{$t('x')}</ButtonLink></Page>", "a link"),
            ("<Page title={$t('t')} description={$t('d')}><ButtonLink href='//evil.org'>{$t('x')}</ButtonLink></Page>", "a link"),
            ("<Page title={$t('t')} description={$t('d')}><Image src='mailto:a@b' alt={$t('a')} /></Page>", "picture"),
            ("<Page title={$t('t')} description={$t('d')}><Columns><Feature icon='skull' heading={$t('h')}>{$t('b')}</Feature></Columns></Page>", "one of"),
            ("<Page title={$t('t')} description={$t('d')}><Hero heading={$t('h')}><Section /></Hero></Page>", "cannot stand in <Hero>"),
            ("<Page title={$t('t')} description={$t('d')}><Text /></Page>", "exactly one"),
            ("<Page title={$t('t')} description={$t('d')}><DownloadButtons><Text>{$t('x')}</Text></DownloadButtons></Page>", "no children"),
            ("<Page title={$t('t')} description={$t('d')}>{$t('loose')}</Page>", "put it in a <Text>"),
            ("<Page title={$t('t')} description={$t('d')}><Page title={$t('t')} description={$t('d')} /></Page>", "only at the root"),
            ("<Page title={$t('Title')} description={$t('d')} />", "not a text key"),
            ("<Page title={$t('site.mine')} description={$t('d')} />", "site's own texts"),
            ("<Page title={$t('t')} description={$t('d')}><PlaceLink place='Grote Markt'>{$t('x')}</PlaceLink></Page>", "a place"),
            ("<Page title={$t('t')} description={$t('d')}><Text lead='yes'>{$t('x')}</Text></Page>", "true or false"),
        ];
        for (src, expect) in cases {
            let got = problems(src);
            assert!(
                got.iter().any(|m| m.contains(expect)),
                "{src}: wanted {expect:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn every_problem_at_once() {
        let got = problems("<Page title='a' description='b'><Blink /></Page>");
        assert_eq!(got.len(), 3, "{got:?}");
    }

    #[test]
    fn links_and_places() {
        for ok in [
            "/register",
            "/server/",
            "https://example.org/x",
            "mailto:a@b.nl",
        ] {
            assert!(safe_href(ok), "{ok}");
        }
        for bad in [
            "//evil.org",
            "/\\evil.org",
            "javascript:alert(1)",
            "http://plain.org",
            "data:text/html,x",
            "https://",
            "/a b",
        ] {
            assert!(!safe_href(bad), "{bad}");
        }
        assert!(valid_place("53.2194,6.5665"));
        assert!(valid_place("53.21940,6.56650,120m,-12.5,45.0"));
        assert!(!valid_place("53.2"));
        assert!(!valid_place("a,b"));
        assert!(!valid_place("1,2,3,4,5,6"));
    }

    #[test]
    fn keys_look_like_keys() {
        for ok in ["title", "hero.heading", "why.map-1.body", "a_b", "2col"] {
            assert!(valid_key(ok), "{ok}");
        }
        for bad in [
            "",
            "Title",
            "a..b",
            "a.",
            ".a",
            "a b",
            "a/b",
            &"x".repeat(101),
        ] {
            assert!(!valid_key(bad), "{bad}");
        }
    }
}
