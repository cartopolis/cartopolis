//! The calendar on the component library, inside the Chats panel's shell
//! (`lib_chats`): the agenda with its week strip, the month, a meeting's or
//! a meet-up's page, the "+" page and the settings — every page egui's
//! `calendar::screen` routes on `Calendar::page` (`docs/notes/calendar.md`).
//!
//! **One set of logic, two editions.** What a tap changes is a `Calendar`
//! method (`open_item`, `switch_view`, `pick_day`, `step_month`,
//! `plan_new`, `new_next`, `set_answered_only`) or a `calendar::Out` built
//! by the same functions egui's pages call (`answer`, `toggle_skip`,
//! `stop_after`), handed to `chats::calendar_out` as egui's is, so an answer
//! from here is the answer from the chat's card. The rows' words are
//! `calendar`'s (`row_under`, `row_sub`, `when_line`, `series_line`,
//! `by_line`, `answer_word`).
//!
//! **What differs from egui's.** The library cannot scroll a list to a
//! row, so a day tapped in the week strip starts the agenda at that day
//! rather than scrolling it there; a skipped or declined row is faded and
//! (in the browser) struck through. The meet-up's tone, egui's brand
//! orange, is the library's warning tone, the nearest token.

use std::rc::Rc;

use cartopolis_core::meeting::Rsvp;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::Button;
use mithril::prelude::*;

use crate::systems::calendar::{self as cal, Calendar, Item, NewKind, Out, Page, Pick, What};
use crate::systems::chat::ChatState;
use crate::systems::chats::{self, Actions, ChatsScreen};
use crate::systems::communities::Communities;
use crate::systems::icons;
use crate::systems::lib_chats::{
    back, body_style, group, header, icon, note, ChatsLive, ChatsWorld,
};

// ─── What is drawn ───────────────────────────────────────────────────────────

/// Which page shows: the router of [`screen`] follows it. List and Month
/// are one page with a switch on it.
#[derive(Clone, Copy, PartialEq, Default)]
enum Route {
    #[default]
    None,
    Overview,
    Detail,
    New,
    Settings,
}

/// One row: a meeting's date or a meet-up.
#[derive(Clone, PartialEq)]
struct RowV {
    pick: Pick,
    /// "18:00".
    time: String,
    /// Under the time: a meeting's length.
    under: Option<String>,
    title: String,
    /// After its icon: the chat, how many go, that it repeats.
    sub: String,
    meetup: bool,
    /// Skipped, can't come, or over.
    faded: bool,
    /// Skipped or can't come: struck through.
    struck: bool,
    /// The answer pill's word, when it shows one.
    pill: Option<&'static str>,
}

/// A day of the week strip, or a cell of the month.
#[derive(Clone, PartialEq)]
struct DayV {
    day: i64,
    /// "Thu", in the strip.
    weekday: String,
    number: i8,
    /// "Thursday 22 October", for a reader.
    name: String,
    chosen: bool,
    today: bool,
    /// In the month the grid shows.
    inside: bool,
    meeting: bool,
    meetup: bool,
}

/// One thing in the agenda.
#[derive(Clone, PartialEq)]
enum AgendaV {
    /// A day's heading: "Today", and the long date beside it.
    Day {
        title: String,
        date: String,
        today: bool,
    },
    /// Now, between today's rows.
    Now(String),
    Row(RowV),
}

#[derive(Clone, PartialEq, Default)]
struct ListV {
    week: Vec<DayV>,
    agenda: Vec<AgendaV>,
}

#[derive(Clone, PartialEq, Default)]
struct MonthV {
    title: String,
    cells: Vec<DayV>,
    /// The chosen day, long.
    chosen: String,
    chosen_today: bool,
    rows: Vec<RowV>,
}

/// A meeting's own part of its page.
#[derive(Clone, PartialEq)]
struct MeetingV {
    /// "Sanne took this date out of the series."
    skipped: Option<String>,
    /// Not over and not skipped: it can be answered.
    answerable: bool,
    repeats: bool,
    mine: Option<Rsvp>,
    going: Vec<String>,
    maybe: Vec<String>,
    no: Vec<String>,
    text: Option<String>,
    agenda: Vec<(String, u32)>,
    join: String,
    can_join: bool,
    /// Your own series, not over: Skip's words, and whether this is its
    /// last date (no Stop after this).
    own: Option<(String, bool)>,
}

/// A meet-up's own part of its page.
#[derive(Clone, PartialEq)]
struct MeetupV {
    walk: Option<u64>,
    /// Who goes, you as "you".
    going: Vec<String>,
    /// You are going.
    mine: bool,
    over: bool,
}

#[derive(Clone, PartialEq)]
enum KindV {
    Meeting(MeetingV),
    Meetup(MeetupV),
}

#[derive(Clone, PartialEq)]
struct DetailV {
    title: String,
    when: String,
    series: Option<String>,
    by: String,
    kind: KindV,
}

#[derive(Clone, PartialEq, Default)]
struct NewV {
    /// "Thursday 22 October".
    day: String,
    /// The chats a plan can go to: (key, name, chosen).
    chats: Vec<(String, String, bool)>,
}

#[derive(Clone, PartialEq, Default)]
struct SettingsV {
    /// Every community you are in: (id, name, only answered).
    communities: Vec<(String, String, bool)>,
    plans: usize,
}

// ─── The live state ──────────────────────────────────────────────────────────

/// The calendar's own signals, kept in `ChatsLive::calendar`.
#[derive(Clone)]
pub(crate) struct CalendarLive {
    route: Signal<Route>,
    /// List or Month.
    view: Signal<Page>,
    list: Signal<ListV>,
    month: Signal<MonthV>,
    detail: Signal<Option<DetailV>>,
    /// The page's answer goes to every date of the series.
    every: Signal<bool>,
    new: Signal<NewV>,
    kind: Signal<NewKind>,
    /// No chat chosen yet: Next cannot be pressed.
    next_off: Signal<bool>,
    settings: Signal<SettingsV>,
    show_declined: Signal<bool>,
    coming_up: Signal<bool>,
    leave_reminder: Signal<bool>,
    meeting_reminder: Signal<bool>,
}

impl CalendarLive {
    /// Made when the panel mounts; [`mirror`] fills it before anything reads it.
    pub(crate) fn new(w: &ChatsWorld) -> Self {
        let p = &w.calendar.book.prefs;
        CalendarLive {
            route: signal(Route::None),
            view: signal(Page::List),
            list: signal(ListV::default()),
            month: signal(MonthV::default()),
            detail: signal(None),
            every: signal(false),
            new: signal(NewV::default()),
            kind: signal(w.calendar.new_kind),
            next_off: signal(true),
            settings: signal(SettingsV::default()),
            show_declined: signal(p.show_declined),
            coming_up: signal(p.coming_up),
            leave_reminder: signal(p.leave_reminder),
            meeting_reminder: signal(p.meeting_reminder),
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

/// The world → [`CalendarLive`], every frame the panel is mounted. Answers
/// whether what shows moved, for the HTML beside a capture.
pub(crate) fn mirror(live: &ChatsLive, w: &mut ChatsWorld, now: u64) -> bool {
    let l = &live.calendar;
    if w.screen() != ChatsScreen::Calendar {
        return put(&l.route, Route::None);
    }
    // What egui's overview does while it is drawn: settle on a day and a
    // month. Its scroll to the day is this edition's starting point.
    let today = cal::local_day(now);
    if w.calendar.day.is_none() {
        w.calendar.day = Some(today);
    }
    if w.calendar.scroll_to_day {
        w.calendar.scroll_to_day = false;
    }
    if w.calendar.page == Page::Month {
        let shown = w.calendar.shown_month(now);
        if w.calendar.month != Some(shown) {
            w.calendar.month = Some(shown);
        }
    }
    let c: &Calendar = &w.calendar;
    let route = match c.page {
        Page::List | Page::Month => Route::Overview,
        Page::Detail => Route::Detail,
        Page::New => Route::New,
        Page::Settings => Route::Settings,
    };
    let mut moved = put(&l.route, route);
    let p = &c.book.prefs;
    put(&l.show_declined, p.show_declined);
    put(&l.coming_up, p.coming_up);
    put(&l.leave_reminder, p.leave_reminder);
    put(&l.meeting_reminder, p.meeting_reminder);
    put(&l.every, c.every);
    put(&l.kind, c.new_kind);
    put(&l.next_off, c.new_chat.is_none());
    match c.page {
        Page::List => {
            moved |= put(&l.view, Page::List);
            moved |= put(&l.list, list_v(c, &w.chat, &w.communities, now));
        }
        Page::Month => {
            moved |= put(&l.view, Page::Month);
            moved |= put(&l.month, month_v(c, &w.chat, &w.communities, now));
        }
        Page::Detail => moved |= put(&l.detail, detail_v(c, &w.chat, &w.communities, now)),
        Page::New => moved |= put(&l.new, new_v(c, &w.chat, &w.communities, now)),
        Page::Settings => moved |= put(&l.settings, settings_v(c, &w.communities)),
    }
    moved
}

fn row_v(item: &Item, chat: &ChatState, communities: &Communities, now: u64) -> RowV {
    let label = cal::chat_name(chat, communities, &item.channel);
    RowV {
        pick: item.pick(),
        time: chats::clock_time(item.start_ms),
        under: cal::row_under(item, None),
        title: item.title(),
        sub: cal::row_sub(item, &label),
        meetup: item.is_meetup(),
        faded: cal::row_faded(item, now),
        struck: item.skipped || item.mine == Some(Rsvp::No),
        pill: cal::shows_answer(item, now).then(|| cal::answer_word(item)),
    }
}

fn day_v(day: i64, items: &[Item], chosen: i64, today: i64, month: Option<i8>) -> DayV {
    let (weekday, number, in_month) = cal::day_face(day);
    let (meeting, meetup) = cal::day_kinds(items, day);
    DayV {
        day,
        weekday,
        number,
        name: cal::long_day(cal::day_start_ms(day) + 12 * 3_600_000),
        chosen: day == chosen,
        today: day == today,
        inside: month.is_none_or(|m| m == in_month),
        meeting,
        meetup,
    }
}

fn list_v(c: &Calendar, chat: &ChatState, communities: &Communities, now: u64) -> ListV {
    let today = cal::local_day(now);
    let chosen = c.day.unwrap_or(today);
    let from = cal::day_start_ms(today);
    let week_items = c.visible(from, cal::day_start_ms(today + 7), communities);
    let week = (0..7)
        .map(|k| day_v(today + k, &week_items, chosen, today, None))
        .collect();
    // The agenda from the day chosen: where egui's scrolls to.
    let start = cal::day_start_ms(chosen.max(today));
    let items = c.visible(start, from + 120 * 24 * 3_600_000, communities);
    let mut agenda = Vec::new();
    let mut last_day = None;
    let mut now_drawn = false;
    for item in &items {
        let d = cal::local_day(item.start_ms);
        if last_day != Some(d) {
            last_day = Some(d);
            agenda.push(AgendaV::Day {
                title: cal::day_heading(item.start_ms, now),
                date: if d - today < 2 {
                    cal::long_day(item.start_ms)
                } else {
                    String::new()
                },
                today: d == today,
            });
        }
        if !now_drawn && d == today && item.start_ms > now {
            now_drawn = true;
            agenda.push(AgendaV::Now(chats::clock_time(now)));
        }
        agenda.push(AgendaV::Row(row_v(item, chat, communities, now)));
    }
    ListV { week, agenda }
}

fn month_v(c: &Calendar, chat: &ChatState, communities: &Communities, now: u64) -> MonthV {
    let today = cal::local_day(now);
    let chosen = c.day.unwrap_or(today);
    let (y, m) = c.shown_month(now);
    let (start, weeks) = cal::month_grid(y, m);
    let items = c.visible(
        cal::day_start_ms(start),
        cal::day_start_ms(start + 42),
        communities,
    );
    let cells = (0..weeks * 7)
        .map(|k| day_v(start + k, &items, chosen, today, Some(m)))
        .collect();
    let rows = items
        .iter()
        .filter(|i| cal::local_day(i.start_ms) == chosen)
        .map(|i| row_v(i, chat, communities, now))
        .collect();
    MonthV {
        title: cal::month_title(y, m),
        cells,
        chosen: cal::long_day(cal::day_start_ms(chosen) + 12 * 3_600_000),
        chosen_today: chosen == today,
        rows,
    }
}

fn detail_v(
    c: &Calendar,
    chat: &ChatState,
    communities: &Communities,
    now: u64,
) -> Option<DetailV> {
    let item = c.picked.as_ref().and_then(|p| c.find(p))?;
    let from = cal::chat_name(chat, communities, &item.channel);
    let kind = match &item.what {
        What::Meeting { plan } => {
            let group = crate::systems::private_chat::group_of(&item.channel);
            let own =
                (item.author == c.me && plan.repeat.is_some() && item.end_ms > now).then(|| {
                    let words = if item.skipped {
                        format!("Put {} back", cal::short_day(item.start_ms))
                    } else {
                        format!("Skip {}", cal::short_day(item.start_ms))
                    };
                    let last = plan.repeat.and_then(|r| r.until_ms) == Some(item.start_ms);
                    (words, last)
                });
            KindV::Meeting(MeetingV {
                skipped: item
                    .skipped
                    .then(|| format!("{} took this date out of the series.", item.author)),
                answerable: !item.skipped && item.end_ms > now,
                repeats: plan.repeat.is_some(),
                mine: item.mine,
                going: item.going.clone(),
                maybe: item.maybe.clone(),
                no: item.no.clone(),
                text: plan.text.clone(),
                agenda: plan
                    .agenda
                    .iter()
                    .map(|a| (a.name.clone(), a.minutes))
                    .collect(),
                join: cal::join_label(&item, now),
                can_join: cal::is_live(&item, now) && group.is_some(),
                own,
            })
        }
        What::Meetup { point } => KindV::Meetup(MeetupV {
            walk: c.here.map(|here| cal::walk_minutes(here, point)),
            going: item
                .going
                .iter()
                .map(|n| {
                    if *n == c.me {
                        "you".to_string()
                    } else {
                        n.clone()
                    }
                })
                .collect(),
            mine: item.mine.is_some(),
            over: item.end_ms <= now,
        }),
    };
    Some(DetailV {
        title: item.title(),
        when: cal::when_line(&item, now),
        series: cal::series_line(&item),
        by: cal::by_line(&item, &from, &c.me),
        kind,
    })
}

fn new_v(c: &Calendar, chat: &ChatState, communities: &Communities, now: u64) -> NewV {
    let day = c.new_day(now);
    NewV {
        day: cal::long_day(cal::day_start_ms(day) + 12 * 3_600_000),
        chats: chat
            .channels
            .iter()
            .filter(|ch| crate::systems::private_chat::group_of(&ch.key).is_some())
            .map(|ch| {
                (
                    ch.key.clone(),
                    cal::chat_name(chat, communities, &ch.key),
                    c.new_chat.as_deref() == Some(ch.key.as_str()),
                )
            })
            .collect(),
    }
}

fn settings_v(c: &Calendar, communities: &Communities) -> SettingsV {
    let only = &c.book.prefs.answered_only;
    SettingsV {
        communities: communities
            .list
            .iter()
            .map(|x| {
                let id = x.id.to_string();
                let on = only.contains(&id);
                (id, x.settings.name.clone(), on)
            })
            .collect(),
        plans: c.plans_kept(),
    }
}

// ─── Taps ────────────────────────────────────────────────────────────────────

/// A calendar tap whose answer is an `Out`, handed to the panel as egui's
/// is (`chats::calendar_out`).
fn run_out(w: &mut ChatsWorld<'_>, a: &mut Actions, f: &dyn Fn(&mut ChatsWorld<'_>, &mut Out)) {
    let mut out = Out::default();
    f(w, &mut out);
    chats::calendar_out(out, &mut w.calendar, &mut w.view, a);
}

/// A component's handler for a tap with an `Out`.
fn out(
    l: &ChatsLive,
    f: impl for<'w> Fn(&mut ChatsWorld<'w>, &mut Out) + 'static,
) -> Option<Handler> {
    l.on(move |w, a| run_out(w, a, &f))
}

/// A meeting tap on the open row: `f` with the row and its plan.
fn on_plan(
    l: &ChatsLive,
    f: impl Fn(&Calendar, &Item, &cartopolis_core::meeting::Plan, &mut Out) + 'static,
) -> Option<Handler> {
    out(l, move |w, o| {
        let c: &Calendar = &w.calendar;
        if let Some(item) = c.picked.as_ref().and_then(|p| c.find(p)) {
            if let What::Meeting { plan } = &item.what {
                f(c, &item, plan, o);
            }
        }
    })
}

/// A meet-up tap on the open row: `f` with the row and its place.
fn on_point(
    l: &ChatsLive,
    f: impl Fn(&Item, &cartopolis_core::point::Point, &mut Out) + 'static,
) -> Option<Handler> {
    out(l, move |w, o| {
        let c: &Calendar = &w.calendar;
        if let Some(item) = c.picked.as_ref().and_then(|p| c.find(p)) {
            if let What::Meetup { point } = &item.what {
                f(&item, point, o);
            }
        }
    })
}

/// A preference changed: written, and the book saved.
fn pref(l: &ChatsLive, write: impl Fn(&mut Calendar, bool) + 'static) -> Option<Rc<dyn Fn(&bool)>> {
    let l = l.clone();
    let write = Rc::new(write);
    Some(Rc::new(move |v: &bool| {
        let (v, write) = (*v, write.clone());
        let go = l.later(move |w, a| {
            run_out(w, a, &|w, o| {
                write(&mut *w.calendar, v);
                o.save = true;
            });
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

/// A sub-page's header: back to the agenda, and ✕.
fn sub_head(l: &ChatsLive, title: &'static str) -> View {
    let lead = back(
        l.on(|w, _| w.calendar.page = Page::List),
        "Back to the calendar",
    );
    header(lead, title.into_view(), close(l))
}

/// A meeting's colour, or a meet-up's.
fn tone(meetup: bool) -> &'static str {
    if meetup {
        "var(--m-warning)"
    } else {
        "var(--m-accent)"
    }
}

/// The dots under a day: one per kind it holds.
fn dots(d: &DayV) -> View {
    let dot = |colour: &str| -> View {
        let style = format!(
            "width: 5px; height: 5px; border-radius: var(--m-radius-full); background: {colour}"
        );
        view! { <span style={style}></span> }
    };
    let pick = |meetup: bool| {
        if d.chosen {
            "var(--m-on-accent)"
        } else {
            tone(meetup)
        }
    };
    let mut out = Vec::new();
    if d.meeting {
        out.push(dot(pick(false)));
    }
    if d.meetup {
        out.push(dot(pick(true)));
    }
    view! { <span style="display: flex; justify-content: center; gap: 3px; height: 5px">{out}</span> }
}

/// One row: the time over its length, a bar in the kind's colour, the
/// title over where it came from, and the answer.
fn row(l: &ChatsLive, r: &RowV) -> View {
    let pick = r.pick.clone();
    let open = l.tap(move |w, _| w.calendar.open_item(pick.clone()));
    let (strong, dim) = if r.faded {
        ("var(--m-text-dim)", "var(--m-text-faint)")
    } else {
        ("var(--m-text-strong)", "var(--m-text-dim)")
    };
    let time_style = format!("color: {strong}; font-size: 15px; font-weight: 600");
    let under: View = match &r.under {
        Some(u) => {
            let style = format!("color: {dim}; font-size: 12px");
            let u = u.clone();
            view! { <span style={style}>{u}</span> }
        }
        None => View::Empty,
    };
    let bar = format!(
        "flex: none; align-self: stretch; width: 3px; border-radius: 2px; background: {}",
        if r.faded {
            "var(--m-border-strong)"
        } else {
            tone(r.meetup)
        }
    );
    let title_style = format!(
        "color: {strong}; font-weight: 600; white-space: nowrap; text-overflow: ellipsis; overflow: hidden{}",
        if r.struck {
            "; text-decoration: line-through"
        } else {
            ""
        }
    );
    let sub_style = format!(
        "display: flex; align-items: center; gap: 6px; min-width: 0; color: {dim}; font-size: var(--m-font-size-sm)"
    );
    let glyph = if r.meetup {
        icons::FLAG
    } else {
        icons::VIDEOCAM
    };
    let pill: View = match r.pill {
        Some(word) => {
            let tone = match word {
                "Going" => Tone::Success,
                "Maybe" => Tone::Warning,
                "Answer" => Tone::Accent,
                _ => Tone::Neutral,
            };
            view! { <Badge tone={tone}>{word}</Badge> }
        }
        None => View::Empty,
    };
    let (time, title, sub) = (r.time.clone(), r.title.clone(), r.sub.clone());
    view! {
        <button title={r.title.clone()} style="display: flex; align-items: center; gap: 12px; width: 100%; margin: 0; padding: 10px 12px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface); color: var(--m-text); font: inherit; text-align: start; cursor: pointer" on:click={open}>
            <span style="flex: none; width: 48px; display: flex; flex-direction: column; align-items: flex-end; gap: 2px">
                <span style={time_style}>{time}</span>
                {under}
            </span>
            <span style={bar}></span>
            <span style="flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 3px">
                <span style={title_style}>{title}</span>
                <span style={sub_style}>
                    <span style="flex: none; font-family: 'Material Symbols Rounded'; font-size: 15px; line-height: 1">{glyph}</span>
                    <span style="min-width: 0; white-space: nowrap; text-overflow: ellipsis; overflow: hidden">{sub}</span>
                </span>
            </span>
            {pill}
        </button>
    }
}

fn rows(l: &ChatsLive, rows: &[RowV]) -> View {
    let rows: Vec<View> = rows.iter().map(|r| row(l, r)).collect();
    view! { <div style="display: flex; flex-direction: column; gap: 8px">{rows}</div> }
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

/// A raised block.
const CARD: &str = "display: flex; flex-direction: column; gap: 10px; padding: 14px 16px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)";

/// Two buttons side by side, each half the width.
fn pair(a: View, b: View) -> View {
    view! {
        <div style="display: flex; gap: 12px">
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column">{a}</div>
            <div style="flex: 1; min-width: 0; display: flex; flex-direction: column">{b}</div>
        </div>
    }
}

// ─── The router ──────────────────────────────────────────────────────────────

/// Whichever calendar page is up.
pub(crate) fn screen(live: &ChatsLive) -> View {
    let l = live.clone();
    (move || -> View {
        match l.calendar.route.get() {
            Route::None => View::Empty,
            Route::Overview => overview(&l),
            Route::Detail => detail_page(&l),
            Route::New => new_page(&l),
            Route::Settings => settings_page(&l),
        }
    })
    .into_view()
}

// ─── List and Month ──────────────────────────────────────────────────────────

fn overview(l: &ChatsLive) -> View {
    let lead = back(out(l, |_, o| o.back = true), "Back to the list");
    let settings = l.on(|w, _| w.calendar.page = Page::Settings);
    let plan = l.on(|w, _| w.calendar.plan_new());
    let end = view! {
        <Button variant=Variant::Ghost icon={icon(icons::SETTINGS)} icon_only=true onclick={settings} title="Calendar settings" />
        <Button variant=Variant::Ghost icon={icon(icons::ADD)} icon_only=true onclick={plan} title="Plan something" />
        {close(l)}
    };
    let switch: Rc<dyn Fn(&Page)> = {
        let l = l.clone();
        Rc::new(move |p: &Page| {
            let p = *p;
            let go = l.later(move |w, _| w.calendar.switch_view(p));
            go();
        })
    };
    let options = vec![Opt::new(Page::List, "List"), Opt::new(Page::Month, "Month")];
    let content = {
        let l = l.clone();
        move || -> View {
            match l.calendar.view.get() {
                Page::Month => month_body(&l, &l.calendar.month.get()),
                _ => list_body(&l, &l.calendar.list.get()),
            }
        }
    };
    let body = body_style(l);
    view! {
        {header(lead, "Calendar".into_view(), end)}
        <div style={body}>
            <FieldSegmented model={l.calendar.view.clone()} options={options} onchange={Some(switch)} id="calendar-view" />
            {content}
        </div>
    }
}

/// The week from today, a dot per kind on each day that has something; a
/// tap starts the agenda there.
fn week_strip(l: &ChatsLive, week: &[DayV]) -> View {
    let days: Vec<View> = week
        .iter()
        .map(|d| {
            let day = d.day;
            let pick = l.tap(move |w, _| w.calendar.pick_day(day));
            let (fill, ink, dim) = if d.chosen {
                ("var(--m-accent)", "var(--m-on-accent)", "var(--m-on-accent)")
            } else if d.today {
                ("var(--m-surface)", "var(--m-accent-text)", "var(--m-accent-text)")
            } else {
                ("var(--m-surface)", "var(--m-text-strong)", "var(--m-text-dim)")
            };
            let style = format!(
                "display: flex; flex-direction: column; align-items: center; gap: 4px; min-width: 0; margin: 0; padding: 8px 0; border: 0; border-radius: 12px; background: {fill}; font: inherit; cursor: pointer"
            );
            let top = format!("color: {dim}; font-size: 12px");
            let num = format!("color: {ink}; font-size: 17px; font-weight: 600; line-height: 1.2");
            let (weekday, number, name) = (d.weekday.clone(), d.number.to_string(), d.name.clone());
            view! {
                <button title={name} style={style} on:click={pick}>
                    <span style={top}>{weekday}</span>
                    <span style={num}>{number}</span>
                    {dots(d)}
                </button>
            }
        })
        .collect();
    view! { <div style="display: grid; grid-template-columns: repeat(7, 1fr); gap: 4px">{days}</div> }
}

fn list_body(l: &ChatsLive, v: &ListV) -> View {
    let agenda: View = if v.agenda.is_empty() {
        empty(
            "Nothing planned",
            "Meet-ups and meetings from your chats show here. Plan one with +.",
        )
    } else {
        let mut out = Vec::new();
        let mut run: Vec<RowV> = Vec::new();
        for a in &v.agenda {
            match a {
                AgendaV::Row(r) => run.push(r.clone()),
                other => {
                    if !run.is_empty() {
                        out.push(rows(l, &std::mem::take(&mut run)));
                    }
                    out.push(match other {
                        AgendaV::Day { title, date, today } => day_heading(title, date, *today),
                        AgendaV::Now(time) => now_line(time),
                        AgendaV::Row(_) => View::Empty,
                    });
                }
            }
        }
        if !run.is_empty() {
            out.push(rows(l, &run));
        }
        view! { <div style="display: flex; flex-direction: column; gap: 10px">{out}</div> }
    };
    view! {
        {week_strip(l, &v.week)}
        <div style="height: 1px; background: var(--m-border)"></div>
        {agenda}
    }
}

/// A day's heading in the agenda: today's in the accent.
fn day_heading(title: &str, date: &str, today: bool) -> View {
    let style = format!(
        "flex: 1; min-width: 0; color: {}; font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase",
        if today {
            "var(--m-accent-text)"
        } else {
            "var(--m-text-dim)"
        }
    );
    let (title, date) = (title.to_string(), date.to_string());
    view! {
        <div style="display: flex; align-items: baseline; gap: 8px; margin-top: 8px; padding-inline: 4px">
            <span style={style}>{title}</span>
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{date}</span>
        </div>
    }
}

/// Now, as a line between today's rows.
fn now_line(time: &str) -> View {
    let time = time.to_string();
    view! {
        <div style="display: flex; align-items: center; gap: 8px; padding-inline: 4px">
            <span style="width: 8px; height: 8px; border-radius: var(--m-radius-full); background: var(--m-warning)"></span>
            <span style="color: var(--m-warning); font-size: 12px; font-weight: 600">{time}</span>
            <span style="flex: 1; height: 1px; background: var(--m-warning)"></span>
        </div>
    }
}

fn month_body(l: &ChatsLive, v: &MonthV) -> View {
    let prev = l.on(|w, _| w.calendar.step_month(-1, crate::utils::clock::unix_ms()));
    let next = l.on(|w, _| w.calendar.step_month(1, crate::utils::clock::unix_ms()));
    let letters: Vec<View> = ["M", "T", "W", "T", "F", "S", "S"]
        .iter()
        .map(|d| view! { <span style="color: var(--m-text-dim); font-size: 12px; text-align: center">{*d}</span> })
        .collect();
    let cells: Vec<View> = v
        .cells
        .iter()
        .map(|d| {
            let day = d.day;
            let pick = l.tap(move |w, _| w.calendar.pick_day(day));
            let (fill, ink) = if d.chosen {
                ("var(--m-accent)", "var(--m-on-accent)")
            } else if d.today {
                ("none", "var(--m-accent-text)")
            } else if d.inside {
                ("none", "var(--m-text)")
            } else {
                ("none", "var(--m-text-faint)")
            };
            let style = format!(
                "display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 4px; height: 46px; min-width: 0; margin: 0; padding: 0; border: 0; border-radius: 12px; background: {fill}; color: {ink}; font: inherit; font-size: 15px; font-weight: {}; cursor: pointer",
                if d.today || d.chosen { 650 } else { 400 }
            );
            let (number, name) = (d.number.to_string(), d.name.clone());
            view! {
                <button title={name} style={style} on:click={pick}>
                    <span>{number}</span>
                    {dots(d)}
                </button>
            }
        })
        .collect();
    let chosen_style = format!(
        "margin: 4px 0 0; padding-inline: 4px; color: {}; font-size: 17px; font-weight: 600",
        if v.chosen_today {
            "var(--m-accent-text)"
        } else {
            "var(--m-text-strong)"
        }
    );
    let day: View = if v.rows.is_empty() {
        note("Nothing planned on this day.")
    } else {
        rows(l, &v.rows)
    };
    let (title, chosen) = (v.title.clone(), v.chosen.clone());
    view! {
        <div style="display: flex; align-items: center; gap: 4px">
            <h2 style="flex: 1; min-width: 0; margin: 0; padding-inline: 4px; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{title}</h2>
            <Button variant=Variant::Ghost icon={icon(icons::CHEVRON_LEFT)} icon_only=true onclick={prev} title="Previous month" />
            <Button variant=Variant::Ghost icon={icon(icons::CHEVRON_RIGHT)} icon_only=true onclick={next} title="Next month" />
        </div>
        <div style="display: flex; flex-direction: column; gap: 4px; padding: 12px 8px; border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); background: var(--m-surface)">
            <div style="display: grid; grid-template-columns: repeat(7, 1fr); gap: 2px">{letters}</div>
            <div style="display: grid; grid-template-columns: repeat(7, 1fr); gap: 2px">{cells}</div>
        </div>
        <h3 style={chosen_style}>{chosen}</h3>
        {day}
    }
}

// ─── A plan's page ───────────────────────────────────────────────────────────

fn detail_page(l: &ChatsLive) -> View {
    let content = {
        let l = l.clone();
        move || -> View {
            match l.calendar.detail.get() {
                Some(d) => detail_body(&l, &d),
                None => empty("This plan is gone", "It may have been over for a while."),
            }
        }
    };
    let body = body_style(l);
    view! {
        {sub_head(l, "Calendar")}
        <div style={body}>{content}</div>
    }
}

fn detail_body(l: &ChatsLive, d: &DetailV) -> View {
    let meetup = matches!(d.kind, KindV::Meetup(_));
    let (glyph, kicker) = if meetup {
        (icons::FLAG, "Meet-up · a place and a time")
    } else {
        (icons::VIDEOCAM, "Meeting · a call")
    };
    let kicker_style = format!(
        "display: flex; align-items: center; gap: 8px; color: {}; font-size: 12px; font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase",
        if meetup {
            "var(--m-warning)"
        } else {
            "var(--m-accent-text)"
        }
    );
    let series: View = match &d.series {
        Some(s) => {
            let s = s.clone();
            view! { <span style="color: var(--m-accent-text); font-size: var(--m-font-size-sm)">{s}</span> }
        }
        None => View::Empty,
    };
    let rest = match &d.kind {
        KindV::Meeting(m) => meeting_part(l, m),
        KindV::Meetup(m) => meetup_part(l, m),
    };
    let (title, when, by) = (d.title.clone(), d.when.clone(), d.by.clone());
    view! {
        <div style="display: flex; flex-direction: column; gap: 6px">
            <span style={kicker_style}>
                <span style="font-family: 'Material Symbols Rounded'; font-size: 16px; line-height: 1">{glyph}</span>
                <span>{kicker}</span>
            </span>
            <h2 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{title}</h2>
            <span style="color: var(--m-text)">{when}</span>
            {series}
            <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{by}</span>
        </div>
        {rest}
    }
}

/// "Going 2  sanne, jeroen".
fn names(label: &'static str, names: &[String]) -> View {
    let count = format!("{label} {}", names.len());
    let who = if names.is_empty() {
        "—".to_string()
    } else {
        names.join(", ")
    };
    view! {
        <div style="display: flex; gap: 10px; font-size: var(--m-font-size-sm)">
            <span style="flex: none; width: 96px; color: var(--m-text-dim)">{count}</span>
            <span style="flex: 1; min-width: 0; color: var(--m-text)">{who}</span>
        </div>
    }
}

fn meeting_part(l: &ChatsLive, m: &MeetingV) -> View {
    let skipped: View = match &m.skipped {
        Some(s) => note(s.clone()),
        None => View::Empty,
    };
    let answer: View = if m.answerable {
        let every: View = if m.repeats {
            let set: Rc<dyn Fn(&bool)> = {
                let l = l.clone();
                Rc::new(move |v: &bool| {
                    let v = *v;
                    let go = l.later(move |w, _| w.calendar.every = v);
                    go();
                })
            };
            let options = vec![Opt::new(false, "This date"), Opt::new(true, "Every date")];
            view! { <FieldSegmented model={l.calendar.every.clone()} options={options} onchange={Some(set)} id="calendar-every" /> }
        } else {
            View::Empty
        };
        let choices: Vec<View> = [Rsvp::Going, Rsvp::Maybe, Rsvp::No]
            .into_iter()
            .map(|r| {
                let go = on_plan(l, move |c, item, plan, o| {
                    o.meeting = Some(cal::answer(item, plan, r, c.every));
                });
                let variant = if m.mine == Some(r) {
                    Variant::Primary
                } else {
                    Variant::Secondary
                };
                view! {
                    <div style="flex: 1; min-width: 0; display: flex; flex-direction: column">
                        <Button variant={variant} onclick={go}>{r.words()}</Button>
                    </div>
                }
            })
            .collect();
        view! {
            {every}
            <div style="display: flex; gap: 8px">{choices}</div>
        }
    } else {
        View::Empty
    };
    let text: View = match &m.text {
        Some(t) => {
            let t = t.clone();
            view! { <p style="margin: 0; color: var(--m-text); white-space: pre-wrap">{t}</p> }
        }
        None => View::Empty,
    };
    let agenda: View = if m.agenda.is_empty() {
        View::Empty
    } else {
        let items: Vec<View> = m
            .agenda
            .iter()
            .map(|(name, minutes)| {
                let name = name.clone();
                let minutes = if *minutes > 0 {
                    format!("{minutes} min")
                } else {
                    String::new()
                };
                view! {
                    <div style="display: flex; gap: 12px">
                        <span style="flex: 1; min-width: 0; color: var(--m-text)">{name}</span>
                        <span style="flex: none; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{minutes}</span>
                    </div>
                }
            })
            .collect();
        view! {
            {group("Agenda")}
            <div style={CARD}>{items}</div>
        }
    };
    let open = on_plan(l, |_, item, _, o| o.open_chat = Some(item.channel.clone()));
    let join = on_plan(l, |_, item, _, o| {
        o.join = crate::systems::private_chat::group_of(&item.channel);
    });
    let join_words = m.join.clone();
    let buttons = pair(
        view! { <Button icon={icon(icons::CHAT)} onclick={open}>"Open chat"</Button> },
        view! { <Button icon={icon(icons::VIDEOCAM)} disabled={!m.can_join} onclick={join}>{join_words}</Button> },
    );
    let own: View = match &m.own {
        Some((skip, last)) => {
            let skip_go = on_plan(l, |_, item, plan, o| {
                o.meeting = Some(cal::toggle_skip(item, plan));
            });
            let stop: View = if *last {
                View::Empty
            } else {
                let stop_go = on_plan(l, |_, item, plan, o| {
                    o.meeting = Some(cal::stop_after(item, plan));
                });
                view! { <Button icon={icon(icons::STOP)} onclick={stop_go}>"Stop after this"</Button> }
            };
            let skip = skip.clone();
            view! {
                {group("You planned this")}
                {pair(view! { <Button icon={icon(icons::STOP)} onclick={skip_go}>{skip}</Button> }, stop)}
            }
        }
        None => View::Empty,
    };
    view! {
        {skipped}
        {answer}
        <div style={CARD}>
            {names("Going", &m.going)}
            {names("Maybe", &m.maybe)}
            {names("Can't come", &m.no)}
        </div>
        {text}
        {agenda}
        {buttons}
        {own}
    }
}

fn meetup_part(l: &ChatsLive, m: &MeetupV) -> View {
    let walk: View = match m.walk {
        Some(min) => {
            let text = format!("About {min} min walk from where the map is");
            view! {
                <p style="display: flex; align-items: center; gap: 8px; margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm)">
                    {icon(icons::WALK)}<span>{text}</span>
                </p>
            }
        }
        None => View::Empty,
    };
    let going: View = if m.mine {
        view! { <Button icon={icon(icons::CHECK)} disabled=true>"I'm going"</Button> }
    } else if m.over {
        View::Empty
    } else {
        let go = on_point(l, |item, point, o| {
            o.meetup_going = Some((item.channel.clone(), point.clone()));
        });
        view! { <Button variant=Variant::Primary icon={icon(icons::CHECK)} onclick={go}>"I'm going"</Button> }
    };
    let walk_there = on_point(l, |_, point, o| {
        let name = point.name.clone().unwrap_or_else(|| "Meet-up".into());
        o.route_to = Some((point.latitude, point.longitude, name));
    });
    let open = on_point(l, |item, _, o| o.open_chat = Some(item.channel.clone()));
    let show = on_point(l, |_, point, o| o.go_to = Some(point.clone()));
    view! {
        {walk}
        <div style={CARD}>{names("Going", &m.going)}</div>
        {pair(going, view! { <Button icon={icon(icons::WALK)} onclick={walk_there}>"Walk there"</Button> })}
        {pair(
            view! { <Button icon={icon(icons::CHAT)} onclick={open}>"Open chat"</Button> },
            view! { <Button icon={icon(icons::PIN)} onclick={show}>"Show in world"</Button> },
        )}
        {note("Going shares your location with this chat from 15 minutes before, until you arrive or an hour after the start.")}
    }
}

// ─── Plan something ──────────────────────────────────────────────────────────

fn new_page(l: &ChatsLive) -> View {
    let kind: Rc<dyn Fn(&NewKind)> = {
        let l = l.clone();
        Rc::new(move |k: &NewKind| {
            let k = *k;
            let go = l.later(move |w, _| w.calendar.new_kind = k);
            go();
        })
    };
    let options = vec![
        Opt::new(NewKind::Meetup, "Meet up"),
        Opt::new(NewKind::Meeting, "Meeting"),
    ];
    let what = {
        let k = l.calendar.kind.clone();
        move || -> View {
            note(if k.get() == NewKind::Meetup {
                "A place and a time. The place is where the map is; move the map first to choose another."
            } else {
                "A call, with an agenda. Everybody in the chat can say whether they come."
            })
        }
    };
    let day = {
        let n = l.calendar.new.clone();
        move || -> View {
            let text = format!("On {}", n.get().day);
            view! { <h2 style="margin: 0; padding-inline: 4px; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{text}</h2> }
        }
    };
    let chats = {
        let l = l.clone();
        move || -> View {
            let v = l.calendar.new.get();
            if v.chats.is_empty() {
                return note("No private chats or groups yet: a plan goes to the people in one.");
            }
            let items: Vec<View> = v
                .chats
                .iter()
                .map(|(key, name, on)| {
                    let key = key.clone();
                    let pick = l.on(move |w, _| w.calendar.new_chat = Some(key.clone()));
                    let end: View = if *on {
                        view! { <span style="color: var(--m-accent-text)">{icon(icons::CHECK)}</span> }
                    } else {
                        View::Empty
                    };
                    let name = name.clone();
                    view! { <ListItem icon={icon(icons::CHAT)} end={end} onclick={pick}>{name}</ListItem> }
                })
                .collect();
            view! { <List>{items}</List> }
        }
    };
    let next = out(l, |w, o| {
        w.calendar.new_next(crate::utils::clock::unix_ms(), o)
    });
    let body = body_style(l);
    view! {
        {sub_head(l, "Plan something")}
        <div style={body}>
            {day}
            <FieldSegmented model={l.calendar.kind.clone()} options={options} onchange={Some(kind)} id="calendar-new-kind" />
            {what}
            {group("In which chat")}
            {chats}
            <div style="display: flex; flex-direction: column">
                <Button variant=Variant::Primary disabled={l.calendar.next_off.clone()} onclick={next} title="Choose a chat first">"Next: where and when"</Button>
            </div>
        </div>
    }
}

// ─── Settings ────────────────────────────────────────────────────────────────

fn settings_page(l: &ChatsLive) -> View {
    let c = &l.calendar;
    let declined = pref(l, |c, v| c.book.prefs.show_declined = v);
    let coming = pref(l, |c, v| c.book.prefs.coming_up = v);
    let leave = pref(l, |c, v| c.book.prefs.leave_reminder = v);
    let meeting = pref(l, |c, v| c.book.prefs.meeting_reminder = v);
    let communities = {
        let l = l.clone();
        move || -> View {
            let v = l.calendar.settings.get();
            if v.communities.is_empty() {
                return note("Communities you are in can be limited here.");
            }
            let boxes: Vec<View> = v
                .communities
                .iter()
                .map(|(id, name, only)| {
                    let id2 = id.clone();
                    let set = pref(&l, move |c, v| c.set_answered_only(&id2, v));
                    let label = name.clone();
                    let key = format!("calendar-only-{id}");
                    view! { <FieldCheckbox model={signal(*only)} label={label} onchange={set} id={key} /> }
                })
                .collect();
            View::Fragment(boxes)
        }
    };
    let kept = {
        let s = l.calendar.settings.clone();
        move || -> View { note(cal::plans_note(s.get().plans)) }
    };
    let body = body_style(l);
    view! {
        {sub_head(l, "Calendar settings")}
        <div style={body}>
            {group("Show")}
            <FieldCheckbox model={c.show_declined.clone()} label="Plans I can't come to, faded" onchange={declined} id="calendar-declined" />
            <FieldCheckbox model={c.coming_up.clone()} label="Coming up, on the Community list" onchange={coming} id="calendar-coming" />
            {group("Only the plans I answered, from")}
            {communities}
            {group("Remind me")}
            <FieldCheckbox model={c.leave_reminder.clone()} label="When it's time to leave for a meet-up" onchange={leave} id="calendar-leave" />
            <FieldCheckbox model={c.meeting_reminder.clone()} label="10 minutes before a meeting I said Going or Maybe to" onchange={meeting} id="calendar-meeting" />
            {group("On this device")}
            {kept}
        </div>
    }
}
