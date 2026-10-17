// How the browser picks its send codec and tells the worker each frame's codec
// (docs/notes/calls.md): `bun test crates/cartopolis/web`.
import { expect, test } from "bun:test";
import { frameCodec } from "./sframe.js";
import { sendCodecs, videoPayloadTypes } from "./webcall.js";

const chrome = [
  { mimeType: "video/VP8", clockRate: 90000 },
  { mimeType: "video/rtx", clockRate: 90000 },
  { mimeType: "video/VP9", clockRate: 90000, sdpFmtpLine: "profile-id=0" },
  { mimeType: "video/VP9", clockRate: 90000, sdpFmtpLine: "profile-id=2" },
  { mimeType: "video/VP9", clockRate: 90000, sdpFmtpLine: "profile-id=1" },
  { mimeType: "video/H264", clockRate: 90000, sdpFmtpLine: "level-asymmetry-allowed=1;packetization-mode=1" },
  { mimeType: "video/AV1", clockRate: 90000 },
];

test("sends VP9 profile 0 where the browser has it, and only that", () => {
  const pick = sendCodecs(chrome);
  expect(pick.codec).toBe("vp9");
  expect(pick.prefs.map((c) => c.sdpFmtpLine)).toEqual(["profile-id=0"]);
});

test("a VP9 entry without a profile-id is profile 0 (Firefox)", () => {
  const firefox = [{ mimeType: "video/VP8" }, { mimeType: "video/VP9", sdpFmtpLine: "max-fs=12288;max-fr=60" }];
  expect(sendCodecs(firefox).codec).toBe("vp9");
});

test("falls back to VP8 where the browser has no VP9 profile 0 encoder", () => {
  const old = [{ mimeType: "video/VP8" }, { mimeType: "video/VP9", sdpFmtpLine: "profile-id=2" }, { mimeType: "video/H264" }];
  const pick = sendCodecs(old);
  expect(pick.codec).toBe("vp8");
  expect(pick.prefs.map((c) => c.mimeType)).toEqual(["video/VP8"]);
});

// The shape str0m answers and offers in: audio first, rtx beside each codec.
const sdp = [
  "v=0",
  "o=str0m-0 1 2 IN IP4 0.0.0.0",
  "m=audio 9 UDP/TLS/RTP/SAVPF 111",
  "a=rtpmap:111 opus/48000/2",
  "m=video 9 UDP/TLS/RTP/SAVPF 98 99 96 97",
  "a=rtpmap:98 VP9/90000",
  "a=fmtp:98 profile-id=0",
  "a=rtpmap:99 rtx/90000",
  "a=rtpmap:96 VP8/90000",
  "a=rtpmap:97 rtx/90000",
  "m=video 9 UDP/TLS/RTP/SAVPF 100",
  "a=rtpmap:100 vp8/90000",
  "",
].join("\r\n");

test("reads every video payload type and the first one sent", () => {
  const t = videoPayloadTypes(sdp);
  expect(t.pts).toEqual({ 98: "vp9", 96: "vp8", 100: "vp8" });
  expect(t.first).toBe("vp9");
});

test("an SDP with no video says so", () => {
  expect(videoPayloadTypes("v=0\nm=audio 9 UDP/TLS/RTP/SAVPF 111\na=rtpmap:111 opus/48000/2\n")).toEqual({ pts: {}, first: null });
});

test("a frame's codec: mimeType, then payload type, then the connection's, then the fallback", () => {
  const table = { pts: { 98: "vp9", 96: "vp8" }, codec: "vp9" };
  expect(frameCodec({ mimeType: "video/VP8", payloadType: 98 }, table, "video")).toEqual({ codec: "vp8", known: true });
  expect(frameCodec({ payloadType: 96 }, table, "video")).toEqual({ codec: "vp8", known: true });
  expect(frameCodec({ payloadType: 120 }, table, "video")).toEqual({ codec: "vp9", known: false });
  expect(frameCodec({}, { pts: {}, codec: null }, "vp8")).toEqual({ codec: "vp8", known: false });
});
