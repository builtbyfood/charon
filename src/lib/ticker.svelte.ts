// CHARON_DEBUG_TICKER=1: a plain DOM counter in the terminal status bar, bumped
// every 250 ms by setInterval, no xterm involved. Isolates "the page changed
// but WebKitGTK didn't present it" from anything xterm does: compare the logged
// `[charon] ticker n=` lines (stdout) with what the status bar shows. Each tick
// also pokes the Linux repaint workaround (`repaint.rs`), so with
// linux_repaint = "off" it is the bare test and with it on it tests the fix.
import { repaintPoke } from "./api";
import { epochMs } from "./diag";

export const ticker = $state({ on: false, n: 0 });

let timer: ReturnType<typeof setInterval> | undefined;

export function startTicker(): void {
  if (timer) return;
  ticker.on = true;
  timer = setInterval(() => {
    const n = ++ticker.n;
    repaintPoke()
      .catch(() => "?")
      .then((mode) => console.info(`[charon] ticker n=${n} t=${epochMs().toFixed(1)} repaint=${mode}`));
  }, 250);
}
