//! The screen a call shares (`systems::calls`, `docs/notes/calls.md`): its
//! picture a few times a second, as I420, from a thread of its own.
//!
//! One way per platform, each already in the build:
//!
//! - **Android**: MediaProjection (`ScreenCapture.java`). The system asks the
//!   person first, in its own dialog, every time; a foreground service shows
//!   the sharing in the notifications for as long as it lasts.
//! - **macOS**: Core Graphics' picture of the main display. The first time,
//!   macOS asks for the Screen Recording permission; until it is given the
//!   picture is the desktop without its windows.
//! - **Linux on X11**: the root window through X11's own protocol (`x11rb`,
//!   pure Rust). **Not Wayland**: there the screen is the compositor's, handed
//!   out only through the desktop portal and PipeWire, a C library this build
//!   does not take; the app says so rather than share a black screen.
//! - **Windows**: GDI's copy of the primary screen.
//!
//! Like `platform::camera`, a capture that cannot start still yields a
//! [`ScreenCapture`], whose [`ScreenCapture::error`] says why.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cartopolis_calls::video::I420;

use crate::platform::sync::{Arc, Mutex};

/// How often the screen is looked at. A shared screen is slides and windows:
/// two or three pictures a second keep up with a person talking over them.
const EVERY: Duration = Duration::from_millis(400);

#[derive(Default)]
struct Shared {
    latest: Mutex<Option<I420>>,
    error: Mutex<Option<String>>,
    /// The first picture arrived (on Android: the person said yes).
    running: AtomicBool,
    stop: AtomicBool,
}

/// A screen being shared. Stops when dropped.
pub struct ScreenCapture {
    shared: Arc<Shared>,
}

impl ScreenCapture {
    /// Start sharing. On Android this puts up the system's dialog.
    #[must_use]
    pub fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let thread_shared = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("screen".into())
            .spawn(move || run(&thread_shared));
        if let Err(e) = spawned {
            set_error(&shared, format!("Could not start sharing: {e}"));
        }
        Self { shared }
    }

    /// Why sharing stopped or never started, if it did.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }

    /// Whether pictures are coming: false while Android's dialog is up.
    #[must_use]
    pub fn running(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    /// The newest picture since the last call, if there is one.
    #[must_use]
    pub fn take(&self) -> Option<I420> {
        self.shared.latest.lock().ok().and_then(|mut l| l.take())
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

fn set_error(shared: &Shared, why: String) {
    if let Ok(mut e) = shared.error.lock() {
        e.get_or_insert(why);
    }
}

fn run(shared: &Shared) {
    let mut grab = match imp::Grabber::open() {
        Ok(g) => g,
        Err(why) => {
            set_error(shared, why);
            return;
        }
    };
    while !shared.stop.load(Ordering::Relaxed) {
        match grab.next() {
            Ok(Some(frame)) => {
                shared.running.store(true, Ordering::Relaxed);
                if let Ok(mut l) = shared.latest.lock() {
                    *l = Some(frame);
                }
            }
            Ok(None) => {}
            Err(why) => {
                set_error(shared, why);
                return;
            }
        }
        std::thread::sleep(EVERY);
    }
}

#[cfg(target_os = "android")]
mod imp {
    use super::I420;
    use crate::platform::android;

    pub struct Grabber {
        seen: u64,
        ran: bool,
    }

    impl Grabber {
        pub fn open() -> Result<Self, String> {
            android::screen_start()?;
            Ok(Self {
                seen: 0,
                ran: false,
            })
        }

        pub fn next(&mut self) -> Result<Option<I420>, String> {
            match android::screen_status().as_deref() {
                None | Some("asking") => Ok(None),
                Some("running") => {
                    self.ran = true;
                    let Some((w, h, stride, n, px)) = android::screen_latest(self.seen)? else {
                        return Ok(None);
                    };
                    self.seen = n;
                    Ok(I420::from_rgba(w, h, stride, &px, false))
                }
                Some("stopped") if self.ran => Err("You stopped sharing the screen".into()),
                Some("stopped") => Ok(None),
                Some(why) => Err(why.to_string()),
            }
        }
    }

    impl Drop for Grabber {
        fn drop(&mut self) {
            android::screen_stop();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::I420;
    use core_graphics::display::CGDisplay;

    pub struct Grabber;

    impl Grabber {
        pub fn open() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn next(&mut self) -> Result<Option<I420>, String> {
            let image = CGDisplay::main().image().ok_or_else(|| {
                "macOS gave no picture of the screen: allow Cartopolis under System \
                 Settings › Privacy & Security › Screen Recording"
                    .to_string()
            })?;
            if image.bits_per_pixel() != 32 {
                return Err(format!(
                    "the screen is {} bits a pixel, not 32",
                    image.bits_per_pixel()
                ));
            }
            let (w, h) = (image.width() as u32, image.height() as u32);
            let data = image.data();
            // Core Graphics' screen is BGRA in memory (32-bit little-endian,
            // alpha first), rows `bytes_per_row` apart.
            Ok(I420::from_rgba(
                w,
                h,
                image.bytes_per_row(),
                data.bytes(),
                true,
            ))
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::I420;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};
    use x11rb::rust_connection::RustConnection;

    pub struct Grabber {
        conn: RustConnection,
        root: u32,
        width: u16,
        height: u16,
    }

    impl Grabber {
        pub fn open() -> Result<Self, String> {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                return Err(
                    "On Wayland the app cannot share the screen yet: share it from \
                            the browser, or sign in to an X11 session"
                        .into(),
                );
            }
            let (conn, n) =
                x11rb::connect(None).map_err(|e| format!("No X display to share: {e}"))?;
            let screen = &conn.setup().roots[n];
            let (root, width, height, depth) = (
                screen.root,
                screen.width_in_pixels,
                screen.height_in_pixels,
                screen.root_depth,
            );
            let bpp = conn
                .setup()
                .pixmap_formats
                .iter()
                .find(|f| f.depth == depth)
                .map(|f| f.bits_per_pixel);
            if bpp != Some(32) {
                return Err(format!("the screen is {bpp:?} bits a pixel, not 32"));
            }
            Ok(Self {
                conn,
                root,
                width,
                height,
            })
        }

        pub fn next(&mut self) -> Result<Option<I420>, String> {
            let image = self
                .conn
                .get_image(
                    ImageFormat::Z_PIXMAP,
                    self.root,
                    0,
                    0,
                    self.width,
                    self.height,
                    !0,
                )
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| e.to_string())?;
            let (w, h) = (u32::from(self.width), u32::from(self.height));
            // A 24-bit root in 32-bit pixels: B, G, R, unused, rows unpadded.
            Ok(I420::from_rgba(w, h, w as usize * 4, &image.data, true))
        }
    }
}

#[cfg(windows)]
mod imp {
    use super::I420;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    pub struct Grabber;

    impl Grabber {
        pub fn open() -> Result<Self, String> {
            Ok(Self)
        }

        pub fn next(&mut self) -> Result<Option<I420>, String> {
            // SAFETY: plain GDI calls on handles made and released here, into a
            // buffer sized for the bitmap they describe.
            unsafe {
                let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
                if w <= 0 || h <= 0 {
                    return Err("Windows reports no screen".into());
                }
                let screen = GetDC(std::ptr::null_mut());
                let memory = CreateCompatibleDC(screen);
                let bitmap = CreateCompatibleBitmap(screen, w, h);
                let old = SelectObject(memory, bitmap);
                let copied = BitBlt(memory, 0, 0, w, h, screen, 0, 0, SRCCOPY);
                let mut info: BITMAPINFO = std::mem::zeroed();
                info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                info.bmiHeader.biWidth = w;
                // Negative: rows top to bottom.
                info.bmiHeader.biHeight = -h;
                info.bmiHeader.biPlanes = 1;
                info.bmiHeader.biBitCount = 32;
                info.bmiHeader.biCompression = BI_RGB;
                let mut px = vec![0u8; w as usize * h as usize * 4];
                let rows = GetDIBits(
                    memory,
                    bitmap,
                    0,
                    h as u32,
                    px.as_mut_ptr().cast(),
                    &mut info,
                    DIB_RGB_COLORS,
                );
                SelectObject(memory, old);
                DeleteObject(bitmap);
                DeleteDC(memory);
                ReleaseDC(std::ptr::null_mut(), screen);
                if copied == 0 || rows == 0 {
                    return Err("Windows would not copy the screen".into());
                }
                Ok(I420::from_rgba(
                    w as u32,
                    h as u32,
                    w as usize * 4,
                    &px,
                    true,
                ))
            }
        }
    }
}

#[cfg(not(any(
    target_os = "android",
    target_os = "macos",
    target_os = "linux",
    windows
)))]
mod imp {
    use super::I420;

    pub struct Grabber;

    impl Grabber {
        pub fn open() -> Result<Self, String> {
            Err("This platform cannot share its screen from the app".into())
        }

        pub fn next(&mut self) -> Result<Option<I420>, String> {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One picture of the screen this test runs on: an X display on Linux
    /// (`Xvfb :99 & DISPLAY=:99`), the main display on macOS. Ignored by
    /// default — a build node usually has no screen to share.
    #[test]
    #[ignore = "needs a display to share"]
    fn a_picture_of_the_screen_arrives() {
        let capture = ScreenCapture::start();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let picture = loop {
            if let Some(p) = capture.take() {
                break p;
            }
            assert!(capture.error().is_none(), "{:?}", capture.error());
            assert!(std::time::Instant::now() < deadline, "no picture in 5 s");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(
            picture.width >= 320 && picture.height >= 200,
            "{}×{}",
            picture.width,
            picture.height
        );
        assert!(capture.running());
    }
}
