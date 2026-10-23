import { test } from "node:test";
import assert from "node:assert/strict";
import { installRafFallback, type RafHost } from "./rafguard.ts";

/** A host whose frames and timers fire only when the test says so. */
function fakeHost() {
  const frames = new Map<number, FrameRequestCallback>();
  const timers = new Map<number, () => void>();
  let id = 0;
  const host: RafHost = {
    requestAnimationFrame: (cb) => { frames.set(++id, cb); return id; },
    cancelAnimationFrame: (i) => { frames.delete(i); },
    setTimeout: (cb) => { timers.set(++id, cb); return id; },
    clearTimeout: (i) => { timers.delete(i as number); },
    performance: { now: () => 1234 },
  };
  const flush = (m: Map<number, (t: number) => void>) => { const all = [...m.values()]; m.clear(); all.forEach((f) => f(99)); };
  return { host, frames, timers, frame: () => flush(frames), tick: () => flush(timers) };
}

test("frames flowing: the callback runs once, from the frame, and the timer is dropped", () => {
  const f = fakeHost();
  const stats = installRafFallback(f.host);
  const got: number[] = [];
  f.host.requestAnimationFrame((t) => got.push(t));
  f.frame();
  assert.deepEqual(got, [99]);
  assert.equal(f.timers.size, 0);
  f.tick();
  assert.deepEqual(got, [99]);
  assert.deepEqual(stats, { native: 1, fallback: 0 });
});

test("frames stalled: the timer runs the callback, and a late frame does not repeat it", () => {
  const f = fakeHost();
  const stats = installRafFallback(f.host);
  const got: number[] = [];
  f.host.requestAnimationFrame((t) => got.push(t));
  f.tick();
  assert.deepEqual(got, [1234]);
  assert.equal(f.frames.size, 0);
  f.frame();
  assert.deepEqual(got, [1234]);
  assert.deepEqual(stats, { native: 0, fallback: 1 });
});

test("cancel stops both paths", () => {
  const f = fakeHost();
  installRafFallback(f.host);
  let ran = false;
  const id = f.host.requestAnimationFrame(() => (ran = true));
  f.host.cancelAnimationFrame(id);
  f.frame(); f.tick();
  assert.equal(ran, false);
  assert.equal(f.frames.size + f.timers.size, 0);
});
