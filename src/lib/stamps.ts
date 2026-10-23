// Line timestamps (Terminal.svelte). The terminal records one stamp per second in
// which a new line started: a marker on that line plus the time. A line's time is
// the newest stamp at or above it, so a burst of output costs at most one marker a
// second. Pure helpers here; the markers and the gutter live in Terminal.svelte.

export interface Stamp { line: number; t: number }

/** Index of the newest stamp at or above `line` (stamps sorted by line), or -1. */
export function stampIndex(stamps: Stamp[], line: number): number {
  let lo = 0, hi = stamps.length - 1, hit = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (stamps[mid].line <= line) { hit = mid; lo = mid + 1; } else hi = mid - 1;
  }
  return hit;
}

/** HH:MM:SS in local time; the date too when it is not today. */
export function fmtStamp(t: number, now = Date.now()): string {
  const d = new Date(t);
  const p = (n: number) => String(n).padStart(2, "0");
  const hms = `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
  return new Date(now).toDateString() === d.toDateString() ? hms : `${d.getMonth() + 1}/${d.getDate()} ${hms}`;
}

/** One gutter label per viewport row: the time for rows that hold text and start a
 *  line (soft-wrapped continuations get none); `repeat` marks a row with the same
 *  time as the labelled row above it, which the gutter draws dimmer. */
export interface GutterRow { text: string; repeat: boolean }
export function gutterRows(
  stamps: Stamp[], top: number, rows: number,
  lineAt: (line: number) => { text: string; wrapped: boolean } | undefined,
  now = Date.now(),
): GutterRow[] {
  const out: GutterRow[] = [];
  let prev = "";
  for (let r = 0; r < rows; r++) {
    const l = lineAt(top + r);
    const i = stampIndex(stamps, top + r);
    if (!l || l.wrapped || !l.text.trim() || i < 0) { out.push({ text: "", repeat: false }); continue; }
    const text = fmtStamp(stamps[i].t, now);
    out.push({ text, repeat: text === prev });
    prev = text;
  }
  return out;
}
