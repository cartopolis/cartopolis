// The browser's sealed reactions against the native ones: `bun test crates/cartopolis/web`.
// The vectors are written by the Rust test `the_browser_vectors_are_what_this_seals`
// (crates/calls/src/events.rs); a difference here is a reaction the other side drops.
import { expect, test } from "bun:test";
import { openEvent, sealEvent } from "./call-events.js";
import vectors from "../../calls/testdata/event-vectors.json";

const bytes = (hex: string) => Uint8Array.from(hex.match(/../g) ?? [], (b) => parseInt(b, 16));
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const base = bytes(vectors.base_key);

test("seals what the native client seals", async () => {
  for (const v of vectors.events) {
    const sealed = bytes(v.sealed);
    expect(hex(await sealEvent(base, BigInt(v.epoch), bytes(v.plain), sealed.slice(0, 12)))).toBe(v.sealed);
  }
});

test("opens what the native client seals, and nothing from another epoch", async () => {
  for (const v of vectors.events) {
    expect(hex((await openEvent(base, BigInt(v.epoch), bytes(v.sealed)))!)).toBe(v.plain);
    expect(await openEvent(base, BigInt(v.epoch) - 1n, bytes(v.sealed))).toBeNull();
  }
});
