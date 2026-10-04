// The encoded-transform worker for calls in the browser (docs/notes/calls.md).
//
// The browser encodes with its own Opus and VP8 and decodes VP8 and VP9 (what
// the app sends); each encoded frame
// passes through here on its way out (sealed) and on its way in (opened), so
// the forwarder only ever carries SFrame, as it does for the native client.
//
// Two ways in, because browsers differ: `RTCRtpScriptTransform` (Firefox,
// Safari, recent Chrome) fires `rtctransform` on this worker; Chrome's older
// `createEncodedStreams()` hands the page a pair of streams, which it posts
// here. Either way the same `run` does the work.
import { CallKeys } from "./sframe.js";

const keys = new CallKeys(crypto.getRandomValues(new Uint32Array(1))[0]);
// Counted and reported now and then, so a call that carries nothing says why.
const dropped = { send: 0, receive: 0 };

/** The codec of one encoded frame. A video track carries VP8 or VP9 depending
 *  on who sent it (the app sends VP9, a browser VP8), and the metadata says
 *  which where the browser fills it in. */
function codecOf(frame, fallback) {
  const mime = frame.getMetadata?.().mimeType;
  if (typeof mime === "string") {
    if (/vp9/i.test(mime)) return "vp9";
    if (/vp8/i.test(mime)) return "vp8";
  }
  return fallback;
}

/** Open a received frame. Without a mimeType to go on, a video frame is tried
 *  as each codec: the clear prefix is authenticated, so only the right reading
 *  of it opens. */
async function open(codec, data, known) {
  if (known || codec === "opus") return (await keys.open(codec, data)).frame;
  try {
    return (await keys.open("vp9", data)).frame;
  } catch {
    return (await keys.open("vp8", data)).frame;
  }
}

function run(readable, writable, side, codec) {
  readable
    .pipeThrough(
      new TransformStream({
        async transform(frame, out) {
          try {
            const data = new Uint8Array(frame.data);
            const c = codecOf(frame, codec);
            const known = typeof frame.getMetadata?.().mimeType === "string";
            const done = side === "send" ? await keys.seal(c, data) : await open(c, data, known);
            frame.data = done.buffer;
            out.enqueue(frame);
          } catch {
            // No key yet, or not for this call: a frame nobody could open is
            // a frame not sent, and one that does not open is not played.
            dropped[side]++;
          }
        },
      }),
    )
    .pipeTo(writable)
    .catch(() => {});
}

self.onrtctransform = (event) => {
  const { side, codec } = event.transformer.options;
  run(event.transformer.readable, event.transformer.writable, side, codec);
};

self.onmessage = (event) => {
  const m = event.data;
  if (m.kind === "keys") {
    keys.setEpoch(m.epoch, m.key, m.index);
  } else if (m.kind === "streams") {
    run(m.readable, m.writable, m.side, m.codec);
  }
};

setInterval(() => {
  if (dropped.send || dropped.receive) {
    self.postMessage({ kind: "dropped", ...dropped });
    dropped.send = dropped.receive = 0;
  }
}, 5000);
