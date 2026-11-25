//! The contacts screens on the component library, inside the Chats panel's
//! shell (`lib_chats`): the requests tray, one request, the contacts list,
//! Add a contact (your link, its QR code, a username looked up) and the
//! privacy setting — every screen egui's `contacts::screen` routes on
//! `contacts::Sub` (`docs/notes/contacts.md`).
//!
//! **One set of logic, two editions.** A tap fills the same
//! `contacts::Act` egui's drawing fills and ends in the same
//! `contacts::act`, so accepting, declining, blocking, withdrawing and the
//! settings mean the same in either; the rows' words are `contacts`'
//! (`request_line`, `contact_line`, `found_line`, `person_facts`' pieces).
//! What egui's drawing did besides drawing — keeping the nearby list fresh
//! while the contacts list shows (`Contacts::want_nearby`) — the mirror
//! does.
//!
//! **What differs from egui's.** egui's right-click menu on a contact
//! (Message, Remove contact) is a ⋯ at the row's end that opens the two
//! under it. Block on a request asks to be pressed again, as the person
//! menu's does. The QR code is an `<img>` from a PNG data URL, made once
//! per link. A declined or a waiting row's words are the same; a person's
//! picture is their disc, as everywhere on the library.

use std::cell::RefCell;
use std::rc::Rc;

use cartopolis_core::{ContactRequestDto, NearbyPersonDto, Relation, RequestPolicy};
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::chats::{self, Actions, ChatsScreen};
use crate::systems::contacts::{self, Act, Contacts, Sub};
use crate::systems::discs::ChatKind;
use crate::systems::lib_chats::{
    back, body_style, disc, group, header, icon, note, ChatsLive, ChatsWorld, Typed,
};
use crate::systems::{icons, theme};

// ─── What is drawn ───────────────────────────────────────────────────────────

/// One person's row: their disc, name and line.
#[derive(Clone, PartialEq)]
struct PersonV {
    name: String,
    line: String,
    /// The green dot on the disc.
    online: bool,
}

#[derive(Clone, PartialEq, Default)]
struct RequestsV {
    rest: Vec<PersonV>,
    /// From accounts younger than a week, when they wait apart.
    new: Vec<PersonV>,
}

/// One of the small facts under a request's name.
#[derive(Clone, PartialEq)]
struct FactV {
    pin: bool,
    text: String,
    new: bool,
}

/// What a request's sender wrote: a line, or a point held back.
#[derive(Clone, PartialEq)]
enum SaidV {
    Line(String),
    Point,
}

#[derive(Clone, PartialEq)]
struct RequestV {
    name: String,
    facts: Vec<FactV>,
    said: Vec<SaidV>,
    /// The last line, for a report.
    last: Option<String>,
}

/// The requests row over the contacts list (`chats::requests_summary`).
#[derive(Clone, PartialEq)]
struct TrayV {
    title: String,
    sub: String,
    first: String,
    count: usize,
}

#[derive(Clone, PartialEq, Default)]
struct ListV {
    error: Option<String>,
    /// No contacts and no requests sent.
    empty: bool,
    tray: Option<TrayV>,
    online: Vec<PersonV>,
    all: Vec<PersonV>,
    sent: Vec<PersonV>,
    near: Vec<PersonV>,
}

/// What looking a username up found.
#[derive(Clone, PartialEq, Default)]
enum FoundV {
    #[default]
    Nothing,
    Looking,
    Nobody,
    /// Somebody, and whether a request can go to them.
    Person(PersonV, bool),
}

#[derive(Clone, PartialEq, Default)]
struct AddV {
    me: String,
    link: String,
    /// The link as a QR code: a PNG data URL, empty when it could not be made.
    qr: String,
    found: FoundV,
    error: Option<String>,
}

// ─── The live state ──────────────────────────────────────────────────────────

/// The contacts screens' own signals, kept in `ChatsLive::contacts`.
#[derive(Clone)]
pub(crate) struct ContactsLive {
    requests: Signal<RequestsV>,
    request: Signal<Option<RequestV>>,
    list: Signal<ListV>,
    add: Signal<AddV>,
    /// Add's username field, bridged to `Contacts::search`.
    search: Typed,
    /// Nothing typed: Find cannot be pressed.
    find_off: Signal<bool>,
    policy: Signal<RequestPolicy>,
    separate: Signal<bool>,
    notify: Signal<bool>,
    show_online: Signal<bool>,
    /// The contact whose Message / Remove row is open.
    open_row: Signal<Option<String>>,
    /// A confirmation pressed once, by its key: the second press does it.
    armed: Signal<Option<String>>,
    /// The last link drawn as a QR code, and its data URL.
    qr: Rc<RefCell<(String, String)>>,
}

impl ContactsLive {
    /// Made when the panel mounts, from the world as it is then.
    pub(crate) fn new(w: &ChatsWorld) -> Self {
        let s = &w.contacts.settings;
        ContactsLive {
            requests: signal(RequestsV::default()),
            request: signal(None),
            list: signal(ListV::default()),
            add: signal(AddV::default()),
            search: Typed::new(&w.contacts.search),
            find_off: signal(w.contacts.search.trim().is_empty()),
            policy: signal(s.policy),
            separate: signal(s.separate_new_accounts),
            notify: signal(s.notify_requests),
            show_online: signal(s.show_online),
            open_row: signal(None),
            armed: signal(None),
            qr: Rc::new(RefCell::new((String::new(), String::new()))),
        }
    }
}

/// Write `value` into `s` only when it differs: a write re-runs every view
/// that reads it. Answers whether it did.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, value: T) -> bool {
    let moved = s.get_untracked() != value;
    if moved {
        s.set(value);
    }
    moved
}

// ─── The world → the signals ─────────────────────────────────────────────────

/// The world → [`ContactsLive`], every frame the panel is mounted. Answers
/// whether what shows moved, for the HTML beside a capture.
pub(crate) fn mirror(live: &ChatsLive, w: &mut ChatsWorld, now: u64) -> bool {
    let l = &live.contacts;
    let ChatsScreen::Contacts(sub) = w.screen() else {
        put(&l.open_row, None);
        put(&l.armed, None);
        return false;
    };
    sync_search(l, w);
    put(&l.find_off, w.contacts.search.trim().is_empty());
    let s = w.contacts.settings.clone();
    put(&l.policy, s.policy);
    put(&l.separate, s.separate_new_accounts);
    put(&l.notify, s.notify_requests);
    put(&l.show_online, s.show_online);
    match &sub {
        Sub::Requests => put(&l.requests, requests_v(&w.contacts, now)),
        Sub::Request(name) => put(&l.request, request_v(&w.contacts, &w.chat, name, now)),
        Sub::Contacts => {
            // As egui's list does while it is drawn: ask who is near.
            w.contacts.want_nearby = true;
            put(&l.list, list_v(&w.contacts, now))
        }
        Sub::Add => {
            let (me, _) = w.me();
            let v = add_v(l, &w.contacts, &me);
            put(&l.add, v)
        }
        Sub::Privacy => false,
    }
}

/// Add's field → `Contacts::search` as it is typed, and back when the world
/// moved it (a pasted link opens a card and clears it).
fn sync_search(l: &ContactsLive, w: &mut ChatsWorld) {
    if let Some(v) = l.search.typed(&w.contacts.search) {
        w.contacts.search = v;
    }
    l.search.follow(&w.contacts.search);
}

fn person(name: &str, line: String, online: bool) -> PersonV {
    PersonV {
        name: name.to_string(),
        line,
        online,
    }
}

fn requests_v(c: &Contacts, now: u64) -> RequestsV {
    let (new, rest): (Vec<&ContactRequestDto>, Vec<&ContactRequestDto>) = c
        .incoming
        .iter()
        .partition(|r| c.settings.separate_new_accounts && r.new_account);
    let rows = |list: Vec<&ContactRequestDto>| -> Vec<PersonV> {
        list.into_iter()
            .map(|r| person(&r.username, contacts::request_line(r, now), false))
            .collect()
    };
    RequestsV {
        rest: rows(rest),
        new: rows(new),
    }
}

fn request_v(
    c: &Contacts,
    chat: &crate::systems::chat::ChatState,
    name: &str,
    now: u64,
) -> Option<RequestV> {
    let r = c.incoming_from(name)?;
    let mut facts = Vec::new();
    if let Some(m) = &r.met {
        facts.push(FactV {
            pin: true,
            text: contacts::met_text(m, now),
            new: false,
        });
    }
    facts.push(FactV {
        pin: false,
        text: contacts::joined(r.joined_ms, r.new_account, now),
        new: r.new_account,
    });
    facts.push(FactV {
        pin: false,
        text: contacts::in_common_text(&r.in_common),
        new: false,
    });
    let lines = contacts::request_lines(chat, &r.username);
    // A point is held back until you accept: from a stranger, a place and a
    // camera pose is exactly the kind of thing not to open unasked.
    let said = lines
        .iter()
        .map(|l| {
            if l.point.is_some() {
                SaidV::Point
            } else {
                SaidV::Line(l.body.clone())
            }
        })
        .collect();
    Some(RequestV {
        name: r.username.clone(),
        facts,
        said,
        last: lines.last().map(|l| l.body.clone()),
    })
}

fn list_v(c: &Contacts, now: u64) -> ListV {
    let near = c
        .nearby
        .iter()
        .filter(|p| p.relation == Relation::Nobody && p.may_request)
        .map(|p: &NearbyPersonDto| person(&p.username, format!("{:.0} m", p.distance_m), false))
        .collect();
    ListV {
        error: c.error.clone(),
        empty: c.contacts.is_empty() && c.outgoing.is_empty(),
        tray: chats::requests_summary(c).map(|(title, sub, people)| TrayV {
            title,
            sub,
            first: people.first().cloned().unwrap_or_default(),
            count: c.incoming.len(),
        }),
        online: c
            .contacts
            .iter()
            .filter(|x| x.online == Some(true))
            .map(|x| person(&x.username, contacts::online_line(x), true))
            .collect(),
        all: c
            .contacts
            .iter()
            .map(|x| {
                person(
                    &x.username,
                    contacts::contact_line(x, now),
                    x.online == Some(true),
                )
            })
            .collect(),
        sent: c
            .outgoing
            .iter()
            .map(|r| {
                person(
                    &r.username,
                    format!("waiting · sent {}", contacts::ago(r.sent_at_ms, now)),
                    false,
                )
            })
            .collect(),
        near,
    }
}

fn add_v(l: &ContactsLive, c: &Contacts, me: &str) -> AddV {
    let link = c.my_link(me);
    let qr = {
        let mut cache = l.qr.borrow_mut();
        if cache.0 != link {
            *cache = (link.clone(), qr_url(&link).unwrap_or_default());
        }
        cache.1.clone()
    };
    let found = match &c.found {
        None if c.searching() => FoundV::Looking,
        None => FoundV::Nothing,
        Some(None) => FoundV::Nobody,
        Some(Some(p)) => FoundV::Person(
            person(&p.username, contacts::found_line(p).to_string(), false),
            p.relation == Relation::Nobody && p.may_request,
        ),
    };
    AddV {
        me: me.to_string(),
        link,
        qr,
        found,
        error: c.error.clone(),
    }
}

// ─── The QR code ─────────────────────────────────────────────────────────────

/// Pixels per module of the QR picture: about twice what it is drawn at,
/// so the scaling down keeps its edges.
const QR_SCALE: i32 = 8;

/// `text` as a QR code, dark on white with the standard's four modules of
/// quiet zone, as a PNG data URL (`<img>`, `mithril_bevy::images`).
fn qr_url(text: &str) -> Option<String> {
    let code = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let quiet = 4;
    let n = code.size();
    let side = u32::try_from((n + 2 * quiet) * QR_SCALE).ok()?;
    let ink = theme::DARK.ink;
    // A QR code is read dark on white whatever the theme: white is part of
    // the code, not a colour of the chrome.
    let picture = image::RgbImage::from_fn(side, side, |x, y| {
        let mx = x as i32 / QR_SCALE - quiet;
        let my = y as i32 / QR_SCALE - quiet;
        if mx >= 0 && my >= 0 && mx < n && my < n && code.get_module(mx, my) {
            image::Rgb([ink.r(), ink.g(), ink.b()])
        } else {
            image::Rgb([255, 255, 255])
        }
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(picture)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(format!("data:image/png;base64,{}", base64(&png)))
}

/// Standard base64 with padding. The `base64` crate is not in the web
/// build, which draws this screen too.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ABC[(n >> 18) as usize & 63] as char);
        out.push(ABC[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ABC[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// ─── Taps ────────────────────────────────────────────────────────────────────

/// A contacts tap, done as egui's are (`contacts::act`).
fn run(w: &mut ChatsWorld<'_>, sub: &Sub, act: Act) {
    let (me, _) = w.me();
    contacts::act(
        sub,
        act,
        &mut w.contacts,
        &mut w.chat,
        &mut w.view,
        w.online.as_deref(),
        &me,
    );
}

/// A component's handler doing the `Act` `make` builds.
fn tap(l: &ChatsLive, sub: &Sub, make: impl Fn() -> Act + 'static) -> Option<Handler> {
    let sub = sub.clone();
    l.on(move |w, _| run(w, &sub, make()))
}

/// The same, for a raw element's `on:click`.
fn click(l: &ChatsLive, sub: &Sub, make: impl Fn() -> Act + 'static) -> impl Fn(&Event) + 'static {
    let sub = sub.clone();
    l.tap(move |w, _| run(w, &sub, make()))
}

/// A setting changed: written, and sent to the server.
fn setting(
    l: &ChatsLive,
    write: impl Fn(&mut cartopolis_core::ContactSettings, bool) + 'static,
) -> Option<Rc<dyn Fn(&bool)>> {
    let l = l.clone();
    let write = Rc::new(write);
    Some(Rc::new(move |v: &bool| {
        let (v, write) = (*v, write.clone());
        let go = l.later(move |w, _| {
            write(&mut w.contacts.settings, v);
            run(
                w,
                &Sub::Privacy,
                Act {
                    settings: true,
                    ..Act::default()
                },
            );
        });
        go();
    }))
}

// ─── Pieces ──────────────────────────────────────────────────────────────────

/// The ✕ at a header's end: back to the map, as every page title carries.
fn close(l: &ChatsLive) -> View {
    view! {
        <Button variant=Variant::Ghost icon={icon(icons::CLEAR)} icon_only=true onclick={l.on(|_, a| a.close = true)} title="Close: back to the map" />
    }
}

/// A sub-page's header: back, the title, what `end` adds, and ✕.
fn head(l: &ChatsLive, sub: &Sub, title: &'static str, end: View) -> View {
    let lead = back(
        tap(l, sub, || Act {
            back: true,
            ..Act::default()
        }),
        "Back",
    );
    header(lead, title.into_view(), view! { {end}{close(l)} })
}

/// A group's heading, with how many are in it at its end.
fn section(title: &'static str, count: Option<usize>) -> View {
    let count: View = match count {
        Some(n) => {
            view! { <span style="padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{n.to_string()}</span> }
        }
        None => View::Empty,
    };
    view! {
        <div style="display: flex; align-items: flex-end; gap: 8px">
            <div style="flex: 1; min-width: 0">{group(title)}</div>
            {count}
        </div>
    }
}

/// Nothing here, and what would be.
fn empty(title: &'static str, text: &'static str) -> View {
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 28px 16px; text-align: center">
            <span style="color: var(--m-text-strong); font-weight: 600">{title}</span>
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{text}</span>
        </div>
    }
}

/// A person's disc, with the green dot when they are online.
fn face(p: &PersonV, size: u32) -> View {
    let dot: View = if p.online {
        view! {
            <span style="position: absolute; right: -1px; bottom: -1px; width: 14px; height: 14px; border-radius: var(--m-radius-full); background: var(--m-success); border: 3px solid var(--m-surface)"></span>
        }
    } else {
        View::Empty
    };
    view! {
        <span style="position: relative; flex: none; display: flex">
            {disc(&p.name, ChatKind::Private, size)}
            {dot}
        </span>
    }
}

/// The name over its line.
fn words(p: &PersonV, line_style: &'static str) -> View {
    let (name, line) = (p.name.clone(), p.line.clone());
    view! {
        <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
            <span style="color: var(--m-text-strong); font-weight: 600; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{name}</span>
            <span style={line_style}>{line}</span>
        </span>
    }
}

const LINE: &str = "color: var(--m-text-dim); font-size: var(--m-font-size-sm); white-space: nowrap; text-overflow: ellipsis; overflow: hidden";
const LINE_FAINT: &str = "color: var(--m-text-faint); font-size: var(--m-font-size-sm); white-space: nowrap; text-overflow: ellipsis; overflow: hidden";
const ROW: &str = "display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)";
const ROW_MAIN: &str = "flex: 1; min-width: 0; display: flex; align-items: center; gap: 12px; margin: 0; padding: 6px 0; border: 0; background: none; color: var(--m-text); font: inherit; text-align: start; cursor: pointer";

/// A person's row: a tap on its main part does `open`, and `end` holds its
/// own controls (a `ListItem` cannot hold a button without nesting them).
fn person_row(p: &PersonV, open: Option<Box<dyn Fn(&Event)>>, end: View) -> View {
    let main: View = match open {
        Some(open) => {
            let title = p.name.clone();
            view! {
                <button title={title} style={ROW_MAIN} on:click={move |e: &Event| open(e)}>
                    {face(p, 44)}
                    {words(p, LINE)}
                </button>
            }
        }
        None => view! {
            <div style="flex: 1; min-width: 0; display: flex; align-items: center; gap: 12px; padding: 6px 0">
                {face(p, 44)}
                {words(p, LINE_FAINT)}
            </div>
        },
    };
    view! { <div style={ROW}>{main}{end}</div> }
}

/// A button that asks to be pressed again before it does `go`: egui's
/// `theme::confirm_button`. `key` tells the confirmations on a screen apart.
fn confirm(
    l: &ChatsLive,
    key: String,
    label: View,
    again: &'static str,
    go: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Actions) + 'static,
) -> View {
    let armed = l.contacts.armed.clone();
    let go = l.later(go);
    let press: Handler = {
        let (armed, key) = (armed.clone(), key.clone());
        Rc::new(move |_: &Event| {
            if armed.get_untracked().as_deref() == Some(key.as_str()) {
                armed.set(None);
                go();
            } else {
                armed.set(Some(key.clone()));
            }
        })
    };
    (move || -> View {
        if armed.get().as_deref() == Some(key.as_str()) {
            view! { <Button variant=Variant::Danger size=Size::Sm onclick={Some(press.clone())}>{again}</Button> }
        } else {
            view! { <Button size=Size::Sm onclick={Some(press.clone())}>{label.clone()}</Button> }
        }
    })
    .into_view()
}

/// A small fact, as a pill: where you met, when they joined.
fn fact(f: &FactV) -> View {
    let style = if f.new {
        "display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: var(--m-radius-full); background: var(--m-warning-soft); color: var(--m-warning); font-size: var(--m-font-size-sm)"
    } else {
        "display: inline-flex; align-items: center; gap: 6px; padding: 5px 12px; border-radius: var(--m-radius-full); background: var(--m-surface-raised); color: var(--m-text-dim); font-size: var(--m-font-size-sm)"
    };
    let pin: View = if f.pin {
        view! { <span style="font-family: 'Material Symbols Rounded'; font-size: 16px; line-height: 1">{icons::PIN}</span> }
    } else {
        View::Empty
    };
    let text = f.text.clone();
    view! { <span style={style}>{pin}<span>{text}</span></span> }
}

// ─── The router ──────────────────────────────────────────────────────────────

/// Whichever contacts screen `sub` names.
pub(crate) fn screen(live: &ChatsLive, sub: &Sub) -> View {
    match sub {
        Sub::Requests => requests_page(live, sub),
        Sub::Request(_) => request_page(live, sub),
        Sub::Contacts => contacts_page(live, sub),
        Sub::Add => add_page(live, sub),
        Sub::Privacy => privacy_page(live, sub),
    }
}

// ─── Message requests ────────────────────────────────────────────────────────

fn requests_page(l: &ChatsLive, sub: &Sub) -> View {
    let rows = {
        let (l, sub) = (l.clone(), sub.clone());
        move || -> View {
            let v = l.contacts.requests.get();
            if v.rest.is_empty() && v.new.is_empty() {
                return empty("No requests", "When somebody new says hi, it waits here.");
            }
            let mut out = Vec::new();
            for (title, list) in [("Requests", &v.rest), ("From new accounts", &v.new)] {
                if list.is_empty() {
                    continue;
                }
                out.push(section(title, Some(list.len())));
                let rows: Vec<View> = list
                    .iter()
                    .map(|p| {
                        let name = p.name.clone();
                        let open = click(&l, &sub, move || Act {
                            to: Some(Sub::Request(name.clone())),
                            ..Act::default()
                        });
                        let chevron = view! { <span style="color: var(--m-text-dim)">{icon(icons::CHEVRON_RIGHT)}</span> };
                        person_row(p, Some(Box::new(open)), chevron)
                    })
                    .collect();
                out.push(view! { <div style="display: flex; flex-direction: column; gap: 8px">{rows}</div> });
            }
            View::Fragment(out)
        }
    };
    let body = body_style(l);
    view! {
        {head(l, sub, "Message requests", View::Empty)}
        <div style={body}>
            {note("People who aren't contacts yet. They can't tell whether you've read a request.")}
            {rows}
        </div>
    }
}

// ─── One request ─────────────────────────────────────────────────────────────

fn request_page(l: &ChatsLive, sub: &Sub) -> View {
    let content = {
        let (l, sub) = (l.clone(), sub.clone());
        move || -> View {
            match l.contacts.request.get() {
                Some(r) => request_body(&l, &sub, &r),
                None => empty("No request", "It was answered, or withdrawn."),
            }
        }
    };
    let body = body_style(l);
    view! {
        {head(l, sub, "Message request", View::Empty)}
        <div style={body}>{content}</div>
    }
}

fn request_body(l: &ChatsLive, sub: &Sub, r: &RequestV) -> View {
    let facts: Vec<View> = r.facts.iter().map(fact).collect();
    let said: View = if r.said.is_empty() {
        note("No message: they only asked to be contacts.")
    } else {
        let lines: Vec<View> = r
            .said
            .iter()
            .map(|s| match s {
                SaidV::Point => view! {
                    <div style="display: flex; align-items: center; gap: 8px; padding: 10px 14px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface); color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                        {icon(icons::PIN)}<span>"A point is held back until you accept"</span>
                    </div>
                },
                SaidV::Line(text) => {
                    let text = text.clone();
                    view! {
                        <div style="align-self: flex-start; max-width: 80%; padding: 10px 14px; border-radius: 18px 18px 18px 6px; background: var(--m-surface-raised); color: var(--m-text); white-space: pre-wrap">{text}</div>
                    }
                }
            })
            .collect();
        view! { <div style="display: flex; flex-direction: column; gap: 8px">{lines}</div> }
    };
    let name = r.name.clone();
    let act = |make: fn(String) -> Act| {
        let name = r.name.clone();
        tap(l, sub, move || make(name.clone()))
    };
    let decline = act(|n| Act {
        decline: Some(n),
        ..Act::default()
    });
    let accept = act(|n| Act {
        accept: Some(n),
        ..Act::default()
    });
    let report = {
        let (name, last) = (r.name.clone(), r.last.clone());
        tap(l, sub, move || Act {
            report: Some((name.clone(), last.clone())),
            ..Act::default()
        })
    };
    let block = {
        let (name, sub) = (r.name.clone(), sub.clone());
        confirm(
            l,
            format!("block:{name}"),
            "Block".into_view(),
            "Block them?",
            move |w, _| {
                run(
                    w,
                    &sub,
                    Act {
                        block: Some(name.clone()),
                        ..Act::default()
                    },
                )
            },
        )
    };
    let unseen = format!("{name} won't know you've seen this unless you accept.");
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 12px; padding-top: 4px; text-align: center">
            {disc(&r.name, ChatKind::Private, 72)}
            <h2 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{name}</h2>
            <div style="display: flex; flex-wrap: wrap; justify-content: center; gap: 8px">{facts}</div>
        </div>
        <div style="height: 1px; background: var(--m-border)"></div>
        {said}
        <p style="margin: 4px 0 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); text-align: center">{unseen}</p>
        <div style="display: flex; gap: 12px">
            <div style="flex: 1; display: flex; flex-direction: column">
                <Button icon={icon(icons::CLEAR)} onclick={decline}>"Decline"</Button>
            </div>
            <div style="flex: 1; display: flex; flex-direction: column">
                <Button variant=Variant::Primary icon={icon(icons::CHECK)} onclick={accept}>"Accept"</Button>
            </div>
        </div>
        <div style="display: flex; justify-content: center; gap: 8px">
            {block}
            <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::FLAG)} onclick={report}>"Report"</Button>
        </div>
    }
}

// ─── Contacts ────────────────────────────────────────────────────────────────

fn contacts_page(l: &ChatsLive, sub: &Sub) -> View {
    let to = |page: Sub| {
        tap(l, sub, move || Act {
            to: Some(page.clone()),
            ..Act::default()
        })
    };
    let end = view! {
        <Button variant=Variant::Ghost icon={icon(icons::SETTINGS)} icon_only=true onclick={to(Sub::Privacy)} title="Who can reach you" />
        <Button variant=Variant::Ghost icon={icon(icons::ADD)} icon_only=true onclick={to(Sub::Add)} title="Add a contact" />
    };
    let content = {
        let (l, sub) = (l.clone(), sub.clone());
        move || -> View { contacts_body(&l, &sub, &l.contacts.list.get()) }
    };
    let body = body_style(l);
    view! {
        {head(l, sub, "Contacts", end)}
        <div style={body}>{content}</div>
    }
}

fn contacts_body(l: &ChatsLive, sub: &Sub, v: &ListV) -> View {
    let error: View = match &v.error {
        Some(e) => {
            let dismiss = Some(l.later(|w, _| w.contacts.error = None));
            let e = e.clone();
            view! { <Alert tone=Tone::Danger ondismiss={dismiss}>{e}</Alert> }
        }
        None => View::Empty,
    };
    let empty_state: View = if v.empty {
        empty(
            "No contacts yet",
            "Say hi to someone near you, or send your link to a friend who's already here.",
        )
    } else {
        View::Empty
    };
    let tray: View = match &v.tray {
        Some(t) => {
            let open = tap(l, sub, || Act {
                to: Some(Sub::Requests),
                ..Act::default()
            });
            let (title, line) = (t.title.clone(), t.sub.clone());
            let count = t.count.to_string();
            view! {
                <List>
                    <ListItem icon={disc(&t.first, ChatKind::Private, 32)} sub={line.into_view()} end={view! { <Badge tone=Tone::Warning>{count}</Badge> }} chevron=true onclick={open}>{title}</ListItem>
                </List>
            }
        }
        None => View::Empty,
    };
    let message = |name: &str| {
        let name = name.to_string();
        move || Act {
            open_chat: Some(name.clone()),
            ..Act::default()
        }
    };
    let chat_glyph = || view! { <span style="flex: none; padding-right: 8px; color: var(--m-text-dim)">{icon(icons::CHAT)}</span> };
    let block = |rows: Vec<View>| view! { <div style="display: flex; flex-direction: column; gap: 8px">{rows}</div> };
    let online: View = if v.online.is_empty() {
        View::Empty
    } else {
        let rows = v
            .online
            .iter()
            .map(|p| {
                person_row(
                    p,
                    Some(Box::new(click(l, sub, message(&p.name)))),
                    chat_glyph(),
                )
            })
            .collect();
        view! { {section("Online now", Some(v.online.len()))}{block(rows)} }
    };
    let all: View = if v.all.is_empty() {
        View::Empty
    } else {
        let rows = v.all.iter().map(|p| contact_row(l, sub, p)).collect();
        view! { {section("All contacts", Some(v.all.len()))}{block(rows)} }
    };
    let sent: View = if v.sent.is_empty() {
        View::Empty
    } else {
        let rows = v
            .sent
            .iter()
            .map(|p| {
                let name = p.name.clone();
                let withdraw = tap(l, sub, move || Act {
                    withdraw: Some(name.clone()),
                    ..Act::default()
                });
                person_row(
                    p,
                    None,
                    view! { <Button variant=Variant::Ghost size=Size::Sm onclick={withdraw}>"Withdraw"</Button> },
                )
            })
            .collect();
        view! { {section("Sent requests", Some(v.sent.len()))}{block(rows)} }
    };
    let near: View = if v.near.is_empty() {
        View::Empty
    } else {
        let rows = v
            .near
            .iter()
            .map(|p| {
                let (a, b) = (p.name.clone(), p.name.clone());
                let card = click(l, sub, move || Act {
                    card: Some(a.clone()),
                    ..Act::default()
                });
                let hi = tap(l, sub, move || Act {
                    hi: Some(b.clone()),
                    ..Act::default()
                });
                person_row(
                    p,
                    Some(Box::new(card)),
                    view! { <Button size=Size::Sm onclick={hi}>"Say hi"</Button> },
                )
            })
            .collect();
        view! { {section("Near you now", None)}{block(rows)} }
    };
    let meet: View = if v.empty {
        let share = tap(l, sub, || Act {
            share_link: true,
            ..Act::default()
        });
        view! {
            {section("Places to meet people", None)}
            {note("Here: whoever is talking around your avatar. Global: everyone on this server. Both are public.")}
            <div style="display: flex; flex-direction: column">
                <Button variant=Variant::Primary icon={icon(icons::OPEN_EXTERNAL)} onclick={share}>"Share my link"</Button>
            </div>
        }
    } else {
        View::Empty
    };
    view! {
        {error}
        {empty_state}
        {tray}
        {online}
        {all}
        {sent}
        {near}
        {meet}
    }
}

/// A contact in "All contacts": a tap opens your chat; ⋯ opens Message and
/// Remove contact under it (egui's right-click menu).
fn contact_row(l: &ChatsLive, sub: &Sub, p: &PersonV) -> View {
    let name = p.name.clone();
    let open = {
        let name = name.clone();
        click(l, sub, move || Act {
            open_chat: Some(name.clone()),
            ..Act::default()
        })
    };
    let rows = l.contacts.open_row.clone();
    let toggle: Handler = {
        let (rows, name) = (rows.clone(), name.clone());
        Rc::new(move |_: &Event| {
            let now = rows.get_untracked();
            rows.set(if now.as_deref() == Some(name.as_str()) {
                None
            } else {
                Some(name.clone())
            });
        })
    };
    let more = view! {
        <Button variant=Variant::Ghost size=Size::Sm icon={icon(icons::MORE)} icon_only=true onclick={Some(toggle)} title="Message or remove" />
    };
    let row = person_row(p, Some(Box::new(open)), more);
    let actions = {
        let (l, sub, name) = (l.clone(), sub.clone(), name.clone());
        move || -> View {
            if rows.get().as_deref() != Some(name.as_str()) {
                return View::Empty;
            }
            let message = {
                let name = name.clone();
                tap(&l, &sub, move || Act {
                    open_chat: Some(name.clone()),
                    ..Act::default()
                })
            };
            let remove = {
                let (name, sub) = (name.clone(), sub.clone());
                confirm(
                    &l,
                    format!("remove:{name}"),
                    view! { <span style="display: inline-flex; align-items: center; gap: 6px">{icon(icons::PERSON_REMOVE)}"Remove contact"</span> },
                    "Remove them?",
                    move |w, _| {
                        run(
                            w,
                            &sub,
                            Act {
                                remove: Some(name.clone()),
                                ..Act::default()
                            },
                        )
                    },
                )
            };
            view! {
                <div style="display: flex; flex-wrap: wrap; gap: 8px; padding: 0 4px 4px 64px">
                    <Button size=Size::Sm icon={icon(icons::CHAT)} onclick={message}>"Message"</Button>
                    {remove}
                </div>
            }
        }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            {row}
            {actions}
        </div>
    }
}

// ─── Add a contact ───────────────────────────────────────────────────────────

fn add_page(l: &ChatsLive, sub: &Sub) -> View {
    let top = {
        let (l, sub) = (l.clone(), sub.clone());
        move || -> View {
            let v = l.contacts.add.get();
            add_top(&l, &sub, &v.me, &v.link, &v.qr)
        }
    };
    let find = {
        let (search, sub) = (l.contacts.search.clone(), sub.clone());
        l.later(move |w, _| {
            if let Some(v) = search.typed(&w.contacts.search) {
                w.contacts.search = v;
            }
            if !w.contacts.search.trim().is_empty() {
                run(
                    w,
                    &sub,
                    Act {
                        search: true,
                        ..Act::default()
                    },
                );
            }
        })
    };
    let keys: Handler = {
        let find = find.clone();
        Rc::new(move |e: &Event| {
            if e.key.as_deref() == Some("Enter") {
                find();
            }
        })
    };
    let press: Handler = Rc::new(move |_: &Event| find());
    let result = {
        let (l, sub) = (l.clone(), sub.clone());
        move || -> View {
            let v = l.contacts.add.get();
            add_result(&l, &sub, &v)
        }
    };
    let body = body_style(l);
    view! {
        {head(l, sub, "Add a contact", View::Empty)}
        <div style={body}>
            {top}
            {group("Or find a username")}
            <div style="display: flex; align-items: center; gap: 8px">
                <div style="flex: 1; min-width: 0">
                    <FieldText model={l.contacts.search.model.clone()} placeholder="Exact username, or a contact link" autocomplete="off" onkeydown={Some(keys)} id="contacts-search" />
                </div>
                <Button disabled={l.contacts.find_off.clone()} onclick={Some(press)} title="Type a username first">"Find"</Button>
            </div>
            {result}
            {note("Only an exact username finds someone: there's no list of everybody to browse.")}
        </div>
    }
}

/// Your QR code and name, your link, and what to do with it.
fn add_top(l: &ChatsLive, sub: &Sub, me: &str, link: &str, qr: &str) -> View {
    let picture: View = if qr.is_empty() {
        View::Empty
    } else {
        let src = qr.to_string();
        view! {
            <img src={src} alt="Your contact link as a QR code" style="width: 200px; height: 200px; border-radius: 16px" />
        }
    };
    let copy = {
        let link = link.to_string();
        l.on(move |_, _| crate::utils::clipboard::set_text(&link))
    };
    let share = tap(l, sub, || Act {
        share_link: true,
        ..Act::default()
    });
    let reset = {
        let sub = sub.clone();
        confirm(
            l,
            "reset-link".to_string(),
            view! { <span style="display: inline-flex; align-items: center; gap: 6px">{icon(icons::REFRESH)}"Reset link"</span> },
            "Old links stop working",
            move |w, _| {
                run(
                    w,
                    &sub,
                    Act {
                        reset_link: true,
                        ..Act::default()
                    },
                )
            },
        )
    };
    let (me, link) = (me.to_string(), link.to_string());
    view! {
        <div style="display: flex; flex-direction: column; align-items: center; gap: 12px; padding-top: 4px">
            {picture}
            <h2 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{me}</h2>
        </div>
        <div style="display: flex; flex-direction: column; gap: 12px; padding: 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)">
            <span style="color: var(--m-text); font-family: var(--m-font-mono); font-size: var(--m-font-size-sm); white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{link}</span>
            <div style="display: flex; flex-wrap: wrap; gap: 8px">
                <Button size=Size::Sm icon={icon(icons::COPY)} onclick={copy}>"Copy"</Button>
                <Button size=Size::Sm icon={icon(icons::OPEN_EXTERNAL)} onclick={share}>"Share"</Button>
                {reset}
            </div>
        </div>
        {note("Somebody who opens your link or scans this can send you a request, whatever your privacy setting. Reset link stops old copies working.")}
    }
}

/// What looking a username up found, and a refusal.
fn add_result(l: &ChatsLive, sub: &Sub, v: &AddV) -> View {
    let found: View = match &v.found {
        FoundV::Nothing => View::Empty,
        FoundV::Looking => view! {
            <div style="display: flex; align-items: center; gap: 12px; padding: 4px; color: var(--m-text-dim)">
                <Spinner size=Size::Sm />"Looking…"
            </div>
        },
        FoundV::Nobody => note("Nobody by that name."),
        FoundV::Person(p, may) => {
            let end: View = if *may {
                let name = p.name.clone();
                let request = tap(l, sub, move || Act {
                    add: Some(name.clone()),
                    ..Act::default()
                });
                view! { <Button variant=Variant::Primary size=Size::Sm onclick={request}>"Request"</Button> }
            } else {
                View::Empty
            };
            person_row(p, None, end)
        }
    };
    let error: View = match &v.error {
        Some(e) => {
            let dismiss = Some(l.later(|w, _| w.contacts.error = None));
            let e = e.clone();
            view! { <Alert tone=Tone::Danger ondismiss={dismiss}>{e}</Alert> }
        }
        None => View::Empty,
    };
    view! { {found}{error} }
}

// ─── Privacy ─────────────────────────────────────────────────────────────────

/// The three request policies, as the privacy screen offers them.
const POLICIES: [(RequestPolicy, &str, &str); 3] = [
    (RequestPolicy::Anyone, "Anyone", "including by username"),
    (
        RequestPolicy::Near,
        "People you've been near",
        "in the world in the last hour, or who share a contact with you",
    ),
    (
        RequestPolicy::LinkOnly,
        "Only with my link",
        "nobody finds you by name or in the world",
    ),
];

fn privacy_page(l: &ChatsLive, sub: &Sub) -> View {
    let cards = {
        let l = l.clone();
        move || -> View {
            let chosen = l.contacts.policy.get();
            let cards: Vec<View> = POLICIES
                .iter()
                .map(|&(policy, title, line)| {
                    policy_card(&l, policy, title, line, policy == chosen)
                })
                .collect();
            view! { <div role="radiogroup" style="display: flex; flex-direction: column; gap: 8px">{cards}</div> }
        }
    };
    let c = &l.contacts;
    let separate = setting(l, |s, v| s.separate_new_accounts = v);
    let notify = setting(l, |s, v| s.notify_requests = v);
    let online = setting(l, |s, v| s.show_online = v);
    let body = body_style(l);
    view! {
        {head(l, sub, "Privacy", View::Empty)}
        <div style={body}>
            <div style="display: flex; flex-direction: column; gap: 8px">
                <h2 style="margin: 0; padding-inline: 4px; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">"Who can send you a request"</h2>
                {note("Your contacts can always message you. Anyone with your link can always send a request.")}
            </div>
            {cards}
            {group("Requests")}
            <FieldCheckbox model={c.separate.clone()} label="Requests from accounts younger than a week wait in their own section" onchange={separate} id="contacts-separate" />
            <FieldCheckbox model={c.notify.clone()} label="Say so when a request arrives (off: they wait quietly in the tray)" onchange={notify} id="contacts-notify" />
            {group("Contacts can see")}
            <FieldCheckbox model={c.show_online.clone()} label="That I'm online" onchange={online} id="contacts-online" />
            {note("Where your avatar is stays per chat and until a time you set, as before. Being a contact doesn't show it.")}
        </div>
    }
}

/// One policy, as a card with its radio dot: the chosen one in the accent.
fn policy_card(
    l: &ChatsLive,
    policy: RequestPolicy,
    title: &'static str,
    line: &'static str,
    on: bool,
) -> View {
    let pick = l.tap(move |w, _| {
        if w.contacts.settings.policy != policy {
            w.contacts.settings.policy = policy;
            run(
                w,
                &Sub::Privacy,
                Act {
                    settings: true,
                    ..Act::default()
                },
            );
        }
    });
    let (edge, fill) = if on {
        ("var(--m-accent)", "var(--m-accent-soft)")
    } else {
        ("var(--m-border)", "var(--m-surface)")
    };
    let style = format!(
        "display: flex; align-items: flex-start; gap: 14px; width: 100%; margin: 0; padding: 14px 16px; border: 1px solid {edge}; border-radius: var(--m-radius-lg); background: {fill}; color: var(--m-text); font: inherit; text-align: start; cursor: pointer"
    );
    let ring = format!(
        "flex: none; display: flex; align-items: center; justify-content: center; width: 18px; height: 18px; margin-top: 2px; border: 2px solid {}; border-radius: var(--m-radius-full)",
        if on {
            "var(--m-accent)"
        } else {
            "var(--m-border-strong)"
        }
    );
    let dot: View = if on {
        view! { <span style="width: 9px; height: 9px; border-radius: var(--m-radius-full); background: var(--m-accent)"></span> }
    } else {
        View::Empty
    };
    view! {
        <button role="radio" aria-checked={if on { "true" } else { "false" }} title={title} style={style} on:click={pick}>
            <span style={ring}>{dot}</span>
            <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px">
                <span style="color: var(--m-text-strong); font-weight: 600">{title}</span>
                <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{line}</span>
            </span>
        </button>
    }
}
