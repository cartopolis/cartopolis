//! The avatar's look, read and changed by an agent: `avatar` lists the look
//! and every choice, `set_avatar` changes it as the Appearance panel's Apply
//! does (`ui::render_avatar_chooser`).
//!
//! **Why this exists (2026-11-05).** The person asked that an agent driving
//! the phone can model the avatar on a photo. Working the designer through
//! `press` is a tap per slider step; this takes the look as values.
//!
//! **Only what the panel offers.** Every name and shape control is checked
//! against the designer's own lists (`avatar_designer`), so an agent makes no
//! look a person could not, and none the pack lacks. The server's own check
//! (`AvatarLook::is_reasonable`) runs last, as it will there.

use serde_json::{json, Value};

use crate::resources::{AvatarChooserState, RightPanel};
use crate::systems::avatar_designer::{
    FaceSlider, BEARDS, BODY, BOTTOMS, CLOTH_COLOURS, DRESSES, EYEBROWS, EYES, FACE, GLASSES, HAIR,
    HAIR_COLOURS, HATS, OUTFITS, SHOES, SKINS, STARTERS, TOPS,
};
use crate::systems::human_avatar::{current_look, HumanDraft};
use crate::systems::user_store::{HumanLook, UserStore};
use cartopolis_mcp::args::AgentReply;
use cartopolis_mcp::device::SetAvatarArgs;

/// The extras, by kind: a look wears at most one of each.
const EXTRAS: [(&str, &[(&str, &str)]); 4] = [
    ("beards", &BEARDS),
    ("glasses", &GLASSES),
    ("tops", &TOPS),
    ("bottoms", &BOTTOMS),
];

/// The look the avatar wears now, saved or not, and whether it was designed.
fn saved_look(store: &UserStore) -> (HumanLook, bool) {
    let appearance = &store.data.appearance;
    (
        current_look(appearance, &HumanDraft::default()),
        appearance.human.is_some(),
    )
}

/// Open the Appearance panel on the saved look, reseeding it if it was open
/// on an older one.
fn show(panel: &mut RightPanel, chooser: &mut AvatarChooserState) {
    *panel = RightPanel::Appearance;
    chooser.seeded = false;
}

pub fn avatar(
    store: &UserStore,
    panel: &mut RightPanel,
    chooser: &mut AvatarChooserState,
    open: bool,
) -> AgentReply {
    let (look, designed) = saved_look(store);
    if open {
        show(panel, chooser);
    }
    let named = |list: &[(&str, &str)]| -> Value {
        list.iter()
            .map(|(name, label)| json!({ "name": name, "label": label }))
            .collect()
    };
    let coloured = |list: &[(&str, [u8; 3])]| -> Value {
        list.iter()
            .map(|(name, rgb)| json!({ "name": name, "rgb": rgb }))
            .collect()
    };
    let controls = |part: String, sliders: &[FaceSlider]| -> Vec<Value> {
        sliders
            .iter()
            .map(|FaceSlider(modifier, label, (low, high), one_sided)| {
                json!({
                    "modifier": modifier,
                    "part": part,
                    "label": label,
                    "low": low,
                    "high": high,
                    "range": if *one_sided { [0, 100] } else { [-100, 100] },
                })
            })
            .collect()
    };
    let mut shape = Vec::new();
    for (part, sliders) in &FACE {
        shape.extend(controls(format!("face {part:?}").to_lowercase(), sliders));
    }
    for (part, sliders) in &BODY {
        shape.extend(controls(format!("body {part:?}").to_lowercase(), sliders));
    }
    let extras: serde_json::Map<String, Value> = EXTRAS
        .iter()
        .map(|(kind, list)| ((*kind).to_string(), named(list)))
        .collect();
    let data = json!({
        "look": look,
        "designed": designed,
        "choices": {
            "skins": coloured(&SKINS),
            "eyes": coloured(&EYES),
            "hair": named(&HAIR),
            "eyebrows": named(&EYEBROWS),
            "outfits": named(&OUTFITS),
            "dresses": named(&DRESSES),
            "hats": named(&HATS),
            "extras": extras,
            "shoes": named(&SHOES),
            "shape": shape,
            "starters": STARTERS.iter().map(|s| s.label).collect::<Vec<_>>(),
            "hair_colours": HAIR_COLOURS,
            "cloth_colours": CLOTH_COLOURS,
        },
    });
    let text = format!(
        "{} {}, age {}, {} skin, hair {}, wearing {}.{} `look` is the saved look; \
         `choices` is what `set_avatar` accepts (any [r, g, b] goes for a colour; the \
         palettes are the panel's).",
        if designed {
            "Your avatar:"
        } else {
            "Your avatar (never designed, the default):"
        },
        match look.body_shape {
            0..=33 => "feminine",
            34..=66 => "androgynous",
            _ => "masculine",
        },
        look.age,
        look.skin,
        look.hair.as_deref().unwrap_or("none"),
        if look.outfit.is_empty() {
            "separates"
        } else {
            &look.outfit
        },
        if open {
            " The Appearance panel is open."
        } else {
            ""
        },
    );
    AgentReply::ok(text, data)
}

pub fn set_avatar(
    store: &mut UserStore,
    panel: &mut RightPanel,
    chooser: &mut AvatarChooserState,
    args: &SetAvatarArgs,
) -> AgentReply {
    let (before, _) = saved_look(store);
    let look = match changed(before.clone(), args) {
        Ok(look) => look,
        Err(why) => return AgentReply::refuse(format!("Nothing changed: {why}")),
    };
    let same = look == before && store.data.appearance.human.is_some();
    if !same {
        store.data.appearance.human = Some(look.clone());
        store.dirty = true;
        // An open panel holds a draft of the old look, which its Apply would
        // put back: start it again from this one.
        chooser.seeded = false;
    }
    if args.show == Some(true) {
        show(panel, chooser);
    }
    let text = if same {
        "That is the look already; nothing changed.".to_string()
    } else {
        "Saved: everyone sees the new look, and it is what the Appearance panel opens on."
            .to_string()
    };
    AgentReply::ok(text, json!({ "look": look }))
}

/// `look` with `args` applied, or why not.
fn changed(mut look: HumanLook, args: &SetAvatarArgs) -> Result<HumanLook, String> {
    if let Some(label) = &args.starter {
        let starter = STARTERS
            .iter()
            .find(|s| s.label.eq_ignore_ascii_case(label))
            .ok_or_else(|| {
                let all: Vec<_> = STARTERS.iter().map(|s| s.label).collect();
                format!("no starter \"{label}\"; there are {}", all.join(", "))
            })?;
        look = starter.look();
    }
    let percent = |what: &str, v: Option<u8>, to: &mut u8| -> Result<(), String> {
        match v {
            Some(v) if v > 100 => Err(format!("{what} is 0–100, not {v}")),
            Some(v) => {
                *to = v;
                Ok(())
            }
            None => Ok(()),
        }
    };
    percent("body_shape", args.body_shape, &mut look.body_shape)?;
    percent("height", args.height, &mut look.height)?;
    percent("weight", args.weight, &mut look.weight)?;
    percent("muscle", args.muscle, &mut look.muscle)?;
    percent("proportions", args.proportions, &mut look.proportions)?;
    percent("chest", args.chest, &mut look.chest)?;
    if let Some(age) = args.age {
        if !(18..=90).contains(&age) {
            return Err(format!("age is 18–90, not {age}"));
        }
        look.age = age;
    }
    if let Some(tone) = args.skin_tone {
        if !(-100..=100).contains(&tone) {
            return Err(format!("skin_tone is -100–100, not {tone}"));
        }
        look.skin_tone = tone;
    }
    if let Some(skin) = &args.skin {
        look.skin = pick("skin", skin, &names(&SKINS))?;
    }
    if let Some(eyes) = &args.eyes {
        look.eyes = pick("eyes", eyes, &names(&EYES))?;
    }
    if let Some(hair) = &args.hair {
        look.hair = optional("hair", hair, &labelled(&HAIR))?;
    }
    if let Some(brows) = &args.eyebrows {
        look.eyebrows = pick("eyebrows", brows, &labelled(&EYEBROWS))?;
    }
    if let Some(shoes) = &args.shoes {
        look.shoes = pick("shoes", shoes, &labelled(&SHOES))?;
    }
    if let Some(hat) = &args.hat {
        look.hat = optional("hat", hat, &labelled(&HATS))?;
    }
    if let Some(c) = &args.hair_colour {
        look.hair_colour = colour("hair_colour", c)?;
    }
    if let Some(c) = &args.top_colour {
        look.top_colour = colour("top_colour", c)?;
    }
    if let Some(c) = &args.bottom_colour {
        look.bottom_colour = colour("bottom_colour", c)?;
    }
    if let Some(extras) = &args.extras {
        let mut kinds = Vec::new();
        for e in extras {
            let kind = EXTRAS
                .iter()
                .find(|(_, list)| list.iter().any(|(n, _)| n == e))
                .map(|(kind, _)| *kind)
                .ok_or_else(|| format!("no extra \"{e}\"; `avatar` lists them by kind"))?;
            if kinds.contains(&kind) {
                return Err(format!("two {kind}: a look wears one of each kind"));
            }
            kinds.push(kind);
        }
        look.extras = extras.clone();
    }
    if let Some(outfit) = &args.outfit {
        if outfit.eq_ignore_ascii_case("none") {
            look.outfit.clear();
        } else {
            let all: Vec<&str> = OUTFITS.iter().chain(&DRESSES).map(|(n, _)| *n).collect();
            look.outfit = pick("outfit", outfit, &all)?;
            // A whole outfit is worn instead of separates, as the panel does.
            look.extras
                .retain(|e| !TOPS.iter().chain(&BOTTOMS).any(|(n, _)| n == e));
        }
    }
    if look.outfit.is_empty() {
        let worn = |list: &[(&str, &str)]| {
            list.iter()
                .any(|(n, _)| look.extras.contains(&(*n).to_string()))
        };
        if !worn(&TOPS) || !worn(&BOTTOMS) {
            return Err("with no outfit, `extras` must hold a top and a bottom".into());
        }
    }
    if args.reset_shape == Some(true) {
        look.face.clear();
    }
    for (modifier, value) in args.shape.iter().flatten() {
        let slider = FACE
            .iter()
            .flat_map(|(_, sliders)| sliders.iter())
            .chain(BODY.iter().flat_map(|(_, sliders)| sliders.iter()))
            .find(|s| s.0 == modifier)
            .ok_or_else(|| format!("no shape control \"{modifier}\"; `avatar` lists them"))?;
        let low = if slider.3 { 0 } else { -100 };
        if !(low..=100).contains(value) {
            return Err(format!("{modifier} is {low}–100, not {value}"));
        }
        look.set_face(modifier, *value as i8);
    }
    if !look.is_reasonable() {
        return Err("the server would not keep that look".into());
    }
    Ok(look)
}

/// The names of a list the panel shows by colour.
fn names(list: &[(&'static str, [u8; 3])]) -> Vec<&'static str> {
    list.iter().map(|(n, _)| *n).collect()
}

/// The names of a list the panel shows by label.
fn labelled(list: &[(&'static str, &str)]) -> Vec<&'static str> {
    list.iter().map(|(n, _)| *n).collect()
}

/// `name` if it is one of `known`.
fn pick(what: &str, name: &str, known: &[&str]) -> Result<String, String> {
    known
        .iter()
        .find(|k| k.eq_ignore_ascii_case(name))
        .map(|k| (*k).to_string())
        .ok_or_else(|| format!("no {what} \"{name}\"; there are {}", known.join(", ")))
}

/// As [`pick`], with "none" for none.
fn optional(what: &str, name: &str, known: &[&str]) -> Result<Option<String>, String> {
    if name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    pick(what, name, known).map(Some)
}

/// `[r, g, b]`, or `[]` for the thing's own colour.
fn colour(what: &str, c: &[u8]) -> Result<Option<[u8; 3]>, String> {
    match c {
        [] => Ok(None),
        [r, g, b] => Ok(Some([*r, *g, *b])),
        _ => Err(format!(
            "{what} is [r, g, b] or [], not {} numbers",
            c.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_is_passed_changes() {
        let before = HumanLook::default_for(false);
        let args = SetAvatarArgs {
            age: Some(60),
            hair_colour: Some(vec![150, 150, 150]),
            shape: Some([("nose/nose-hump-decr-incr".to_string(), 40)].into()),
            ..Default::default()
        };
        let after = changed(before.clone(), &args).unwrap();
        assert_eq!(after.age, 60);
        assert_eq!(after.hair_colour, Some([150, 150, 150]));
        assert_eq!(after.face_value("nose/nose-hump-decr-incr"), 40);
        assert_eq!(after.outfit, before.outfit);
        assert_eq!(after.hair, before.hair);
    }

    #[test]
    fn a_starter_comes_first_and_the_rest_over_it() {
        let args = SetAvatarArgs {
            starter: Some("professor".into()),
            age: Some(50),
            ..Default::default()
        };
        let after = changed(HumanLook::default_for(true), &args).unwrap();
        let professor = STARTERS.iter().find(|s| s.label == "Professor").unwrap();
        assert_eq!(after.outfit, professor.look().outfit);
        assert_eq!(after.age, 50);
    }

    #[test]
    fn names_the_panel_lacks_are_refused() {
        let look = HumanLook::default_for(false);
        for args in [
            SetAvatarArgs {
                hair: Some("mohawk".into()),
                ..Default::default()
            },
            SetAvatarArgs {
                shape: Some([("nose/no-such".to_string(), 10)].into()),
                ..Default::default()
            },
            SetAvatarArgs {
                // One-sided: 0 … 100.
                shape: Some([("head/head-oval".to_string(), -10)].into()),
                ..Default::default()
            },
            SetAvatarArgs {
                extras: Some(vec![
                    "punkduck_glasses_modern_form".into(),
                    "toigo_round_glasses_leopard".into(),
                ]),
                ..Default::default()
            },
            SetAvatarArgs {
                outfit: Some("none".into()),
                ..Default::default()
            },
            SetAvatarArgs {
                age: Some(12),
                ..Default::default()
            },
        ] {
            assert!(changed(look.clone(), &args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn an_outfit_takes_off_separates() {
        let separates = SetAvatarArgs {
            outfit: Some("none".into()),
            extras: Some(vec![
                "toigo_fisherman_sweater".into(),
                "toigo_wool_pants".into(),
                "elvs_scruffy_beard1".into(),
            ]),
            ..Default::default()
        };
        let look = changed(HumanLook::default_for(false), &separates).unwrap();
        assert!(look.outfit.is_empty());
        let suit = SetAvatarArgs {
            outfit: Some("male_elegantsuit01".into()),
            ..Default::default()
        };
        let look = changed(look, &suit).unwrap();
        assert_eq!(look.extras, vec!["elvs_scruffy_beard1".to_string()]);
    }
}
