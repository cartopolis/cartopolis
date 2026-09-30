// The encoded-transform worker for calls in the browser (docs/notes/calls.md).
//
// The browser encodes and decodes with its own Opus and VP8; each encoded frame
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

function run(readable, writable, side, codec) {
  readable
    .pipeThrough(
      new TransformStream({
        async transform(frame, out) {
          try {
            const data = new Uint8Array(frame.data);
            const done =
              side === "send" ? await keys.seal(codec, data) : (await keys.open(codec, data)).frame;
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
