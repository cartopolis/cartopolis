//! Avatars on the wire: how one looks, where it stands, what it plays.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use super::*;

/// How an avatar looks: a MakeHuman body and face, a skin, eyes, hair and what
/// it wears (protocol 42; the client builds it, `docs/notes/makehuman-avatars.md`).
///
/// Every slider is a whole percent and every colour three bytes, so a look
/// compares exactly and travels small — about 150 bytes. Things worn are named
/// as in the client's avatar pack; a name a client's pack lacks is skipped
/// there, so a newer wardrobe never breaks an older client. A colour that is
/// `None` is the garment's or the hair's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarLook {
    /// 0 feminine … 100 masculine (MakeHuman's gender, which is continuous).
    pub body_shape: u8,
    /// Years, 18–90.
    pub age: u8,
    /// 0–100, 50 average, for each of these.
    pub height: u8,
    pub weight: u8,
    pub muscle: u8,
    pub proportions: u8,
    pub chest: u8,
    /// A skin family (`african`, `asian`, `caucasian`); the age and body shape
    /// choose among its skins.
    #[serde(default = "default_look_skin")]
    pub skin: String,
    #[serde(default = "default_look_eyes")]
    pub eyes: String,
    /// A hair style, or `None` for none.
    pub hair: Option<String>,
    #[serde(default)]
    pub hair_colour: Option<[u8; 3]>,
    #[serde(default = "default_look_eyebrows")]
    pub eyebrows: String,
    /// The outfit, top and bottom in one, and a colour for each half.
    pub outfit: String,
    #[serde(default)]
    pub top_colour: Option<[u8; 3]>,
    #[serde(default)]
    pub bottom_colour: Option<[u8; 3]>,
    #[serde(default = "default_look_shoes")]
    pub shoes: String,
    #[serde(default)]
    pub hat: Option<String>,
    /// Shape controls by MakeHuman modifier — the face's
    /// (`nose/nose-scale-horiz-decr-incr`) and, since protocol 45, the body's
    /// (`hip/hip-scale-horiz-decr-incr`) — -100 … 100; one left at 0 is not
    /// listed.
    #[serde(default)]
    pub face: Vec<(String, i8)>,
    /// -100 darker … 100 lighter, over the skin family's own colour.
    #[serde(default)]
    pub skin_tone: i8,
    /// Things worn beyond the outfit — a beard, glasses — by pack name.
    #[serde(default)]
    pub extras: Vec<String>,
}

/// [`AvatarLook`] as protocols 42–44 wrote it, for
/// [`AvatarLook::from_stored`]. Frozen: never edit, only add a newer one.
#[derive(Deserialize)]
struct AvatarLookV42 {
    body_shape: u8,
    age: u8,
    height: u8,
    weight: u8,
    muscle: u8,
    proportions: u8,
    chest: u8,
    skin: String,
    eyes: String,
    hair: Option<String>,
    hair_colour: Option<[u8; 3]>,
    eyebrows: String,
    outfit: String,
    top_colour: Option<[u8; 3]>,
    bottom_colour: Option<[u8; 3]>,
    shoes: String,
    hat: Option<String>,
    face: Vec<(String, i8)>,
}

fn default_look_skin() -> String {
    "caucasian".into()
}

fn default_look_eyes() -> String {
    "brown".into()
}

fn default_look_eyebrows() -> String {
    "eyebrow001".into()
}

fn default_look_shoes() -> String {
    "shoes06".into()
}

/// The longest name an [`AvatarLook`] may carry, and the most face controls.
pub const MAX_LOOK_NAME: usize = 48;
pub const MAX_LOOK_FACE: usize = 128;
pub const MAX_LOOK_EXTRAS: usize = 8;

impl AvatarLook {
    /// The body a fresh avatar starts as.
    #[must_use]
    pub fn default_for(feminine: bool) -> Self {
        let (body_shape, hair, outfit, shoes) = if feminine {
            (0, "bob01", "female_elegantsuit01", "shoes01")
        } else {
            (100, "short02", "male_casualsuit01", "shoes06")
        };
        Self {
            body_shape,
            age: 25,
            height: 50,
            weight: 50,
            muscle: 50,
            proportions: 50,
            chest: 50,
            skin: default_look_skin(),
            eyes: default_look_eyes(),
            hair: Some(hair.into()),
            hair_colour: None,
            eyebrows: default_look_eyebrows(),
            outfit: outfit.into(),
            top_colour: None,
            bottom_colour: None,
            shoes: shoes.into(),
            hat: None,
            face: Vec::new(),
            skin_tone: 0,
            extras: Vec::new(),
        }
    }

    /// A look from the postcard bytes the server keeps (`looks.rs`), in this
    /// layout or an older one. postcard is not self-describing: a field
    /// added at the end does **not** take its serde default when older bytes
    /// end before it — the read fails — so each older layout is read as
    /// itself and carried forward.
    #[must_use]
    pub fn from_stored(bytes: &[u8]) -> Option<Self> {
        if let Ok(look) = postcard::from_bytes::<Self>(bytes) {
            return Some(look);
        }
        let v42: AvatarLookV42 = postcard::from_bytes(bytes).ok()?;
        Some(Self {
            body_shape: v42.body_shape,
            age: v42.age,
            height: v42.height,
            weight: v42.weight,
            muscle: v42.muscle,
            proportions: v42.proportions,
            chest: v42.chest,
            skin: v42.skin,
            eyes: v42.eyes,
            hair: v42.hair,
            hair_colour: v42.hair_colour,
            eyebrows: v42.eyebrows,
            outfit: v42.outfit,
            top_colour: v42.top_colour,
            bottom_colour: v42.bottom_colour,
            shoes: v42.shoes,
            hat: v42.hat,
            face: v42.face,
            skin_tone: 0,
            extras: Vec::new(),
        })
    }

    /// A face control's value, 0 when it is not listed.
    #[must_use]
    pub fn face_value(&self, modifier: &str) -> i8 {
        self.face
            .iter()
            .find(|(m, _)| m == modifier)
            .map_or(0, |(_, v)| *v)
    }

    /// Set a face control, keeping the list to the ones off zero.
    pub fn set_face(&mut self, modifier: &str, value: i8) {
        self.face.retain(|(m, _)| m != modifier);
        if value != 0 {
            self.face.push((modifier.to_string(), value));
            self.face.sort();
        }
    }

    /// What the server accepts: sliders in range, names short, the face list
    /// bounded. Not whether the names exist — that is the client's pack.
    #[must_use]
    pub fn is_reasonable(&self) -> bool {
        let short = |s: &str| s.len() <= MAX_LOOK_NAME;
        [
            self.body_shape,
            self.height,
            self.weight,
            self.muscle,
            self.proportions,
            self.chest,
        ]
        .iter()
        .all(|v| *v <= 100)
            && (18..=90).contains(&self.age)
            && short(&self.skin)
            && short(&self.eyes)
            && self.hair.as_deref().is_none_or(short)
            && short(&self.eyebrows)
            && short(&self.outfit)
            && short(&self.shoes)
            && self.hat.as_deref().is_none_or(short)
            && self.face.len() <= MAX_LOOK_FACE
            && self
                .face
                .iter()
                .all(|(m, v)| short(m) && (-100..=100).contains(v))
            && (-100..=100).contains(&self.skin_tone)
            && self.extras.len() <= MAX_LOOK_EXTRAS
            && self.extras.iter().all(|e| short(e))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AvatarStateDto {
    pub id: u64,
    /// Where the avatar stands on Earth (protocol 47). The sim holds every
    /// avatar's place as a latitude and longitude and integrates its movement
    /// there, so two clients anchored on different places see one avatar at one
    /// spot; each client converts it into the frame its own map is anchored in,
    /// as it does for a prim's `PrimDto::geo`. Until protocol 46 this was metres
    /// in the avatar's *own* client's frame, which only two clients sharing an
    /// anchor could agree on (docs/notes/interest-by-reach.md).
    pub geo: GeoAnchor,
    /// Height of the feet above the sim's flat ground (`motion::GROUND_Y`), in
    /// metres: the altitude a jump or a fall changes. The ground's own
    /// relief is each viewer's business (`terrain_lift`), as it is for prims.
    pub alt_m: f32,
    pub yaw: f32,
    /// An emote this avatar started recently, so a client that just came into
    /// range still sees a wave in progress rather than missing it. The sim clears
    /// it after [`EMOTE_REPLICATE_SECS`]; a receiver plays it once per
    /// `(avatar, sequence)` it has not seen.
    pub emote: Option<EmoteState>,
    /// An expression this avatar's face started recently, held as `emote` is,
    /// for [`EXPRESSION_REPLICATE_SECS`] (protocol 44).
    pub expression: Option<ExpressionState>,
    /// Set while this avatar is sitting on something: metres in world XZ from
    /// [`Self::position`] to where its body is drawn.
    ///
    /// **An offset, not a place**, for two reasons. The sim has no map and no idea
    /// what a bench is, so it cannot hold a seat as a location it could check;
    /// and the sitter's own client measured the seat in the frame its map is
    /// anchored in, which is a *translation* away from every other client's. A
    /// delta survives that translation, a position does not. It is bounded by the
    /// sim ([`MAX_SEAT_OFFSET_M`]) — an avatar may lean out of its authoritative
    /// position by about the width of a bench, not by a street.
    ///
    /// The facing needs no field of its own: a sitter turns to face out of the
    /// seat, and that is already [`Self::yaw`].
    pub seated: Option<[f32; 2]>,
    /// World velocity (m/s) the sim integrated this avatar with on the tick the
    /// snapshot was taken: horizontal travel, plus the vertical rate of a jump
    /// or a fall. Zero while seated.
    ///
    /// Replicated rather than derived because a receiver cannot derive it
    /// well. Differencing two snapshots is a tick late and noisy at the tick
    /// rate, and differencing the *smoothed* position — which is what the
    /// client did — gets it wrong exactly at a start or a stop, the moments
    /// the gait changes. With it, a receiver draws another avatar where it is
    /// now (`position + velocity × age`) instead of trailing where it was.
    pub velocity: Vec3,
    /// The physical prim this avatar is carrying (protocol 48), held in front
    /// of it by the sim until it is thrown, put down, or the hold ends. A
    /// level, resent every snapshot, as `seated` is.
    pub holding: Option<i64>,
}

/// How far a seat may draw an avatar from the position the sim holds for it.
///
/// A seat is the one thing that separates where an avatar *is* from where it is
/// *drawn*, so it needs a ceiling: without one a client could sit and put its
/// body an arbitrary distance away, which is a way to appear somewhere it is not
/// and to be missed by anything that reasons about proximity. The furthest real
/// seat is the far bench of a picnic table, a little over a metre off, reached
/// from a standing position the colliders keep about 0.6 m short of it.
pub const MAX_SEAT_OFFSET_M: f32 = 2.0;

/// A one-shot full-body animation an avatar can play: a clip every body shares
/// (the motion capture in the MakeHuman pack), so an emote costs no asset, only
/// a name.
///
/// Deliberately a closed enum rather than a clip index: the index is a property of
/// the *asset*, and putting it on the wire would freeze the character set. A
/// receiver maps the name to whatever its own models call that motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Emote {
    Wave,
    /// The generic "use the thing in front of me" motion, also played by the
    /// world-interaction prompt.
    Interact,
    Roll,
    Punch,
    Kick,
    /// Play dead. Every set has one, and someone will find it.
    Faint,
    /// The two greetings (protocol 49, [`crate::greet::Greeting`]). **Only the
    /// sim starts these**, on both avatars at once once one has offered and
    /// the other accepted; one asked for in a `ClientIntent` is dropped, or
    /// anyone could shake hands with the air beside a stranger.
    Handshake,
    HighFive,
    /// An overhand throw (protocol 49): what letting go with a throw plays,
    /// where the punch stood in before.
    Throw,
}

impl Emote {
    /// A gesture made with somebody, which only the sim may start.
    #[must_use]
    pub const fn is_paired(self) -> bool {
        matches!(self, Self::Handshake | Self::HighFive)
    }
}

/// An emote plus the sequence number that makes it re-playable: the same emote
/// twice in a row is two distinct events, and a receiver diffing on the emote
/// alone would show only the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmoteState {
    pub emote: Emote,
    pub sequence: u32,
}

/// How long an emote stays on an avatar's replicated state. A little longer than
/// the longest clip, so a client that receives its first snapshot mid-wave still
/// plays it, and short enough that it does not replay on a much later snapshot.
pub const EMOTE_REPLICATE_SECS: f32 = 2.5;

/// A face an avatar pulls for a few seconds ([`EXPRESSION_SECS`]): a mix of
/// the face's units every client composes the same way
/// (`cartopolis_human::face::recipe`, by [`Expression::name`]). A closed enum
/// for [`Emote`]'s reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Expression {
    Happy,
    Laugh,
    Sad,
    Angry,
    Surprised,
    Afraid,
    Disgusted,
    Wink,
}

impl Expression {
    pub const ALL: [Self; 8] = [
        Self::Happy,
        Self::Laugh,
        Self::Sad,
        Self::Angry,
        Self::Surprised,
        Self::Afraid,
        Self::Disgusted,
        Self::Wink,
    ];

    /// The recipe's name in `cartopolis_human::face::recipe`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Happy => "happy",
            Self::Laugh => "laugh",
            Self::Sad => "sad",
            Self::Angry => "angry",
            Self::Surprised => "surprised",
            Self::Afraid => "afraid",
            Self::Disgusted => "disgusted",
            Self::Wink => "wink",
        }
    }
}

/// An expression and the sequence that makes it re-playable, as [`EmoteState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpressionState {
    pub expression: Expression,
    pub sequence: u32,
}

/// How long a face holds an expression, seconds, easing in and out inside it.
pub const EXPRESSION_SECS: f32 = 4.0;

/// How long an expression stays on an avatar's replicated state: as long as
/// it shows, so one who comes into view mid-smile still sees the smile.
pub const EXPRESSION_REPLICATE_SECS: f32 = EXPRESSION_SECS;

/// How many weights a face has: the ARKit units, in the order
/// `cartopolis_human::face::UNITS` lists them.
pub const FACE_UNITS: usize = 52;

/// How often a tracked face is sent, frames a second: a camera's pace, and
/// enough for speech to read on the lips.
pub const FACE_FRAME_HZ: f32 = 15.0;
