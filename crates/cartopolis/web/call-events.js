// Sealing a call's reactions (protocol 41, `CallReact`), as the native client
// does in `crates/calls/src/events.rs` — read that for the why. The key is
// HMAC-SHA256(base key, label ‖ epoch LE), first 16 bytes, for AES-128-GCM;
// a sealed event is a random 12-byte nonce then the ciphertext and tag, with
// the epoch as associated data. `call-events.test.ts` holds this to the
// native vectors in `crates/calls/testdata/event-vectors.json`.

const LABEL = new TextEncoder().encode("cartopolis call events");

/** The epoch as 8 little-endian bytes. `epoch` may be a number or a bigint. */
function epochBytes(epoch) {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(epoch), true);
  return b;
}

async function eventKey(baseKey, epoch) {
  const mac = await crypto.subtle.importKey("raw", baseKey, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const info = new Uint8Array(LABEL.length + 8);
  info.set(LABEL);
  info.set(epochBytes(epoch), LABEL.length);
  const out = new Uint8Array(await crypto.subtle.sign("HMAC", mac, info));
  return crypto.subtle.importKey("raw", out.slice(0, 16), "AES-GCM", false, ["encrypt", "decrypt"]);
}

/** Seal `plain` (bytes) for `epoch`. `nonce` is only for the test vectors. */
export async function sealEvent(baseKey, epoch, plain, nonce = crypto.getRandomValues(new Uint8Array(12))) {
  const key = await eventKey(baseKey, epoch);
  const body = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: epochBytes(epoch) }, key, plain),
  );
  const out = new Uint8Array(12 + body.length);
  out.set(nonce);
  out.set(body, 12);
  return out;
}

/** Open what `sealEvent` (or the native client) sealed; null for anything else. */
export async function openEvent(baseKey, epoch, sealed) {
  if (sealed.length < 12) return null;
  try {
    const key = await eventKey(baseKey, epoch);
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: sealed.slice(0, 12), additionalData: epochBytes(epoch) },
      key,
      sealed.slice(12),
    );
    return new Uint8Array(plain);
  } catch {
    return null;
  }
}
