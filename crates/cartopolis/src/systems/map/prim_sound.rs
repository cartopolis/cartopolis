//! Sounds a prim's script plays (`play_sound`, `loop_sound`; protocol 33).
//!
//! The library is closed (`cartopolis_core::SoundName`) and every clip in it is
//! synthesised at startup, as the footsteps are — `soundscape`'s module note has
//! why a shipped audio file is the wrong shape here. So a sound is always loaded
//! before anybody plays it.
//!
//! **The listener is the camera, and the distance is measured here.** The server
//! cannot measure it: an avatar's sim position is metres in its own client's
//! frame, so it sends every one-shot to everybody and each client drops the ones
//! out of range. Prims and the camera share one `GlobalTransform` space under
//! `big_space`, which makes the measurement a subtraction. It is the same rule
//! `prim_video` uses for a clip's sound track, so a screen and a speaker on the
//! same prim fade together.
//!
//! A loop is a level on the prim (`Prim::sound_loop`), not an event, so walking up
//! to a fountain starts it. Its player exists only while the camera is in range:
//! a world full of looping prims would otherwise hold a playing sink for every one
//! of them, each at volume zero.

// Without the `audio` feature nothing plays, but the clips and their tests still
// compile — the same split `soundscape` makes.
#![cfg_attr(not(feature = "audio"), allow(dead_code))]

use bevy::prelude::*;
use cartopolis_core::PrimSound;

/// One-shot sounds the network has delivered and nothing has played yet.
/// Filled by `network::apply_network_snapshot`, drained every frame.
#[derive(Resource, Default)]
pub struct PrimSoundInbox(pub Vec<(i64, PrimSound)>);

/// Most one-shots playing at once. A volley is several shells a second from
/// several launchers; past this the newest is dropped, which a listener cannot
/// tell from the others masking it.
const MAX_ONE_SHOTS: usize = 16;

/// A loop's player is started inside its range and stopped this far past it, so
/// standing on the edge does not start and stop it every frame.
const LOOP_HYSTERESIS: f32 = 1.1;

#[cfg(feature = "audio")]
pub use playback::*;

#[cfg(feature = "audio")]
mod playback {
    use super::{clips, PrimSoundInbox, LOOP_HYSTERESIS, MAX_ONE_SHOTS};
    use bevy::audio::{
        AudioPlayer, AudioSink, AudioSource, PlaybackMode, PlaybackSettings, Volume,
    };
    use bevy::prelude::*;
    use cartopolis_core::{PrimSound, SoundName};
    use std::collections::HashMap;

    use crate::components::{FreeCamera, Prim};
    use crate::resources::AppPrefs;

    /// The synthesised library, built once.
    #[derive(Resource)]
    pub struct PrimSoundBank(HashMap<SoundName, Handle<AudioSource>>);

    /// A one-shot that is playing, counted against [`MAX_ONE_SHOTS`].
    #[derive(Component)]
    pub struct PrimOneShot;

    /// The player for one prim's loop.
    #[derive(Component)]
    pub struct PrimLoopSound {
        prim: i64,
        sound: SoundName,
    }

    pub fn setup_prim_sounds(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let bank = SoundName::ALL
            .into_iter()
            .map(|name| {
                let handle = sources.add(AudioSource {
                    bytes: clips::clip(name).into(),
                });
                (name, handle)
            })
            .collect();
        commands.insert_resource(PrimSoundBank(bank));
    }

    /// Play what arrived, at the level the camera's distance gives it.
    pub fn play_prim_sounds(
        mut commands: Commands,
        mut inbox: ResMut<PrimSoundInbox>,
        bank: Option<Res<PrimSoundBank>>,
        prefs: Res<AppPrefs>,
        camera: Query<&GlobalTransform, With<FreeCamera>>,
        prims: Query<(&Prim, &GlobalTransform)>,
        playing: Query<(), With<PrimOneShot>>,
    ) {
        if inbox.0.is_empty() {
            return;
        }
        let arrived = std::mem::take(&mut inbox.0);
        let (Some(bank), Ok(eye)) = (bank, camera.single().map(GlobalTransform::translation))
        else {
            return;
        };
        let gain = prefs.effects_gain();
        let mut count = playing.iter().count();
        for (prim_id, sound) in arrived {
            if count >= MAX_ONE_SHOTS {
                break;
            }
            // A prim this client has not been sent yet has no place to be heard
            // from, and is out of range by any honest measure.
            let Some((_, at)) = prims.iter().find(|(p, _)| p.id == prim_id) else {
                continue;
            };
            let level = sound.gain_at(at.translation().distance(eye)) * gain;
            if level <= 1e-3 {
                continue;
            }
            let Some(handle) = bank.0.get(&sound.sound) else {
                continue;
            };
            commands.spawn((
                AudioPlayer(handle.clone()),
                PlaybackSettings {
                    mode: PlaybackMode::Despawn,
                    volume: Volume::Linear(level),
                    ..default()
                },
                PrimOneShot,
            ));
            count += 1;
        }
    }

    /// Keep one player per in-range looping prim, at the level its distance gives.
    pub fn drive_prim_loops(
        mut commands: Commands,
        bank: Option<Res<PrimSoundBank>>,
        prefs: Res<AppPrefs>,
        camera: Query<&GlobalTransform, With<FreeCamera>>,
        prims: Query<(&Prim, &GlobalTransform)>,
        mut players: Query<(Entity, &PrimLoopSound, Option<&mut AudioSink>)>,
    ) {
        let Some(bank) = bank else {
            return;
        };
        let eye = camera.single().map(GlobalTransform::translation).ok();
        let gain = prefs.effects_gain();
        // What each looping prim wants: its sound and its level now.
        let wanted: HashMap<i64, (PrimSound, f32)> = match eye {
            Some(eye) => prims
                .iter()
                .filter_map(|(p, at)| {
                    let sound = p.sound_loop?;
                    let d = at.translation().distance(eye);
                    Some((p.id, (sound, d)))
                })
                .collect(),
            None => HashMap::new(),
        };

        let mut have: HashMap<i64, SoundName> = HashMap::new();
        for (entity, player, sink) in &mut players {
            match wanted.get(&player.prim) {
                Some((sound, d))
                    if sound.sound == player.sound && *d < sound.range_m * LOOP_HYSTERESIS =>
                {
                    if let Some(mut sink) = sink {
                        sink.set_volume(Volume::Linear(sound.gain_at(*d) * gain));
                    }
                    have.insert(player.prim, player.sound);
                }
                // Stopped, changed to another sound, walked out of range, or the
                // prim is gone: the player goes, and a changed one is started
                // again below.
                _ => commands.entity(entity).try_despawn(),
            }
        }

        for (prim, (sound, d)) in wanted {
            if have.contains_key(&prim) || d >= sound.range_m {
                continue;
            }
            let Some(handle) = bank.0.get(&sound.sound) else {
                continue;
            };
            commands.spawn((
                AudioPlayer(handle.clone()),
                PlaybackSettings {
                    mode: PlaybackMode::Loop,
                    volume: Volume::Linear(sound.gain_at(d) * gain),
                    ..default()
                },
                PrimLoopSound {
                    prim,
                    sound: sound.sound,
                },
            ));
        }
    }
}

/// Without a backend there is nothing to play, but the inbox still has to be
/// emptied or it grows for as long as the session lasts.
#[cfg(not(feature = "audio"))]
pub fn play_prim_sounds(mut inbox: ResMut<PrimSoundInbox>) {
    inbox.0.clear();
}

/// The library's clips. Pure functions returning complete WAV files, so they
/// are compiled and tested whether or not the build can play them.
pub mod clips {
    use crate::systems::soundscape::{synth::wav, SAMPLE_RATE};
    use cartopolis_core::SoundName;
    use std::f32::consts::TAU;

    /// Every clip is scaled so its loudest sample sits here: the same headroom
    /// rule the footsteps keep, and loudness is then the script's `volume`.
    const PEAK: f32 = 0.7;

    #[must_use]
    pub fn clip(name: SoundName) -> Vec<u8> {
        let raw = match name {
            SoundName::Firework => firework(),
            SoundName::Crackle => crackle(),
            SoundName::Pop => pop(),
            SoundName::Chime => chime(),
            SoundName::Bell => bell(),
            SoundName::Click => click(),
            SoundName::Whoosh => whoosh(),
        };
        wav(&normalise(&raw))
    }

    fn len(secs: f32) -> usize {
        (secs * SAMPLE_RATE as f32) as usize
    }

    /// Deterministic white noise in -1..1 (xorshift32), so a clip is the same
    /// on every run and every device.
    fn noise(seed: &mut u32) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        (*seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    }

    fn normalise(samples: &[f32]) -> Vec<i16> {
        let peak = samples.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
        let scale = if peak > 1e-6 { PEAK / peak } else { 0.0 };
        samples
            .iter()
            .map(|v| ((v * scale).clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect()
    }

    /// A 5 ms fade in, so no clip opens on a step (an audible click).
    fn attack(i: usize) -> f32 {
        (i as f32 / len(0.005).max(1) as f32).min(1.0)
    }

    /// A shell bursting: a bright crack, then a dark boom that rolls away for a
    /// second and a half. The crack is what makes it read as an explosion rather
    /// than as thunder; the boom is what makes it read as big.
    fn firework() -> Vec<f32> {
        let n = len(1.8);
        let mut seed = 0xF12E_0001;
        let (mut a, mut b) = (0.0_f32, 0.0_f32);
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let white = noise(&mut seed);
                let crack = white * (-90.0 * t).exp();
                // Two poles low: the boom is the energy below ~150 Hz.
                a += 0.03 * (white - a);
                b += 0.03 * (a - b);
                let boom = b * 9.0 * (-2.6 * t).exp() * (1.0 - (-t * 60.0).exp());
                let thump = (t * 48.0 * TAU).sin() * (-5.0 * t).exp() * 0.5;
                (crack * 0.6 + boom + thump) * attack(i)
            })
            .collect()
    }

    /// The glitter after a shell: sparse, sharp ticks thinning out over a second.
    fn crackle() -> Vec<f32> {
        let n = len(1.4);
        let tick = len(0.003);
        let mut seed = 0xC2AC_7E01;
        let mut out = vec![0.0_f32; n];
        // The first tick is a few milliseconds in, so the clip does not open on one.
        let mut i = len(0.01);
        while i < n {
            let t = i as f32 / n as f32;
            // Gaps lengthen as it dies away.
            let gap =
                len(0.004 + 0.05 * t * t) + (noise(&mut seed).abs() * len(0.02) as f32) as usize;
            let level = (1.0 - t).powf(1.5) * (0.5 + 0.5 * noise(&mut seed).abs());
            for k in 0..tick {
                if let Some(s) = out.get_mut(i + k) {
                    *s += noise(&mut seed) * level * (1.0 - k as f32 / tick as f32);
                }
            }
            i += gap.max(1);
        }
        out
    }

    /// A dry pop: a falling pitch over a few tens of milliseconds.
    fn pop() -> Vec<f32> {
        let n = len(0.09);
        let mut phase = 0.0_f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let freq = 300.0 + 700.0 * (-60.0 * t).exp();
                phase += freq / SAMPLE_RATE as f32;
                (phase * TAU).sin() * (-55.0 * t).exp() * attack(i)
            })
            .collect()
    }

    /// A struck note, C6, with a soft octave over it.
    fn chime() -> Vec<f32> {
        let n = len(0.9);
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let env = (-5.0 * t).exp() * attack(i);
                ((t * 1046.5 * TAU).sin() + 0.3 * (t * 2093.0 * TAU).sin()) * env
            })
            .collect()
    }

    /// A large bell: inharmonic partials (a bell is not a string), the high ones
    /// dying first, so it starts bright and hums.
    fn bell() -> Vec<f32> {
        const F: f32 = 196.0;
        const PARTIALS: [(f32, f32, f32); 5] = [
            // (ratio to the strike note, level, decay per second)
            (0.5, 0.5, 0.6),
            (1.0, 1.0, 1.0),
            (1.19, 0.6, 1.4),
            (2.0, 0.45, 2.0),
            (2.74, 0.3, 3.2),
        ];
        let n = len(3.5);
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                PARTIALS
                    .iter()
                    .map(|(r, l, d)| (t * F * r * TAU).sin() * l * (-d * t).exp())
                    .sum::<f32>()
                    * attack(i)
            })
            .collect()
    }

    /// A switch: one very short, mid-bright tick.
    fn click() -> Vec<f32> {
        let n = len(0.025);
        let mut seed = 0xC11C_0001;
        let mut lp = 0.0_f32;
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                lp += 0.4 * (noise(&mut seed) - lp);
                (lp + 0.5 * (t * 2200.0 * TAU).sin()) * (-300.0 * t).exp() * attack(i)
            })
            .collect()
    }

    /// Air going past: band-passed noise sweeping up and back down, swelling and
    /// fading, so it is something passing rather than a steady rush.
    fn whoosh() -> Vec<f32> {
        let n = len(0.9);
        let mut seed = 0x0005_EEE5;
        let (mut low, mut band) = (0.0_f32, 0.0_f32);
        (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                let centre = 250.0 + 1_800.0 * (t * std::f32::consts::PI).sin();
                let f = (TAU / 2.0 * centre / SAMPLE_RATE as f32 * 2.0).min(1.2);
                low += f * band;
                band += f * (noise(&mut seed) - low - 0.5 * band);
                band * (t * std::f32::consts::PI).sin().powi(2)
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn samples(bytes: &[u8]) -> Vec<f32> {
            (0..(bytes.len() - 44) / 2)
                .map(|i| {
                    f32::from(i16::from_le_bytes([bytes[44 + i * 2], bytes[45 + i * 2]])) / 32767.0
                })
                .collect()
        }

        /// Every sound in the library exists, is audible, keeps its headroom
        /// and does not open on a click.
        #[test]
        fn every_sound_is_a_clean_audible_clip() {
            for name in SoundName::ALL {
                let bytes = clip(name);
                assert_eq!(&bytes[0..4], b"RIFF", "{name:?}");
                let s = samples(&bytes);
                assert!(!s.is_empty(), "{name:?} is empty");
                let peak = s.iter().fold(0.0_f32, |a, b| a.max(b.abs()));
                assert!((0.5..=0.8).contains(&peak), "{name:?} peaks at {peak:.3}");
                assert!(s[0].abs() < 0.05, "{name:?} opens on a step of {:.3}", s[0]);
            }
        }

        /// The sounds are distinct: a library of seven names that all came out
        /// as one noise would be worse than having one name.
        #[test]
        fn no_two_sounds_are_the_same() {
            let all: Vec<_> = SoundName::ALL.map(clip).to_vec();
            for (i, a) in all.iter().enumerate() {
                for b in &all[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }

        /// A firework's boom is low: most of its energy, after the crack, sits
        /// under a few hundred hertz. A boom measured as a hiss is a broken one.
        #[test]
        fn a_firework_booms() {
            let s = samples(&clip(SoundName::Firework));
            let tail = &s[len(0.1)..len(0.8)];
            let crossings = (1..tail.len())
                .filter(|&i| (tail[i] >= 0.0) != (tail[i - 1] >= 0.0))
                .count();
            let hz = crossings as f32 / (tail.len() as f32 / SAMPLE_RATE as f32) / 2.0;
            assert!(hz < 400.0, "the boom crosses at {hz:.0} Hz");
        }
    }
}
