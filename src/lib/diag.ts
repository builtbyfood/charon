// Diagnostics that work in release builds (no devtools): `[charon]` console lines
// are copied to the backend's stdout, and the output trace (Settings → Terminal)
// logs each output chunk's emit / receive / parse / paint times with one clock.

/** Live switches read by the terminals. */
export const diag = { trace: false };

/** ms since the epoch with sub-ms precision, comparable with the backend's `t`. */
export const epochMs = () => performance.timeOrigin + performance.now();

const text = (args: unknown[]) =>
  args.map((a) => {
    if (typeof a === "string") return a;
    if (a instanceof Error) return a.message;
    try { return JSON.stringify(a); } catch { return String(a); }
  }).join(" ");

/** Wrap console.info/log/warn/error: lines starting with `[charon]` also go to `send`. */
export function forwardConsole(send: (level: "info" | "warn" | "error", msg: string) => unknown) {
  for (const level of ["info", "log", "warn", "error"] as const) {
    const orig = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      orig(...args);
      if (typeof args[0] === "string" && args[0].startsWith("[charon]")) {
        try { Promise.resolve(send(level === "log" ? "info" : level, text(args))).catch(() => {}); } catch { /* backend gone */ }
      }
    };
  }
}
