//! People's discs — a picture or a round initial — and the round button, which
//! the world draws as well as the messenger: on the dock, over people on the
//! map, on the plans card. Moved out of `systems::chats` so a build without
//! `comms` (the web build, whose messenger is the page) keeps them; `chats`
//! names them back, so its callers are unchanged.

use crate::systems::chat::ChatState;
use crate::systems::icons;
use crate::systems::theme::Rgba8;

/// The round initials, picked by name so a person keeps theirs (`.c1`–`.c6`).
///
/// Lifted from the mockup's muted set on 2026-10-04: on the ink screen those
/// read as six shades of the same mud, and a person's colour is the one
/// thing in a list of names that is theirs. Each still carries white initials
/// at better than 3:1.
const AVATAR_TONES: [Rgba8; 6] = [
    Rgba8::from_rgb(0x86, 0x5C, 0xC4),
    Rgba8::from_rgb(0x2C, 0x91, 0x7C),
    Rgba8::from_rgb(0xC0, 0x66, 0x30),
    Rgba8::from_rgb(0x43, 0x74, 0xC4),
    Rgba8::from_rgb(0xB8, 0x48, 0x68),
    Rgba8::from_rgb(0x5F, 0x8A, 0x2E),
];

/// The "Here" disc: the v1 inbox's dark slate, with a location pin on it.
const HERE_TONE: Rgba8 = Rgba8::from_rgb(0x3F, 0x3B, 0x38);

/// What kind of chat a channel is, which decides what it says about itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChatKind {
    Private,
    Here,
    Everyone,
    /// A topic of a community: a private chat, drawn as a place people gather.
    Topic,
    /// A community itself (its row in the list and its screens).
    Community,
}

#[must_use]
pub fn kind_of(key: &str) -> ChatKind {
    if ChatState::is_private(key) {
        ChatKind::Private
    } else if key == "local" {
        ChatKind::Here
    } else {
        // "geo:", and anything else public: rooms are gone (protocol 39).
        ChatKind::Everyone
    }
}

pub(crate) fn tone_for(name: &str) -> Rgba8 {
    let h = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    AVATAR_TONES[h as usize % AVATAR_TONES.len()]
}

/// How a chat's disc looks when it has no picture: its tone, what is on it
/// (white), how large that is against the disc, and its shape — for the
/// component library's discs (`lib_chats`).
#[derive(Clone, Debug, PartialEq)]
pub struct DiscLook {
    pub tone: Rgba8,
    /// The initial, `#`, or an icon glyph.
    pub text: String,
    /// `text` is a glyph of the icon font.
    #[cfg_attr(not(all(feature = "ui-lib", feature = "comms")), allow(dead_code))]
    pub icon: bool,
    /// The text's size as a share of the disc's.
    pub scale: f32,
    /// A topic or a community is a place people gather rather than a
    /// person, so it is the design's rounded square (`.room`), not a circle.
    pub square: bool,
}

/// The look of `name`'s disc for a chat of `kind`.
#[must_use]
pub fn disc_look(name: &str, kind: ChatKind) -> DiscLook {
    let initial = || -> String { name.chars().next().unwrap_or('?').to_uppercase().collect() };
    let (tone, text, icon, scale) = match kind {
        ChatKind::Here => (HERE_TONE, icons::PIN.to_string(), true, 0.45),
        ChatKind::Everyone => (AVATAR_TONES[3], icons::CAM_EARTH.to_string(), true, 0.45),
        ChatKind::Topic => (AVATAR_TONES[5], "#".to_string(), false, 0.42),
        ChatKind::Community => (tone_for(name), initial(), false, 0.4),
        ChatKind::Private => (tone_for(name), initial(), false, 0.38),
    };
    DiscLook {
        tone,
        text,
        icon,
        scale,
        square: matches!(kind, ChatKind::Topic | ChatKind::Community),
    }
}
