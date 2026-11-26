//! Two of the dialogs the Chats panel raises, drawn by the component library
//! (`lib_host`) beside egui's: the report form (`chat::render_report_dialog`)
//! and the account-delete confirmation (`communities::confirm_account_delete`).
//!
//! On with `CARTO_UI_LIB=dialogs` (or `1`). egui's system steps aside
//! (`ChatsView::lib_dialogs`) but still decides when each is up:
//!
//! - **The report form** is up whenever `ChatState::report` holds a draft and
//!   a session is there — with the Chats panel shut too, since a report is
//!   raised from names in the overlay and over avatars. What is typed and
//!   chosen goes into the draft each frame; Send report and Cancel are done
//!   by `chat::report_act`, the code egui's buttons call.
//! - **The confirmation** is handed over in `ChatsView::lib_confirm` by
//!   `chats::egui_only`, which runs only while the panel is (egui's window
//!   showed on the same frames). Yes asks the admin pages to delete, as
//!   egui's does.
//!
//! What stays egui's: the community picture's picker has nothing to draw
//! (the platform's own sheet is in front), and the full-size picture viewer
//! keeps its egui texture. Its JPEG could be had (`ChatPictures::jpeg`), but
//! `mithril_bevy`'s `<img>` keeps every source it decodes for as long as the
//! app runs (`images.rs`), so a 1600 px picture per view would be a few MB
//! never given back: thumbnails only, until the cache lets go.
//!
//! **Placement.** As egui's windows: centred on the screen, 380 px wide or
//! the screen less its margins. Only the card can hold the pointer: it hangs
//! in a column of no width (`Placement::style`).
//!
//! **What differs from egui's.** egui's reasons are radio buttons and so are
//! these; the form's title sits in the card with its ✕ rather than in a
//! window's title bar.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use cartopolis_core::ReportReason;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::resources::OnlineSession;
use crate::systems::chat::{
    report_act, report_goes_to, ChatState, ReportAction, ReportDraft, REPORT_NOTE_HINT,
};
use crate::systems::chats::ChatsView;
use crate::systems::communities::Communities;
use crate::systems::lib_settings::LibScreens;
use crate::systems::theme;
use crate::systems::ui_input::UiMetrics;

/// The report form's name in the library's host.
const REPORT_KEY: &str = "report_dialog";

/// The confirmation's.
const CONFIRM_KEY: &str = "delete_confirm";

/// A dialog's widest, px, its padding included.
const WIDTH: f32 = 380.0;

/// Where a dialog stands, px in the library's root: the screen's height, the
/// card's left edge, width and tallest.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Placement {
    height: f32,
    left: f32,
    width: f32,
    max_height: f32,
}

impl Placement {
    /// For a viewport of `w` × `h` egui points. The library's px are egui's
    /// points on the Bevy backend; as DOM they are the page's, `ui_scale`
    /// times larger.
    fn of(w: f32, h: f32, ui_scale: f32) -> Self {
        let k = if cfg!(all(feature = "ui-dom", target_arch = "wasm32")) {
            ui_scale
        } else {
            1.0
        };
        let width = WIDTH.min(w - 2.0 * theme::OSD_MARGIN).max(0.0);
        Placement {
            height: h * k,
            left: ((w - width) / 2.0).max(0.0) * k,
            width: width * k,
            max_height: (h - 2.0 * theme::OSD_MARGIN).max(160.0) * k,
        }
    }

    /// A column the screen's height and no width, at its left edge, that
    /// centres the card top to bottom. Having no width it holds no pointer
    /// under either backend: a box over the screen would block the other
    /// screens' taps in `bevy_ui`'s picking.
    fn style(&self) -> String {
        format!(
            "position: absolute; left: 0px; top: 0px; width: 0px; height: {:.0}px; \
             display: flex; flex-direction: column; justify-content: center; \
             align-items: flex-start",
            self.height
        )
    }

    /// The card, centred across by its margin: the library's card surface.
    fn card(&self) -> String {
        format!(
            "flex: none; margin-left: {:.0}px; width: {:.0}px; max-height: {:.0}px; \
             overflow-y: auto; display: flex; flex-direction: column; gap: 16px; padding: 20px; \
             background: var(--m-surface); border: 1px solid var(--m-border); \
             border-radius: var(--m-radius-lg); box-shadow: var(--m-shadow-lg)",
            self.left, self.width, self.max_height
        )
    }
}

/// Write a signal only when its value moves.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.get_untracked() != v {
        s.set(v);
    }
}

/// A dialog's title.
fn title(text: &'static str) -> View {
    view! {
        <h1 style="flex: 1; min-width: 0; margin: 0; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{text}</h1>
    }
}

/// A quiet line.
fn note(text: String) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: 12.5px">{text}</p>
    }
}

/// The dialog's frame: the box over the screen, the card in it. The card's
/// own node listens (a click that does nothing), so the pointer over its
/// padding is the library's too.
fn frame(placement: &Signal<Placement>, body: View) -> View {
    let outer = {
        let placement = placement.clone();
        move || placement.get().style()
    };
    let inner = {
        let placement = placement.clone();
        move || placement.get().card()
    };
    view! {
        <div style={outer}>
            <div style={inner} on:click={|_: &Event| {}}>{body}</div>
        </div>
    }
}

/// The library's string renderer's HTML of a view, for the browser beside a
/// capture.
fn write_html(view: View, what: &str) {
    let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") else {
        return;
    };
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view, &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    if let Err(e) = std::fs::write(&path, out) {
        warn!(?path, error = %e, what, "lib dialogs: could not write the HTML");
    }
}

// ─── The report form ─────────────────────────────────────────────────────────

/// Who and what a draft is about: another subject mounts the form again.
#[derive(Clone, PartialEq)]
struct Subject {
    username: String,
    message: Option<String>,
    asset: Option<String>,
    community: Option<(uuid::Uuid, Vec<u8>, u64)>,
}

impl Subject {
    fn of(d: &ReportDraft) -> Self {
        Subject {
            username: d.username.clone(),
            message: d.message.clone(),
            asset: d.asset.clone(),
            community: d.community.clone(),
        }
    }
}

/// The reason's place in `ReportReason::ALL`, the radio's value.
fn reason_index(r: ReportReason) -> usize {
    ReportReason::ALL.iter().position(|x| *x == r).unwrap_or(0)
}

#[derive(Clone)]
struct ReportLive {
    subject: Subject,
    /// The reason, by its place in `ReportReason::ALL`.
    reason: Signal<usize>,
    note: Signal<String>,
    /// The reason and note as last agreed with the draft.
    last: Rc<RefCell<(usize, String)>>,
    /// Where it goes, said under the form.
    goes_to: &'static str,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<ReportAction>>>,
}

impl ReportLive {
    fn act(&self, a: ReportAction) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

// ─── The confirmation ────────────────────────────────────────────────────────

/// What a tap on the confirmation asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmAct {
    Delete,
    Cancel,
}

#[derive(Clone)]
struct ConfirmLive {
    /// The account it asks about: another mounts it again.
    who: String,
    placement: Signal<Placement>,
    acts: Rc<RefCell<Vec<ConfirmAct>>>,
}

impl ConfirmLive {
    fn act(&self, a: ConfirmAct) -> Option<Handler> {
        let acts = self.acts.clone();
        Some(Rc::new(move |_: &Event| acts.borrow_mut().push(a)) as Handler)
    }
}

thread_local! {
    static REPORT: RefCell<Option<ReportLive>> = const { RefCell::new(None) };
    static CONFIRM: RefCell<Option<ConfirmLive>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the report form and the account-delete
/// confirmation. `PostUpdate`, before `DriveUi`, as every library screen.
pub fn drive_lib_dialogs(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut chat: ResMut<ChatState>,
    mut view: ResMut<ChatsView>,
    mut communities: ResMut<Communities>,
    online: Option<Res<OnlineSession>>,
    metrics: Res<UiMetrics>,
) {
    if !lib.dialogs {
        return;
    }
    if !view.lib_dialogs {
        view.lib_dialogs = true;
    }
    let size = metrics.viewport_pts();
    let placement = Placement::of(size.x, size.y, metrics.ui_scale());
    drive_report(&mut chat, online.as_deref(), placement);
    let confirm = view.lib_confirm.take();
    drive_confirm(&mut communities, confirm, placement);
}

fn drive_report(chat: &mut ChatState, online: Option<&OnlineSession>, placement: Placement) {
    REPORT.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(live) = m.as_ref() {
            // What was chosen and typed → the draft, before a tap sends it.
            let typed = (live.reason.get_untracked(), live.note.get_untracked());
            if typed != *live.last.borrow() {
                if let Some(d) = chat.report.as_mut() {
                    d.reason = ReportReason::ALL.get(typed.0).copied().unwrap_or(d.reason);
                    d.note = typed.1.clone();
                }
                *live.last.borrow_mut() = typed;
            }
            // The taps of the frame, done as egui's buttons do them.
            let acts: Vec<ReportAction> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                match online {
                    Some(online) => report_act(chat, online, a),
                    // The session went: nothing to send it with, as egui's
                    // form would no longer be up to send.
                    None => chat.report = None,
                }
            }
        }
        // As egui's: up while there is a draft and a session to send it.
        let draft = chat.report.as_ref().filter(|_| online.is_some());
        let stale = m
            .as_ref()
            .is_some_and(|live| draft.is_none_or(|d| Subject::of(d) != live.subject));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(REPORT_KEY);
        }
        let Some(draft) = draft else {
            return;
        };
        match m.as_mut() {
            None => {
                let typed = (reason_index(draft.reason), draft.note.clone());
                let live = ReportLive {
                    subject: Subject::of(draft),
                    reason: signal(typed.0),
                    note: signal(typed.1.clone()),
                    last: Rc::new(RefCell::new(typed)),
                    goes_to: report_goes_to(draft),
                    placement: signal(placement),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    REPORT_KEY,
                    report_view(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                write_html(report_view(&live), "report");
                *m = Some(live);
            }
            Some(live) => {
                // The draft moved elsewhere → the view.
                let held = (reason_index(draft.reason), draft.note.clone());
                if held != *live.last.borrow() {
                    put(&live.reason, held.0);
                    put(&live.note, held.1.clone());
                    *live.last.borrow_mut() = held;
                }
                put(&live.placement, placement);
            }
        }
    });
}

/// The form: who, the line it is about, why, anything else, where it goes,
/// Send report and Cancel.
fn report_view(l: &ReportLive) -> View {
    let who = l.subject.username.clone();
    let line: View = match &l.subject.message {
        Some(message) => note(format!("“{message}”")),
        None => View::Empty,
    };
    let reasons: Vec<Opt<usize>> = ReportReason::ALL
        .iter()
        .enumerate()
        .map(|(i, r)| Opt::new(i, r.label()))
        .collect();
    let body = view! {
        <div style="display: flex; align-items: center; gap: 12px">
            {title("Report a person")}
            <CloseButton onclick={l.act(ReportAction::Cancel)} />
        </div>
        <div style="display: flex; flex-direction: column; gap: 4px">
            <span style="font-size: 14px; font-weight: 600; color: var(--m-text-strong)">{who}</span>
            {line}
        </div>
        <div style="display: flex; flex-direction: column; gap: 8px">
            <span style="font-size: 14px; color: var(--m-text)">"What happened?"</span>
            <FieldRadio model={l.reason.clone()} options={reasons} id="report-reason" />
        </div>
        <FieldTextarea model={l.note.clone()} placeholder={REPORT_NOTE_HINT} rows={3u32} id="report-note" />
        {note(l.goes_to.to_string())}
        <div style="display: flex; flex-wrap: wrap; gap: 8px">
            <Button variant=Variant::Primary onclick={l.act(ReportAction::Send)}>"Send report"</Button>
            <Button onclick={l.act(ReportAction::Cancel)}>"Cancel"</Button>
        </div>
    };
    frame(&l.placement, body)
}

fn drive_confirm(cs: &mut Communities, who: Option<String>, placement: Placement) {
    CONFIRM.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(live) = m.as_ref() {
            // The tap of the frame, done as egui's buttons do it.
            let acts: Vec<ConfirmAct> = std::mem::take(&mut *live.acts.borrow_mut());
            if let Some(a) = acts.first() {
                // Only while it is still asked about this account.
                if cs.confirm_delete.as_deref() == Some(live.who.as_str()) {
                    if *a == ConfirmAct::Delete {
                        cs.admin_act(live.who.clone(), true);
                    }
                    cs.confirm_delete = None;
                }
            }
        }
        // Up on the frames egui's window would have been, and not after a
        // tap answered it.
        let who = who.filter(|w| cs.confirm_delete.as_deref() == Some(w.as_str()));
        let stale = m
            .as_ref()
            .is_some_and(|live| who.as_deref() != Some(live.who.as_str()));
        if stale {
            *m = None;
            crate::systems::lib_host::clear_keyed(CONFIRM_KEY);
        }
        let Some(who) = who else {
            return;
        };
        match m.as_mut() {
            None => {
                let live = ConfirmLive {
                    who,
                    placement: signal(placement),
                    acts: Rc::new(RefCell::new(Vec::new())),
                };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    CONFIRM_KEY,
                    confirm_view(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                write_html(confirm_view(&live), "confirm");
                *m = Some(live);
            }
            Some(live) => put(&live.placement, placement),
        }
    });
}

/// The question, Delete the account and Cancel.
fn confirm_view(l: &ConfirmLive) -> View {
    let question = format!(
        "Delete {}'s account? Their devices, memberships and contacts go, \
         and it cannot be undone.",
        l.who
    );
    let body = view! {
        <div style="display: flex; align-items: center; gap: 12px">{title("Delete account")}</div>
        <p style="margin: 0; font-size: 14px; color: var(--m-text-strong)">{question}</p>
        <div style="display: flex; flex-wrap: wrap; gap: 8px">
            <Button variant=Variant::Danger onclick={l.act(ConfirmAct::Delete)}>"Delete the account"</Button>
            <Button onclick={l.act(ConfirmAct::Cancel)}>"Cancel"</Button>
        </div>
    };
    frame(&l.placement, body)
}
