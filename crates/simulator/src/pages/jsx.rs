//! A content page's JSX, read into a tree — never run (`docs/mockups/cms`).
//!
//! The subset is small on purpose: elements, attributes whose value is a
//! string, a number, `true`/`false` or `{$t('key')}`, and children that are
//! elements or `{$t('key')}`. Anything else — an expression, a spread, a
//! closure, text typed straight into the page — is an error with the line and
//! column, because the page is data and the only code that draws it is the web
//! UI's own. Which elements and props exist is [`super::check`]'s question, not
//! this module's: here a lower-case tag is refused only because it cannot be a
//! component (they are capitalised), so the message can say so.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `{$t('key')}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextRef {
    pub t: String,
}

/// A prop's value. Untagged, in the order serde tries them: a `{"t": …}` object
/// is a text reference, never a string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropValue {
    Text(TextRef),
    Bool(bool),
    Number(f64),
    Str(String),
}

/// One element of the tree, as `src/cms/tree.ts` reads it: `c` the component,
/// `p` its props, `ch` its children.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub c: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub p: BTreeMap<String, PropValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ch: Vec<Child>,
    /// Where the element starts in the source, for [`super::check`]'s messages.
    #[serde(skip)]
    pub at: Pos,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Child {
    Text(TextRef),
    Node(Node),
}

/// A place in the source, both counted from 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

/// Why a page was refused: where, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl Problem {
    pub fn at(pos: Pos, message: impl Into<String>) -> Self {
        Self {
            line: pos.line,
            col: pos.col,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{} {}", self.line, self.col, self.message)
    }
}

/// Deeper than any page needs, and shallow enough that the recursion below
/// cannot run out of stack on a hostile input.
const MAX_DEPTH: usize = 32;

/// Read a page's JSX into its tree, or say where it stops being the subset.
pub fn parse(src: &str) -> Result<Node, Problem> {
    let mut p = Parser {
        chars: src.chars().collect(),
        i: 0,
        line: 1,
        col: 1,
    };
    p.skip_space_and_comments()?;
    if p.peek() != Some('<') {
        return Err(Problem::at(
            p.pos(),
            "a page is one element, starting with <Page",
        ));
    }
    let root = p.element(0)?;
    p.skip_space_and_comments()?;
    if p.peek().is_some() {
        return Err(Problem::at(
            p.pos(),
            "there is something after the page's last closing tag; a page is one element",
        ));
    }
    Ok(root)
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    line: u32,
    col: u32,
}

impl Parser {
    fn pos(&self) -> Pos {
        Pos {
            line: self.line,
            col: self.col,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.i + ahead).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(k, c)| self.peek_at(k) == Some(c))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn expect(&mut self, c: char, what: &str) -> Result<(), Problem> {
        if self.peek() == Some(c) {
            self.bump();
            Ok(())
        } else {
            Err(Problem::at(self.pos(), format!("expected {what}")))
        }
    }

    fn skip_space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    /// Whitespace, and `{/* … */}` comments where a child could stand.
    fn skip_space_and_comments(&mut self) -> Result<(), Problem> {
        loop {
            self.skip_space();
            if self.starts_with("{/*") {
                let at = self.pos();
                while !self.starts_with("*/") {
                    if self.bump().is_none() {
                        return Err(Problem::at(at, "a comment that never ends"));
                    }
                }
                self.bump();
                self.bump();
                self.skip_space();
                self.expect('}', "} after the comment")?;
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        s
    }

    fn element(&mut self, depth: usize) -> Result<Node, Problem> {
        let at = self.pos();
        if depth > MAX_DEPTH {
            return Err(Problem::at(
                at,
                format!("nested deeper than {MAX_DEPTH} elements"),
            ));
        }
        self.expect('<', "<")?;
        let c = self.name();
        if c.is_empty() {
            return Err(Problem::at(
                at,
                "an element needs a name: <>…</> fragments are not part of a page",
            ));
        }
        if !c.starts_with(|ch: char| ch.is_ascii_uppercase()) {
            return Err(Problem::at(
                at,
                format!("<{c}> is not a component: a page is built only from the components `components` lists, whose names start with a capital"),
            ));
        }
        let mut p = BTreeMap::new();
        loop {
            self.skip_space();
            match self.peek() {
                Some('/') => {
                    self.bump();
                    self.expect('>', "> to close the element")?;
                    return Ok(Node {
                        c,
                        p,
                        ch: Vec::new(),
                        at,
                    });
                }
                Some('>') => {
                    self.bump();
                    break;
                }
                Some('{') => {
                    return Err(Problem::at(
                        self.pos(),
                        "{…} cannot stand among the props; write name={…}",
                    ));
                }
                Some(_) => {
                    let prop_at = self.pos();
                    let name = self.name();
                    if name.is_empty() {
                        return Err(Problem::at(
                            prop_at,
                            format!("unexpected {:?} in <{c}>", self.peek().unwrap_or(' ')),
                        ));
                    }
                    self.skip_space();
                    let value = if self.peek() == Some('=') {
                        self.bump();
                        self.skip_space();
                        self.prop_value()?
                    } else {
                        PropValue::Bool(true)
                    };
                    if p.insert(name.clone(), value).is_some() {
                        return Err(Problem::at(prop_at, format!("{name} is given twice")));
                    }
                }
                None => return Err(Problem::at(at, format!("<{c}> is never closed"))),
            }
        }
        let mut ch = Vec::new();
        loop {
            self.skip_space_and_comments()?;
            match self.peek() {
                None => {
                    return Err(Problem::at(
                        at,
                        format!("<{c}> is never closed: add </{c}>"),
                    ))
                }
                Some('<') if self.peek_at(1) == Some('/') => {
                    let close_at = self.pos();
                    self.bump();
                    self.bump();
                    let closing = self.name();
                    self.skip_space();
                    self.expect('>', ">")?;
                    if closing != c {
                        return Err(Problem::at(
                            close_at,
                            format!(
                                "</{closing}> closes <{c}> (opened at {}:{})",
                                at.line, at.col
                            ),
                        ));
                    }
                    return Ok(Node { c, p, ch, at });
                }
                Some('<') => ch.push(Child::Node(self.element(depth + 1)?)),
                Some('{') => {
                    let expr_at = self.pos();
                    self.bump();
                    self.skip_space();
                    match self.expression()? {
                        PropValue::Text(t) => ch.push(Child::Text(t)),
                        _ => {
                            return Err(Problem::at(expr_at, "inside an element only {$t('key')} may stand: every text a reader sees is a $t"));
                        }
                    }
                    self.skip_space();
                    self.expect('}', "} after the $t")?;
                }
                Some(_) => {
                    let text_at = self.pos();
                    let mut text = String::new();
                    while let Some(ch) = self.peek() {
                        if ch == '<' || ch == '{' || text.chars().count() >= 40 {
                            break;
                        }
                        text.push(ch);
                        self.bump();
                    }
                    return Err(Problem::at(
                        text_at,
                        format!("bare text in <{c}> ({:?}): write {{$t('…')}} and give the words in texts, so they can be translated", text.trim()),
                    ));
                }
            }
        }
    }

    fn prop_value(&mut self) -> Result<PropValue, Problem> {
        match self.peek() {
            Some(q @ ('"' | '\'')) => Ok(PropValue::Str(self.quoted(q)?)),
            Some('{') => {
                self.bump();
                self.skip_space();
                let v = self.expression()?;
                self.skip_space();
                self.expect('}', "} after the value")?;
                Ok(v)
            }
            _ => Err(Problem::at(self.pos(), "a prop's value is \"text\" or {…}")),
        }
    }

    /// What may stand between `{` and `}`: `$t('key')`, a number, `true`,
    /// `false` or a quoted string.
    fn expression(&mut self) -> Result<PropValue, Problem> {
        let at = self.pos();
        if self.starts_with("$t(") {
            for _ in 0..3 {
                self.bump();
            }
            self.skip_space();
            let key = match self.peek() {
                Some(q @ ('"' | '\'' | '`')) => self.quoted(q)?,
                _ => {
                    return Err(Problem::at(
                        self.pos(),
                        "$t takes the key as a quoted string: $t('hero.heading')",
                    ))
                }
            };
            self.skip_space();
            if self.peek() == Some(',') {
                return Err(Problem::at(
                    self.pos(),
                    "$t takes only the key; a page has no values to put into a text",
                ));
            }
            self.expect(')', ") after the key")?;
            return Ok(PropValue::Text(TextRef { t: key }));
        }
        match self.peek() {
            Some(q @ ('"' | '\'')) => Ok(PropValue::Str(self.quoted(q)?)),
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let mut s = String::new();
                while let Some(c) = self.peek() {
                    if c == '-' || c == '.' || c.is_ascii_digit() {
                        s.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
                s.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .map(PropValue::Number)
                    .ok_or_else(|| Problem::at(at, format!("{s} is not a number")))
            }
            _ => {
                let word = self.name();
                match word.as_str() {
                    "true" => Ok(PropValue::Bool(true)),
                    "false" => Ok(PropValue::Bool(false)),
                    _ => Err(Problem::at(at, "only $t('key'), a number, true, false or a quoted string may stand in {…}: a page holds no code")),
                }
            }
        }
    }

    fn quoted(&mut self, q: char) -> Result<String, Problem> {
        let at = self.pos();
        self.bump();
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(Problem::at(at, "a quoted string that never ends")),
                Some(c) if c == q => return Ok(s),
                // Only the quote itself and the backslash are escaped.
                Some('\\') if self.peek().is_some_and(|c| c == q || c == '\\') => {
                    s.extend(self.bump());
                }
                Some('$') if q == '`' && self.peek() == Some('{') => {
                    return Err(Problem::at(at, "a key is plain text: no ${…} in it"));
                }
                Some(c) => s.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str) -> PropValue {
        PropValue::Text(TextRef { t: key.into() })
    }

    #[test]
    fn reads_a_page() {
        let src = r#"<Page title={$t('title')} description={ $t("description") }>
  {/* the opening */}
  <Hero heading={$t('hero.heading')} image="/img/a.svg">
    <Text lead>{$t('hero.lead')}</Text>
  </Hero>
  <Columns count={3} wide={false} />
</Page>"#;
        let page = parse(src).unwrap();
        assert_eq!(page.c, "Page");
        assert_eq!(page.p["title"], t("title"));
        assert_eq!(page.p["description"], t("description"));
        let Child::Node(hero) = &page.ch[0] else {
            panic!()
        };
        assert_eq!(hero.p["image"], PropValue::Str("/img/a.svg".into()));
        assert_eq!(hero.at, Pos { line: 3, col: 3 });
        let Child::Node(text) = &hero.ch[0] else {
            panic!()
        };
        assert_eq!(text.p["lead"], PropValue::Bool(true));
        assert_eq!(
            text.ch,
            vec![Child::Text(TextRef {
                t: "hero.lead".into()
            })]
        );
        let Child::Node(cols) = &page.ch[1] else {
            panic!()
        };
        assert_eq!(cols.p["count"], PropValue::Number(3.0));
        assert_eq!(cols.p["wide"], PropValue::Bool(false));
    }

    #[test]
    fn the_tree_is_what_the_web_ui_reads() {
        let page = parse("<Page title={$t('a')}><Text>{$t('b')}</Text></Page>").unwrap();
        assert_eq!(
            serde_json::to_string(&page).unwrap(),
            r#"{"c":"Page","p":{"title":{"t":"a"}},"ch":[{"c":"Text","ch":[{"t":"b"}]}]}"#
        );
        let back: Node = serde_json::from_str(&serde_json::to_string(&page).unwrap()).unwrap();
        assert_eq!(back.c, "Page");
        assert_eq!(back.ch.len(), 1);
    }

    fn refused(src: &str) -> Problem {
        parse(src).expect_err(src)
    }

    #[test]
    fn bare_text_is_refused_where_it_stands() {
        let p = refused("<Page>\n  <Text>Welkom</Text>\n</Page>");
        assert_eq!((p.line, p.col), (2, 9));
        assert!(p.message.contains("bare text"), "{}", p.message);
        assert!(p.message.contains("Welkom"), "{}", p.message);
    }

    #[test]
    fn code_is_refused() {
        for src in [
            "<Page>{alert(1)}</Page>",
            "<Page title={1 + 1} />",
            "<Page title={window.name} />",
            "<Page {...props} />",
            "<Page title={$t('a', {n: 1})} />",
            "<Page title={$t(key)} />",
            "<Page>{'a string child'}</Page>",
            "<Page title={`${x}`} />",
        ] {
            refused(src);
        }
    }

    #[test]
    fn html_is_refused() {
        assert!(refused("<Page><div /></Page>")
            .message
            .contains("not a component"));
        assert!(refused("<Page><script>{$t('x')}</script></Page>")
            .message
            .contains("not a component"));
        assert!(refused("<Page><></></Page>").message.contains("fragments"));
    }

    #[test]
    fn structure_errors() {
        assert!(refused("<Page><Text></Page>")
            .message
            .contains("closes <Text>"));
        assert!(refused("<Page>").message.contains("never closed"));
        assert!(refused("<Page /><Page />").message.contains("one element"));
        assert!(refused("hello").message.contains("one element"));
        assert!(refused("<Page a='1' a='2' />").message.contains("twice"));
        assert!(refused("<Page>{/* open</Page>")
            .message
            .contains("never ends"));
    }

    #[test]
    fn depth_is_bounded() {
        let mut src = String::new();
        for _ in 0..40 {
            src.push_str("<Section>");
        }
        for _ in 0..40 {
            src.push_str("</Section>");
        }
        assert!(refused(&src).message.contains("deeper"));
    }
}
