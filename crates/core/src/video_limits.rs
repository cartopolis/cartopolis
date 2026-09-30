//! What the video store accepts (`docs/notes/video-on-a-prim.md`). Here rather
//! than in the simulator, which enforces them, for the reason `model_limits`
//! gives: the client says them beside the file picker too.

/// The largest clip accepted, in bytes. Applied before parsing, as the read
/// bound. A minute of 720p VP8 at a comfortable bitrate is ~20 MB.
pub const MAX_VIDEO_BYTES: usize = 64 * 1024 * 1024;

/// The largest picture accepted. Everything played on a prim is decoded on
/// the device of everybody standing near it, a phone included.
pub const MAX_VIDEO_WIDTH: u32 = 1280;
pub const MAX_VIDEO_HEIGHT: u32 = 720;

/// The longest clip accepted, in milliseconds: ten minutes.
pub const MAX_VIDEO_MS: u64 = 10 * 60 * 1000;

/// What a person is told before they pick a file.
pub const VIDEO_FORMAT_HINT: &str = "WebM with VP8 video (and Opus sound, optional), up to \
     1280×720, 10 minutes and 64 MB. With ffmpeg: \
     ffmpeg -i in.mp4 -c:v libvpx -b:v 1M -c:a libopus out.webm";
