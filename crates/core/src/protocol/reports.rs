//! Reporting a person: the reasons and the bounds on what a report carries.

use serde::{Deserialize, Serialize};

/// Why a person is being reported. A closed list rather than free text: it is
/// what an operator triages by, and it is the vocabulary the report dialog
/// offers. `note` carries anything this does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReportReason {
    /// Targeted abuse of a person.
    Harassment,
    /// Slurs or attacks on a protected group.
    HateSpeech,
    /// Sexual content, or sexual attention that was not asked for.
    SexualContent,
    /// A threat of violence, or content suggesting someone is in danger.
    Threat,
    /// Flooding, advertising, scams.
    Spam,
    /// Anything else; `note` carries it.
    Other,
}

impl ReportReason {
    /// Every reason, in the order the report dialog offers them.
    pub const ALL: &'static [Self] = &[
        Self::Harassment,
        Self::HateSpeech,
        Self::SexualContent,
        Self::Threat,
        Self::Spam,
        Self::Other,
    ];

    /// Short label for the UI and for the operator's report list.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Harassment => "Harassment or bullying",
            Self::HateSpeech => "Hate speech or a slur",
            Self::SexualContent => "Sexual content",
            Self::Threat => "Threat or self-harm",
            Self::Spam => "Spam or a scam",
            Self::Other => "Something else",
        }
    }

    /// Stable string for storage and for the admin API. Parsed back by
    /// [`Self::from_wire`], so these two must move together.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Harassment => "harassment",
            Self::HateSpeech => "hate_speech",
            Self::SexualContent => "sexual_content",
            Self::Threat => "threat",
            Self::Spam => "spam",
            Self::Other => "other",
        }
    }

    /// Inverse of [`Self::as_str`]; an unrecognised string reads as `Other`
    /// rather than dropping a report on the floor.
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            "harassment" => Self::Harassment,
            "hate_speech" => Self::HateSpeech,
            "sexual_content" => Self::SexualContent,
            "threat" => Self::Threat,
            "spam" => Self::Spam,
            _ => Self::Other,
        }
    }
}

/// Longest report note the server will store, in characters.
///
/// The field is a `String` off the wire and its only other bound is
/// `MAX_FRAME`, which makes an unbounded one an allocation multiplier. Long enough for a paragraph explaining
/// what happened, short enough that a report table cannot be used as storage.
pub const MAX_REPORT_NOTE_LEN: usize = 2000;

/// Longest reported-message excerpt the server will store, in characters.
pub const MAX_REPORT_MESSAGE_LEN: usize = 2000;
