// requestAnimationFrame fallback (Linux). WebKitGTK on some Wayland sessions stops
// delivering frame callbacks until an input event arrives, so xterm's render
// debouncer (and Monaco, the SFTP pane) sit one update behind: output lands in the
// buffer but is painted only on the next key or click. `term.refresh()` does not
// help — it waits for the same frame. Each rAF here also arms a timer; whichever
// fires first runs the callback and cancels the other. When frames flow the timer
// never wins, so normal rendering is untouched.

export interface RafHost {
  requestAnimationFrame(cb: FrameRequestCallback): number;
  cancelAnimationFrame(id: number): void;
  setTimeout(cb: () => void, ms: number): unknown;
  clearTimeout(id: unknown): void;
  performance: { now(): number };
}

export interface RafStats { native: number; fallback: number }

/** Wrap `host`'s rAF/cancel in place. Returns live counters of which path ran. */
export function installRafFallback(host: RafHost, delayMs = 50): RafStats {
  const stats: RafStats = { native: 0, fallback: 0 };
  const raf = host.requestAnimationFrame.bind(host);
  const caf = host.cancelAnimationFrame.bind(host);
  const st = host.setTimeout.bind(host);
  const ct = host.clearTimeout.bind(host);
  const pending = new Map<number, { frame: number; timer: unknown }>();
  let next = 0;
  host.requestAnimationFrame = (cb) => {
    const id = ++next;
    const run = (via: keyof RafStats, t: number) => {
      const p = pending.get(id);
      if (!p) return;
      pending.delete(id);
      if (via === "native") ct(p.timer); else caf(p.frame);
      stats[via]++;
      cb(t);
    };
    const frame = raf((t) => run("native", t));
    const timer = st(() => run("fallback", host.performance.now()), delayMs);
    pending.set(id, { frame, timer });
    return id;
  };
  host.cancelAnimationFrame = (id) => {
    const p = pending.get(id);
    if (!p) return;
    pending.delete(id);
    caf(p.frame); ct(p.timer);
  };
  return stats;
}
