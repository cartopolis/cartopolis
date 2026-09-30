// The browser's SFrame against the native one: `bun test crates/cartopolis/web`.
// The vectors are written by the Rust test `the_browser_vectors_are_what_this_seals`
// (crates/calls/src/keys.rs); a difference here is a call the two cannot hear.
import { expect, test } from "bun:test";
import { CallKeys } from "./sframe.js";
import vectors from "../../calls/testdata/sframe-vectors.json";

const bytes = (hex: string) => Uint8Array.from(hex.match(/../g) ?? [], (b) => parseInt(b, 16));
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const base = bytes(vectors.base_key);

test("seals what the native client seals", async () => {
  const senders = new Map<number, CallKeys>();
  for (const v of vectors.frames) {
    let keys = senders.get(v.index);
    if (!keys) {
      keys = new CallKeys(vectors.context);
      keys.setEpoch(vectors.epoch, base, v.index);
      senders.set(v.index, keys);
    }
    expect(hex(await keys.seal(v.codec, bytes(v.frame)))).toBe(v.sealed);
  }
});

test("opens what the native client seals, and says who sent it", async () => {
  const rx = new CallKeys(1);
  rx.setEpoch(vectors.epoch, base, 9);
  for (const v of vectors.frames) {
    const opened = await rx.open(v.codec, bytes(v.sealed));
    expect(hex(opened.frame)).toBe(v.frame);
    expect(opened.sender).toBe(v.index);
    expect(opened.epoch).toBe(BigInt(vectors.epoch));
  }
});

test("refuses a frame that was tampered with or is from another epoch", async () => {
  const rx = new CallKeys(1);
  rx.setEpoch(vectors.epoch, base, 9);
  const v = vectors.frames[1];
  const bad = bytes(v.sealed);
  bad[0] ^= 1; // the clear VP8 prefix is authenticated too
  await expect(rx.open(v.codec, bad)).rejects.toThrow();
  const other = new CallKeys(1);
  other.setEpoch(vectors.epoch + 1, base, 9);
  await expect(other.open(v.codec, bytes(v.sealed))).rejects.toThrow();
});
