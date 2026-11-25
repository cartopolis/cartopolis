//! The object editor and its texture picker drawn by the component library
//! (`lib_host`), beside egui's (`ui::render_edit_dialog`): the Basics,
//! Shape and Surface tabs of a built object, Save · Cancel · Delete under
//! them, and the texture library's grid that Surface ▸ Pick… opens.
//!
//! On with `CARTO_UI_LIB=object_editor` (or `1`), on the web `?ui=lib`.
//! egui's system steps aside (`ui::ObjectEditorLib::lib`). The editor's
//! state stays `EditDialogState`, which the live preview
//! (`ui::apply_live_prim_edits`), the gizmo and the save all read: each field
//! here is bridged to its value both ways — an edit here is written there,
//! and a change made elsewhere (the gizmo's drag, another selection) shows
//! here — and every button goes through `ui::apply_edit_act`, the code
//! egui's buttons run.
//!
//! **Numbers are typed or slid, not dragged.** egui's drag-to-change values
//! became the library's number boxes where a value has no natural range
//! (position, size, rotation, repeats) and sliders where it has one (path
//! cut, hollow, twist, taper, shear, slice, transparency, glow, texture
//! rotation, offset). The colour, egui's colour button, is three sliders and
//! a swatch: the library has no colour picker.
//!
//! **Placement.** As egui's on a phone: a sheet along the bottom, 8 above
//! the edge, as wide as 320 or the screen less 16, no taller than
//! `ui::SHEET_HEIGHT` of it. On a desktop egui left the window where it
//! fell; here it stands top left, clear of the round buttons
//! (`walk_guide::TURN_INSET`), and the picker beside it where there is room.
//!
//! **The picker shows a page of tiles at a time** (egui's scrolled the
//! whole catalogue): a tile asks for its texture when it is shown
//! (`PrimTextureCache::want`), and the catalogue must never be fetched
//! whole (`texture-catalogue-must-stay-small`). Its thumbnails are the
//! fetched textures themselves, registered in `mithril_bevy::LiveImages` as
//! `texture_thumb_<id>` while the picker is open; the web's DOM backend has
//! no `bevy:` images, so there each tile shows its name, as egui's does
//! while a texture is on its way.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::{EditDialogState, GameState, PrimTextureCache, TextureLibrary};
use crate::systems::egui_manager::EguiRef;
use crate::systems::gizmo::{GizmoMode, GizmoState};
use crate::systems::lib_place_card::{header, icon};
use crate::systems::lib_settings::LibScreens;
use crate::systems::ui::{
    apply_edit_act, dialog_width, EditAct, ObjectEditorLib, SHAPES, SHEET_HEIGHT,
};
use crate::systems::{icons, theme};

/// The editor's name in the library's host.
const KEY: &str = "object_editor";
/// The picker's: mounted after the editor, so over it.
const PICKER_KEY: &str = "texture_picker";
/// A thumbnail's name in `LiveImages` is this and the texture's id.
const THUMB: &str = "texture_thumb_";
/// Tiles on a page of the picker.
const PAGE: usize = 12;
/// A tile's side, px: egui's.
const TILE: f32 = 72.0;

/// A number of the editor: where it lives in `EditDialogState`, and how it
/// is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Num {
    Pos(usize),
    Size(usize),
    Rot(usize),
    CutBegin,
    CutEnd,
    Hollow,
    TwistBegin,
    TwistEnd,
    TaperX,
    TaperY,
    ShearX,
    ShearY,
    SliceBegin,
    SliceEnd,
    Colour(usize),
    Transparency,
    Glow,
    RepeatU,
    RepeatV,
    TexRotation,
    PerMetre,
    OffsetU,
    OffsetV,
}

/// The numbers typed into a box.
const BOXES: &[Num] = &[
    Num::Pos(0),
    Num::Pos(1),
    Num::Pos(2),
    Num::Size(0),
    Num::Size(1),
    Num::Size(2),
    Num::Rot(0),
    Num::Rot(1),
    Num::Rot(2),
    Num::RepeatU,
    Num::RepeatV,
    Num::PerMetre,
];

/// The numbers slid.
const SLIDERS: &[Num] = &[
    Num::CutBegin,
    Num::CutEnd,
    Num::Hollow,
    Num::TwistBegin,
    Num::TwistEnd,
    Num::TaperX,
    Num::TaperY,
    Num::ShearX,
    Num::ShearY,
    Num::SliceBegin,
    Num::SliceEnd,
    Num::Colour(0),
    Num::Colour(1),
    Num::Colour(2),
    Num::Transparency,
    Num::Glow,
    Num::TexRotation,
    Num::OffsetU,
    Num::OffsetV,
];

/// A number's range and step, as egui's `DragValue`s had them.
struct Spec {
    lo: Option<f64>,
    hi: Option<f64>,
    step: f64,
    /// Decimals shown, and kept when a value is read.
    decimals: usize,
    unit: &'static str,
}

const AXES: [&str; 3] = ["X", "Y", "Z"];

impl Num {
    fn spec(self) -> Spec {
        let s = |lo: f64, hi: f64, step: f64, decimals: usize, unit: &'static str| Spec {
            lo: Some(lo),
            hi: Some(hi),
            step,
            decimals,
            unit,
        };
        match self {
            Num::Pos(_) => Spec {
                lo: None,
                hi: None,
                step: 0.1,
                decimals: 3,
                unit: "",
            },
            Num::Size(_) => Spec {
                lo: Some(0.001),
                hi: None,
                step: 0.1,
                decimals: 3,
                unit: "",
            },
            Num::Rot(_) => Spec {
                lo: None,
                hi: None,
                step: 1.0,
                decimals: 1,
                unit: "°",
            },
            Num::CutBegin | Num::CutEnd | Num::SliceBegin | Num::SliceEnd => {
                s(0.0, 1.0, 0.005, 3, "")
            }
            Num::Hollow => s(0.0, 95.0, 0.5, 1, ""),
            Num::TwistBegin | Num::TwistEnd | Num::TexRotation => s(-360.0, 360.0, 1.0, 0, "°"),
            Num::TaperX | Num::TaperY => s(-1.0, 1.0, 0.01, 2, ""),
            Num::ShearX | Num::ShearY => s(-0.5, 0.5, 0.01, 2, ""),
            Num::Colour(_) => s(0.0, 255.0, 1.0, 0, ""),
            Num::Transparency => s(0.0, 100.0, 0.5, 1, " %"),
            Num::Glow => s(0.0, 1.0, 0.01, 2, ""),
            Num::RepeatU | Num::RepeatV => s(0.0, 100.0, 0.05, 3, ""),
            Num::PerMetre => s(0.0, 10.0, 0.05, 3, ""),
            Num::OffsetU | Num::OffsetV => s(-1.0, 1.0, 0.005, 3, ""),
        }
    }

    /// Its value in the editor, as shown (degrees, percent, 0–255).
    fn raw(self, d: &EditDialogState) -> f32 {
        match self {
            Num::Pos(i) => d.position[i],
            Num::Size(i) => d.scale[i],
            Num::Rot(i) => d.rotation[i].to_degrees(),
            Num::CutBegin => d.path_cut_begin,
            Num::CutEnd => d.path_cut_end,
            Num::Hollow => d.hollow * 100.0,
            Num::TwistBegin => d.warp.twist_begin,
            Num::TwistEnd => d.warp.twist_end,
            Num::TaperX => d.warp.taper_x,
            Num::TaperY => d.warp.taper_y,
            Num::ShearX => d.warp.top_shear_x,
            Num::ShearY => d.warp.top_shear_y,
            Num::SliceBegin => d.warp.slice_begin,
            Num::SliceEnd => d.warp.slice_end,
            Num::Colour(i) => d.color[i] * 255.0,
            Num::Transparency => (1.0 - d.surface.alpha) * 100.0,
            Num::Glow => d.surface.glow,
            Num::RepeatU => d.surface.repeat_u,
            Num::RepeatV => d.surface.repeat_v,
            Num::TexRotation => d.surface.rotation,
            Num::PerMetre => d.repeats_per_meter,
            Num::OffsetU => d.surface.offset_u,
            Num::OffsetV => d.surface.offset_v,
        }
    }

    /// [`raw`](Self::raw) rounded to the decimals shown, so a value written
    /// and read back through `f32` is the value typed.
    fn read(self, d: &EditDialogState) -> f64 {
        let k = 10f64.powi(self.spec().decimals as i32);
        (f64::from(self.raw(d)) * k).round() / k
    }

    /// Write what was typed or slid, held to egui's ranges: a begin never
    /// past its end, a size never below a millimetre.
    fn write(self, d: &mut EditDialogState, v: f64) {
        let v = v as f32;
        match self {
            Num::Pos(i) => d.position[i] = v,
            Num::Size(i) => d.scale[i] = v.max(0.001),
            Num::Rot(i) => d.rotation[i] = v.to_radians(),
            Num::CutBegin => d.path_cut_begin = v.min(d.path_cut_end).max(0.0),
            Num::CutEnd => d.path_cut_end = v.max(d.path_cut_begin).min(1.0),
            Num::Hollow => d.hollow = v.clamp(0.0, 95.0) / 100.0,
            Num::TwistBegin => d.warp.twist_begin = v.clamp(-360.0, 360.0),
            Num::TwistEnd => d.warp.twist_end = v.clamp(-360.0, 360.0),
            Num::TaperX => d.warp.taper_x = v.clamp(-1.0, 1.0),
            Num::TaperY => d.warp.taper_y = v.clamp(-1.0, 1.0),
            Num::ShearX => d.warp.top_shear_x = v.clamp(-0.5, 0.5),
            Num::ShearY => d.warp.top_shear_y = v.clamp(-0.5, 0.5),
            Num::SliceBegin => d.warp.slice_begin = v.min(d.warp.slice_end).max(0.0),
            Num::SliceEnd => d.warp.slice_end = v.max(d.warp.slice_begin).min(1.0),
            Num::Colour(i) => d.color[i] = v.clamp(0.0, 255.0) / 255.0,
            Num::Transparency => d.surface.alpha = (1.0 - v / 100.0).clamp(0.0, 1.0),
            Num::Glow => d.surface.glow = v.clamp(0.0, 1.0),
            Num::RepeatU => d.surface.repeat_u = v.clamp(0.0, 100.0),
            Num::RepeatV => d.surface.repeat_v = v.clamp(0.0, 100.0),
            Num::TexRotation => d.surface.rotation = v.clamp(-360.0, 360.0),
            Num::PerMetre => d.repeats_per_meter = v.clamp(0.0, 10.0),
            Num::OffsetU => d.surface.offset_u = v.clamp(-1.0, 1.0),
            Num::OffsetV => d.surface.offset_v = v.clamp(-1.0, 1.0),
        }
    }

    /// The control's id, for its label.
    fn id(self) -> String {
        format!("oe-{:?}", self)
            .to_lowercase()
            .replace(['(', ')'], "")
    }

    /// How a slider's value reads beside its track.
    fn says(self) -> Rc<dyn Fn(f64) -> String> {
        let Spec { decimals, unit, .. } = self.spec();
        Rc::new(move |v: f64| format!("{v:.decimals$}{unit}"))
    }
}

/// A switch of the editor that is only a field: no message of its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Flag {
    FullBright,
    FlipU,
    FlipV,
}

const FLAGS: &[Flag] = &[Flag::FullBright, Flag::FlipU, Flag::FlipV];

impl Flag {
    fn read(self, d: &EditDialogState) -> bool {
        match self {
            Flag::FullBright => d.surface.full_bright,
            Flag::FlipU => d.surface.flip_u,
            Flag::FlipV => d.surface.flip_v,
        }
    }

    fn write(self, d: &mut EditDialogState, on: bool) {
        match self {
            Flag::FullBright => d.surface.full_bright = on,
            Flag::FlipU => d.surface.flip_u = on,
            Flag::FlipV => d.surface.flip_v = on,
        }
    }
}

/// A field's model, made once at the mount so an edit is never re-rendered
/// from under the pointer, and the value last bridged, to tell an edit here
/// from a change made elsewhere (the gizmo's drag, another selection).
struct Bound<T: Clone + PartialEq + 'static> {
    model: Signal<T>,
    last: RefCell<T>,
}

impl<T: Clone + PartialEq + 'static> Bound<T> {
    fn new(v: T) -> Self {
        Bound {
            model: signal(v.clone()),
            last: RefCell::new(v),
        }
    }

    /// What was typed here since the last frame; with nothing typed, the
    /// field follows `now`.
    fn sync(&self, now: T) -> Option<T> {
        let typed = self.model.get_untracked();
        if typed != *self.last.borrow() {
            *self.last.borrow_mut() = typed.clone();
            return Some(typed);
        }
        if now != *self.last.borrow() {
            *self.last.borrow_mut() = now.clone();
            self.model.set(now);
        }
        None
    }
}

/// A number box: what was typed into it, if a number. An emptied box waits
/// for the number being typed rather than filling itself again — unless the
/// value moves elsewhere meanwhile.
fn sync_box(b: &Bound<Option<f64>>, now: f64) -> Option<f64> {
    if b.model.get_untracked().is_none() && *b.last.borrow() == Some(now) {
        return None;
    }
    b.sync(Some(now)).flatten()
}

/// Every field of the editor.
struct Fields {
    tab: Bound<u8>,
    name: Bound<String>,
    tool: Bound<GizmoMode>,
    physical: Bound<bool>,
    shape: Bound<String>,
    boxes: Vec<(Num, Bound<Option<f64>>)>,
    sliders: Vec<(Num, Bound<f64>)>,
    flags: Vec<(Flag, Bound<bool>)>,
}

impl Fields {
    fn of(d: &EditDialogState, tool: GizmoMode) -> Self {
        Fields {
            tab: Bound::new(d.active_tab),
            name: Bound::new(d.name.clone()),
            tool: Bound::new(tool),
            physical: Bound::new(d.physical),
            shape: Bound::new(d.shape.clone()),
            boxes: BOXES
                .iter()
                .map(|k| (*k, Bound::new(Some(k.read(d)))))
                .collect(),
            sliders: SLIDERS
                .iter()
                .map(|k| (*k, Bound::new(k.read(d))))
                .collect(),
            flags: FLAGS.iter().map(|f| (*f, Bound::new(f.read(d)))).collect(),
        }
    }

    fn boxed(&self, k: Num) -> Signal<Option<f64>> {
        self.boxes
            .iter()
            .find(|(n, _)| *n == k)
            .map_or_else(|| signal(None), |(_, b)| b.model.clone())
    }

    fn slid(&self, k: Num) -> Signal<f64> {
        self.sliders
            .iter()
            .find(|(n, _)| *n == k)
            .map_or_else(|| signal(0.0), |(_, b)| b.model.clone())
    }

    fn flag(&self, f: Flag) -> Signal<bool> {
        self.flags
            .iter()
            .find(|(n, _)| *n == f)
            .map_or_else(|| signal(false), |(_, b)| b.model.clone())
    }
}

/// What the editor shows besides its fields.
#[derive(Clone, PartialEq)]
struct Look {
    playing_video: bool,
    video_note: Option<String>,
    texture: Option<String>,
    /// The library has textures: Pick… is offered.
    can_pick: bool,
}

impl Look {
    fn of(d: &EditDialogState, lib: &TextureLibrary) -> Self {
        Look {
            playing_video: d.playing_video,
            video_note: d.video_note.clone(),
            texture: d.texture_id.clone(),
            can_pick: !lib.entries.is_empty(),
        }
    }
}

/// Where a card stands, px in the library's root.
#[derive(Clone, Copy, PartialEq)]
struct Spot {
    left: f32,
    /// From the top, or (a phone's sheet) from the bottom.
    top: Option<f32>,
    bottom: Option<f32>,
    width: f32,
    max_height: f32,
}

impl Spot {
    fn style(&self) -> String {
        let edge = match (self.top, self.bottom) {
            (Some(t), _) => format!("top: {t:.0}px"),
            (None, Some(b)) => format!("bottom: {b:.0}px"),
            (None, None) => "top: 0".to_owned(),
        };
        format!(
            "position: absolute; left: {:.0}px; {edge}; width: {:.0}px; max-height: {:.0}px; \
             box-sizing: border-box; display: flex; flex-direction: column; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg); overflow: hidden",
            self.left, self.width, self.max_height
        )
    }
}

/// The editor's spot and the picker's, from the viewport (egui points) at
/// `k` px per point.
fn spots(vw: f32, vh: f32, compact: bool, k: f32) -> (Spot, Spot) {
    let width = dialog_width(vw, 320.0);
    let picker_w = dialog_width(vw, 380.0);
    if compact {
        let max_height = (vh * SHEET_HEIGHT).max(200.0);
        let sheet = |w: f32| Spot {
            left: ((vw - w) / 2.0).max(0.0) * k,
            top: None,
            bottom: Some(8.0 * k),
            width: w * k,
            max_height: max_height * k,
        };
        return (sheet(width), sheet(picker_w));
    }
    let left = crate::systems::walk_guide::TURN_INSET;
    let top = theme::OSD_MARGIN;
    let max_height = (vh - 2.0 * theme::OSD_MARGIN).max(200.0);
    let beside = left + width + theme::OSD_MARGIN;
    let picker_left = if beside + picker_w <= vw - theme::OSD_MARGIN {
        beside
    } else {
        ((vw - picker_w) / 2.0).max(0.0)
    };
    let editor = Spot {
        left: left * k,
        top: Some(top * k),
        bottom: None,
        width: width * k,
        max_height: max_height * k,
    };
    let picker = Spot {
        left: picker_left * k,
        top: Some(top * k),
        bottom: None,
        width: picker_w * k,
        max_height: max_height * k,
    };
    (editor, picker)
}

/// One tile of the picker.
#[derive(Clone, PartialEq)]
struct TileV {
    id: String,
    name: String,
    /// Its thumbnail is registered (`LiveImages`): an `<img>`, not its name.
    ready: bool,
    selected: bool,
}

/// The picker's page.
#[derive(Clone, PartialEq)]
struct PickerLook {
    tiles: Vec<TileV>,
    page: usize,
    pages: usize,
    /// "13–24 of 203".
    range: String,
}

/// What a tap asked for; done by the system.
#[derive(Clone)]
enum Act {
    Edit(EditAct),
    Page(isize),
}

type Acts = Rc<RefCell<Vec<Act>>>;

fn on(acts: &Acts, a: Act) -> Handler {
    let acts = acts.clone();
    Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler
}

fn act(acts: &Acts, a: EditAct) -> Option<Handler> {
    Some(on(acts, Act::Edit(a)))
}

/// The editor, mounted.
struct Editor {
    /// What it was mounted for: another object, or a new one, mounts again.
    prim: Option<i64>,
    is_new: bool,
    dark: bool,
    fields: Rc<Fields>,
    look: Signal<Look>,
    spot: Signal<Spot>,
}

/// The picker, mounted.
struct Picker {
    look: Signal<PickerLook>,
    spot: Signal<Spot>,
}

struct Mounted {
    acts: Acts,
    editor: Option<Editor>,
    picker: Option<Picker>,
    /// The picker's page, kept between openings as egui's scroll is.
    page: usize,
}

thread_local! {
    static MOUNTED: RefCell<Option<Mounted>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the object editor and its texture picker.
/// `PostUpdate`, before `DriveUi`, as every library screen.
#[allow(clippy::too_many_arguments)]
pub fn drive_lib_object_editor(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    bridge: Option<ResMut<ObjectEditorLib>>,
    mut edit_dialog: ResMut<EditDialogState>,
    mut game_state: ResMut<GameState>,
    mut gizmo: ResMut<GizmoState>,
    texture_lib: Res<TextureLibrary>,
    mut texture_cache: ResMut<PrimTextureCache>,
    egui: EguiRef,
    #[cfg(all(
        feature = "ui-probe",
        not(all(feature = "ui-dom", target_arch = "wasm32"))
    ))]
    mut live_images: Option<ResMut<mithril_bevy::LiveImages>>,
) {
    if !lib.object_editor {
        return;
    }
    let Some(mut bridge) = bridge else {
        return;
    };
    if !bridge.lib {
        bridge.lib = true;
    }
    let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
        egui.ui_scale()
    } else {
        1.0
    };
    let size = egui.viewport_pts();
    let (editor_spot, picker_spot) = spots(size.x, size.y, egui.compact(), k);
    let dark = theme::p().ink == theme::DARK.ink;

    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let s = m.get_or_insert_with(|| Mounted {
            acts: Rc::new(RefCell::new(Vec::new())),
            editor: None,
            picker: None,
            page: 0,
        });

        // The fields → the editor, as egui's `changed()`.
        if let Some(e) = s.editor.as_ref() {
            bridge_fields(&e.fields, &mut edit_dialog, &mut game_state, &mut gizmo);
        }
        // The taps of the frame, through the code egui's buttons run.
        let acts: Vec<Act> = std::mem::take(&mut *s.acts.borrow_mut());
        let pages = texture_lib.entries.len().div_ceil(PAGE).max(1);
        for a in acts {
            match a {
                Act::Edit(a) => apply_edit_act(a, &mut edit_dialog, &mut game_state),
                Act::Page(d) => {
                    s.page = s.page.saturating_add_signed(d).min(pages - 1);
                }
            }
        }
        s.page = s.page.min(pages - 1);

        // The editor: down when it closes, or to mount again for another
        // object or theme.
        let stale = s.editor.as_ref().is_some_and(|e| {
            !edit_dialog.visible
                || e.prim != edit_dialog.prim_id
                || e.is_new != edit_dialog.is_new
                || e.dark != dark
        });
        if stale {
            s.editor = None;
            crate::systems::lib_host::clear_keyed(KEY);
        }
        let picker_wanted = edit_dialog.visible && edit_dialog.texture_picker_open;
        if s.picker.is_some() && (!picker_wanted || s.editor.is_none()) {
            s.picker = None;
            crate::systems::lib_host::clear_keyed(PICKER_KEY);
        }
        if !edit_dialog.visible {
            #[cfg(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            ))]
            forget_thumbs(live_images.as_deref_mut());
            return;
        }

        let look = Look::of(&edit_dialog, &texture_lib);
        match s.editor.as_ref() {
            None => {
                let e = Editor {
                    prim: edit_dialog.prim_id,
                    is_new: edit_dialog.is_new,
                    dark,
                    fields: Rc::new(Fields::of(&edit_dialog, gizmo.mode)),
                    look: signal(look),
                    spot: signal(editor_spot),
                };
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    editor_view(&e, &s.acts),
                    dark,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(err) = std::fs::write(&path, html(editor_view(&e, &s.acts))) {
                        warn!(?path, error = %err, "lib object editor: could not write the HTML");
                    }
                }
                s.editor = Some(e);
            }
            Some(e) => {
                if e.look.peek(|now| *now != look) {
                    e.look.set(look);
                }
                if e.spot.get_untracked() != editor_spot {
                    e.spot.set(editor_spot);
                }
            }
        }

        // The picker: the page's tiles, each asking for its texture.
        if !picker_wanted {
            #[cfg(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            ))]
            forget_thumbs(live_images.as_deref_mut());
            return;
        }
        let first = s.page * PAGE;
        let shown = texture_lib.entries.iter().skip(first).take(PAGE);
        let mut tiles = Vec::with_capacity(PAGE);
        for entry in shown {
            texture_cache.want(&entry.id);
            // Its thumbnail before the view that shows it: an `<img>` reads
            // its source when it is styled.
            #[cfg(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            ))]
            let ready = match (
                live_images.as_deref_mut(),
                texture_cache.handles.get(&entry.id),
            ) {
                (Some(live), Some(handle)) => {
                    let name = format!("{THUMB}{}", entry.id);
                    if !live.0.contains_key(&name) {
                        live.0.insert(name, handle.clone());
                    }
                    true
                }
                _ => false,
            };
            #[cfg(not(all(
                feature = "ui-probe",
                not(all(feature = "ui-dom", target_arch = "wasm32"))
            )))]
            let ready = false;
            tiles.push(TileV {
                id: entry.id.clone(),
                name: entry.name.clone(),
                ready,
                selected: edit_dialog.texture_id.as_deref() == Some(entry.id.as_str()),
            });
        }
        let total = texture_lib.entries.len();
        let picker_look = PickerLook {
            range: format!(
                "{}–{} of {total}",
                (first + 1).min(total),
                (first + tiles.len()).min(total)
            ),
            tiles,
            page: s.page,
            pages,
        };
        match s.picker.as_ref() {
            None => {
                let p = Picker {
                    look: signal(picker_look),
                    spot: signal(picker_spot),
                };
                crate::systems::lib_host::mount_keyed(
                    PICKER_KEY,
                    picker_view(&p, &s.acts),
                    dark,
                    theme::touch_layout(),
                );
                s.picker = Some(p);
            }
            Some(p) => {
                if p.look.peek(|now| *now != picker_look) {
                    p.look.set(picker_look);
                }
                if p.spot.get_untracked() != picker_spot {
                    p.spot.set(picker_spot);
                }
            }
        }
    });
}

/// The thumbnails go with the picker: nothing shows them again.
#[cfg(all(
    feature = "ui-probe",
    not(all(feature = "ui-dom", target_arch = "wasm32"))
))]
fn forget_thumbs(live: Option<&mut mithril_bevy::LiveImages>) {
    if let Some(live) = live {
        if live.0.keys().any(|k| k.starts_with(THUMB)) {
            live.0.retain(|k, _| !k.starts_with(THUMB));
        }
    }
}

/// Each field's edit of the frame → the editor; a value moved elsewhere →
/// its field. The resources are written only where something was edited.
fn bridge_fields(
    f: &Fields,
    edit_dialog: &mut ResMut<EditDialogState>,
    game_state: &mut ResMut<GameState>,
    gizmo: &mut ResMut<GizmoState>,
) {
    if let Some(t) = f.tab.sync(edit_dialog.active_tab) {
        edit_dialog.active_tab = t;
    }
    if let Some(n) = f.name.sync(edit_dialog.name.clone()) {
        edit_dialog.name = n;
    }
    if let Some(s) = f.shape.sync(edit_dialog.shape.clone()) {
        edit_dialog.shape = s;
    }
    if let Some(mode) = f.tool.sync(gizmo.mode) {
        gizmo.mode = mode;
    }
    let mut writes: Vec<(Num, f64)> = Vec::new();
    for (key, b) in &f.boxes {
        if let Some(v) = sync_box(b, key.read(edit_dialog)) {
            writes.push((*key, v));
        }
    }
    for (key, b) in &f.sliders {
        if let Some(v) = b.sync(key.read(edit_dialog)) {
            writes.push((*key, v));
        }
    }
    for (key, v) in writes {
        key.write(edit_dialog, v);
    }
    let mut flips: Vec<(Flag, bool)> = Vec::new();
    for (flag, b) in &f.flags {
        if let Some(on) = b.sync(flag.read(edit_dialog)) {
            flips.push((*flag, on));
        }
    }
    for (flag, on) in flips {
        flag.write(edit_dialog, on);
    }
    // Physical has a message of its own: through the act egui's box runs.
    if let Some(on) = f.physical.sync(edit_dialog.physical) {
        apply_edit_act(EditAct::Physical(on), edit_dialog, game_state);
    }
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(view: View) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view, &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// A heading over a group of fields, as a field's label is set.
fn heading(text: &'static str) -> View {
    view! {
        <span style="font-size: 14px; font-weight: 600; color: var(--m-text-strong)">{text}</span>
    }
}

/// A line of explanation under a control.
fn note(text: String) -> View {
    view! {
        <span style="color: var(--m-text-dim); font-size: 12.5px; line-height: 1.4">{text}</span>
    }
}

/// A heading and what it heads.
fn group(title: &'static str, body: View) -> View {
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            {heading(title)}
            {body}
        </div>
    }
}

/// A hairline between the tab's parts, as egui's separators.
fn rule() -> View {
    view! { <div style="height: 1px; background: var(--m-border)"></div> }
}

/// A number box, its label above it.
fn number(f: &Fields, key: Num, label: &'static str) -> View {
    let s = key.spec();
    let unit: View = if s.unit.is_empty() {
        View::Empty
    } else {
        let u = s.unit;
        view! { <span>{u}</span> }
    };
    view! {
        <FieldNumber model={f.boxed(key)} min={s.lo} max={s.hi} step={s.step} plain=true unit={unit} label={label} id={key.id()} />
    }
}

/// X, Y and Z boxes sharing a row under their heading: egui's
/// `xyz_fields`, which it made a row of its own for a phone's width.
fn xyz(f: &Fields, title: &'static str, keys: [Num; 3]) -> View {
    let cells: Vec<View> = keys
        .iter()
        .zip(AXES)
        .map(|(key, axis)| {
            let field = number(f, *key, axis);
            view! { <div style="flex: 1; min-width: 0">{field}</div> }
        })
        .collect();
    group(
        title,
        view! { <div style="display: flex; gap: 8px">{cells}</div> },
    )
}

/// A slider with its label above and its value beside the track.
fn slider(f: &Fields, key: Num, label: &'static str) -> View {
    let s = key.spec();
    view! {
        <FieldSlider model={f.slid(key)} min={s.lo.unwrap_or(0.0)} max={s.hi.unwrap_or(1.0)} step={s.step} format={Some(key.says())} label={label} id={key.id()} />
    }
}

/// Two sliders under one heading: a begin and an end, or X and Y.
fn pair(f: &Fields, title: &'static str, a: (Num, &'static str), b: (Num, &'static str)) -> View {
    let first = slider(f, a.0, a.1);
    let second = slider(f, b.0, b.1);
    group(
        title,
        view! {
            <div style="display: flex; flex-direction: column; gap: 12px">
                {first}
                {second}
            </div>
        },
    )
}

/// The editor: its header, the tabs, the tab (scrolling alone) and the
/// buttons under every tab.
fn editor_view(e: &Editor, acts: &Acts) -> View {
    let frame = {
        let spot = e.spot.clone();
        move || spot.get().style()
    };
    let title = if e.is_new {
        "Create object"
    } else {
        "Edit object"
    };
    let tabs = vec![
        Opt::new(0u8, "Basics"),
        Opt::new(1u8, "Shape"),
        Opt::new(2u8, "Surface"),
    ];
    let body = {
        let (f, look, acts, is_new) = (e.fields.clone(), e.look.clone(), acts.clone(), e.is_new);
        move || -> View {
            match f.tab.model.get() {
                0 => basics(&f, is_new),
                1 => shape(&f),
                _ => surface(&f, &look, &acts, is_new),
            }
        }
    };
    let foot = buttons(acts, e.is_new);
    let head = header(
        icons::TUNE,
        title.to_owned(),
        on(acts, Act::Edit(EditAct::Close)),
    );
    // The card's own node listens (a click that does nothing), so the
    // pointer over its padding is the library's too.
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {head}
            <div style="flex: none; padding: 0 16px 12px">
                <FieldSegmented model={e.fields.tab.model.clone()} options={tabs} size=Some(Size::Sm) id="oe-tab" />
            </div>
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 4px 16px 16px; border-top: 1px solid var(--m-border)">
                <div style="display: flex; flex-direction: column; gap: 16px; padding-top: 12px">{body}</div>
            </div>
            <div style="flex: none; padding: 12px 16px 16px; border-top: 1px solid var(--m-border)">
                {foot}
            </div>
        </div>
    }
}

/// Basics: the name, the gizmo's tool, and Physical for an object that
/// exists.
fn basics(f: &Fields, is_new: bool) -> View {
    let tools = vec![
        Opt::new(GizmoMode::Translate, "Move (T)"),
        Opt::new(GizmoMode::Rotate, "Rotate (R)"),
        Opt::new(GizmoMode::Scale, "Scale (S)"),
    ];
    // Physical (protocol 34): its own message, so it applies at once rather
    // than on Save, and only to a prim the server already has.
    let physical: View = if is_new {
        View::Empty
    } else {
        group(
            "Physical",
            view! {
                <FieldCheckbox model={f.physical.model.clone()} label="Falls, bounces and can be pushed" id="oe-physical" />
            },
        )
    };
    view! {
        <FieldText model={f.name.model.clone()} label="Name" autocomplete="off" id="oe-name" />
        <FieldRadio model={f.tool.model.clone()} options={tools} inline=true label="Tool" id="oe-tool" />
        {physical}
    }
}

/// Shape: the block, where it stands, how big and how turned, and the path
/// and warp.
fn shape(f: &Fields) -> View {
    let shapes: Vec<Opt<String>> = SHAPES
        .iter()
        .map(|(key, label)| Opt::new((*key).to_owned(), *label))
        .collect();
    view! {
        <FieldRadio model={f.shape.model.clone()} options={shapes} inline=true label="Building block type" id="oe-shape" />
        {rule()}
        {xyz(f, "Position (meters)", [Num::Pos(0), Num::Pos(1), Num::Pos(2)])}
        {xyz(f, "Size (meters)", [Num::Size(0), Num::Size(1), Num::Size(2)])}
        {xyz(f, "Rotation (degrees)", [Num::Rot(0), Num::Rot(1), Num::Rot(2)])}
        {rule()}
        {pair(f, "Path cut begin and end", (Num::CutBegin, "Begin"), (Num::CutEnd, "End"))}
        {slider(f, Num::Hollow, "Hollow")}
        {pair(f, "Twist begin and end", (Num::TwistBegin, "Begin"), (Num::TwistEnd, "End"))}
        {pair(f, "Taper", (Num::TaperX, "X"), (Num::TaperY, "Y"))}
        {pair(f, "Top shear", (Num::ShearX, "X"), (Num::ShearY, "Y"))}
        {pair(f, "Slice begin and end", (Num::SliceBegin, "Begin"), (Num::SliceEnd, "End"))}
    }
}

/// Surface: the colour, a video, the texture and how it lies.
fn surface(f: &Fields, look: &Signal<Look>, acts: &Acts, is_new: bool) -> View {
    // The colour as it is now, beside its sliders: content, not chrome.
    let swatch = {
        let (r, g, b) = (
            f.slid(Num::Colour(0)),
            f.slid(Num::Colour(1)),
            f.slid(Num::Colour(2)),
        );
        move || {
            format!(
                "flex: none; width: 28px; height: 28px; border-radius: var(--m-radius-sm); \
                 border: 1px solid var(--m-border-strong); background: rgb({:.0} {:.0} {:.0})",
                r.get().clamp(0.0, 255.0),
                g.get().clamp(0.0, 255.0),
                b.get().clamp(0.0, 255.0)
            )
        }
    };
    let colour = view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            <div style="display: flex; align-items: center; gap: 12px">
                {heading("Color")}
                <div style={swatch}></div>
            </div>
            {slider(f, Num::Colour(0), "Red")}
            {slider(f, Num::Colour(1), "Green")}
            {slider(f, Num::Colour(2), "Blue")}
        </div>
    };
    // Video (protocol 28): its own message, not part of the save — so only
    // for a prim that already exists, and only where a clip can be chosen
    // and played.
    let video: View = if !is_new && crate::systems::map::prim_video_available() {
        let (look, acts) = (look.clone(), acts.clone());
        let row = move || -> View {
            let l = look.get();
            let state = if l.playing_video { "Playing" } else { "None" };
            let choose: View = if crate::platform::model_file::is_available() {
                view! { <Button size=Size::Sm onclick={act(&acts, EditAct::ChooseVideo)}>"Choose video…"</Button> }
            } else {
                View::Empty
            };
            let stop: View = if l.playing_video {
                view! { <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::STOP)} onclick={act(&acts, EditAct::StopVideo)}>"Stop"</Button> }
            } else {
                View::Empty
            };
            let said: View = match l.video_note {
                Some(n) => note(n),
                None => View::Empty,
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px">
                    <span style="color: var(--m-text)">{state}</span>
                    {choose}
                    {stop}
                </div>
                {said}
            }
        };
        let hint = cartopolis_core::video_limits::VIDEO_FORMAT_HINT;
        view! {
            <div style="display: flex; flex-direction: column; gap: 8px" title={hint}>
                {heading("Video")}
                {row}
            </div>
        }
    } else {
        View::Empty
    };
    let texture = {
        let (look, acts) = (look.clone(), acts.clone());
        move || -> View {
            let l = look.get();
            let name = l.texture.clone().unwrap_or_else(|| "None".to_owned());
            let pick: View = if l.can_pick {
                view! { <Button size=Size::Sm onclick={act(&acts, EditAct::TogglePicker)}>"Pick…"</Button> }
            } else {
                View::Empty
            };
            let clear: View = if l.texture.is_some() {
                view! { <Button variant=Variant::Ghost size=Size::Sm onclick={act(&acts, EditAct::ClearTexture)}>"Clear"</Button> }
            } else {
                View::Empty
            };
            view! {
                <div style="display: flex; align-items: center; gap: 8px">
                    <span style="flex: 1; min-width: 0; color: var(--m-text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis">{name}</span>
                    {pick}
                    {clear}
                </div>
            }
        }
    };
    let repeats = |key: Num, axis: &'static str, flip: Flag, id: &'static str| -> View {
        let field = number(f, key, axis);
        view! {
            <div style="display: flex; align-items: flex-end; gap: 12px">
                <div style="flex: 1; min-width: 0">{field}</div>
                <div style="flex: none; padding-bottom: 8px"><FieldCheckbox model={f.flag(flip)} label="Flip" id={id} /></div>
            </div>
        }
    };
    let per_metre = number(f, Num::PerMetre, "Repeats per metre");
    view! {
        {colour}
        {video}
        {group("Texture", view! { <div>{texture}</div> })}
        {slider(f, Num::Transparency, "Transparency")}
        {slider(f, Num::Glow, "Glow")}
        <FieldCheckbox model={f.flag(Flag::FullBright)} label="Full bright" id="oe-full-bright" />
        {rule()}
        {group("Repeats per face", view! {
            <div style="display: flex; flex-direction: column; gap: 8px">
                {repeats(Num::RepeatU, "U", Flag::FlipU, "oe-flip-u")}
                {repeats(Num::RepeatV, "V", Flag::FlipV, "oe-flip-v")}
            </div>
        })}
        {slider(f, Num::TexRotation, "Rotation (degrees)")}
        <div style="display: flex; align-items: flex-end; gap: 12px">
            <div style="flex: 1; min-width: 0">{per_metre}</div>
            <div style="flex: none"><Button size=Size::Sm onclick={act(acts, EditAct::ApplyPerMetre)} title="Set the repeats per face from the object's size">"Apply"</Button></div>
        </div>
        {pair(f, "Offset", (Num::OffsetU, "U"), (Num::OffsetV, "V"))}
    }
}

/// Save, Cancel, and Delete for an object that exists: two presses, as a
/// deleted prim has no undo.
fn buttons(acts: &Acts, is_new: bool) -> View {
    let delete: View = if is_new {
        View::Empty
    } else {
        let armed = signal(false);
        let draw = {
            let acts = acts.clone();
            move || -> View {
                if armed.get() {
                    view! { <Button variant=Variant::Danger icon={icon(icons::DELETE)} onclick={act(&acts, EditAct::Delete)}>"Really delete"</Button> }
                } else {
                    let arm = {
                        let armed = armed.clone();
                        Rc::new(move |_: &Event| armed.set(true)) as Handler
                    };
                    view! { <Button variant=Variant::Ghost onclick={Some(arm)}>"Delete"</Button> }
                }
            }
        };
        view! { <div style="margin-left: auto">{draw}</div> }
    };
    view! {
        <div style="display: flex; flex-wrap: wrap; align-items: center; gap: 8px">
            <Button variant=Variant::Primary onclick={act(acts, EditAct::Save)}>"Save"</Button>
            <Button variant=Variant::Ghost onclick={act(acts, EditAct::Cancel)}>"Cancel"</Button>
            {delete}
        </div>
    }
}

/// The texture library: a page of tiles, and the way between pages.
fn picker_view(p: &Picker, acts: &Acts) -> View {
    let frame = {
        let spot = p.spot.clone();
        move || spot.get().style()
    };
    let grid = {
        let (look, acts) = (p.look.clone(), acts.clone());
        move || -> View {
            let l = look.get();
            let tiles: Vec<View> = l.tiles.iter().map(|t| tile(t, &acts)).collect();
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px">{tiles}</div>
            }
        }
    };
    let pager = {
        let (look, acts) = (p.look.clone(), acts.clone());
        move || -> View {
            let l = look.get();
            if l.pages <= 1 {
                return View::Empty;
            }
            let range = l.range.clone();
            view! {
                <div style="display: flex; align-items: center; gap: 8px">
                    <Button variant=Variant::Ghost size=Size::Sm icon_only=true icon={icon(icons::CHEVRON_LEFT)} title="Previous page" disabled={l.page == 0} onclick={Some(on(&acts, Act::Page(-1)))}>"Previous page"</Button>
                    <span style="flex: 1; text-align: center; color: var(--m-text-dim); font-size: 12.5px">{range}</span>
                    <Button variant=Variant::Ghost size=Size::Sm icon_only=true icon={icon(icons::CHEVRON_RIGHT)} title="Next page" disabled={l.page + 1 >= l.pages} onclick={Some(on(&acts, Act::Page(1)))}>"Next page"</Button>
                </div>
            }
        }
    };
    let head = header(
        icons::PALETTE,
        "Texture library".to_owned(),
        on(acts, Act::Edit(EditAct::ClosePicker)),
    );
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {head}
            <div style="flex: 1; min-height: 0; overflow-y: auto; padding: 0 16px 12px">{grid}</div>
            <div style="flex: none; padding: 8px 12px 12px; border-top: 1px solid var(--m-border)">{pager}</div>
        </div>
    }
}

/// One texture: its thumbnail (its name while it is on its way, or on the
/// web), ringed when it is the object's, and its name under it.
fn tile(t: &TileV, acts: &Acts) -> View {
    let pick = on(acts, Act::Edit(EditAct::PickTexture(t.id.clone())));
    let ring = if t.selected {
        "var(--m-accent)"
    } else {
        "var(--m-border)"
    };
    let face = format!(
        "width: {TILE:.0}px; height: {TILE:.0}px; box-sizing: border-box; overflow: hidden; \
         border: 2px solid {ring}; border-radius: var(--m-radius-sm); \
         background: var(--m-surface-sunken); display: flex; align-items: center; \
         justify-content: center"
    );
    let inner: View = if t.ready {
        let side = TILE - 4.0;
        el("img")
            .attr("src", format!("bevy:{THUMB}{}", t.id))
            .attr("alt", t.name.clone())
            .attr(
                "style",
                format!("width: {side:.0}px; height: {side:.0}px; pointer-events: none"),
            )
            .into_view()
    } else {
        let name = t.name.clone();
        view! {
            <span style="padding: 4px; font-size: 12.5px; line-height: 1.2; text-align: center; color: var(--m-text); overflow: hidden; pointer-events: none">{name}</span>
        }
    };
    let name = t.name.clone();
    let title = t.name.clone();
    view! {
        <div style="width: 72px; display: flex; flex-direction: column; gap: 4px; cursor: pointer" title={title} on:click={move |e: &Event| pick(e)}>
            <div style={face}>{inner}</div>
            <span style="font-size: 12.5px; line-height: 14px; color: var(--m-text-dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; pointer-events: none">{name}</span>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every number writes back what it reads, so a value bridged from the
    /// editor is never taken for an edit.
    #[test]
    fn a_number_writes_back_what_it_reads() {
        let d = EditDialogState {
            position: [1.25, -3.5, 12.125],
            scale: [1.0, 2.5, 0.75],
            rotation: [0.0, 90f32.to_radians(), -45f32.to_radians()],
            color: [0.2, 0.4, 1.0],
            path_cut_begin: 0.125,
            hollow: 0.35,
            surface: cartopolis_core::PrimSurface {
                alpha: 0.75,
                ..Default::default()
            },
            ..Default::default()
        };
        for key in BOXES.iter().chain(SLIDERS) {
            let v = key.read(&d);
            let mut again = d.clone();
            key.write(&mut again, v);
            assert_eq!(key.read(&again), v, "{key:?}");
        }
    }

    /// A begin is never written past its end.
    #[test]
    fn a_begin_stays_before_its_end() {
        let mut d = EditDialogState {
            path_cut_end: 0.5,
            ..Default::default()
        };
        Num::CutBegin.write(&mut d, 0.9);
        assert!(d.path_cut_begin <= d.path_cut_end);
        Num::SliceEnd.write(&mut d, 0.4);
        Num::SliceBegin.write(&mut d, 0.8);
        assert!(d.warp.slice_begin <= d.warp.slice_end);
    }

    /// A field follows a value moved elsewhere, and hands back one typed
    /// here once.
    #[test]
    fn a_field_tells_an_edit_from_a_move() {
        let b = Bound::new(1.0_f64);
        assert_eq!(b.sync(2.0), None);
        assert_eq!(b.model.get_untracked(), 2.0);
        b.model.set(3.0);
        assert_eq!(b.sync(2.0), Some(3.0));
        assert_eq!(b.sync(3.0), None);
    }
}
