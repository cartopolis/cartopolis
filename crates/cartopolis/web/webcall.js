// A call in the browser: the page half of `platform::web_call` (docs/notes/calls.md).
//
// The native client runs WebRTC itself (str0m) and encodes with pure-Rust Opus
// and VP8. The browser already has all of that, so here it is the browser's:
// its `RTCPeerConnection`, its microphone and camera, its codecs. What is ours
// is the shape — one connection *up* that this device sends on and one *down*
// that the forwarder sends everybody else on, exactly as `cartopolis_calls::link`
// does it — and the encryption, which runs in `call-worker.js` on every frame.
//
// Rust drives this through a handful of methods and hears back through one
// callback, `onEvent(kind, value)`: "offer" (the up connection's offer, for
// `CallJoin`), "answer" (the down connection's, for `CallAnswer`),
// "connected", "failed" and "notice" (something to tell the person).

const transform =
  typeof RTCRtpScriptTransform !== "undefined"
    ? "script"
    : typeof RTCRtpSender !== "undefined" && "createEncodedStreams" in RTCRtpSender.prototype
      ? "streams"
      : null;

/** Whether this browser can take part in an encrypted call at all. */
export function webCallSupported() {
  return transform !== null && !!navigator.mediaDevices && typeof RTCPeerConnection !== "undefined";
}

/** The SDP once every local candidate is in it: the forwarder is ICE-lite and
 *  never sends candidates back, so what is in the SDP is all it will know. */
async function settled(pc) {
  if (pc.iceGatheringState !== "complete") {
    await new Promise((resolve) => {
      const done = () => {
        if (pc.iceGatheringState === "complete") {
          pc.removeEventListener("icegatheringstatechange", done);
          resolve();
        }
      };
      pc.addEventListener("icegatheringstatechange", done);
      setTimeout(resolve, 3000);
    });
  }
  return pc.localDescription.sdp;
}

function describe(e, what) {
  const name = e && e.name;
  if (name === "NotAllowedError" || name === "SecurityError")
    return `${what} permission denied. Allow it in the browser's site settings.`;
  if (name === "NotFoundError") return `No ${what.toLowerCase()} found.`;
  if (name === "NotReadableError") return `The ${what.toLowerCase()} is in use by another application.`;
  return `Could not open the ${what.toLowerCase()} (${name || e})`;
}

// wasm-bindgen copies this file to `snippets/<crate>-<hash>/web/webcall.js` in the
// built site, and trunk copies `web/` (the worker and its SFrame) to `web/` at
// the site root (`index.html`'s copy-dir). Resolving from this module rather
// than the page keeps it right on every page path, `/go?place=` included.
const WORKER_URL = new URL("../../../web/call-worker.js", import.meta.url);

export class WebCall {
  // `options` is for a page that draws the call itself (the web chat page):
  // `workerUrl` where its build put `call-worker.js`, and `onStream(participant,
  // kind, stream)` to hand it each person's audio and video (`stream` null when
  // they leave) instead of the hidden elements the viewer reads pixels from.
  constructor(onEvent, options = {}) {
    this.onEvent = onEvent;
    this.onStream = options.onStream || null;
    this.worker = new Worker(options.workerUrl || WORKER_URL, { type: "module" });
    this.worker.onmessage = (e) => {
      if (e.data.kind === "dropped") console.debug("call: frames dropped", e.data);
    };
    this.worker.onerror = (e) => this.onEvent("failed", `the call's encryption did not start: ${e.message}`);
    this.config = transform === "streams" ? { encodedInsertableStreams: true } : {};
    this.up = new RTCPeerConnection(this.config);
    this.down = null;
    this.tracks = new Map(); // mid → { participant, video }
    this.people = new Map(); // participant → { audio, video, canvas, fresh }
    this.mic = null;
    this.camera = null;
    this.videoSender = null;
    this.muted = false;
    this.closed = false;
    // For a test driving a page served from this machine (the call test reads
    // the browser's own statistics through it). Never on a public page.
    if (["127.0.0.1", "localhost"].includes(location.hostname)) globalThis.cartopolisCall = this;
    this.up.onconnectionstatechange = () => {
      const s = this.up.connectionState;
      if (s === "connected") this.onEvent("connected", "");
      else if (s === "failed") this.onEvent("failed", "the connection to the call failed");
    };
  }

  /** Put the worker between an encoder and the network, or the network and a decoder. */
  attach(endpoint, side, codec) {
    if (transform === "script") {
      endpoint.transform = new RTCRtpScriptTransform(this.worker, { side, codec });
    } else {
      const { readable, writable } = endpoint.createEncodedStreams();
      this.worker.postMessage({ kind: "streams", readable, writable, side, codec }, [readable, writable]);
    }
  }

  /** Open the microphone and make the up connection's offer. Always offers a
   *  camera track, as the native client does: turning the camera on later must
   *  not need a renegotiation. */
  async start() {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      this.mic = stream.getAudioTracks()[0];
      this.mic.enabled = !this.muted;
    } catch (e) {
      this.onEvent("failed", describe(e, "Microphone"));
      return;
    }
    const audio = this.up.addTransceiver(this.mic, { direction: "sendonly" });
    const video = this.up.addTransceiver("video", { direction: "sendonly" });
    // VP8 to send: every device in the call decodes it (`cartopolis_calls::video`)
    // and the forwarder reads its first bytes. Received video may be VP8 or VP9
    // (the app sends VP9); `call-worker.js` tells them apart per frame.
    if (video.setCodecPreferences && RTCRtpSender.getCapabilities) {
      const vp8 = RTCRtpSender.getCapabilities("video").codecs.filter((c) => /vp8/i.test(c.mimeType));
      if (vp8.length) video.setCodecPreferences(vp8);
    }
    this.videoSender = video.sender;
    this.attach(audio.sender, "send", "opus");
    this.attach(video.sender, "send", "vp8");
    await this.up.setLocalDescription(await this.up.createOffer());
    this.onEvent("offer", await settled(this.up));
    if (this.wantCamera) await this.setCamera(true);
  }

  async joined(answer) {
    try {
      await this.up.setRemoteDescription({ type: "answer", sdp: answer });
    } catch (e) {
      this.onEvent("failed", `the call's answer was not understood (${e})`);
    }
  }

  /** The forwarder's offer for the down connection; `tracks` says whose each
   *  media line is. */
  async offered(sdp, tracks) {
    // Every offer carries the whole set: somebody missing from it has left.
    this.tracks = new Map(tracks.map((t) => [t.mid, t]));
    if (!this.down) {
      this.down = new RTCPeerConnection(this.config);
      this.down.ontrack = (e) => this.track(e);
      this.down.onconnectionstatechange = () => {
        if (this.down.connectionState === "failed")
          this.onEvent("failed", "the connection to the call failed");
      };
    }
    try {
      await this.down.setRemoteDescription({ type: "offer", sdp });
      for (const t of this.down.getTransceivers()) {
        if (!t.receiver.__attached) {
          t.receiver.__attached = true;
          this.attach(t.receiver, "receive", t.receiver.track.kind === "video" ? "vp8" : "opus");
        }
      }
      await this.down.setLocalDescription(await this.down.createAnswer());
      this.onEvent("answer", await settled(this.down));
    } catch (e) {
      this.onEvent("failed", `the call's offer was not understood (${e})`);
    }
  }

  track(e) {
    const info = this.tracks.get(e.transceiver.mid);
    if (!info) return;
    const who = this.people.get(info.participant) || {};
    this.people.set(info.participant, who);
    const stream = new MediaStream([e.track]);
    if (this.onStream) {
      who[e.track.kind] = true;
      this.onStream(info.participant, e.track.kind, stream);
      e.track.onended = () => this.onStream?.(info.participant, e.track.kind, null);
      return;
    }
    if (e.track.kind === "audio") {
      who.audio?.remove();
      who.audio = Object.assign(document.createElement("audio"), { autoplay: true, srcObject: stream });
      who.audio.style.display = "none";
      document.body.appendChild(who.audio);
    } else {
      who.video?.remove();
      who.video = Object.assign(document.createElement("video"), {
        autoplay: true,
        muted: true,
        playsInline: true,
        srcObject: stream,
      });
      who.video.style.cssText = "position:fixed;width:1px;height:1px;opacity:0;pointer-events:none";
      document.body.appendChild(who.video);
      who.fresh = false;
      const watch = () => {
        who.fresh = true;
        if (!this.closed && who.video?.requestVideoFrameCallback) who.video.requestVideoFrameCallback(watch);
      };
      if (who.video.requestVideoFrameCallback) who.video.requestVideoFrameCallback(watch);
      else who.video.ontimeupdate = () => (who.fresh = true);
      e.track.onended = () => {
        who.video?.remove();
        who.video = null;
      };
    }
  }

  /** The chat's keys for an epoch, for the worker. */
  keys(epoch, key, index) {
    // `key` is a view into the wasm module's memory, which is shared in the
    // threaded build: copy it, or the worker is handed a window onto memory
    // the next allocation reuses (and WebCrypto refuses shared buffers).
    this.worker.postMessage({ kind: "keys", epoch: BigInt(epoch), key: key.slice(), index });
  }

  setMuted(muted) {
    this.muted = muted;
    if (this.mic) this.mic.enabled = !muted;
  }

  async setCamera(on) {
    // Asked for before `start` has made the video sender (the camera pressed
    // in the first moment of a call): remembered, and applied by `start`.
    this.wantCamera = on;
    if (!this.videoSender) return;
    if (on && !this.camera) {
      try {
        const stream = await navigator.mediaDevices.getUserMedia({
          video: { width: 320, height: 240, frameRate: 15, facingMode: "user" },
        });
        this.camera = stream.getVideoTracks()[0];
        await this.videoSender.replaceTrack(this.camera);
      } catch (e) {
        this.onEvent("notice", describe(e, "Camera"));
        this.onEvent("camera-off", "");
      }
    } else if (!on && this.camera) {
      await this.videoSender.replaceTrack(null);
      this.camera.stop();
      this.camera = null;
    }
  }

  /** This device's own camera, for a self-view, or null. */
  selfView() {
    return this.camera ? new MediaStream([this.camera]) : null;
  }

  /** Drop the media of anybody no longer in the call (a page that draws it
   *  itself calls this on every roster; `pictures` does it for the viewer). */
  forgetGone() {
    const live = new Set([...this.tracks.values()].map((t) => t.participant));
    for (const participant of [...this.people.keys()]) {
      if (!live.has(participant)) {
        this.people.delete(participant);
        this.onStream?.(participant, "audio", null);
        this.onStream?.(participant, "video", null);
      }
    }
  }

  /** Participants whose picture changed since the last call, each as
   *  `[participant, width, height, rgba]`. Drawn through a canvas, at most once
   *  per new video frame. */
  pictures() {
    const out = [];
    for (const [participant, who] of this.people) {
      const v = who.video;
      if (!v || !who.fresh || !v.videoWidth) continue;
      who.fresh = false;
      const w = v.videoWidth;
      const h = v.videoHeight;
      if (!who.canvas || who.canvas.width !== w || who.canvas.height !== h) {
        who.canvas = new OffscreenCanvas(w, h);
        who.context = who.canvas.getContext("2d", { willReadFrequently: true });
      }
      who.context.drawImage(v, 0, 0, w, h);
      out.push([participant, w, h, who.context.getImageData(0, 0, w, h).data]);
    }
    // Somebody who left the call.
    const live = new Set([...this.tracks.values()].map((t) => t.participant));
    for (const [participant, who] of this.people) {
      if (!live.has(participant)) {
        who.audio?.remove();
        who.video?.remove();
        this.people.delete(participant);
      }
    }
    return out;
  }

  close() {
    this.closed = true;
    // Nothing reports after a hang-up. `start()` and the answer handling are
    // still awaiting (a microphone prompt can be answered long after), and the
    // viewer frees its Rust callback the moment it closes: calling it then is
    // `closure invoked recursively or after being dropped`. Every report goes
    // through `this.onEvent`/`this.onStream`, so replacing them here is enough.
    this.onEvent = () => {};
    this.onStream = null;
    this.mic?.stop();
    this.camera?.stop();
    this.up.close();
    this.down?.close();
    this.worker.terminate();
    for (const who of this.people.values()) {
      who.audio?.remove();
      who.video?.remove();
    }
    this.people.clear();
  }
}
