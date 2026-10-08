//! Attaching the camera to something that moves — a satellite today, anything
//! with a position tomorrow.
//!
//! The split is one of ownership. **The owner of a target** (the satellite
//! layer, say) knows where it is and nothing about cameras: it calls
//! [`CameraAttach::attach`] when the user asks, then [`CameraAttach::report`]
//! every frame with the target's position, before `camera_controls` runs.
//! **The camera** knows nothing about satellites: while a target has a position
//! it rides along at a spherical offset the user turns by dragging and zooms with
//! the wheel or a pinch (`free_camera::attached_mode`), instead of running the
//! camera mode it was in.
//!
//! Three rules keep that from going wrong in ways nobody chose:
//!
//! - **A target that stops being reported is let go.** If the owner goes quiet
//!   for [`MISSED_FRAMES_LIMIT`] frames — its layer switched off, the thing
//!   streamed out — the camera does not hang at the last position it was told.
//! - **Switching camera mode lets go.** The mode the user picks next is a clear
//!   statement that they want a different camera.
//! - **Letting go hands the view to Earth mode from where the camera is**, so
//!   detaching never jumps the view back to an Earth pose from before the
//!   attachment (`free_camera::release_attachment`).
//!
//! Positions are in the **anchored world frame**: a `big_space` cell origin plus
//! a translation, the frame `cell_origin(cell) + transform.translation` gives for
//! the camera and the globe's `center_local` is expressed in. A child of the
//! `BigSpace` root at `GridCell::default()` with an identity transform has
//! mesh-local coordinates in exactly this frame.

use bevy::math::DVec3;
use bevy::prelude::*;

use crate::resources::CameraMode;
use crate::systems::egui_manager::EguiManager;
use crate::systems::theme;

/// Frames without a report before an attachment is dropped.
pub const MISSED_FRAMES_LIMIT: u32 = 30;

/// Which thing is attached: the owning layer, and its own id for the thing.
/// An owner compares this to know whether the attachment is still its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttachKey {
    pub source: &'static str,
    pub id: u64,
}

/// How the camera may sit around a target, metres from it.
#[derive(Clone, Copy, Debug)]
pub struct AttachRange {
    /// Where the camera settles after attaching.
    pub start_m: f64,
    pub min_m: f64,
    pub max_m: f64,
}

/// The attached thing, as its owner described it.
#[derive(Clone, Debug)]
pub struct AttachTarget {
    pub key: AttachKey,
    /// Shown in the chip at the top of the screen.
    pub label: String,
    pub range: AttachRange,
    /// The latest reported position, anchored world metres.
    position: Option<DVec3>,
    /// "Up" at the target: the camera keeps its horizon level about this.
    up: DVec3,
    /// Frames since the last report.
    missed: u32,
}

/// The camera's own state while attached: where it sits around the target.
/// Written by `free_camera`, which seeds it from the camera's pose on the first
/// attached frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct AttachRig {
    /// Angle about the target's up, from its north, radians.
    pub yaw: f32,
    /// Elevation above the target's horizon, radians.
    pub pitch: f32,
    pub distance: f64,
    pub distance_target: f64,
    /// The camera mode that was active when the camera took the attachment;
    /// a change of mode lets go.
    pub mode: CameraMode,
}

#[derive(Resource, Default)]
pub struct CameraAttach {
    target: Option<AttachTarget>,
    /// `Some` once the camera has placed itself on the target, and until it has
    /// handed the view back. Separate from `target` because letting go is the
    /// camera's job: the frame the target goes away, the camera still has to
    /// re-seed the mode it returns to from where it now is.
    pub(crate) rig: Option<AttachRig>,
}

impl CameraAttach {
    /// Attach the camera to `key`. The owner must [`report`](Self::report) its
    /// position from this frame on, or the attachment lapses.
    pub fn attach(&mut self, key: AttachKey, label: impl Into<String>, range: AttachRange) {
        self.target = Some(AttachTarget {
            key,
            label: label.into(),
            range,
            position: None,
            up: DVec3::Y,
            missed: 0,
        });
    }

    /// Let go, whoever asks — the chip's ✕, an owner whose thing has gone.
    pub fn detach(&mut self) {
        self.target = None;
    }

    /// Let go of `key` only: an owner retiring its own target must not drop
    /// somebody else's.
    pub fn detach_if(&mut self, key: AttachKey) {
        if self.is(key) {
            self.target = None;
        }
    }

    /// Is the camera attached to `key`?
    #[must_use]
    pub fn is(&self, key: AttachKey) -> bool {
        self.target.as_ref().is_some_and(|t| t.key == key)
    }

    /// The attached key, if any.
    #[must_use]
    pub fn key(&self) -> Option<AttachKey> {
        self.target.as_ref().map(|t| t.key)
    }

    /// The owner's per-frame report: where its target is, and which way is up
    /// there. Ignored unless `key` is the one attached.
    pub fn report(&mut self, key: AttachKey, position: DVec3, up: DVec3) {
        if let Some(t) = self.target.as_mut().filter(|t| t.key == key) {
            if position.is_finite() {
                t.position = Some(position);
                t.up = up.try_normalize().unwrap_or(DVec3::Y);
                t.missed = 0;
            }
        }
    }

    /// What the camera should ride this frame: the target's position, up and
    /// range, or `None` while there is nothing (yet) to place itself on.
    ///
    /// Counts the frame against the owner's reports and drops a target that
    /// has gone quiet, so this is called once a frame, by the camera.
    pub(crate) fn take_frame(&mut self) -> Option<(DVec3, DVec3, AttachRange)> {
        let t = self.target.as_mut()?;
        t.missed += 1;
        if t.missed > MISSED_FRAMES_LIMIT {
            tracing::info!(target = %t.label, "camera attachment lapsed: no position reported");
            self.target = None;
            return None;
        }
        Some((t.position?, t.up, t.range))
    }

    /// Whether a target is held (attached, even if not yet reported).
    #[must_use]
    pub fn active(&self) -> bool {
        self.target.is_some()
    }

    /// The distance the camera is at from its target, while attached — the
    /// space tier's near plane has to sit inside it.
    #[must_use]
    pub fn distance_m(&self) -> Option<f64> {
        self.target.as_ref()?;
        self.rig.map(|r| r.distance)
    }

    fn label(&self) -> Option<&str> {
        self.target.as_ref().map(|t| t.label.as_str())
    }
}

/// The chip at the top of the screen while attached: what the camera follows,
/// and the way to let go. Escape lets go as well.
pub fn render_attach_chip(
    egui: Res<EguiManager>,
    keyboard: Res<ButtonInput<KeyCode>>,
    mut attach: ResMut<CameraAttach>,
) {
    let Some(label) = attach.label().map(str::to_owned) else {
        return;
    };
    if keyboard.just_pressed(KeyCode::Escape) && !egui.ctx.text_edit_focused() {
        attach.detach();
        return;
    }
    let mut release = false;
    egui::Area::new(egui::Id::new("camera_attach_chip"))
        .anchor(
            egui::Align2::CENTER_TOP,
            egui::vec2(0.0, theme::OSD_MARGIN + 44.0),
        )
        .show(egui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Following")
                            .size(theme::SIZE_SUB)
                            .color(theme::TEXT_DIM),
                    );
                    ui.label(egui::RichText::new(&label).strong());
                    ui.add_space(6.0);
                    release = theme::close_button(ui)
                        .on_hover_text("Let go (Esc)")
                        .clicked();
                });
            });
        });
    if release {
        attach.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: AttachKey = AttachKey {
        source: "test",
        id: 7,
    };
    const RANGE: AttachRange = AttachRange {
        start_m: 100.0,
        min_m: 10.0,
        max_m: 1000.0,
    };

    #[test]
    fn nothing_to_ride_until_the_owner_reports() {
        let mut a = CameraAttach::default();
        a.attach(KEY, "thing", RANGE);
        assert!(a.take_frame().is_none());
        a.report(KEY, DVec3::new(1.0, 2.0, 3.0), DVec3::Y * 5.0);
        let (p, up, _) = a.take_frame().expect("reported");
        assert_eq!(p, DVec3::new(1.0, 2.0, 3.0));
        assert_eq!(up, DVec3::Y);
    }

    #[test]
    fn a_report_for_another_key_is_ignored() {
        let mut a = CameraAttach::default();
        a.attach(KEY, "thing", RANGE);
        a.report(
            AttachKey {
                source: "test",
                id: 8,
            },
            DVec3::ONE,
            DVec3::Y,
        );
        assert!(a.take_frame().is_none());
        a.detach_if(AttachKey {
            source: "other",
            id: 7,
        });
        assert!(a.is(KEY), "another owner cannot let go of this target");
    }

    #[test]
    fn a_quiet_owner_loses_the_camera() {
        let mut a = CameraAttach::default();
        a.attach(KEY, "thing", RANGE);
        a.report(KEY, DVec3::ONE, DVec3::Y);
        for _ in 0..MISSED_FRAMES_LIMIT {
            assert!(a.take_frame().is_some());
        }
        assert!(a.take_frame().is_none());
        assert!(!a.active());
    }
}
