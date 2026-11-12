/**
 * Typing the shell hook in on connect, and hiding its echo. Pure (no `?raw`
 * imports, the hook text is passed in), so node tests can drive it.
 */

/**
 * Printed after the injected hook ran, whether or not it installed anything.
 * Session output is swallowed until it (or `INJECT_SKIP`) arrives, so the user
 * only sees the prompt come back.
 */
export const INJECT_END = "\x1b]7770;end\x07";
/** Printed instead of `INJECT_END` when the shell wasn't the one the hook is for. */
export const INJECT_SKIP = "\x1b]7770;skip\x07";
const END_BYTES = Array.from(INJECT_END, (c) => c.charCodeAt(0));
const SKIP_BYTES = Array.from(INJECT_SKIP, (c) => c.charCodeAt(0));

/** Comment and blank lines don't need to cross the wire. */
export function stripHook(s: string): string {
  return s.split("\n").filter((l) => l.trim() && !l.trim().startsWith("#")).join("\n");
}

/**
 * Bash / zsh: one `eval "$(cat <<'EOF' … EOF)"`, behind a `case` on
 * `$BASH_VERSION` / `$ZSH_VERSION` so the prompt detector's guess is checked by
 * the shell itself. Anything else that parses POSIX `case` (BusyBox ash, dash)
 * skips the hook and prints `INJECT_SKIP`; a shell that is what it looked like
 * runs it and prints `INJECT_END`. Either way a marker ends the swallow. A
 * leading space keeps it out of history where HISTCONTROL=ignorespace is set.
 */
export function posixInjection(shell: "bash" | "zsh", hook: string): string {
  const version = shell === "bash" ? "BASH_VERSION" : "ZSH_VERSION";
  const skip = String.raw`printf '\033]7770;skip\a'`;
  const end = String.raw`printf '\033]7770;end\a'`;
  return ` case "\${${version}-}" in "") ${skip};; *) eval "$(cat <<'CHARON_EOF'\n${stripHook(hook)}\nCHARON_EOF\n)"; ${end};; esac\n`;
}

export type InjectResult = "end" | "skip" | "timeout";

/** Swallowing ends this long after the last byte came in without a marker... */
export const SWALLOW_IDLE_MS = 1500;
/** ...and no later than this after it started, however busy the echo is. */
export const SWALLOW_CAP_MS = 20000;
/** Held bytes beyond this are dropped (an echo is a few KB). */
const HOLD_MAX = 256 * 1024;
const MARKER_MAX = Math.max(END_BYTES.length, SKIP_BYTES.length);
const CLEAR_LINE = [0x0d, 0x1b, 0x5b, 0x32, 0x4b]; // \r ESC [ 2 K

/**
 * Drops session output while an injection is being echoed, until a marker
 * (which may straddle chunks) or a deadline. Bytes after the marker pass
 * through, preceded by CR + erase-line (`\r\x1b[2K`) so the freshly-hooked prompt
 * overwrites the original prompt still on screen — otherwise the injection leaves
 * two prompts stacked (the "double bash" on first connect), since the echo of the
 * typed hook is swallowed and never advanced xterm's cursor off that first line.
 *
 * The deadline is inactivity: `SWALLOW_IDLE_MS` with no bytes after the first
 * one, so a slow link that is still echoing keeps it going, up to
 * `SWALLOW_CAP_MS` in all. Before the first byte only the cap applies. When it
 * gives up (`expire`), what it held is shown minus the lines that are the
 * injected text's echo (`withoutEcho`), not the whole echo.
 */
export class InjectionSwallower {
  private active = false;
  private started = 0;
  private lastByte: number | undefined;
  private held: number[] = [];
  private injected = "";
  /** How the last swallow ended; `undefined` while one runs or before the first. */
  result: InjectResult | undefined;

  start(injected = "", now = Date.now()): void {
    this.active = true; this.started = now; this.lastByte = undefined;
    this.held = []; this.injected = injected; this.result = undefined;
  }
  get swallowing(): boolean { return this.active; }

  /** Bytes to display; empty while swallowing. */
  filter(b: Uint8Array, now = Date.now()): Uint8Array {
    if (!this.active) return b;
    const late = this.expire(now);
    if (late) return concat(late, b);
    this.lastByte = now;
    const from = Math.max(0, this.held.length - MARKER_MAX + 1);
    for (const x of b) this.held.push(x);
    const end = indexOf(this.held, END_BYTES, from), skip = indexOf(this.held, SKIP_BYTES, from);
    const hit = end >= 0 && (skip < 0 || end < skip) ? { at: end, len: END_BYTES.length, r: "end" as const }
      : skip >= 0 ? { at: skip, len: SKIP_BYTES.length, r: "skip" as const } : undefined;
    if (!hit) {
      if (this.held.length > HOLD_MAX) this.held = this.held.slice(this.held.length - HOLD_MAX);
      return new Uint8Array(0);
    }
    const rest = this.held.slice(hit.at + hit.len);
    this.active = false; this.held = []; this.result = hit.r;
    return new Uint8Array([...CLEAR_LINE, ...rest]);
  }

  /**
   * Past the deadline: stop swallowing and return what to show of the held
   * bytes (possibly empty). `undefined` while still within it, or not swallowing.
   * Call it on a timer; `filter` also calls it.
   */
  expire(now = Date.now()): Uint8Array | undefined {
    if (!this.active) return undefined;
    const idle = this.lastByte !== undefined && now - this.lastByte >= SWALLOW_IDLE_MS;
    if (!idle && now - this.started < SWALLOW_CAP_MS) return undefined;
    const kept = withoutEcho(new Uint8Array(this.held), this.injected);
    this.active = false; this.held = []; this.result = "timeout";
    return kept.length ? new Uint8Array([...CLEAR_LINE, ...kept]) : kept;
  }
}

/**
 * `held` minus the lines that echo `injected`: a line goes if, once escapes
 * and readline's redraws (`\r` + repainted text) are taken out and a `> `
 * continuation prompt is dropped, it is a piece of the injected text, or it
 * holds the start of its first line (the prompt the echo began on). Other lines
 * (an error message, the new prompt) stay, byte for byte.
 */
export function withoutEcho(held: Uint8Array, injected: string): Uint8Array {
  const text = new TextDecoder().decode(held);
  const want = injected.replace(/\r/g, "");
  const head = want.split("\n")[0].trim().slice(0, 24);
  const isEcho = (line: string): boolean => {
    let t = line.replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "").replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, "");
    t = t.replace(/\r+$/, "");
    t = t.slice(t.lastIndexOf("\r") + 1); // a redraw repaints from the line start
    t = t.replace(/^(> )+/, "").trim();
    if (!t) return false;
    return want.includes(t) || (head.length > 0 && t.includes(head));
  };
  return new TextEncoder().encode(text.split("\n").filter((l) => !isEcho(l)).join("\n"));
}

function concat(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length + b.length);
  out.set(a); out.set(b, a.length);
  return out;
}

function indexOf(hay: number[], needle: number[], from = 0): number {
  outer: for (let i = from; i + needle.length <= hay.length; i++) {
    for (let j = 0; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}
