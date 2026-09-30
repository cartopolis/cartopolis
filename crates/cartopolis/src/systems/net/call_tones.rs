//! The sounds of a call, the way a phone makes them (`docs/notes/calls.md`):
//! a ringtone for a call coming in, the ringback tone while yours rings on the
//! other side, a beep for a call waiting while you are in another, and short
//! cues when somebody joins or leaves and when the call ends.
//!
//! Synthesised at startup like every other sound in this client
//! (`soundscape`'s module note has why: a shipped audio file is an LFS object
//! with a silent failure mode). What plays is decided from the call state
//! each frame, not from events: one looping tone at a time, chosen by
//! [`wanted_loop`], and one-shot cues on the edges of who is in the call.

use crate::systems::soundscape::{synth, SAMPLE_RATE};

/// The looping tone a call state wants, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loop {
    /// Somebody is calling and you are in no call.
    Ringtone,
    /// Somebody is calling while you are in another call.
    Waiting,
    /// You are calling and nobody has picked up.
    Ringback,
}

/// Which loop should be playing: a call coming in outranks your own ringing,
/// and a call coming in while you talk is a quiet beep, not a ringtone over
/// the person you are talking to.
#[must_use]
pub fn wanted_loop(ringing: bool, in_call: bool, anyone_else: bool) -> Option<Loop> {
    match (ringing, in_call, anyone_else) {
        (true, false, _) => Some(Loop::Ringtone),
        (true, true, true) => Some(Loop::Waiting),
        (_, true, false) => Some(Loop::Ringback),
        _ => None,
    }
}

/// Clip synthesis: pure functions, compiled and tested whether or not the
/// build has an audio backend.
pub mod clips {
    use super::{synth, SAMPLE_RATE};

    fn at(secs: f32) -> usize {
        (secs * SAMPLE_RATE as f32) as usize
    }

    /// A struck note from `start` for `len` seconds: a fundamental and a soft
    /// octave, with a 5 ms attack (no click) and an exponential decay, as a
    /// marimba or a phone's chime sounds.
    fn chime(out: &mut [f32], start: f32, len: f32, freq: f32, level: f32) {
        let (from, n) = (at(start), at(len));
        let attack = at(0.005).max(1);
        for i in 0..n {
            let Some(s) = out.get_mut(from + i) else {
                break;
            };
            let t = i as f32 / SAMPLE_RATE as f32;
            let env = if i < attack {
                i as f32 / attack as f32
            } else {
                (-5.5 * t / len).exp()
            };
            let tau = std::f32::consts::TAU;
            *s += level * env * ((t * freq * tau).sin() + 0.3 * (t * freq * 2.0 * tau).sin());
        }
    }

    /// A steady tone from `start` for `len` seconds with 10 ms fades — the
    /// ringback and the waiting beep, which are signals rather than music.
    fn beep(out: &mut [f32], start: f32, len: f32, freq: f32, level: f32) {
        let (from, n) = (at(start), at(len));
        let fade = at(0.010).max(1);
        for i in 0..n {
            let Some(s) = out.get_mut(from + i) else {
                break;
            };
            let t = i as f32 / SAMPLE_RATE as f32;
            let env = (i.min(n - i) as f32 / fade as f32).min(1.0);
            *s += level * env * (t * freq * std::f32::consts::TAU).sin();
        }
    }

    fn render(samples: &[f32]) -> Vec<u8> {
        let pcm: Vec<i16> = samples
            .iter()
            .map(|v| (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
            .collect();
        synth::wav(&pcm)
    }

    /// The ringtone: a rising three-note figure, twice, then quiet, in a
    /// 3-second loop — insistent enough to notice across a room, and ours.
    #[must_use]
    pub fn ringtone() -> Vec<u8> {
        let mut out = vec![0.0; at(3.0)];
        for start in [0.0, 0.62] {
            for (k, freq) in [659.3, 830.6, 987.8].into_iter().enumerate() {
                chime(&mut out, start + k as f32 * 0.14, 0.45, freq, 0.22);
            }
        }
        render(&out)
    }

    /// The ringback tone: 425 Hz, one second on and four off — the tone a
    /// European phone plays while the other side rings.
    #[must_use]
    pub fn ringback() -> Vec<u8> {
        let mut out = vec![0.0; at(5.0)];
        beep(&mut out, 0.0, 1.0, 425.0, 0.28);
        render(&out)
    }

    /// A call waiting: two short beeps every three seconds, quiet enough to
    /// talk over.
    #[must_use]
    pub fn waiting() -> Vec<u8> {
        let mut out = vec![0.0; at(3.0)];
        beep(&mut out, 0.0, 0.15, 440.0, 0.2);
        beep(&mut out, 0.3, 0.15, 440.0, 0.2);
        render(&out)
    }

    /// Somebody joined: two notes rising.
    #[must_use]
    pub fn joined() -> Vec<u8> {
        let mut out = vec![0.0; at(0.5)];
        chime(&mut out, 0.0, 0.25, 523.3, 0.25);
        chime(&mut out, 0.12, 0.35, 784.0, 0.25);
        render(&out)
    }

    /// Somebody left: the same two notes falling.
    #[must_use]
    pub fn left() -> Vec<u8> {
        let mut out = vec![0.0; at(0.5)];
        chime(&mut out, 0.0, 0.25, 784.0, 0.22);
        chime(&mut out, 0.12, 0.35, 523.3, 0.22);
        render(&out)
    }

    /// The call ended: three short falling beeps, as a phone hangs up.
    #[must_use]
    pub fn ended() -> Vec<u8> {
        let mut out = vec![0.0; at(0.8)];
        for (k, freq) in [659.3, 523.3, 392.0].into_iter().enumerate() {
            beep(&mut out, k as f32 * 0.22, 0.16, freq, 0.22);
        }
        render(&out)
    }
}

#[cfg(feature = "audio")]
mod play {
    use bevy::audio::{AudioPlayer, AudioSource, PlaybackMode, PlaybackSettings, Volume};
    use bevy::prelude::*;

    use super::{clips, wanted_loop, Loop};
    use crate::resources::{AppPrefs, AuthIdentity};
    use crate::systems::calls::Calls;

    /// Louder than the footsteps: a ring is meant to be heard.
    const RING_VOLUME: f32 = 0.9;
    const CUE_VOLUME: f32 = 0.6;

    /// The clips, built once.
    #[derive(Resource)]
    pub struct CallTones {
        ringtone: Handle<AudioSource>,
        ringback: Handle<AudioSource>,
        waiting: Handle<AudioSource>,
        joined: Handle<AudioSource>,
        left: Handle<AudioSource>,
        ended: Handle<AudioSource>,
    }

    /// The looping tone playing now.
    #[derive(Component)]
    pub struct CallToneLoop(Loop);

    pub fn setup_call_tones(mut commands: Commands, mut sources: ResMut<Assets<AudioSource>>) {
        let mut add = |bytes: Vec<u8>| {
            sources.add(AudioSource {
                bytes: bytes.into(),
            })
        };
        commands.insert_resource(CallTones {
            ringtone: add(clips::ringtone()),
            ringback: add(clips::ringback()),
            waiting: add(clips::waiting()),
            joined: add(clips::joined()),
            left: add(clips::left()),
            ended: add(clips::ended()),
        });
    }

    /// Start, swap or stop the loop the call state wants, and play the cues on
    /// the edges: somebody joined, somebody left, the call ended.
    #[allow(clippy::too_many_arguments)]
    pub fn play_call_tones(
        mut commands: Commands,
        calls: Res<Calls>,
        identity: Res<AuthIdentity>,
        prefs: Res<AppPrefs>,
        tones: Option<Res<CallTones>>,
        playing: Query<(Entity, &CallToneLoop)>,
        mut last_others: Local<Option<usize>>,
    ) {
        let Some(tones) = tones else { return };
        let me = identity.username.clone().unwrap_or_default();
        let ringing = !calls.ringing(&me).is_empty();
        let in_call = calls.active_group().is_some();
        let others = calls.others().len();
        let want = wanted_loop(ringing, in_call, others > 0);
        let current = playing.iter().next().map(|(e, l)| (e, l.0));
        if current.map(|(_, l)| l) != want {
            if let Some((e, _)) = current {
                commands.entity(e).try_despawn();
            }
            if let Some(l) = want {
                let (clip, volume) = match l {
                    Loop::Ringtone => (tones.ringtone.clone(), RING_VOLUME),
                    Loop::Ringback => (tones.ringback.clone(), CUE_VOLUME),
                    Loop::Waiting => (tones.waiting.clone(), CUE_VOLUME),
                };
                commands.spawn((
                    AudioPlayer(clip),
                    PlaybackSettings {
                        mode: PlaybackMode::Loop,
                        volume: Volume::Linear(volume * prefs.effects_gain()),
                        ..default()
                    },
                    CallToneLoop(l),
                ));
            }
        }

        let now = in_call.then_some(others);
        let cue = match (*last_others, now) {
            (Some(before), Some(after)) if after > before => Some(tones.joined.clone()),
            (Some(before), Some(after)) if after < before && after > 0 => Some(tones.left.clone()),
            // The call ended for this device (or the last other person left,
            // which ends it — `calls::drive_calls`).
            (Some(_), None) => Some(tones.ended.clone()),
            _ => None,
        };
        *last_others = now;
        if let Some(clip) = cue {
            commands.spawn((
                AudioPlayer(clip),
                PlaybackSettings {
                    mode: PlaybackMode::Despawn,
                    volume: Volume::Linear(CUE_VOLUME * prefs.effects_gain()),
                    ..default()
                },
            ));
        }
    }
}

#[cfg(feature = "audio")]
pub use play::{play_call_tones, setup_call_tones};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_rules_for_which_tone_rings() {
        // Nothing going on: silence.
        assert_eq!(wanted_loop(false, false, false), None);
        // A call coming in.
        assert_eq!(wanted_loop(true, false, false), Some(Loop::Ringtone));
        // Calling, nobody has picked up: ringback.
        assert_eq!(wanted_loop(false, true, false), Some(Loop::Ringback));
        // Talking: silence.
        assert_eq!(wanted_loop(false, true, true), None);
        // Talking, and somebody else calls: the waiting beep, not a ringtone.
        assert_eq!(wanted_loop(true, true, true), Some(Loop::Waiting));
    }

    #[test]
    fn the_tones_are_wav_and_leave_headroom() {
        for (name, bytes) in [
            ("ringtone", clips::ringtone()),
            ("ringback", clips::ringback()),
            ("waiting", clips::waiting()),
            ("joined", clips::joined()),
            ("left", clips::left()),
            ("ended", clips::ended()),
        ] {
            assert_eq!(&bytes[0..4], b"RIFF", "{name}");
            let n = (bytes.len() - 44) / 2;
            assert!(n > 0, "{name}: empty");
            let peak = (0..n)
                .map(|i| i16::from_le_bytes([bytes[44 + 2 * i], bytes[45 + 2 * i]]).unsigned_abs())
                .max()
                .unwrap_or(0);
            // Loud enough to hear, and never clipped (`soundscape`'s wind has
            // the story of a clipped clip).
            assert!(peak > 3000, "{name}: peak {peak} is barely audible");
            assert!(peak < 30_000, "{name}: peak {peak} is at the rails");
        }
    }
}
