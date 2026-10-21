//! Where the pointer was, and **when**, as the browser recorded it.
//!
//! A drag moves the camera by the cursor's movement since the last frame. Bevy's
//! `CursorMoved` says where the cursor went but not when, so a frame that the
//! browser handed no pointer event moves the camera not at all and the next one
//! twice as far. Chrome lines pointer events up with animation frames, one per
//! frame, and hides that. Firefox does not: over a steady scripted drag on a Mac
//! (2026-10-21), 27% of its frames got no pointer event and 7% got two, and the
//! map juddered at a steady frame rate.
//!
//! Every pointer event carries `timeStamp`, the moment the browser created it, on
//! the same clock as `performance.now()`. This keeps those samples, each position
//! with its time (`getCoalescedEvents()` where a browser merged several into one
//! event, so none is lost), and answers [`position_at`]: the pointer's position
//! at any recent moment, interpolated between the samples either side. A drag
//! replayed a few milliseconds behind the newest sample (`free_camera`'s
//! `DragReplay`) then moves exactly as evenly as the hand did.
//!
//! Positions are CSS pixels, which are Bevy's logical pixels on the web, so a
//! difference of two is a cursor delta. Natively there is no trail: winit's
//! events carry no time either, and `position_at` answers `None`.

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use bevy::math::Vec2;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    /// About a second of samples at a 1000 Hz mouse; older ones are never asked for.
    const KEEP: usize = 1024;

    thread_local! {
        static TRAIL: RefCell<VecDeque<(f64, Vec2)>> = const { RefCell::new(VecDeque::new()) };
        /// Samples ever kept, so a reader can tell how many arrived since it looked.
        static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    fn push(t: f64, x: f64, y: f64) {
        TRAIL.with(|trail| {
            let mut trail = trail.borrow_mut();
            // Out of order only by a coalesced batch overlapping the event that
            // carried it; keep the trail monotonic.
            if trail.back().is_some_and(|&(last, _)| t < last) {
                return;
            }
            trail.push_back((t, Vec2::new(x as f32, y as f32)));
            COUNT.with(|c| c.set(c.get() + 1));
            while trail.len() > KEEP {
                trail.pop_front();
            }
        });
    }

    /// Listen on the window, in the capture phase, before anything can stop the
    /// event. Once, at startup; the listener lives as long as the page.
    pub fn init() {
        let Some(window) = web_sys::window() else {
            return;
        };
        let record = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            let Ok(e) = event.dyn_into::<web_sys::MouseEvent>() else {
                return;
            };
            let coalesced = js_sys::Reflect::get(&e, &"getCoalescedEvents".into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .and_then(|f| f.call0(&e).ok())
                .and_then(|a| a.dyn_into::<js_sys::Array>().ok())
                .filter(|a| a.length() > 0);
            match coalesced {
                Some(all) => {
                    for c in all.iter() {
                        if let Ok(c) = c.dyn_into::<web_sys::MouseEvent>() {
                            push(
                                c.time_stamp(),
                                f64::from(c.client_x()),
                                f64::from(c.client_y()),
                            );
                        }
                    }
                }
                None => push(
                    e.time_stamp(),
                    f64::from(e.client_x()),
                    f64::from(e.client_y()),
                ),
            }
        });
        let options = web_sys::AddEventListenerOptions::new();
        options.set_capture(true);
        options.set_passive(true);
        for kind in ["pointermove", "pointerdown", "pointerup"] {
            let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
                kind,
                record.as_ref().unchecked_ref(),
                &options,
            );
        }
        record.forget();
    }

    pub fn now_ms() -> Option<f64> {
        Some(web_sys::window()?.performance()?.now())
    }

    pub fn position_at(t: f64) -> Option<Vec2> {
        TRAIL.with(|trail| super::interpolate(&trail.borrow(), t))
    }

    pub fn newest() -> Option<(f64, u64)> {
        let t = TRAIL.with(|trail| trail.borrow().back().map(|&(t, _)| t))?;
        Some((t, COUNT.with(std::cell::Cell::get)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use bevy::math::Vec2;

    pub fn init() {}
    pub fn now_ms() -> Option<f64> {
        None
    }
    pub fn position_at(_t: f64) -> Option<Vec2> {
        None
    }
    pub fn newest() -> Option<(f64, u64)> {
        None
    }
}

/// The position at `t` on a time-ordered trail: interpolated between the
/// samples either side, the first before it, the newest after it.
#[must_use]
pub fn interpolate(
    trail: &std::collections::VecDeque<(f64, bevy::math::Vec2)>,
    t: f64,
) -> Option<bevy::math::Vec2> {
    let &(first_t, first) = trail.front()?;
    if t <= first_t {
        return Some(first);
    }
    // From the newest end: the moments asked for are recent.
    for i in (0..trail.len()).rev() {
        let (ta, a) = trail[i];
        if ta <= t {
            return Some(match trail.get(i + 1) {
                Some(&(tb, b)) if tb > ta => a.lerp(b, ((t - ta) / (tb - ta)) as f32),
                _ => a,
            });
        }
    }
    Some(first)
}

/// Start recording. A no-op natively.
pub fn init() {
    imp::init();
}

/// `performance.now()`, the clock the samples are on; `None` natively.
#[must_use]
pub fn now_ms() -> Option<f64> {
    imp::now_ms()
}

/// The pointer's position at `t` (ms on [`now_ms`]'s clock), interpolated
/// between the samples either side, held at the newest after it; `None` with no
/// trail.
#[must_use]
pub fn position_at(t: f64) -> Option<bevy::math::Vec2> {
    imp::position_at(t)
}

/// When the newest sample was taken, and how many samples have ever been kept.
#[must_use]
pub fn newest() -> Option<(f64, u64)> {
    imp::newest()
}
