// SFrame (RFC 9605) for calls in the browser: the same frames, byte for byte,
// as `cartopolis_calls::keys` seals natively (docs/notes/calls.md).
//
// Why a JavaScript copy and not the Rust compiled to a second wasm module: the
// browser hands each encoded frame to a *worker* (an encoded transform), and a
// second wasm build of its own — with the threads flags the main build needs —
// is a second artifact in the web deploy for ~150 lines of HKDF and AES-GCM
// that WebCrypto already does. What keeps the two the same is
// `crates/calls/testdata/sframe-vectors.json`: written by the Rust test
// `the_browser_vectors_are_what_this_seals`, checked by `sframe.test.ts`.
//
// The pieces, all fixed by the Rust side:
// - Cipher suite AES_128_GCM_SHA256_128: 16-byte key, 12-byte nonce, 16-byte tag.
// - Key ID = context << 24 | member index << 8 | epoch & 0xff (the MLS layout,
//   8 epoch bits and 16 index bits).
// - key/salt = HKDF-SHA256(salt = none, ikm = the chat's base key for the
//   epoch), expanded with "SFrame 1.0 Secret key " / "SFrame 1.0 Secret salt "
//   followed by the key ID (8 bytes) and the suite id (2 bytes), big-endian.
// - A VP8 frame keeps its first 10 bytes (keyframe) or 3 (otherwise) in the
//   clear, in front of the header, and they are authenticated with it: the
//   forwarder needs them to see keyframes. The authenticated data is those
//   bytes *then* the header — the order the `sframe` crate lays them out in
//   its buffer, which is the reverse of RFC 9605 §4.4's header-then-metadata.
//   The native client is what this has to agree with, so it follows the crate.

const SUITE_ID = 0x0004; // RFC 9605: AES_128_GCM_SHA256_128
const KEY_LEN = 16;
const NONCE_LEN = 12;
const EPOCHS_KEPT = 2;
const enc = new TextEncoder();

/** How many bytes of a frame stay readable. */
export function clearPrefix(codec, frame) {
  let n = 0;
  if (codec === "vp8" && frame.length > 0) n = (frame[0] & 1) === 0 ? 10 : 3;
  return Math.min(n, frame.length);
}

function be(value, len) {
  const out = new Uint8Array(len);
  let v = BigInt(value);
  for (let i = len - 1; i >= 0; i--) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

function minLen(value) {
  let n = 1;
  while (n < 8 && BigInt(value) >> BigInt(8 * n) !== 0n) n++;
  return n;
}

function concat(...parts) {
  const out = new Uint8Array(parts.reduce((s, p) => s + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

function encodeHeader(kid, ctr) {
  let config = 0;
  const tail = [];
  if (kid < 8n) {
    config |= Number(kid) << 4;
  } else {
    const n = minLen(kid);
    config |= 0x80 | ((n - 1) << 4);
    tail.push(be(kid, n));
  }
  if (ctr < 8n) {
    config |= Number(ctr);
  } else {
    const n = minLen(ctr);
    config |= 0x08 | (n - 1);
    tail.push(be(ctr, n));
  }
  return concat(Uint8Array.of(config), ...tail);
}

/** `{ kid, ctr, length }` of the header at the start of `bytes`, or null. */
function decodeHeader(bytes) {
  if (bytes.length < 1) return null;
  const config = bytes[0];
  let at = 1;
  const read = (n) => {
    if (at + n > bytes.length) return null;
    let v = 0n;
    for (let i = 0; i < n; i++) v = (v << 8n) | BigInt(bytes[at + i]);
    at += n;
    return v;
  };
  const kid = config & 0x80 ? read(((config >> 4) & 7) + 1) : BigInt((config >> 4) & 7);
  if (kid === null) return null;
  const ctr = config & 0x08 ? read((config & 7) + 1) : BigInt(config & 7);
  if (ctr === null) return null;
  return { kid, ctr, length: at };
}

async function deriveKey(baseKey, kid) {
  const ikm = await crypto.subtle.importKey("raw", baseKey, "HKDF", false, ["deriveBits"]);
  const expand = (label, bits) =>
    crypto.subtle.deriveBits(
      {
        name: "HKDF",
        hash: "SHA-256",
        salt: new Uint8Array(0),
        info: concat(enc.encode("SFrame 1.0 "), enc.encode(label), be(kid, 8), be(SUITE_ID, 2)),
      },
      ikm,
      bits,
    );
  const [key, salt] = await Promise.all([
    expand("Secret key ", KEY_LEN * 8),
    expand("Secret salt ", NONCE_LEN * 8),
  ]);
  return {
    key: await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["encrypt", "decrypt"]),
    salt: new Uint8Array(salt),
  };
}

function nonce(salt, ctr) {
  const c = be(ctr, NONCE_LEN);
  return salt.map((s, i) => s ^ c[i]);
}

/** One device's keys for one call: what it seals with, and what it opens. */
export class CallKeys {
  /** `context`: a random 32-bit number, new for every join. */
  constructor(context) {
    this.context = BigInt(context >>> 0);
    this.epochs = []; // { epoch: bigint, baseKey, ownIndex }
    this.sender = null; // { epoch, kid, key, salt, ctr }
    this.receivers = new Map(); // kid → Promise<{ key, salt }>
  }

  /** The chat moved to a new epoch (or the call began). */
  setEpoch(epoch, baseKey, ownIndex) {
    epoch = BigInt(epoch);
    if (this.epochs.length && this.epochs[this.epochs.length - 1].epoch === epoch) return;
    this.epochs.push({ epoch, baseKey: new Uint8Array(baseKey), ownIndex });
    while (this.epochs.length > EPOCHS_KEPT) this.epochs.shift();
    const live = new Set(this.epochs.map((e) => e.epoch & 0xffn));
    for (const kid of this.receivers.keys()) if (!live.has(kid & 0xffn)) this.receivers.delete(kid);
    this.sender = null;
  }

  async seal(codec, frame) {
    const current = this.epochs[this.epochs.length - 1];
    if (!current) throw new Error("no key for this call yet");
    if (!this.sender || this.sender.epoch !== current.epoch) {
      const kid = (this.context << 24n) | (BigInt(current.ownIndex) << 8n) | (current.epoch & 0xffn);
      this.sender = { epoch: current.epoch, kid, ctr: 0n, ...(await deriveKey(current.baseKey, kid)) };
    }
    const s = this.sender;
    const ctr = s.ctr++;
    const clear = clearPrefix(codec, frame);
    const meta = frame.subarray(0, clear);
    const header = encodeHeader(s.kid, ctr);
    const sealed = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: nonce(s.salt, ctr), additionalData: concat(meta, header), tagLength: 128 },
      s.key,
      frame.subarray(clear),
    );
    return concat(meta, header, new Uint8Array(sealed));
  }

  /** `{ sender, epoch, frame }`, or throws if it is not for this call. */
  async open(codec, bytes) {
    const clear = clearPrefix(codec, bytes);
    const meta = bytes.subarray(0, clear);
    const rest = bytes.subarray(clear);
    const h = decodeHeader(rest);
    if (!h) throw new Error("not an SFrame frame");
    const lsb = h.kid & 0xffn;
    const epoch = this.epochs.find((e) => (e.epoch & 0xffn) === lsb);
    if (!epoch) throw new Error("no key for that epoch");
    let secret = this.receivers.get(h.kid);
    if (!secret) {
      secret = deriveKey(epoch.baseKey, h.kid);
      this.receivers.set(h.kid, secret);
    }
    const { key, salt } = await secret;
    const header = rest.subarray(0, h.length);
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: nonce(salt, h.ctr), additionalData: concat(meta, header), tagLength: 128 },
      key,
      rest.subarray(h.length),
    );
    return {
      sender: Number((h.kid >> 8n) & 0xffffn),
      epoch: epoch.epoch,
      frame: concat(meta, new Uint8Array(plain)),
    };
  }
}
