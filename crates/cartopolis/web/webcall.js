// A call in the browser: the page half of `platform::web_call` (docs/notes/calls.md).
//
// The native client runs WebRTC itself (str0m) and encodes with pure-Rust Opus
// and VP9. The browser already has all of that, so here it is the browser's:
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

/** The send codec and the preferences that ask for it, from
 *  `RTCRtpSender.getCapabilities("video").codecs`: VP9 profile 0 (8-bit 4:2:0,
 *  what every receiver decodes) where the browser has it, else VP8. A VP9 entry
 *  without a profile-id (Firefox) is profile 0. */
export function sendCodecs(codecs) {
  const profile0 = (c) => !/profile-id=(?!0(?:;|$))/.test(c.sdpFmtpLine ?? "");
  const vp9 = codecs.filter((c) => /^video\/vp9$/i.test(c.mimeType) && profile0(c));
  if (vp9.length) return { codec: "vp9", prefs: vp9 };
  return { codec: "vp8", prefs: codecs.filter((c) => /^video\/vp8$/i.test(c.mimeType)) };
}

/** The video payload types an SDP names, as `{ pts: {pt: "vp8"|"vp9"}, first }`
 *  where `first` is the codec of the first video m-line's first format (what a
 *  sender sends), or null. */
export function videoPayloadTypes(sdp) {
  const pts = {};
  let firstPt = null;
  let video = false;
  for (const line of sdp.split(/\r?\n/)) {
    if (line.startsWith("m=")) {
      video = line.startsWith("m=video ");
      if (video && firstPt === null) firstPt = line.split(" ")[3] ?? null;
    } else if (video) {
      const m = /^a=rtpmap:(\d+) (VP8|VP9)\//i.exec(line);
      if (m) pts[m[1]] = m[2].toLowerCase();
    }
  }
  return { pts, first: firstPt === null ? null : (pts[firstPt] ?? null) };
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
  // `workerUrl` where its build put `call-worker.js`, `onStream(participant,
  // kind, stream)` to hand it each person's audio, video and shared screen
  // (`stream` null when they leave) instead of the hidden elements the viewer
  // reads pixels from, and `screen` to offer a third track a screen can be
  // shared on (protocol 41; its media id is `screenMid` once offered).
  constructor(onEvent, options = {}) {
    this.onEvent = onEvent;
    this.onStream = options.onStream || null;
    this.offerScreen = !!options.screen;
    // Which microphone and camera, by `deviceId`; the browser's default when unset.
    this.audioDevice = options.audioDevice || null;
    this.videoDevice = options.videoDevice || null;
    this.blur = false;
    this.screenSender = null;
    this.screenMid = null;
    this.screenTrack = null;
    this.probe = null;
    this.worker = new Worker(options.workerUrl || WORKER_URL, { type: "module" });
    this.worker.onmessage = (e) => {
      if (e.data.kind === "dropped") console.debug("call: frames dropped", e.data);
    };
    this.worker.onerror = (e) => this.onEvent("failed", `the call's encryption did not start: ${e.message}`);
    this.config = transform === "streams" ? { encodedInsertableStreams: true } : {};
    this.up = new RTCPeerConnection(this.config);
    this.down = null;
    this.tracks = new Map(); // mid → { participant, video, kind }
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
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, ...(this.audioDevice ? { deviceId: { exact: this.audioDevice } } : {}) },
      });
      this.mic = stream.getAudioTracks()[0];
      this.mic.enabled = !this.muted;
      // A copy that is never disabled, for the page to hear talking while
      // muted (`levelTrack`). Nothing is sent from it.
      this.probe = this.mic.clone();
    } catch (e) {
      this.onEvent("failed", describe(e, "Microphone"));
      return;
    }
    const audio = this.up.addTransceiver(this.mic, { direction: "sendonly" });
    // One spatial and one temporal layer: the worker seals each layer's frame
    // on its own, and the forwarder would join a picture's spatial layers into
    // one frame no receiver can open. A browser that rejects the option sends
    // its default, which for VP8 is one layer anyway.
    const sendOnly = () => {
      try {
        return this.up.addTransceiver("video", { direction: "sendonly", sendEncodings: [{ scalabilityMode: "L1T1" }] });
      } catch {
        return this.up.addTransceiver("video", { direction: "sendonly" });
      }
    };
    const video = sendOnly();
    // A screen is offered from the start too, so sharing later needs no
    // renegotiation either. The forwarder learns which media id it is from
    // `CallJoin.screen` and drops it until the server says this device shares.
    const screen = this.offerScreen ? sendOnly() : null;
    // VP9 profile 0 to send where the browser encodes it, as the app does
    // (protocol 32 made every receiver decode it), else VP8. Offering one
    // codec makes the answer name one, so the worker knows what every frame
    // is before the first is encoded. Received video may be either;
    // `call-worker.js` tells them apart per frame.
    let codec = "vp8";
    if (video.setCodecPreferences && RTCRtpSender.getCapabilities) {
      const pick = sendCodecs(RTCRtpSender.getCapabilities("video")?.codecs ?? []);
      try {
        if (pick.prefs.length) {
          video.setCodecPreferences(pick.prefs);
          screen?.setCodecPreferences(pick.prefs);
          codec = pick.codec;
        }
      } catch {}
    }
    this.videoSender = video.sender;
    this.attach(audio.sender, "send", "opus");
    this.attach(video.sender, "send", codec);
    if (screen) {
      this.screenSender = screen.sender;
      this.attach(screen.sender, "send", codec);
    }
    await this.up.setLocalDescription(await this.up.createOffer());
    this.screenMid = screen ? screen.mid : null;
    this.onEvent("offer", await settled(this.up));
    if (this.wantCamera) await this.setCamera(true);
  }

  async joined(answer) {
    try {
      await this.up.setRemoteDescription({ type: "answer", sdp: answer });
      // What the sender will really send: the answer's first video format.
      // Without `setCodecPreferences` that is the browser's choice, not ours.
      const t = videoPayloadTypes(answer);
      this.worker.postMessage({ kind: "codecs", side: "send", pts: t.pts, codec: t.first });
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
      // The forwarder's payload types are what received RTP carries; a browser
      // without `mimeType` (Firefox) knows each frame's codec from them.
      this.worker.postMessage({ kind: "codecs", side: "receive", pts: videoPayloadTypes(sdp).pts, codec: null });
      for (const t of this.down.getTransceivers()) {
        if (!t.receiver.__attached) {
          t.receiver.__attached = true;
          this.attach(t.receiver, "receive", t.receiver.track.kind === "video" ? "video" : "opus");
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
    const kind = info.kind === "screen" ? "screen" : e.track.kind;
    if (this.onStream) {
      // Only noted: the page holds the streams. Not `who[kind] = true`, which
      // `close` then took for an element and threw on, leaving the page
      // without its "The call ended".
      (who.kinds ??= new Set()).add(kind);
      this.onStream(info.participant, kind, stream);
      e.track.onended = () => this.onStream?.(info.participant, kind, null);
      return;
    }
    // The viewer draws cameras only.
    if (kind === "screen") return;
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
          video: { width: 320, height: 240, frameRate: 15, ...(this.videoDevice ? { deviceId: { exact: this.videoDevice } } : { facingMode: "user" }) },
        });
        this.camera = stream.getVideoTracks()[0];
        // A blur asked for before the camera was on.
        if (this.blur && this.blurSupported()) await this.camera.applyConstraints({ backgroundBlur: true }).catch(() => undefined);
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

  /** Share a screen, a window or a tab on the screen track (the browser asks
   *  which), or stop. `sharp` asks the encoder for legible text over motion.
   *  Resolves to whether something is being shared; the person ending it from
   *  the browser's own bar is a "screen-off" event. */
  async setScreen(on, sharp = true) {
    if (!this.screenSender) return false;
    if (on && !this.screenTrack) {
      let stream;
      try {
        stream = await navigator.mediaDevices.getDisplayMedia({
          video: { frameRate: { ideal: sharp ? 5 : 15, max: 15 }, width: { max: 1920 }, height: { max: 1080 } },
          audio: false,
          selfBrowserSurface: "exclude",
          surfaceSwitching: "include",
        });
      } catch (e) {
        // Cancelling the browser's picker is not worth a notice.
        if (e && e.name !== "NotAllowedError") this.onEvent("notice", describe(e, "Screen"));
        return false;
      }
      const track = stream.getVideoTracks()[0];
      if ("contentHint" in track) track.contentHint = sharp ? "detail" : "motion";
      this.screenTrack = track;
      track.onended = () => {
        if (this.screenTrack !== track) return;
        void this.setScreen(false);
        this.onEvent("screen-off", "");
      };
      await this.screenSender.replaceTrack(track);
      try {
        const p = this.screenSender.getParameters();
        p.degradationPreference = sharp ? "maintain-resolution" : "maintain-framerate";
        if (p.encodings && p.encodings[0]) p.encodings[0].maxBitrate = 1_200_000;
        await this.screenSender.setParameters(p);
      } catch {
        // A browser that will not take the hint still shares.
      }
      return true;
    }
    if (!on && this.screenTrack) {
      const track = this.screenTrack;
      this.screenTrack = null;
      await this.screenSender.replaceTrack(null);
      track.stop();
    }
    return !!this.screenTrack;
  }

  /** What this device shares, for its own preview, or null. */
  screenView() {
    return this.screenTrack ? new MediaStream([this.screenTrack]) : null;
  }

  /** Switch to another microphone mid-call: the new one replaces the old on
   *  the same sender, so nobody has to renegotiate. */
  async setAudioDevice(deviceId) {
    this.audioDevice = deviceId || null;
    if (!this.mic) return;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, ...(deviceId ? { deviceId: { exact: deviceId } } : {}) },
      });
      const next = stream.getAudioTracks()[0];
      next.enabled = !this.muted;
      const sender = this.up.getSenders().find((s) => s.track === this.mic);
      await sender?.replaceTrack(next);
      this.mic.stop();
      this.probe?.stop();
      this.mic = next;
      this.probe = next.clone();
    } catch (e) {
      this.onEvent("notice", describe(e, "Microphone"));
    }
  }

  /** Switch to another camera mid-call (or the next time it is turned on). */
  async setVideoDevice(deviceId) {
    this.videoDevice = deviceId || null;
    if (!this.camera) return;
    await this.setCamera(false);
    await this.setCamera(true);
  }

  /** Whether this camera can blur what is behind the person itself: the
   *  browser's and the system's own effect (`backgroundBlur`), done on the
   *  device before the picture is encoded. */
  blurSupported() {
    const caps = this.camera?.getCapabilities?.();
    return !!caps && Array.isArray(caps.backgroundBlur) && caps.backgroundBlur.includes(true);
  }

  /** Blur the background, where the camera can; resolves to whether it does. */
  async setBlur(on) {
    this.blur = on;
    if (!this.camera || !this.blurSupported()) return false;
    try {
      await this.camera.applyConstraints({ backgroundBlur: on });
      return on;
    } catch {
      return false;
    }
  }

  /** The microphone, never disabled: for hearing talking while muted. */
  levelTrack() {
    return this.probe;
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
        this.onStream?.(participant, "screen", null);
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
    this.probe?.stop();
    this.camera?.stop();
    this.screenTrack?.stop();
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
