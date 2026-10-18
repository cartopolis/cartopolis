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

// A guest's key (`crates/calls/src/events.rs`, "A guest's key"): the call's
// key a host hands a guest, sealed under the secret in the guest link's `#…`
// with the chat's id as associated data. Plain bytes: epoch u64 LE, own index
// u32 LE, member count u16 LE, members u32 LE each, then the base key.

const GUEST_LABEL = new TextEncoder().encode("cartopolis guest key");

/** Where a guest's sender indices start: above every member's. */
export const GUEST_INDEX_BASE = 0x8000;

/** The sender index of the guest seated as `participant`. */
export const guestIndex = (participant) => GUEST_INDEX_BASE | (Number(participant) & 0x7fff);

async function guestCipher(secret) {
  const mac = await crypto.subtle.importKey("raw", secret, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const out = new Uint8Array(await crypto.subtle.sign("HMAC", mac, GUEST_LABEL));
  return crypto.subtle.importKey("raw", out.slice(0, 16), "AES-GCM", false, ["encrypt", "decrypt"]);
}

/** `{epoch: bigint, baseKey: Uint8Array, ownIndex, members: number[]}` → sealed bytes. */
export async function sealGuestKey(secret, group, key, nonce = crypto.getRandomValues(new Uint8Array(12))) {
  const plain = new Uint8Array(14 + 4 * key.members.length + key.baseKey.length);
  const v = new DataView(plain.buffer);
  v.setBigUint64(0, BigInt(key.epoch), true);
  v.setUint32(8, key.ownIndex, true);
  v.setUint16(12, key.members.length, true);
  key.members.forEach((m, i) => v.setUint32(14 + 4 * i, m, true));
  plain.set(key.baseKey, 14 + 4 * key.members.length);
  const body = new Uint8Array(
    await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce, additionalData: group }, await guestCipher(secret), plain),
  );
  const out = new Uint8Array(12 + body.length);
  out.set(nonce);
  out.set(body, 12);
  return out;
}

/** Open what a host sealed for this guest; null for another link or chat. */
export async function openGuestKey(secret, group, sealed) {
  if (sealed.length < 12) return null;
  try {
    const plain = new Uint8Array(
      await crypto.subtle.decrypt(
        { name: "AES-GCM", iv: sealed.slice(0, 12), additionalData: group },
        await guestCipher(secret),
        sealed.slice(12),
      ),
    );
    const v = new DataView(plain.buffer);
    const n = v.getUint16(12, true);
    if (plain.length <= 14 + 4 * n) return null;
    return {
      epoch: v.getBigUint64(0, true),
      ownIndex: v.getUint32(8, true),
      members: Array.from({ length: n }, (_, i) => v.getUint32(14 + 4 * i, true)),
      baseKey: plain.slice(14 + 4 * n),
    };
  } catch {
    return null;
  }
}
