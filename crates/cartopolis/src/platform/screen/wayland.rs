//! The screen on Wayland: the desktop portal asks the person which screen or
//! window to share, in the desktop's own dialog, and hands back a PipeWire
//! stream of it.
//!
//! The portal is D-Bus (`zbus`, pure Rust). PipeWire is a C library, and is
//! loaded only when sharing starts (`libloading`), so the build needs no
//! PipeWire headers and the app still starts on a system without it; there
//! sharing says what is missing. Every Wayland desktop that can share its
//! screen at all runs PipeWire, since the browsers share through it too.
//!
//! Only the handful of `pw_*` functions a capture stream needs are named, and
//! the two parameter blocks (the formats asked for, the buffer kinds taken)
//! are written by hand in SPA's pod layout (`spa/pod/pod.h`): the C helpers
//! for them are inline functions, which a loaded library does not export.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::sync::OnceLock;

use cartopolis_calls::video::I420;

use crate::platform::sync::Mutex;

/// A screen shared through the portal.
pub struct Grabber {
    // Dropped first: the stream ends before the portal session it reads.
    stream: Stream,
    _session: portal::Session,
}

impl Grabber {
    /// Ask the person what to share, and start reading it. Blocks for as long
    /// as the desktop's dialog is up.
    pub fn open() -> Result<Self, String> {
        let pw = pw::lib()?;
        let (session, fd, node) = portal::start()?;
        let stream = Stream::connect(pw, fd, node)?;
        Ok(Self {
            stream,
            _session: session,
        })
    }

    pub fn next(&mut self) -> Result<Option<I420>, String> {
        let shared = &self.stream.shared;
        if let Some(why) = shared.error.lock().ok().and_then(|e| e.clone()) {
            return Err(why);
        }
        let state = shared.state.load(Ordering::Relaxed);
        if shared.streamed.load(Ordering::Relaxed) && state <= STATE_UNCONNECTED {
            return Err("You stopped sharing the screen".into());
        }
        // Only the picture asked for is copied: the compositor may send many
        // more than the two or three a second a call looks at.
        shared.want.store(true, Ordering::Relaxed);
        let Some(raw) = shared.latest.lock().ok().and_then(|mut l| l.take()) else {
            return Ok(None);
        };
        Ok(I420::from_rgba(
            raw.width, raw.height, raw.stride, &raw.bytes, raw.bgra,
        ))
    }
}

/// The desktop portal's `ScreenCast` interface
/// (`org.freedesktop.portal.ScreenCast`, version 1 and up).
mod portal {
    use super::{HashMap, OwnedFd};
    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    const DEST: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";

    /// Source types: a whole monitor, a window.
    const MONITOR: u32 = 1;
    const WINDOW: u32 = 2;
    /// Cursor modes: hidden, drawn into the picture.
    const CURSOR_HIDDEN: u32 = 1;
    const CURSOR_EMBEDDED: u32 = 2;

    /// The portal session; closing it ends the sharing in the desktop too
    /// (its indicator in the top bar goes away).
    pub struct Session {
        conn: Connection,
        path: String,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            if let Ok(p) = Proxy::new(
                &self.conn,
                DEST,
                self.path.as_str(),
                "org.freedesktop.portal.Session",
            ) {
                let _ = p.call_method("Close", &());
            }
        }
    }

    fn err(e: impl std::fmt::Display) -> String {
        format!("The desktop portal would not share the screen: {e}")
    }

    /// Ask, wait for the person's choice, and open the PipeWire remote that
    /// carries it: the session, the remote's socket, and the stream's node.
    pub fn start() -> Result<(Session, OwnedFd, u32), String> {
        let conn = Connection::session().map_err(err)?;
        let cast = Proxy::new(&conn, DEST, PATH, "org.freedesktop.portal.ScreenCast")
            .map_err(|_| no_portal())?;
        let sources = cast
            .get_property::<u32>("AvailableSourceTypes")
            .map_err(|_| no_portal())?;
        let cursors = cast.get_property::<u32>("AvailableCursorModes").ok();
        let sender = conn
            .unique_name()
            .map(|n| n.as_str().trim_start_matches(':').replace('.', "_"))
            .ok_or_else(|| err("no name on the session bus"))?;
        let mut asker = Asker {
            conn: &conn,
            cast: &cast,
            sender,
            n: 0,
        };

        let mut opts = HashMap::new();
        opts.insert("session_handle_token", Value::from("cartopolis"));
        let created = asker.ask("CreateSession", opts, |o| (o,))?;
        let session_path = match created.get("session_handle").map(|v| &**v) {
            Some(Value::Str(s)) => s.to_string(),
            Some(Value::ObjectPath(p)) => p.to_string(),
            _ => return Err(err("no session in its answer")),
        };
        let session = Session {
            conn: conn.clone(),
            path: session_path.clone(),
        };
        let handle = ObjectPath::try_from(session_path.as_str()).map_err(err)?;

        let mut opts = HashMap::new();
        opts.insert("types", Value::from(sources & (MONITOR | WINDOW)));
        opts.insert("multiple", Value::from(false));
        if let Some(modes) = cursors {
            let mode = if modes & CURSOR_EMBEDDED != 0 {
                CURSOR_EMBEDDED
            } else {
                CURSOR_HIDDEN
            };
            opts.insert("cursor_mode", Value::from(mode));
        }
        asker.ask("SelectSources", opts, |o| (&handle, o))?;

        let started = asker.ask("Start", HashMap::new(), |o| (&handle, "", o))?;
        let node = first_stream(&started).ok_or_else(|| err("no stream in its answer"))?;

        let fd: zbus::zvariant::OwnedFd = cast
            .call(
                "OpenPipeWireRemote",
                &(&handle, HashMap::<&str, Value>::new()),
            )
            .map_err(err)?;
        Ok((session, fd.into(), node))
    }

    fn no_portal() -> String {
        "This desktop offers no screen sharing to apps (the desktop portal's \
         ScreenCast is missing): share it from the browser instead"
            .into()
    }

    /// `a(ua{sv})`: each stream's PipeWire node and what it shows.
    fn first_stream(results: &HashMap<String, OwnedValue>) -> Option<u32> {
        let Value::Array(streams) = &**results.get("streams")? else {
            return None;
        };
        streams.iter().find_map(|s| match s {
            Value::Structure(s) => match s.fields().first() {
                Some(Value::U32(node)) => Some(*node),
                _ => None,
            },
            _ => None,
        })
    }

    /// The portal answers a call later, as a `Response` signal on a request
    /// object whose path the caller can name in advance. Listening starts
    /// before the call, or a quick answer is missed.
    struct Asker<'a> {
        conn: &'a Connection,
        cast: &'a Proxy<'a>,
        sender: String,
        n: u32,
    }

    impl Asker<'_> {
        fn ask<B>(
            &mut self,
            method: &str,
            mut opts: HashMap<&'static str, Value<'static>>,
            body: impl FnOnce(HashMap<&'static str, Value<'static>>) -> B,
        ) -> Result<HashMap<String, OwnedValue>, String>
        where
            B: serde::Serialize + zbus::zvariant::DynamicType,
        {
            self.n += 1;
            let token = format!("cartopolis{}_{}", std::process::id(), self.n);
            let path = format!("{PATH}/request/{}/{token}", self.sender);
            let request = Proxy::new(
                self.conn,
                DEST,
                path.as_str(),
                "org.freedesktop.portal.Request",
            )
            .map_err(err)?;
            let mut answers = request.receive_signal("Response").map_err(err)?;
            opts.insert("handle_token", Value::from(token));
            self.cast.call_method(method, &body(opts)).map_err(err)?;
            let answer = answers
                .next()
                .ok_or_else(|| err("it went away before answering"))?;
            let (code, results): (u32, HashMap<String, OwnedValue>) =
                answer.body().deserialize().map_err(err)?;
            match code {
                0 => Ok(results),
                1 => Err("You did not choose a screen to share".into()),
                _ => Err(err(format!("{method} failed"))),
            }
        }
    }
}

/// `pw_stream_state`: error, unconnected, connecting, paused, streaming.
const STATE_UNCONNECTED: i32 = 0;
const STATE_STREAMING: i32 = 3;

/// One picture as PipeWire gave it, 32 bits a pixel.
struct Raw {
    width: u32,
    height: u32,
    stride: usize,
    bgra: bool,
    bytes: Vec<u8>,
}

/// What the PipeWire thread's callbacks and [`Grabber::next`] share.
struct Shared {
    pw: &'static pw::Lib,
    stream: AtomicPtr<c_void>,
    /// The negotiated video format, width and height.
    format: Mutex<Option<(u32, u32, u32)>>,
    want: AtomicBool,
    latest: Mutex<Option<Raw>>,
    state: AtomicI32,
    streamed: AtomicBool,
    error: Mutex<Option<String>>,
}

/// A PipeWire capture stream on a thread loop of its own.
struct Stream {
    shared: Box<Shared>,
    pw: &'static pw::Lib,
    thread_loop: *mut c_void,
    context: *mut c_void,
    core: *mut c_void,
    /// `spa_hook`, six pointers in C; PipeWire writes into it.
    hook: Box<[usize; 8]>,
}

// SAFETY: the raw pointers are PipeWire objects, touched from `Stream`'s own
// methods only while holding the thread loop's lock (or after it stopped).
unsafe impl Send for Stream {}

impl Stream {
    fn connect(pw: &'static pw::Lib, fd: OwnedFd, node: u32) -> Result<Self, String> {
        let shared = Box::new(Shared {
            pw,
            stream: AtomicPtr::new(std::ptr::null_mut()),
            format: Mutex::new(None),
            want: AtomicBool::new(true),
            latest: Mutex::new(None),
            state: AtomicI32::new(STATE_UNCONNECTED),
            streamed: AtomicBool::new(false),
            error: Mutex::new(None),
        });
        // SAFETY: the calls follow `pw_thread_loop`'s contract: objects are
        // made under its lock, and `shared` and `hook` outlive the stream
        // (they are freed in `Drop` only after it is destroyed).
        unsafe {
            (pw.init)(std::ptr::null_mut(), std::ptr::null_mut());
            let thread_loop = (pw.thread_loop_new)(c"cartopolis-screen".as_ptr(), std::ptr::null());
            if thread_loop.is_null() {
                return Err("PipeWire would not start a loop".into());
            }
            let mut this = Self {
                shared,
                pw,
                thread_loop,
                context: std::ptr::null_mut(),
                core: std::ptr::null_mut(),
                hook: Box::new([0; 8]),
            };
            if (pw.thread_loop_start)(thread_loop) < 0 {
                return Err("PipeWire would not start a loop".into());
            }
            (pw.thread_loop_lock)(thread_loop);
            let made = this.make(fd.into_raw_fd(), node);
            (pw.thread_loop_unlock)(thread_loop);
            made.map(|()| this)
        }
    }

    /// Under the loop's lock.
    unsafe fn make(&mut self, fd: c_int, node: u32) -> Result<(), String> {
        let pw = self.pw;
        unsafe {
            self.context = (pw.context_new)(
                (pw.thread_loop_get_loop)(self.thread_loop),
                std::ptr::null_mut(),
                0,
            );
            if self.context.is_null() {
                return Err("PipeWire would not make a context".into());
            }
            // Takes the socket, also on failure.
            self.core = (pw.context_connect_fd)(self.context, fd, std::ptr::null_mut(), 0);
            if self.core.is_null() {
                return Err("Could not reach PipeWire through the portal".into());
            }
            let props = (pw.properties_new)(
                c"media.type".as_ptr(),
                c"Video".as_ptr(),
                c"media.category".as_ptr(),
                c"Capture".as_ptr(),
                c"media.role".as_ptr(),
                c"Screen".as_ptr(),
                std::ptr::null::<c_char>(),
            );
            let stream = (pw.stream_new)(self.core, c"cartopolis-screen".as_ptr(), props);
            if stream.is_null() {
                return Err("PipeWire would not make a stream".into());
            }
            self.shared.stream.store(stream, Ordering::Relaxed);
            (pw.stream_add_listener)(
                stream,
                self.hook.as_mut_ptr().cast(),
                &EVENTS,
                std::ptr::from_ref::<Shared>(&*self.shared)
                    .cast_mut()
                    .cast(),
            );
            let formats = pod::enum_format();
            let mut params = [formats.as_ptr().cast::<c_void>()];
            let r = (pw.stream_connect)(
                stream,
                DIRECTION_INPUT,
                node,
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS,
                params.as_mut_ptr(),
                1,
            );
            if r < 0 {
                return Err(format!("PipeWire would not connect the stream ({r})"));
            }
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let pw = self.pw;
        // SAFETY: tears down in reverse, the stream under the lock; after
        // `thread_loop_stop` no callback runs, so `shared` may go.
        unsafe {
            (pw.thread_loop_lock)(self.thread_loop);
            let stream = self
                .shared
                .stream
                .swap(std::ptr::null_mut(), Ordering::Relaxed);
            if !stream.is_null() {
                (pw.stream_disconnect)(stream);
                (pw.stream_destroy)(stream);
            }
            if !self.core.is_null() {
                (pw.core_disconnect)(self.core);
            }
            (pw.thread_loop_unlock)(self.thread_loop);
            (pw.thread_loop_stop)(self.thread_loop);
            if !self.context.is_null() {
                (pw.context_destroy)(self.context);
            }
            (pw.thread_loop_destroy)(self.thread_loop);
        }
    }
}

const DIRECTION_INPUT: u32 = 0;
const FLAG_AUTOCONNECT: u32 = 1 << 0;
const FLAG_MAP_BUFFERS: u32 = 1 << 2;
const SPA_PARAM_FORMAT: u32 = 4;
const SPA_CHUNK_FLAG_CORRUPTED: i32 = 1;

/// `struct pw_stream_events`, version 2.
#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Option<unsafe extern "C" fn()>,
    io_changed: Option<unsafe extern "C" fn()>,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const c_void)>,
    add_buffer: Option<unsafe extern "C" fn()>,
    remove_buffer: Option<unsafe extern "C" fn()>,
    process: Option<unsafe extern "C" fn(*mut c_void)>,
    drained: Option<unsafe extern "C" fn()>,
    command: Option<unsafe extern "C" fn()>,
    trigger_done: Option<unsafe extern "C" fn()>,
}

static EVENTS: StreamEvents = StreamEvents {
    version: 2,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

/// `struct pw_buffer`'s first field; the rest is PipeWire's.
#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut c_void,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaData {
    kind: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

/// A callback must not unwind into C.
fn guarded(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

unsafe extern "C" fn on_state(data: *mut c_void, _old: c_int, state: c_int, error: *const c_char) {
    guarded(|| {
        // SAFETY: `data` is the `Shared` given to `stream_add_listener`.
        let shared = unsafe { &*data.cast::<Shared>() };
        shared.state.store(state, Ordering::Relaxed);
        if state == STATE_STREAMING {
            shared.streamed.store(true, Ordering::Relaxed);
        }
        if state < 0 {
            let why = if error.is_null() {
                "PipeWire stopped the stream".to_string()
            } else {
                // SAFETY: PipeWire passes a C string with an error state.
                let e = unsafe { std::ffi::CStr::from_ptr(error) };
                format!("PipeWire stopped the stream: {}", e.to_string_lossy())
            };
            if let Ok(mut e) = shared.error.lock() {
                e.get_or_insert(why);
            }
        }
    });
}

unsafe extern "C" fn on_param(data: *mut c_void, id: u32, param: *const c_void) {
    guarded(|| {
        if id != SPA_PARAM_FORMAT || param.is_null() {
            return;
        }
        // SAFETY: `data` is our `Shared`; `param` a pod PipeWire keeps alive
        // for the callback, its header giving its size.
        let shared = unsafe { &*data.cast::<Shared>() };
        let pod = unsafe {
            let size = *param.cast::<u32>() as usize;
            std::slice::from_raw_parts(param.cast::<u8>(), 8 + size)
        };
        let Some(format) = pod::video_format(pod) else {
            return;
        };
        if let Ok(mut f) = shared.format.lock() {
            *f = Some(format);
        }
        let buffers = pod::buffers();
        let mut params = [buffers.as_ptr().cast::<c_void>()];
        let stream = shared.stream.load(Ordering::Relaxed);
        // SAFETY: on the loop's thread, inside its callback.
        unsafe { (shared.pw.stream_update_params)(stream, params.as_mut_ptr(), 1) };
    });
}

unsafe extern "C" fn on_process(data: *mut c_void) {
    guarded(|| {
        // SAFETY: `data` is our `Shared`, called on the loop's thread.
        let shared = unsafe { &*data.cast::<Shared>() };
        let (pw, stream) = (shared.pw, shared.stream.load(Ordering::Relaxed));
        if stream.is_null() {
            return;
        }
        // Keep only the newest of what is waiting.
        let mut newest: *mut PwBuffer = std::ptr::null_mut();
        loop {
            // SAFETY: the stream is alive while its callbacks run.
            let b = unsafe { (pw.stream_dequeue_buffer)(stream) };
            if b.is_null() {
                break;
            }
            if !newest.is_null() {
                unsafe { (pw.stream_queue_buffer)(stream, newest) };
            }
            newest = b;
        }
        if newest.is_null() {
            return;
        }
        if shared.want.load(Ordering::Relaxed) {
            // SAFETY: a dequeued buffer is ours until queued back.
            if let Some(raw) = unsafe { copy(shared, newest) } {
                if let Ok(mut l) = shared.latest.lock() {
                    *l = Some(raw);
                }
                shared.want.store(false, Ordering::Relaxed);
            }
        }
        unsafe { (pw.stream_queue_buffer)(stream, newest) };
    });
}

/// The buffer's picture, if it holds one in mapped memory.
unsafe fn copy(shared: &Shared, b: *mut PwBuffer) -> Option<Raw> {
    let (format, width, height) = (*shared.format.lock().ok()?)?;
    let bgra = match format {
        pod::BGRX | pod::BGRA => true,
        pod::RGBX | pod::RGBA => false,
        _ => return None,
    };
    // SAFETY: pointers PipeWire filled for a dequeued, mapped buffer.
    unsafe {
        let spa = (*b).buffer;
        if spa.is_null() || (*spa).n_datas == 0 {
            return None;
        }
        let d = &*(*spa).datas;
        if d.data.is_null() || d.chunk.is_null() {
            // DmaBuf, unmapped: not asked for, so not expected.
            return None;
        }
        let chunk = &*d.chunk;
        if chunk.size == 0 || chunk.flags & SPA_CHUNK_FLAG_CORRUPTED != 0 {
            // Some compositors send an empty buffer when only the cursor moved.
            return None;
        }
        let stride = if chunk.stride > 0 {
            chunk.stride as usize
        } else {
            width as usize * 4
        };
        let offset = (chunk.offset % d.maxsize.max(1)) as usize;
        let len = (chunk.size as usize).min(d.maxsize as usize - offset);
        let need = (height as usize).checked_sub(1)? * stride + width as usize * 4;
        if len < need {
            return None;
        }
        let bytes = std::slice::from_raw_parts(d.data.cast::<u8>().add(offset), need).to_vec();
        Some(Raw {
            width,
            height,
            stride,
            bgra,
            bytes,
        })
    }
}

/// `libpipewire-0.3`, loaded on first use.
mod pw {
    use super::{c_char, c_int, c_void, OnceLock, PwBuffer, StreamEvents};
    use libloading::Library;

    type P = *mut c_void;

    macro_rules! lib {
        ($($name:ident: $ty:ty,)*) => {
            pub struct Lib {
                _lib: Library,
                $(pub $name: $ty,)*
            }

            impl Lib {
                fn load() -> Result<Self, String> {
                    // SAFETY: libpipewire's initialisers are safe to run, and
                    // each symbol is given its C signature from `pipewire/*.h`.
                    unsafe {
                        let lib = Library::new("libpipewire-0.3.so.0").map_err(|_| {
                            "Sharing the screen on Wayland needs PipeWire \
                             (libpipewire-0.3), which this system does not have"
                                .to_string()
                        })?;
                        $(
                            let $name = *lib
                                .get::<$ty>(concat!("pw_", stringify!($name), "\0").as_bytes())
                                .map_err(|e| format!("PipeWire is too old: {e}"))?;
                        )*
                        Ok(Self { _lib: lib, $($name,)* })
                    }
                }
            }
        };
    }

    lib! {
        init: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
        thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> P,
        thread_loop_get_loop: unsafe extern "C" fn(P) -> P,
        thread_loop_start: unsafe extern "C" fn(P) -> c_int,
        thread_loop_stop: unsafe extern "C" fn(P),
        thread_loop_destroy: unsafe extern "C" fn(P),
        thread_loop_lock: unsafe extern "C" fn(P),
        thread_loop_unlock: unsafe extern "C" fn(P),
        context_new: unsafe extern "C" fn(P, P, usize) -> P,
        context_connect_fd: unsafe extern "C" fn(P, c_int, P, usize) -> P,
        context_destroy: unsafe extern "C" fn(P),
        core_disconnect: unsafe extern "C" fn(P) -> c_int,
        properties_new: unsafe extern "C" fn(*const c_char, ...) -> P,
        stream_new: unsafe extern "C" fn(P, *const c_char, P) -> P,
        stream_add_listener: unsafe extern "C" fn(P, P, *const StreamEvents, P),
        stream_connect: unsafe extern "C" fn(P, u32, u32, u32, *mut *const c_void, u32) -> c_int,
        stream_update_params: unsafe extern "C" fn(P, *mut *const c_void, u32) -> c_int,
        stream_dequeue_buffer: unsafe extern "C" fn(P) -> *mut PwBuffer,
        stream_queue_buffer: unsafe extern "C" fn(P, *mut PwBuffer) -> c_int,
        stream_disconnect: unsafe extern "C" fn(P) -> c_int,
        stream_destroy: unsafe extern "C" fn(P),
    }

    // SAFETY: only function pointers and the library that keeps them valid.
    unsafe impl Send for Lib {}
    unsafe impl Sync for Lib {}

    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();

    pub fn lib() -> Result<&'static Lib, String> {
        LIB.get_or_init(Lib::load).as_ref().map_err(Clone::clone)
    }
}

/// SPA pods (`spa/pod/pod.h`): an 8-byte header (body size, type), the body,
/// padding to 8. Native byte order.
mod pod {
    const ID: u32 = 3;
    const INT: u32 = 4;
    const RECTANGLE: u32 = 10;
    const FRACTION: u32 = 11;
    const OBJECT: u32 = 15;
    const CHOICE: u32 = 19;

    const CHOICE_RANGE: u32 = 1;
    const CHOICE_ENUM: u32 = 3;
    const CHOICE_FLAGS: u32 = 4;

    const OBJECT_FORMAT: u32 = 0x40003;
    const OBJECT_BUFFERS: u32 = 0x40004;
    const PARAM_ENUM_FORMAT: u32 = 3;
    const PARAM_BUFFERS: u32 = 5;

    const MEDIA_TYPE: u32 = 1;
    const MEDIA_SUBTYPE: u32 = 2;
    const VIDEO_FORMAT: u32 = 0x20001;
    const VIDEO_SIZE: u32 = 0x20003;
    const VIDEO_FRAMERATE: u32 = 0x20004;
    const BUFFERS_DATA_TYPE: u32 = 6;

    const MEDIA_VIDEO: u32 = 2;
    const SUBTYPE_RAW: u32 = 1;

    /// `spa_video_format`s whose bytes `I420::from_rgba` reads.
    pub const RGBX: u32 = 7;
    pub const BGRX: u32 = 8;
    pub const RGBA: u32 = 11;
    pub const BGRA: u32 = 12;

    /// `spa_data_type` bits: plain memory, and a memfd PipeWire maps.
    const DATA_MEMPTR: u32 = 1;
    const DATA_MEMFD: u32 = 2;

    fn words(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|w| w.to_ne_bytes()).collect()
    }

    fn pod(kind: u32, body: &[u8]) -> Vec<u8> {
        let mut out = words(&[body.len() as u32, kind]);
        out.extend_from_slice(body);
        out.resize(out.len().next_multiple_of(8), 0);
        out
    }

    fn id(v: u32) -> Vec<u8> {
        pod(ID, &words(&[v]))
    }

    /// A choice of `kind` over values of one type and size each.
    fn choice(kind: u32, child: u32, child_size: u32, values: &[u32]) -> Vec<u8> {
        let mut body = words(&[kind, 0, child_size, child]);
        body.extend(words(values));
        pod(CHOICE, &body)
    }

    fn object(kind: u32, param: u32, props: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut body = words(&[kind, param]);
        for (key, value) in props {
            body.extend(words(&[*key, 0]));
            body.extend_from_slice(value);
        }
        pod(OBJECT, &body)
    }

    /// Copied to 8-aligned memory, as PipeWire reads it in words.
    fn aligned(bytes: Vec<u8>) -> Vec<u64> {
        bytes
            .chunks(8)
            .map(|c| {
                let mut w = [0u8; 8];
                w[..c.len()].copy_from_slice(c);
                u64::from_ne_bytes(w)
            })
            .collect()
    }

    /// Raw video in the four 32-bit layouts, any size, any rate.
    pub fn enum_format() -> Vec<u64> {
        aligned(object(
            OBJECT_FORMAT,
            PARAM_ENUM_FORMAT,
            &[
                (MEDIA_TYPE, id(MEDIA_VIDEO)),
                (MEDIA_SUBTYPE, id(SUBTYPE_RAW)),
                (
                    VIDEO_FORMAT,
                    choice(CHOICE_ENUM, ID, 4, &[BGRX, BGRX, BGRA, RGBX, RGBA]),
                ),
                (
                    VIDEO_SIZE,
                    choice(CHOICE_RANGE, RECTANGLE, 8, &[1280, 720, 1, 1, 8192, 8192]),
                ),
                (
                    VIDEO_FRAMERATE,
                    choice(CHOICE_RANGE, FRACTION, 8, &[5, 1, 0, 1, 1000, 1]),
                ),
            ],
        ))
    }

    /// Buffers in memory the app can read, never a GPU buffer.
    pub fn buffers() -> Vec<u64> {
        aligned(object(
            OBJECT_BUFFERS,
            PARAM_BUFFERS,
            &[(
                BUFFERS_DATA_TYPE,
                choice(
                    CHOICE_FLAGS,
                    INT,
                    4,
                    &[(1 << DATA_MEMPTR) | (1 << DATA_MEMFD)],
                ),
            )],
        ))
    }

    fn u32_at(b: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_ne_bytes(b.get(at..at + 4)?.try_into().ok()?))
    }

    /// A property's value, or a choice's first (its current) value: its
    /// type and where its body starts.
    fn value(b: &[u8], at: usize) -> Option<(u32, usize)> {
        let kind = u32_at(b, at + 4)?;
        if kind == CHOICE {
            Some((u32_at(b, at + 8 + 12)?, at + 8 + 16))
        } else {
            Some((kind, at + 8))
        }
    }

    /// The negotiated `Format` object: video format, width, height.
    pub fn video_format(b: &[u8]) -> Option<(u32, u32, u32)> {
        if u32_at(b, 4)? != OBJECT {
            return None;
        }
        let end = 8 + u32_at(b, 0)? as usize;
        let (mut format, mut size) = (None, None);
        let mut at = 16;
        while at + 16 <= end.min(b.len()) {
            let key = u32_at(b, at)?;
            let value_size = u32_at(b, at + 8)? as usize;
            let (kind, body) = value(b, at + 8)?;
            match (key, kind) {
                (VIDEO_FORMAT, ID) => format = u32_at(b, body),
                (VIDEO_SIZE, RECTANGLE) => size = Some((u32_at(b, body)?, u32_at(b, body + 4)?)),
                _ => {}
            }
            at += 16 + value_size.next_multiple_of(8);
        }
        let (w, h) = size?;
        Some((format?, w, h))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn bytes(words: &[u64]) -> Vec<u8> {
            words.iter().flat_map(|w| w.to_ne_bytes()).collect()
        }

        /// The layout `spa_pod_builder` writes for the same object: header,
        /// object type and id, then key, flags and a padded value per property.
        #[test]
        fn a_format_object_is_laid_out_as_spa_writes_it() {
            let b = bytes(&enum_format());
            assert_eq!(u32_at(&b, 4), Some(OBJECT));
            assert_eq!(b.len(), 8 + u32_at(&b, 0).unwrap() as usize);
            assert_eq!(u32_at(&b, 8), Some(OBJECT_FORMAT));
            assert_eq!(u32_at(&b, 12), Some(PARAM_ENUM_FORMAT));
            // mediaType: key 1, flags 0, an Id pod of 4 bytes padded to 8.
            assert_eq!(&b[16..40], &words(&[1, 0, 4, ID, MEDIA_VIDEO, 0])[..]);
            // The format choice: Enum of Ids, default first.
            let at = 16 + 24 + 24;
            assert_eq!(u32_at(&b, at), Some(VIDEO_FORMAT));
            assert_eq!(u32_at(&b, at + 12), Some(CHOICE));
            assert_eq!(u32_at(&b, at + 8), Some(16 + 5 * 4));
            assert_eq!(u32_at(&b, at + 16), Some(CHOICE_ENUM));
            assert_eq!(u32_at(&b, at + 28), Some(ID));
            assert_eq!(u32_at(&b, at + 32), Some(BGRX));
        }

        /// What a compositor answers with: a fixed format, the size either
        /// bare or wrapped in a choice of one.
        #[test]
        fn the_negotiated_format_is_read_back() {
            let fixed = object(
                OBJECT_FORMAT,
                4,
                &[
                    (MEDIA_TYPE, id(MEDIA_VIDEO)),
                    (MEDIA_SUBTYPE, id(SUBTYPE_RAW)),
                    (VIDEO_FORMAT, id(BGRA)),
                    (VIDEO_SIZE, pod(RECTANGLE, &words(&[2560, 1440]))),
                    (VIDEO_FRAMERATE, pod(FRACTION, &words(&[0, 1]))),
                ],
            );
            assert_eq!(video_format(&fixed), Some((BGRA, 2560, 1440)));
            let chosen = object(
                OBJECT_FORMAT,
                4,
                &[
                    (VIDEO_SIZE, choice(0, RECTANGLE, 8, &[1920, 1080])),
                    (VIDEO_FORMAT, choice(0, ID, 4, &[RGBX])),
                ],
            );
            assert_eq!(video_format(&chosen), Some((RGBX, 1920, 1080)));
            assert_eq!(video_format(&id(3)), None);
        }

        #[test]
        fn the_buffer_request_takes_only_mappable_memory() {
            let b = bytes(&buffers());
            assert_eq!(u32_at(&b, 8), Some(OBJECT_BUFFERS));
            assert_eq!(u32_at(&b, 16), Some(BUFFERS_DATA_TYPE));
            assert_eq!(u32_at(&b, 24 + 8), Some(CHOICE_FLAGS));
            assert_eq!(u32_at(&b, 24 + 24), Some(0b110));
        }
    }
}
