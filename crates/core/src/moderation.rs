//! Text filtering for user-generated content.
//!
//! This exists because the App Store requires it. Guideline 1.2 asks an app
//! carrying user-generated content for four things, and this module is the first
//! of them — "a method for filtering objectionable material from being posted to
//! the app". The other three are a report path, a block, and published contact
//! details; they live in [`crate::protocol`] (`ReportUser`, `SetBlocked`) and in
//! the client's About panel. All four are needed together, so changing one is a
//! reason to re-read the others.
//!
//! # Why the filter is here and not in the simulator
//!
//! It is pure text in, text out, so it belongs where both ends can reach it: the
//! server masks before it stores and delivers, and a client can mask a line it
//! renders from *local* scrollback the server never saw. Keeping one
//! implementation is also what keeps the two from disagreeing about what was
//! filtered, which reads to a user as the filter being broken.
//!
//! # What it deliberately does not try to be
//!
//! It masks words from a list. It is not a classifier and cannot judge intent,
//! context or a sentence that is abusive without containing a listed word — that
//! is what the *report* path is for, and why a filter alone does not satisfy the
//! guideline. The design goal is a low false-positive rate: a filter that mangles
//! ordinary speech gets worked around, and once people are typing around it the
//! reports stop describing what was actually said.
//!
//! Two consequences of aiming that way. Matching is **whole-word**, so
//! "Scunthorpe" and "classic" survive (a substring match is how every naive
//! filter earns its reputation). And the built-in list is short, unambiguous and
//! **replaceable**: an operator supplies their own list through
//! `simulator`'s `moderation_words` config, because the words a community needs
//! blocked are a property of that community and not of this source file.

use std::collections::HashSet;

/// The built-in seed list: terms with essentially no innocent use.
///
/// Deliberately small. It is a floor for a server whose operator has configured
/// nothing, not an attempt at coverage — see the module note on false positives.
/// Slurs are the ones worth shipping by default, because they are the class an
/// operator is least likely to be able to enumerate before the first incident.
pub const DEFAULT_WORDS: &[&str] = &[
    "nigger", "nigga", "faggot", "fag", "tranny", "kike", "spic", "chink", "wetback", "retard",
    "retarded", "cunt", "whore", "rape", "rapist",
];

/// Characters people substitute for letters when typing around a filter.
///
/// Only the unambiguous ones. `l` → `i` is a real substitution and also turns
/// "flag" into something the list matches, so it is not here: an aggressive
/// de-obfuscation table costs exactly the false positives the module note is
/// about.
fn deleet(c: char) -> char {
    match c {
        '@' | '4' => 'a',
        '0' => 'o',
        '3' => 'e',
        '1' | '!' => 'i',
        '$' | '5' => 's',
        '7' => 't',
        c => c.to_ascii_lowercase(),
    }
}

/// A compiled word list.
///
/// Cheap to build and cheap to clone; a server builds one at startup and shares
/// it. Words are normalised on the way in so the list may be written however the
/// operator likes.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Normalised forms — what an ordinary candidate is compared against.
    words: HashSet<String>,
    /// The same words with *every* repeated letter collapsed to one. Consulted
    /// only for a candidate that padded a letter three or more deep; see
    /// [`Filter::mask`].
    squeezed: HashSet<String>,
}

impl Filter {
    /// The built-in [`DEFAULT_WORDS`].
    #[must_use]
    pub fn seeded() -> Self {
        Self::from_words(DEFAULT_WORDS.iter().copied())
    }

    /// A filter over an operator's own list. Blank entries and `#` comments are
    /// skipped, so a plain text file can be handed straight to this.
    pub fn from_words<'a>(words: impl IntoIterator<Item = &'a str>) -> Self {
        let mut this = Self::default();
        for w in words {
            let w = w.trim();
            if w.is_empty() || w.starts_with('#') {
                continue;
            }
            let normalized = normalize_word(w);
            if normalized.is_empty() {
                continue;
            }
            this.squeezed.insert(squeeze(&normalized));
            this.words.insert(normalized);
        }
        this
    }

    /// Whether the list is empty — i.e. whether this filter can mask anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Mask every listed word in `text`, returning the masked text and how many
    /// words were hit.
    ///
    /// Masked runs keep their original length, so a line's shape survives and the
    /// reader can tell that something was removed rather than that the sender
    /// typed a strange sentence. The original is never returned by this function;
    /// what the server stores and delivers is the masked form, and the *report*
    /// path is what preserves the unmasked words for an operator.
    #[must_use]
    pub fn mask(&self, text: &str) -> (String, usize) {
        if self.words.is_empty() {
            return (text.to_string(), 0);
        }
        let mut out = String::with_capacity(text.len());
        let mut hits = 0;
        for token in split_words(text) {
            match token {
                Token::Word(w) => {
                    let (lead, core, trail) = trim_edges(w);
                    if !core.is_empty() && self.matches(core) {
                        hits += 1;
                        // The punctuation around the word is not the word: masking
                        // the whole token turns "stop!" into "*****" and loses the
                        // shout. Only the core is starred.
                        out.push_str(lead);
                        // One `*` per *character*, not per byte: masking by byte
                        // length turns a non-ASCII word into a visibly longer run
                        // of stars than the word it replaced.
                        out.extend(std::iter::repeat_n('*', core.chars().count()));
                        out.push_str(trail);
                    } else {
                        out.push_str(w);
                    }
                }
                Token::Gap(g) => out.push_str(g),
            }
        }
        (out, hits)
    }

    /// Whether `text` contains a listed word. Same matching as [`Self::mask`].
    #[must_use]
    pub fn flags(&self, text: &str) -> bool {
        self.mask(text).1 > 0
    }

    /// Whether one already-trimmed word is listed.
    ///
    /// Two comparisons, and the second is conditional for a reason.
    /// [`normalize_word`] collapses a repeated letter to *two* — "bookkeeper" and
    /// "spelled" have real doubles — which is not enough for "baaaadwooord". The
    /// obvious fix, collapsing every run to one on both sides, is a false-positive
    /// machine: "ass" squeezes to "as", so the word *as* would be masked in every
    /// sentence that contains it. So the squeezed comparison is only offered a
    /// candidate that padded a letter **three deep or more**, which no ordinary
    /// English word does and which is a deliberate stretch every time.
    fn matches(&self, core: &str) -> bool {
        let normalized = normalize_word(core);
        if self.words.contains(&normalized) {
            return true;
        }
        has_long_run(core) && self.squeezed.contains(&squeeze(&normalized))
    }
}

/// Whether a word repeats some letter three or more times in a row.
fn has_long_run(word: &str) -> bool {
    let mut run: Option<(char, usize)> = None;
    for c in word.chars().map(deleet) {
        run = match run {
            Some((prev, n)) if prev == c => {
                if n + 1 >= 3 {
                    return true;
                }
                Some((c, n + 1))
            }
            _ => Some((c, 1)),
        };
    }
    false
}

/// Collapse every run of a repeated character to a single one.
fn squeeze(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        if out.chars().next_back() != Some(c) {
            out.push(c);
        }
    }
    out
}

/// Normalise one word for comparison: de-leet, lowercase, drop the separators
/// people sprinkle inside a word (`n-i-c-e`), and collapse a run of a repeated
/// letter to two (`niiiice`).
///
/// Collapsing to two rather than one is deliberate: "bookkeeper" and "spelled"
/// have real doubles, and no listed word depends on a triple.
fn normalize_word(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut run: Option<(char, usize)> = None;
    for c in word.chars() {
        // Separators inside a word are noise for matching purposes.
        if matches!(c, '.' | '-' | '_' | '*' | '\'' | '`' | ' ') {
            continue;
        }
        let c = deleet(c);
        match run {
            Some((prev, n)) if prev == c => {
                if n < 2 {
                    out.push(c);
                    run = Some((c, n + 1));
                }
            }
            _ => {
                out.push(c);
                run = Some((c, 1));
            }
        }
    }
    out
}

enum Token<'a> {
    Word(&'a str),
    Gap(&'a str),
}

/// Peel the leading and trailing punctuation off a token, returning
/// `(lead, core, trail)`.
///
/// The token grammar has to be generous — a substituted `@` or `$` is part of
/// the word it hides in — and being generous at both ends is what swallowed the
/// `!` in "stop!" into the word. Matching runs on the core; the edges are put
/// back verbatim.
fn trim_edges(word: &str) -> (&str, &str, &str) {
    let start = word
        .char_indices()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(word.len(), |(i, _)| i);
    let end = word
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_alphanumeric())
        .map_or(start, |(i, c)| i + c.len_utf8());
    (&word[..start], &word[start..end], &word[end..])
}

/// Split into word and non-word runs, preserving every byte.
///
/// A "word" is a run of alphanumerics plus the in-word noise characters
/// [`normalize_word`] strips and the punctuation [`deleet`] reads as a letter, so
/// `f.u.c.k` and `b@dw0rd` each arrive as one token rather than as fragments the
/// matcher never sees together.
fn split_words(text: &str) -> Vec<Token<'_>> {
    let is_wordish = |c: char| {
        c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | '\'' | '@' | '$' | '!')
    };
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut in_word = false;
    for (i, c) in text.char_indices() {
        let w = is_wordish(c);
        if w != in_word {
            if i > start {
                tokens.push(if in_word {
                    Token::Word(&text[start..i])
                } else {
                    Token::Gap(&text[start..i])
                });
            }
            start = i;
            in_word = w;
        }
    }
    if start < text.len() {
        tokens.push(if in_word {
            Token::Word(&text[start..])
        } else {
            Token::Gap(&text[start..])
        });
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> Filter {
        Filter::from_words(["badword", "worse"])
    }

    #[test]
    fn masks_a_listed_word_and_keeps_its_length() {
        let (out, hits) = filter().mask("you badword thing");
        assert_eq!(out, "you ******* thing");
        assert_eq!(hits, 1);
    }

    #[test]
    fn leaves_a_clean_line_byte_identical() {
        let line = "meet me at the bridge, 10 min — bring the map!";
        let (out, hits) = filter().mask(line);
        assert_eq!(out, line);
        assert_eq!(hits, 0);
    }

    /// The failure mode every naive filter has: a substring match censors
    /// innocent words and the community routes around the filter entirely.
    #[test]
    fn a_listed_word_inside_a_longer_word_is_not_a_hit() {
        let f = Filter::from_words(["ass", "cum"]);
        let (out, hits) = f.mask("The class went to Scunthorpe to document circumstances");
        assert_eq!(hits, 0, "masked: {out}");
    }

    #[test]
    fn sees_through_leetspeak_and_padding() {
        let f = filter();
        assert!(f.flags("b@dw0rd"));
        assert!(f.flags("b.a.d.w.o.r.d"));
        assert!(f.flags("BADWORD"));
        assert!(f.flags("baaaadwooord"));
    }

    /// Squeezing every repeat to one letter would make the word *as* match a
    /// listed slur. Padding is only undone for a candidate that stretched a
    /// letter three deep, which is never an accident.
    #[test]
    fn squeezing_repeats_does_not_censor_short_ordinary_words() {
        let f = Filter::from_words(["ass", "boo"]);
        assert!(!f.flags("as"), "the word 'as' must survive");
        assert!(!f.flags("bo"));
        assert!(!f.flags("bookkeeper"));
        // …but the stretch it exists for still lands.
        assert!(f.flags("aaassss"));
    }

    /// `l` → `i` would make "flag" match a listed slur. The table stops short of
    /// the ambiguous substitutions on purpose; this pins that decision.
    #[test]
    fn ambiguous_substitutions_are_not_undone() {
        let f = Filter::from_words(["fig"]);
        assert!(!f.flags("flag"), "l is not folded into i");
    }

    #[test]
    fn punctuation_and_spacing_survive_a_mask() {
        let (out, _) = filter().mask("no, badword! really?");
        assert_eq!(out, "no, *******! really?");
    }

    /// The token grammar has to swallow `@` and `!` to see through substitution,
    /// which is exactly how trailing punctuation once stopped a word matching.
    #[test]
    fn trailing_punctuation_does_not_hide_a_word() {
        let f = filter();
        assert!(f.flags("badword!"));
        assert!(f.flags("...badword..."));
        assert_eq!(f.mask("badword!!!").0, "*******!!!");
    }

    #[test]
    fn an_empty_list_is_a_pass_through() {
        let f = Filter::from_words(std::iter::empty());
        assert!(f.is_empty());
        let (out, hits) = f.mask("anything at all");
        assert_eq!(out, "anything at all");
        assert_eq!(hits, 0);
    }

    #[test]
    fn comments_and_blanks_in_an_operator_list_are_skipped() {
        let f = Filter::from_words(["# slurs", "", "  badword  ", "   "]);
        assert!(f.flags("badword"));
        assert!(!f.flags("#"));
    }

    #[test]
    fn the_seed_list_compiles_and_matches_itself() {
        let f = Filter::seeded();
        assert!(!f.is_empty());
        for w in DEFAULT_WORDS {
            assert!(f.flags(w), "seed word {w} does not match itself");
        }
    }

    /// Multi-byte input must not produce more stars than characters.
    #[test]
    fn masking_counts_characters_not_bytes() {
        let f = Filter::from_words(["café"]);
        let (out, hits) = f.mask("a café here");
        assert_eq!(hits, 1);
        assert_eq!(out, "a **** here");
    }
}
