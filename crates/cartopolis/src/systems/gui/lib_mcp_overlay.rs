//! The editor's log drawn by the component library (`lib_host`): every MCP
//! call this client serves, in a box in the top-right corner as it happens
//! — the tool, what it was asked, what it answered.
//!
//! `mcp::overlay::publish_mcp_activity` decides what shows — nothing under
//! an open panel or in driver mode, a finished line for `LINGER_S`, under
//! the Midas button while the link is up — and publishes it in
//! `McpActivityLib::view`.
//!
//! A readout and nothing else: no buttons, and nothing in it listens, so a
//! tap on it reaches the map.
//!
//! **Placement.** `OSD_MARGIN` in from the right edge and down from the top
//! (plus the Midas button and its gap while the link is up), its content
//! `overlay::WIDTH` wide in an overlay frame.
//!
//! Desktop only (`cfg(mcp)`, and not registered on Android): one px per ui
//! point.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;

use crate::systems::lib_settings::LibScreens;
use crate::systems::mcp::overlay::{ActivityView, McpActivityLib, WIDTH};
use crate::systems::mcp::Outcome;
use crate::systems::theme;

/// The box's name in the library's host.
const KEY: &str = "mcp_activity";

/// The overlay frame across: both paddings (14) and rims (1).
const FRAME_X: f32 = 2.0 * 14.0 + 2.0;

#[derive(Clone)]
struct Live {
    view: Signal<ActivityView>,
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the log. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_mcp_activity(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    state: Res<McpActivityLib>,
) {
    if !lib.mcp_activity {
        return;
    }
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        let Some(view) = state.view.clone() else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        match m.as_mut() {
            None => {
                let live = Live { view: signal(view) };
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    view_of(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                if let Some(path) = std::env::var_os("CARTO_UI_LIB_HTML") {
                    if let Err(e) = std::fs::write(&path, html(&live)) {
                        warn!(?path, error = %e, "lib mcp activity: could not write the HTML");
                    }
                }
                *m = Some(live);
            }
            Some(live) => {
                if live.view.peek(|now| *now != view) {
                    live.view.set(view);
                }
            }
        }
    });
}

/// The HTML from the library's string renderer, for the browser beside a
/// capture.
fn html(l: &Live) -> String {
    use mithril::view::{mount, StringRenderer};
    let r = Rc::new(RefCell::new(StringRenderer::new()));
    let root = r.borrow().root();
    let mounted = mount(view_of(l), &r, &root, None);
    let out = r.borrow().html();
    drop(mounted);
    out
}

/// The root: the whole window, listening nowhere, the box in its corner.
fn view_of(l: &Live) -> View {
    let frame = {
        let view = l.view.clone();
        move || {
            format!(
                "position: absolute; right: {:.0}px; top: {:.0}px; width: {:.0}px; \
                 box-sizing: border-box; padding: 12px 14px; display: flex; \
                 flex-direction: column; gap: 3px; background: var(--m-surface); \
                 border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
                 box-shadow: var(--m-shadow); pointer-events: none",
                theme::OSD_MARGIN,
                theme::OSD_MARGIN + view.get().below,
                WIDTH + FRAME_X,
            )
        }
    };
    let body = {
        let view = l.view.clone();
        move || -> View { log_body(&view.get()) }
    };
    view! {
        <div style="position: absolute; left: 0; top: 0; right: 0; bottom: 0; pointer-events: none">
            <div style={frame}>{body}</div>
        </div>
    }
}

/// The head — the spinner while a call waits, "Editor", where it listens —
/// and a line per call.
fn log_body(v: &ActivityView) -> View {
    let spinner: View = if v.busy {
        view! { <Spinner size=Size::Sm /> }
    } else {
        View::Empty
    };
    let title = format!(
        "flex: none; font-size: {}px; font-weight: 600; color: var(--m-text-strong)",
        theme::SIZE_CHIP
    );
    let listen_style = format!(
        "flex: 1; min-width: 0; font-size: {}px; color: var(--m-text-dim); \
         white-space: nowrap; overflow: hidden; text-overflow: ellipsis",
        theme::SIZE_TINY
    );
    let listen = v.listen.clone();
    let lines: Vec<View> = v
        .lines
        .iter()
        .map(|(label, detail, outcome)| {
            let tone = match outcome {
                Outcome::Working => "var(--m-accent)",
                Outcome::Done => "var(--m-text-dim)",
                Outcome::Failed => "var(--m-danger)",
            };
            let label_style = format!(
                "flex: none; font-family: monospace; font-size: {}px; color: {tone}",
                theme::SIZE_TINY
            );
            let detail_style = format!(
                "flex: 1; min-width: 0; font-size: {}px; color: var(--m-text-dim); \
                 overflow-wrap: anywhere",
                theme::SIZE_TINY
            );
            let label = label.clone();
            let detail = detail.clone();
            view! {
                <div style="display: flex; align-items: flex-start; gap: 8px">
                    <span style={label_style}>{label}</span>
                    <span style={detail_style}>{detail}</span>
                </div>
            }
        })
        .collect();
    let lines = View::Fragment(lines);
    view! {
        <div style="display: flex; align-items: center; gap: 8px">
            {spinner}
            <span style={title}>"Editor"</span>
            <span style={listen_style}>{listen}</span>
        </div>
        {lines}
    }
}
