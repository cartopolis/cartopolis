//! The lens drawn by the component library (`lib_host`), beside egui's
//! (`scenario::render_scenario_chrome`): the key strip over the map with
//! the sheet put away, and the one surface that holds the rest — the
//! figures with their key, the place under the crosshair, the clock and the
//! tracks, and the shelf of lenses (`docs/mockups/lenses/`,
//! `scenarios.md`).
//!
//! On with `CARTO_UI_LIB=scenario` (or `1`), on the web `?ui=lib`. egui's
//! system steps aside (`ScenarioView::lib`) but keeps what only it can do
//! (the mode's frame round the map and the marks on it are
//! `lib_scenario_map`'s): it works out whether the lens stands aside (the dock's
//! reasons, the keyboard, the route sheet, the place card), puts the sheet
//! away when a drag starts on the map, finds the place under the
//! crosshair, publishes the lens's slot for the pins (`lens_rect`) and the
//! whole lens as data (`ScenarioView::lib_view`, built by the helpers its
//! own bodies call). A tap here goes through `ScenarioView::act`, which
//! egui's controls call too.
//!
//! **What differs from egui's.** A band is singled out by a tap only: the
//! library has no hover, so pointing at one previews nothing. The grabber
//! is a tap that puts the sheet away, not a drag. An exercise's mode is
//! outlined rather than striped, and the forecast's part of the clock's
//! rail is shaded rather than hatched. Sources fold open under the foot
//! instead of in a menu.

use std::cell::RefCell;
use std::rc::Rc;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
// The library's, not Bevy's: an explicit import outranks the two globs.
use mithril::prelude::*;
use mithril::prelude::{Button, Event};

use crate::systems::layer_state::DataLayers;
use crate::systems::lib_place_card::{grabber, icon};
use crate::systems::lib_settings::LibScreens;
use crate::systems::scenario::{
    LensAct, LensClock, LensFigure, LensGeom, LensHead, LensHere, LensKey, LensPart, LensPlace,
    LensRail, LensScale, LensShelf, LensView, ModeChip, ScenarioView, Sheet,
};
use crate::systems::theme::tokens::hex;
use crate::systems::{icons, theme};

/// The lens's name in the library's host.
const KEY: &str = "scenario";

#[derive(Clone)]
struct Live {
    geom: Signal<LensGeom>,
    part: Signal<LensPart>,
    head: Signal<LensHead>,
    key: Signal<LensKey>,
    here: Signal<Option<LensHere>>,
    clock: Signal<Option<LensClock>>,
    rail: Signal<Option<LensRail>>,
    /// The clock on its rail: the slider's own model.
    t: Signal<f64>,
    figures: Signal<Vec<LensFigure>>,
    scale: Signal<LensScale>,
    sources: Signal<Vec<String>>,
    /// The sources folded open under the foot.
    show_sources: Signal<bool>,
    place: Signal<LensPlace>,
    shelf: Signal<LensShelf>,
    waiting: Signal<String>,
    acts: Rc<RefCell<Vec<LensAct>>>,
}

/// Write a signal only when the value moved: a write re-runs what reads it.
fn put<T: Clone + PartialEq + 'static>(s: &Signal<T>, v: T) {
    if s.peek(|now| *now != v) {
        s.set(v);
    }
}

impl Live {
    fn new(v: &LensView, geom: LensGeom) -> Self {
        Live {
            geom: signal(geom),
            part: signal(v.part),
            head: signal(v.head.clone()),
            key: signal(v.key.clone()),
            here: signal(v.here.clone()),
            clock: signal(v.clock.clone()),
            rail: signal(v.rail.clone()),
            t: signal(v.t),
            figures: signal(v.figures.clone()),
            scale: signal(v.scale.clone()),
            sources: signal(v.sources.clone()),
            show_sources: signal(false),
            place: signal(v.place.clone()),
            shelf: signal(v.shelf.clone()),
            waiting: signal(v.waiting.clone()),
            acts: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Everything shown, from the published lens, into the signals that
    /// moved. The rail's value is left alone in a frame the person moved
    /// it: the published lens is a frame behind the move.
    fn mirror(&self, v: &LensView, geom: LensGeom, seeked: bool) {
        put(&self.geom, geom);
        put(&self.part, v.part);
        put(&self.head, v.head.clone());
        put(&self.key, v.key.clone());
        put(&self.here, v.here.clone());
        put(&self.clock, v.clock.clone());
        put(&self.rail, v.rail.clone());
        if !seeked {
            put(&self.t, v.t);
        }
        put(&self.figures, v.figures.clone());
        put(&self.scale, v.scale.clone());
        put(&self.sources, v.sources.clone());
        put(&self.place, v.place.clone());
        put(&self.shelf, v.shelf.clone());
        put(&self.waiting, v.waiting.clone());
    }

    fn on(&self, a: LensAct) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().push(a.clone())) as Handler
    }

    fn act(&self, a: LensAct) -> Option<Handler> {
        Some(self.on(a))
    }

    /// Several asks for one tap, in order.
    fn on_all(&self, all: Vec<LensAct>) -> Handler {
        let acts = self.acts.clone();
        Rc::new(move |_: &Event| acts.borrow_mut().extend(all.iter().cloned())) as Handler
    }
}

thread_local! {
    static MOUNTED: RefCell<Option<Live>> = const { RefCell::new(None) };
}

/// Mounts, bridges and unmounts the lens. `PostUpdate`, before `DriveUi`,
/// as every library screen.
pub fn drive_lib_scenario(
    _main: NonSendMarker,
    lib: Res<LibScreens>,
    mut view: ResMut<ScenarioView>,
    mut layers: ResMut<DataLayers>,
) {
    if !lib.scenario {
        return;
    }
    MOUNTED.with(|m| {
        let mut m = m.borrow_mut();
        // The taps of the frame, through the same `act` egui's controls call.
        let mut seeked = false;
        if let Some(live) = m.as_ref() {
            let acts: Vec<LensAct> = std::mem::take(&mut *live.acts.borrow_mut());
            for a in acts {
                seeked |= matches!(a, LensAct::Seek(_));
                if let Some(wanted) = view.act(a, layers.scenario.as_deref()) {
                    layers.scenario = wanted;
                }
            }
        }
        let Some(geom) = view.lib_view.geom else {
            if m.take().is_some() {
                crate::systems::lib_host::clear_keyed(KEY);
            }
            return;
        };
        match m.as_ref() {
            Some(live) => live.mirror(&view.lib_view, geom, seeked),
            None => {
                let live = Live::new(&view.lib_view, geom);
                let p = theme::p();
                crate::systems::lib_host::mount_keyed(
                    KEY,
                    lens(&live),
                    p.ink == theme::DARK.ink,
                    theme::touch_layout(),
                );
                *m = Some(live);
            }
        }
    });
}

/// A colour of the figures' own (a content palette, as on the map).
fn rgb(c: [u8; 3]) -> String {
    format!("rgb({} {} {})", c[0], c[1], c[2])
}

/// The lens's frame, where egui's stands.
fn frame_style(g: LensGeom) -> String {
    let edge = if g.sheet {
        format!("bottom: {:.0}px", g.edge)
    } else {
        format!("top: {:.0}px", g.edge)
    };
    format!(
        "position: absolute; left: {:.0}px; {edge}; width: {:.0}px; max-height: {:.0}px; \
         display: flex; flex-direction: column; background: var(--m-surface); \
         border: 1px solid var(--m-border); border-radius: var(--m-radius-lg); \
         box-shadow: var(--m-shadow-lg); overflow: hidden",
        g.left, g.width, g.max_height
    )
}

/// A quiet line.
fn dim(text: String) -> View {
    view! {
        <p style="margin: 0; color: var(--m-text-dim); font-size: var(--m-font-size-sm); line-height: 1.45">{text}</p>
    }
}

/// A group's heading, as the library's section titles are set.
fn group(text: String) -> View {
    view! {
        <p style="margin: 8px 0 0; padding-inline: 4px; color: var(--m-text-dim); font-size: var(--m-font-size-sm); font-weight: 650; letter-spacing: 0.06em; text-transform: uppercase">{text}</p>
    }
}

/// A row of a name and a value at the right.
fn pair(name: String, value: String) -> View {
    view! {
        <div style="display: flex; align-items: baseline; gap: 12px; padding-inline: 4px; font-size: var(--m-font-size-sm)">
            <span style="flex: 1; min-width: 0; color: var(--m-text)">{name}</span>
            <span style="color: var(--m-text-dim)">{value}</span>
        </div>
    }
}

/// The mode as a chip: the mode's colour, an exercise outlined in it.
fn mode_chip(m: ModeChip) -> View {
    let ink = hex(theme::DARK.ink);
    let colour = hex(m.colour);
    let look = if m.striped {
        format!("background: {ink}; color: {colour}; border: 1px solid {colour}")
    } else {
        format!("background: {colour}; color: {ink}")
    };
    let style = format!(
        "flex: none; padding: 2px 10px; border-radius: var(--m-radius-full); font-size: 12px; font-weight: 650; white-space: nowrap; {look}"
    );
    view! { <span style={style}>{m.word}</span> }
}

/// A picked row's plate: the accent's soft fill and its edge.
fn lit(on: bool) -> &'static str {
    if on {
        "border-radius: var(--m-radius); background: var(--m-accent-soft); border: 1px solid var(--m-accent)"
    } else {
        "border-radius: var(--m-radius); border: 1px solid transparent"
    }
}

/// The sheet's three views as one control under its header.
fn tabs(l: &Live, now: Sheet) -> View {
    let model = signal(now);
    let acts = l.acts.clone();
    let change: Rc<dyn Fn(&Sheet)> =
        Rc::new(move |s: &Sheet| acts.borrow_mut().push(LensAct::Sheet(*s)));
    let options = vec![
        Opt::new(Sheet::Figures, "Figures"),
        Opt::new(Sheet::Place, "This place"),
        Opt::new(Sheet::Shelf, "All lenses"),
    ];
    view! {
        <div style="flex: none; padding: 4px 16px 8px">
            <FieldSegmented model={model} options={options} size=Some(Size::Sm) onchange={Some(change)} id="lens-tabs" />
        </div>
    }
}

/// A header: what leads it, the title, and ✕ with what it does.
fn header(lead: View, title: View, close: Handler, what: &'static str) -> View {
    view! {
        <div style="flex: none; display: flex; align-items: center; gap: 12px; min-height: 52px; padding: 4px 12px 0 16px">
            {lead}
            <div style="flex: 1; min-width: 0">{title}</div>
            <CloseButton onclick={Some(close)} label={what} />
        </div>
    }
}

/// The open figure's scale as one bar that answers: a tap singles its
/// band out on the map (a second lets go), a tick stands over the band of
/// the place under the crosshair; under it the first and last band's
/// words, or the singled one's.
fn legend(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let k = l.key.get();
        let n = k.bands.len();
        if n == 0 {
            return View::Empty;
        }
        let cells: Vec<View> = k
            .bands
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let up = k.singled == Some(i);
                let shown = k.singled.is_none_or(|s| s == i);
                let bar = format!(
                    "height: {}px; border-radius: 3px; background: {}; opacity: {};{}",
                    if up { 16 } else { 12 },
                    rgb(b.colour),
                    if shown { "1" } else { "0.3" },
                    if up {
                        " border: 2px solid var(--m-text-strong)"
                    } else {
                        ""
                    },
                );
                let tap = l.on(LensAct::Band(i));
                let label = b.label.clone();
                view! {
                    <div title={label} on:click={move |e: &Event| tap(e)} style="flex: 1; min-width: 0; height: 28px; display: flex; flex-direction: column; justify-content: center; cursor: pointer">
                        <div style={bar}></div>
                    </div>
                }
            })
            .collect();
        let marker: View = match k.marker {
            Some(m) => {
                let pct = (m as f32 + 0.5) / n as f32 * 100.0;
                let style = format!(
                    "position: absolute; top: 0; left: {pct:.1}%; margin-left: -2px; width: 4px; height: 28px; border-radius: 2px; background: var(--m-text-strong)"
                );
                view! { <div style={style}></div> }
            }
            None => View::Empty,
        };
        let words: View = match k.singled {
            Some(b) => {
                let label = k.bands[b].label.clone();
                let all: View = match k.focus {
                    Some(f) => view! {
                        <Button variant=Variant::Ghost size=Size::Sm onclick={l.act(LensAct::Band(f))}>"Show all"</Button>
                    },
                    None => View::Empty,
                };
                view! {
                    <div style="display: flex; align-items: center; justify-content: space-between; gap: 12px; font-size: var(--m-font-size-sm)">
                        <span style="color: var(--m-text-strong)">{label}</span>
                        {all}
                    </div>
                }
            }
            None => {
                let (first, last) = (k.bands[0].label.clone(), k.bands[n - 1].label.clone());
                view! {
                    <div style="display: flex; justify-content: space-between; gap: 12px; color: var(--m-text-dim); font-size: 12px">
                        <span>{first}</span>
                        <span>{last}</span>
                    </div>
                }
            }
        };
        view! {
            <div style="display: flex; flex-direction: column; gap: 4px">
                <div style="position: relative; display: flex; gap: 2px">{cells}{marker}</div>
                {words}
            </div>
        }
    })
    .into_view()
}

/// With the sheet put away: the figure and its year as a row that raises
/// the sheet, and its scale.
fn strip(l: &Live) -> View {
    let title = {
        let key = l.key.clone();
        move || key.get().title
    };
    let raise = l.on(LensAct::Sheet(Sheet::Figures));
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px; padding: 12px 16px 16px">
            <div title="Figures, this place and other lenses" on:click={move |e: &Event| raise(e)} style="display: flex; align-items: center; gap: 8px; min-height: 36px; cursor: pointer">
                <span style="flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--m-text-strong); font-size: var(--m-font-size-sm); font-weight: 600">{title}</span>
                <span style="color: var(--m-text-dim)">{icon(icons::CHEVRON_RIGHT)}</span>
            </div>
            {legend(l)}
        </div>
    }
}

/// The place under the crosshair and the figure there, as a row that
/// opens the place.
fn here_row(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let Some(h) = l.here.get() else {
            return dim("Nothing is published right under the crosshair".to_owned());
        };
        let edge: View = match h.edge {
            Some(c) => {
                let style = format!(
                    "display: block; width: 4px; height: 28px; border-radius: var(--m-radius-full); background: {}",
                    rgb(c)
                );
                view! { <span style={style}></span> }
            }
            None => view! { <span style="display: block; width: 4px; height: 28px"></span> },
        };
        let value = h.value.clone();
        let end: View = view! {
            <span style="font-size: 20px; font-weight: 600; color: var(--m-text-strong)">{value}</span>
        };
        let below = h.below.clone();
        view! {
            <div style="border-radius: var(--m-radius); background: var(--m-surface-raised); border: 1px solid var(--m-border)" title="Every figure for this place">
                <ListItem icon={edge} sub={below.into_view()} end={end} chevron=true onclick={l.act(LensAct::Sheet(Sheet::Place))}>{h.name.clone()}</ListItem>
            </div>
        }
    })
    .into_view()
}

/// The pause sign: the icon font has no pause.
fn pause() -> View {
    const BAR: &str = "display: block; width: 4px; height: 14px; border-radius: 1px; background: var(--m-on-accent)";
    view! {
        <span style="display: flex; gap: 4px; align-items: center; justify-content: center; width: 20px; height: 20px">
            <span style={BAR}></span>
            <span style={BAR}></span>
        </span>
    }
}

/// The clock: play, where it stands, the speeds; the rail, the forecast's
/// share of it, its ends.
fn clock(l: &Live) -> View {
    let head = {
        let l = l.clone();
        move || -> View {
            let Some(c) = l.clock.get() else {
                return View::Empty;
            };
            let (glyph, title) = if c.playing {
                (pause(), "Pause")
            } else {
                (icon(icons::PLAY), "Play")
            };
            let speeds: Vec<View> = c
                .speeds
                .iter()
                .map(|&(speed, word)| {
                    let variant = if c.speed == speed {
                        Variant::Primary
                    } else {
                        Variant::Ghost
                    };
                    view! {
                        <Button variant={variant} size=Size::Sm onclick={l.act(LensAct::Speed(speed))}>{word}</Button>
                    }
                })
                .collect();
            view! {
                <div style="display: flex; align-items: center; gap: 12px">
                    <Button variant=Variant::Primary icon={glyph} icon_only=true onclick={l.act(LensAct::Play)} title={title} />
                    <span style="flex: 1; min-width: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">{c.label}</span>
                    <div style="display: flex; gap: 4px">{speeds}</div>
                </div>
            }
        }
    };
    let rail = {
        let l = l.clone();
        move || -> View {
            let Some(r) = l.rail.get() else {
                return View::Empty;
            };
            let acts = l.acts.clone();
            let seek: Rc<dyn Fn(&f64)> =
                Rc::new(move |v: &f64| acts.borrow_mut().push(LensAct::Seek(*v)));
            let hatch: View = match r.forecast {
                Some(f) => {
                    let measured = format!(
                        "width: {:.1}%; height: 6px; background: var(--m-border-strong)",
                        f * 100.0
                    );
                    view! {
                        <div style="display: flex; height: 6px; border-radius: 3px; overflow: hidden; background: var(--m-surface-raised); border: 1px solid var(--m-border)">
                            <div style={measured}></div>
                        </div>
                    }
                }
                None => View::Empty,
            };
            let from: View = match r.forecast_from {
                Some(f) => view! { <span>{f}</span> },
                None => View::Empty,
            };
            view! {
                <FieldSlider model={l.t.clone()} min=0.0 max={r.max} step=1.0 show_value=false onchange={Some(seek)} id="lens-clock" />
                {hatch}
                <div style="display: flex; justify-content: space-between; gap: 12px; color: var(--m-text-dim); font-size: 12px">
                    <span>{r.start}</span>
                    {from}
                    <span>{r.end}</span>
                </div>
            }
        }
    };
    view! {
        <div style="display: flex; flex-direction: column; gap: 8px">
            {head}
            {rail}
        </div>
    }
}

/// A figure's scale drawn small, and where the place here falls on it.
fn mini_ramp(ramp: &[[u8; 3]], band: Option<usize>) -> View {
    let n = ramp.len().max(1);
    let cells: Vec<View> = ramp
        .iter()
        .map(|c| {
            let style = format!("flex: 1; height: 6px; background: {}", rgb(*c));
            view! { <span style={style}></span> }
        })
        .collect();
    let tick: View = match band {
        Some(b) => {
            let pct = (b as f32 + 0.5) / n as f32 * 100.0;
            let style = format!(
                "position: absolute; top: -4px; left: {pct:.1}%; margin-left: -1px; width: 3px; height: 14px; border-radius: 1px; background: var(--m-text-strong)"
            );
            view! { <span style={style}></span> }
        }
        None => View::Empty,
    };
    view! {
        <span style="position: relative; flex: none; display: flex; width: 40px; height: 6px; border-radius: 3px">{cells}{tick}</span>
    }
}

/// A radio: its dot in while it is the one.
fn radio(on: bool) -> View {
    let ring = if on {
        "display: flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: var(--m-radius-full); border: 2px solid var(--m-accent)"
    } else {
        "display: flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: var(--m-radius-full); border: 2px solid var(--m-border-strong)"
    };
    let dot: View = if on {
        view! { <span style="display: block; width: 8px; height: 8px; border-radius: var(--m-radius-full); background: var(--m-accent)"></span> }
    } else {
        View::Empty
    };
    view! { <span style={ring}>{dot}</span> }
}

/// Every figure of the lens to pick one from: a pick, never a tick.
fn figures_list(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let rows: Vec<View> = l
            .figures
            .get()
            .into_iter()
            .map(|f| {
                let heading = match f.group {
                    Some(g) => group(g),
                    None => View::Empty,
                };
                let value = f.value.unwrap_or_default();
                let end: View = view! {
                    <span style="display: flex; align-items: center; gap: 10px">
                        {mini_ramp(&f.ramp, f.band)}
                        <span style="min-width: 48px; text-align: right; color: var(--m-text-strong)">{value}</span>
                    </span>
                };
                let click = l.act(LensAct::Open(f.key.clone()));
                view! {
                    {heading}
                    <div style={lit(f.on)}>
                        <ListItem icon={radio(f.on)} end={end} onclick={click}>{f.label}</ListItem>
                    </div>
                }
            })
            .collect();
        view! { <div style="display: flex; flex-direction: column; gap: 4px">{rows}</div> }
    })
    .into_view()
}

/// Under the figures: a derived state's bands with their counts, the
/// attribution, what is next to be reached, the tracks.
fn scale_card(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let s = l.scale.get();
        let counted: View = match &s.counted {
            Some(heading) => {
                let rows: Vec<View> = s
                    .counts
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let swatch = format!(
                            "display: block; width: 12px; height: 12px; border-radius: 2px; background: {}",
                            rgb(c.colour)
                        );
                        let count = c.count.to_string();
                        view! {
                            <div style={lit(c.on)}>
                                <ListItem icon={view! { <span style={swatch}></span> }} end={count.into_view()} onclick={l.act(LensAct::Band(i))}>{c.label.clone()}</ListItem>
                            </div>
                        }
                    })
                    .collect();
                view! {
                    {group(heading.clone())}
                    <div style="display: flex; flex-direction: column; gap: 4px">{rows}</div>
                }
            }
            None => View::Empty,
        };
        let attribution: View = match &s.attribution {
            Some(a) => view! {
                <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: 12px">{a.clone()}</p>
            },
            None => View::Empty,
        };
        let next: View = if s.next.is_empty() {
            View::Empty
        } else {
            let rows: Vec<View> = s
                .next
                .iter()
                .map(|(name, when)| pair(name.clone(), when.clone()))
                .collect();
            view! { {group("Next to be reached".to_owned())}{rows} }
        };
        let tracks: View = if s.has_tracks {
            let rows: Vec<View> = s
                .tracks
                .iter()
                .map(|t| {
                    view! {
                        <div style={lit(t.selected)}>
                            <ListItem onclick={l.act(LensAct::Track(t.index))}>{t.label.clone()}</ListItem>
                        </div>
                    }
                })
                .collect();
            view! { {group("Tracks".to_owned())}{rows} }
        } else {
            View::Empty
        };
        let picked: View = match &s.picked {
            Some(p) => {
                let reach: View = if p.reach.is_empty() {
                    View::Empty
                } else {
                    let rows: Vec<View> = p
                        .reach
                        .iter()
                        .map(|(name, when)| pair(name.clone(), when.clone()))
                        .collect();
                    view! { {dim("Its cone reaches".to_owned())}{rows} }
                };
                view! {
                    <div style="display: flex; flex-direction: column; gap: 6px; padding: 12px 16px; border-radius: var(--m-radius); background: var(--m-surface-raised)">
                        <p style="margin: 0; color: var(--m-text-strong); font-size: var(--m-font-size-sm)">{p.line.clone()}</p>
                        {reach}
                        {dim(p.cone.clone())}
                    </div>
                }
            }
            None => View::Empty,
        };
        view! {
            {counted}
            {attribution}
            {next}
            {tracks}
            {picked}
        }
    })
    .into_view()
}

/// The foot: the sources folded open, and the way to no lens.
fn figures_foot(l: &Live) -> View {
    let sources = {
        let l = l.clone();
        move || -> View {
            if !l.show_sources.get() {
                return View::Empty;
            }
            let lines: Vec<View> = l.sources.get().into_iter().map(dim).collect();
            view! { <div style="display: flex; flex-direction: column; gap: 4px; padding-bottom: 12px">{lines}</div> }
        }
    };
    let toggle: Handler = {
        let show = l.show_sources.clone();
        Rc::new(move |_: &Event| show.set(!show.get_untracked()))
    };
    view! {
        <div style="flex: none; padding: 12px 16px 16px; border-top: 1px solid var(--m-border)">
            {sources}
            <div style="display: flex; align-items: center; gap: 8px">
                <Button variant=Variant::Ghost size=Size::Sm onclick={Some(toggle)}>"Sources"</Button>
                <div style="flex: 1"></div>
                <Button size=Size::Sm onclick={l.act(LensAct::Close)} title="Turn the lens off">"Lens off"</Button>
            </div>
        </div>
    }
}

/// The lens's header: its mode, its name, ✕ to put the sheet away.
fn lens_header(l: &Live) -> View {
    let l = l.clone();
    (move || -> View {
        let h = l.head.get();
        let lead = match h.mode {
            Some(m) => mode_chip(m),
            None => View::Empty,
        };
        let title = view! {
            <p style="margin: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 17px; font-weight: 600; color: var(--m-text-strong)">{h.name}</p>
        };
        header(
            lead,
            title,
            l.on(LensAct::Sheet(Sheet::Hidden)),
            "Hide; the lens stays on",
        )
    })
    .into_view()
}

/// Figures: the header and the views, the key and the place here, the
/// clock, the figures, what is under them, and the foot.
fn figures(l: &Live) -> View {
    let honest = {
        let key = l.key.clone();
        move || dim(key.get().honest)
    };
    view! {
        {lens_header(l)}
        {tabs(l, Sheet::Figures)}
        <div style="min-height: 0; overflow-y: auto; padding: 8px 16px 20px; display: flex; flex-direction: column; gap: 16px">
            <div style="display: flex; flex-direction: column; gap: 8px">
                {honest}
                {legend(l)}
            </div>
            {here_row(l)}
            {clock(l)}
            {figures_list(l)}
            {scale_card(l)}
        </div>
        {figures_foot(l)}
    }
}

/// One row of the place's figures: the figure, its value here, the same
/// for the town and the country; `head` sets the column names instead.
fn place_row(label: String, value: String, context: String, head: bool) -> View {
    let (size, ink) = if head {
        ("12px", "var(--m-text-dim)")
    } else {
        ("var(--m-font-size)", "var(--m-text-strong)")
    };
    let value_style =
        format!("flex: none; width: 72px; text-align: right; font-size: {size}; color: {ink}");
    view! {
        <span style="flex: 1; min-width: 0; color: var(--m-text); font-size: var(--m-font-size-sm)">{label}</span>
        <span style={value_style}>{value}</span>
        <span style="flex: none; width: 96px; text-align: right; color: var(--m-text-dim); font-size: 12px">{context}</span>
    }
}

/// Place: the area under the crosshair with every figure of the lens, each
/// beside its town and its country. A row puts that figure on the map.
fn place(l: &Live) -> View {
    let title = {
        let place = l.place.clone();
        move || -> View {
            view! {
                <p style="margin: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 20px; font-weight: 600; color: var(--m-text-strong)">{place.get().name}</p>
            }
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            let p = l.place.get();
            if !p.found {
                return dim(
                    "Nothing is published for the place under the crosshair. Move the map to one."
                        .to_owned(),
                );
            }
            let rows: Vec<View> = p
                .rows
                .iter()
                .map(|r| {
                    let style = format!(
                        "display: flex; align-items: center; gap: 8px; padding: 8px 10px; cursor: pointer; {}",
                        lit(r.on)
                    );
                    let tap = l.on(LensAct::Open(r.key.clone()));
                    view! {
                        <div style={style} on:click={move |e: &Event| tap(e)}>
                            {place_row(r.label.clone(), r.value.clone(), r.context.clone(), false)}
                        </div>
                    }
                })
                .collect();
            view! {
                {dim(p.about.clone())}
                <div style="display: flex; align-items: center; gap: 8px; padding: 0 10px">
                    {place_row(String::new(), "here".to_owned(), p.compare.to_owned(), true)}
                </div>
                <div style="display: flex; flex-direction: column; gap: 2px">{rows}</div>
                <p style="margin: 0; padding-inline: 4px; color: var(--m-text-dim); font-size: 12px">{p.attribution.clone()}</p>
            }
        }
    };
    view! {
        {header(View::Empty, title.into_view(), l.on(LensAct::Sheet(Sheet::Hidden)), "Hide; the lens stays on")}
        {tabs(l, Sheet::Place)}
        <div style="min-height: 0; overflow-y: auto; padding: 8px 16px 20px; display: flex; flex-direction: column; gap: 12px">
            {body}
        </div>
    }
}

/// Shelf: the lenses there are, under the question each answers. A lens
/// opens at the figure it had, or its first.
fn shelf(l: &Live) -> View {
    let tabs_up = {
        let l = l.clone();
        move || -> View {
            if l.shelf.get().tabs {
                tabs(&l, Sheet::Shelf)
            } else {
                View::Empty
            }
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            let groups: Vec<View> = l
                .shelf
                .get()
                .groups
                .into_iter()
                .map(|(question, cards)| {
                    let cards: Vec<View> = cards
                        .into_iter()
                        .map(|c| {
                            let edge = if c.is_open {
                                format!("border: 2px solid {}", hex(c.mode.colour))
                            } else {
                                "border: 2px solid transparent".to_owned()
                            };
                            let style = format!(
                                "display: flex; flex-direction: column; gap: 6px; padding: 12px 16px; border-radius: var(--m-radius-lg); background: var(--m-surface-raised); cursor: pointer; {edge}"
                            );
                            let tap = l.on_all(vec![
                                LensAct::Open(c.opens.clone()),
                                LensAct::Sheet(Sheet::Figures),
                            ]);
                            view! {
                                <div style={style} title={c.name.clone()} on:click={move |e: &Event| tap(e)}>
                                    <p style="margin: 0; font-size: var(--m-font-size); font-weight: 600; color: var(--m-text-strong)">{c.name.clone()}</p>
                                    {dim(c.summary.clone())}
                                    <div style="display: flex; align-items: center; gap: 10px">
                                        {mode_chip(c.mode)}
                                        <span style="color: var(--m-text-dim); font-size: var(--m-font-size-sm)">{c.figures.clone()}</span>
                                    </div>
                                </div>
                            }
                        })
                        .collect();
                    view! {
                        <p style="margin: 8px 0 0; font-size: var(--m-font-size); color: var(--m-text-strong)">{question}</p>
                        <div style="display: flex; flex-direction: column; gap: 8px">{cards}</div>
                    }
                })
                .collect();
            groups.into_view()
        }
    };
    let title = view! {
        <h1 style="margin: 0; font-size: 19px; font-weight: 600; color: var(--m-text-strong)">"Lenses"</h1>
    };
    view! {
        {header(View::Empty, title, l.on(LensAct::Sheet(Sheet::Hidden)), "Close")}
        {tabs_up}
        <div style="min-height: 0; overflow-y: auto; padding: 8px 16px 20px; display: flex; flex-direction: column; gap: 12px">
            {dim("Figures about places, read over the map. One lens at a time.".to_owned())}
            {body}
        </div>
    }
}

/// A lens on its way, or one that would not open, with the way out.
fn waiting(l: &Live) -> View {
    let words = {
        let waiting = l.waiting.clone();
        move || -> View {
            view! { <p style="margin: 0; color: var(--m-text); font-size: var(--m-font-size-sm)">{waiting.get()}</p> }
        }
    };
    view! {
        {header(View::Empty, words.into_view(), l.on(LensAct::Close), "Turn the lens off")}
        <div style="height: 12px"></div>
    }
}

/// The lens: its frame where egui's stands, the grabber on a sheet, and
/// whichever surface is up.
fn lens(l: &Live) -> View {
    let frame = {
        let geom = l.geom.clone();
        move || frame_style(geom.get())
    };
    let grab = {
        let l = l.clone();
        move || -> View {
            if l.geom.get().sheet && l.part.get() != LensPart::Strip {
                grabber(l.on(LensAct::Sheet(Sheet::Hidden)))
            } else {
                View::Empty
            }
        }
    };
    let body = {
        let l = l.clone();
        move || -> View {
            match l.part.get() {
                LensPart::Strip => strip(&l),
                LensPart::Figures => figures(&l),
                LensPart::Place => place(&l),
                LensPart::Shelf => shelf(&l),
                LensPart::Waiting => waiting(&l),
            }
        }
    };
    // The frame listens, so the pointer over its padding is the library's
    // too (`UiPointer` counts only nodes that listen).
    view! {
        <div style={frame} on:click={|_: &Event| {}}>
            {grab}
            {body}
        </div>
    }
}
