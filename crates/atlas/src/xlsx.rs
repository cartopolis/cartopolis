//! **Just enough of a spreadsheet**: the cells of one sheet of an `.xlsx`, as
//! text, for a publisher that offers its figures only that way (PBL's WLO
//! regional tables, `official_areas::wlo_2025`).
//!
//! An `.xlsx` is a zip of XML parts. This reads the zip's central directory,
//! inflates the parts it needs with `flate2`, and scans the sheet XML for
//! cells — no styles, no formulas (a formula cell's cached value is read like
//! any other), no dates. A crate for it would be the first zip reader in the
//! lockfile for one table.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{bail, Context};

/// The parts of a workbook, by their path in the zip.
pub struct Workbook {
    parts: HashMap<String, Vec<u8>>,
}

fn u16_at(b: &[u8], at: usize) -> anyhow::Result<usize> {
    let s = b.get(at..at + 2).context("zip: cut short")?;
    Ok(usize::from(u16::from_le_bytes([s[0], s[1]])))
}

fn u32_at(b: &[u8], at: usize) -> anyhow::Result<usize> {
    let s = b.get(at..at + 4).context("zip: cut short")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

impl Workbook {
    /// Read a workbook's zip.
    pub fn read(bytes: &[u8]) -> anyhow::Result<Workbook> {
        // The end-of-central-directory record, searched for from the end: a
        // trailing comment can sit after it.
        let eocd = (0..bytes.len().saturating_sub(21))
            .rev()
            .find(|&i| bytes[i..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
            .context("not a zip file")?;
        let entries = u16_at(bytes, eocd + 10)?;
        let mut at = u32_at(bytes, eocd + 16)?;
        let mut parts = HashMap::new();
        for _ in 0..entries {
            if !bytes[at..].starts_with(&[0x50, 0x4b, 0x01, 0x02]) {
                bail!("zip: bad central directory");
            }
            let method = u16_at(bytes, at + 10)?;
            let size = u32_at(bytes, at + 20)?;
            let (name_len, extra_len, comment_len) = (
                u16_at(bytes, at + 28)?,
                u16_at(bytes, at + 30)?,
                u16_at(bytes, at + 32)?,
            );
            let local = u32_at(bytes, at + 42)?;
            let name = String::from_utf8_lossy(
                bytes
                    .get(at + 46..at + 46 + name_len)
                    .context("zip: cut short")?,
            )
            .into_owned();
            at += 46 + name_len + extra_len + comment_len;
            // The local header repeats the name and has its own extra field.
            let data = local + 30 + u16_at(bytes, local + 26)? + u16_at(bytes, local + 28)?;
            let raw = bytes.get(data..data + size).context("zip: cut short")?;
            let body = match method {
                0 => raw.to_vec(),
                8 => {
                    let mut out = Vec::new();
                    flate2::read::DeflateDecoder::new(raw)
                        .read_to_end(&mut out)
                        .with_context(|| format!("zip: inflating {name}"))?;
                    out
                }
                m => bail!("zip: {name} is packed with method {m}"),
            };
            parts.insert(name, body);
        }
        Ok(Workbook { parts })
    }

    fn part(&self, name: &str) -> anyhow::Result<String> {
        let b = self
            .parts
            .get(name)
            .with_context(|| format!("workbook has no {name}"))?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }

    /// A sheet's rows by its name on the tab, each cell's text in column
    /// order; an empty cell is an empty string.
    pub fn sheet(&self, name: &str) -> anyhow::Result<Vec<Vec<String>>> {
        let book = self.part("xl/workbook.xml")?;
        let rid = elements(&book, "sheet")
            .find(|s| attr(s, "name").as_deref() == Some(name))
            .and_then(|s| attr(s, "r:id"))
            .with_context(|| format!("no sheet {name:?}"))?;
        let rels = self.part("xl/_rels/workbook.xml.rels")?;
        let target = elements(&rels, "Relationship")
            .find(|r| attr(r, "Id").as_deref() == Some(rid.as_str()))
            .and_then(|r| attr(r, "Target"))
            .with_context(|| format!("sheet {name:?} has no part"))?;
        let path = match target.strip_prefix('/') {
            Some(abs) => abs.to_owned(),
            None => format!("xl/{target}"),
        };
        let shared: Vec<String> = match self.part("xl/sharedStrings.xml") {
            Ok(s) => blocks(&s, "si").map(|si| texts(si, "t")).collect(),
            Err(_) => Vec::new(),
        };
        let sheet = self.part(&path)?;
        let mut rows = Vec::new();
        for row in blocks(&sheet, "row") {
            let mut cells: Vec<String> = Vec::new();
            for (open, body) in cells_of(row) {
                let col = attr(open, "r").map_or(cells.len(), |r| column(&r));
                let value = match attr(open, "t").as_deref() {
                    Some("s") => texts(body, "v")
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i).cloned())
                        .unwrap_or_default(),
                    Some("inlineStr") => texts(body, "t"),
                    _ => texts(body, "v"),
                };
                if cells.len() < col {
                    cells.resize(col, String::new());
                }
                cells.push(value);
            }
            rows.push(cells);
        }
        Ok(rows)
    }
}

/// A cell reference's column, from 0: `"C7"` → 2, `"AA1"` → 26.
fn column(r: &str) -> usize {
    r.bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0, |n, b| {
            n * 26 + usize::from(b.to_ascii_uppercase() - b'A' + 1)
        })
        .saturating_sub(1)
}

/// Each `<tag …>` or `<tag …/>` opening tag in `s`, without its brackets.
fn elements<'a>(s: &'a str, tag: &str) -> impl Iterator<Item = &'a str> + 'a {
    let open = format!("<{tag} ");
    let found: Vec<&'a str> = s
        .match_indices(open.as_str())
        .filter_map(|(i, _)| {
            let rest = &s[i + 1..];
            rest.find('>').map(|end| &rest[..end])
        })
        .collect();
    found.into_iter()
}

/// The bodies of each `<tag>…</tag>` (with or without attributes) in `s`.
fn blocks<'a>(s: &'a str, tag: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let close = format!("</{tag}>");
    let mut at = 0;
    std::iter::from_fn(move || loop {
        let i = at + s[at..].find(&format!("<{tag}"))?;
        let after = &s[i + 1 + tag.len()..];
        // `<row` must not match `<rowBreaks`.
        if !after.starts_with([' ', '>', '/']) {
            at = i + 1;
            continue;
        }
        let open_end = i + s[i..].find('>')?;
        if s[..open_end].ends_with('/') {
            at = open_end + 1;
            return Some("");
        }
        let end = open_end + s[open_end..].find(&close)?;
        at = end + close.len();
        return Some(&s[open_end + 1..end]);
    })
}

/// Each cell `<c …>…</c>` or `<c …/>` of a row: its opening tag and body.
fn cells_of(row: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = row[at..].find("<c").map(|i| at + i) {
        let Some(open_end) = row[i..].find('>').map(|e| i + e) else {
            break;
        };
        let open = &row[i + 1..open_end];
        if !open.starts_with("c ") && open != "c" && !open.starts_with("c/") {
            at = open_end + 1;
            continue;
        }
        if open.ends_with('/') {
            out.push((open, ""));
            at = open_end + 1;
        } else {
            let end = row[open_end..]
                .find("</c>")
                .map_or(row.len(), |e| open_end + e);
            out.push((open, &row[open_end + 1..end]));
            at = end + 4;
        }
    }
    out
}

/// Every `<tag>` text in `s` joined (a rich-text string is several runs),
/// unescaped.
fn texts(s: &str, tag: &str) -> String {
    let mut out = String::new();
    for b in blocks(s, tag) {
        let raw = quick_xml::escape::unescape(b).map_or_else(|_| b.to_owned(), |c| c.into_owned());
        out.push_str(&raw);
    }
    out
}

/// An attribute's value in an opening tag.
fn attr(open: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let i = open.find(&key)? + key.len();
    let end = open[i..].find('"')? + i;
    Some(
        quick_xml::escape::unescape(&open[i..end])
            .map_or_else(|_| open[i..end].to_owned(), |c| c.into_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_from_a() {
        assert_eq!(column("A1"), 0);
        assert_eq!(column("C7"), 2);
        assert_eq!(column("Z3"), 25);
        assert_eq!(column("AA1"), 26);
    }

    #[test]
    fn a_row_reads_shared_inline_and_number_cells_in_place() {
        let shared =
            "<sst><si><t>code</t></si><si><r><t>Oost-</t></r><r><t>Groningen</t></r></si></sst>";
        let s: Vec<String> = blocks(shared, "si").map(|si| texts(si, "t")).collect();
        assert_eq!(s, ["code", "Oost-Groningen"]);
        let row = r#"<c r="A2" t="s"><v>1</v></c><c r="C2"><v>134</v></c><c r="D2" s="3"/><c r="E2" t="inlineStr"><is><t>a &amp; b</t></is></c>"#;
        let cells = cells_of(row);
        assert_eq!(cells.len(), 4);
        assert_eq!(attr(cells[1].0, "r").as_deref(), Some("C2"));
        assert_eq!(texts(cells[1].1, "v"), "134");
        assert_eq!(texts(cells[3].1, "t"), "a & b");
    }
}
