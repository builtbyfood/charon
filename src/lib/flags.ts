// Extract command-line flags from a tldr page and/or a `--help` dump, for the
// flag picker (P5). Pure and tested; the UI merges the two sources.

export interface Flag {
  /** All spellings that mean the same option, e.g. ["-l", "--long"]. */
  names: string[];
  /** One-line description, if known. */
  desc?: string;
  /** Where it came from (for the badge / ranking). */
  from: "help" | "tldr";
}

const FLAG = /^--?[A-Za-z0-9][\w-]*$/;

/** Split "-l, --long=FILE" style lead-ins into flag names (dropping =ARG / <ARG>). */
function namesIn(segment: string): string[] {
  const names: string[] = [];
  for (const raw of segment.split(/[,\s]+/)) {
    const tok = raw.replace(/[=\[].*$/, "").trim();
    if (FLAG.test(tok)) names.push(tok);
  }
  return names;
}

/**
 * Parse a `--help` dump. Recognises the common layouts:
 *   `  -l, --long        description`
 *   `  --long=FILE       description` (description may wrap to the next indented line)
 */
export function flagsFromHelp(text: string): Flag[] {
  const out: Flag[] = [];
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const m = /^\s{1,10}(--?[A-Za-z0-9][\w-]*(?:[=\[][^\s]*)?(?:[,\s]+--?[A-Za-z0-9][\w-]*(?:[=\[][^\s]*)?)*)(?:\s{2,}(.*))?$/.exec(line);
    if (!m) continue;
    const names = namesIn(m[1]);
    if (!names.length) continue;
    let desc = (m[2] ?? "").trim();
    // Description continues on the next line when this line was only the flag.
    if (!desc && /^\s{6,}\S/.test(lines[i + 1] ?? "")) desc = lines[i + 1].trim();
    out.push({ names, desc: desc || undefined, from: "help" });
  }
  return dedupe(out);
}

/** Pull flags out of a tldr page's example code spans, tagged with the example text. */
export function flagsFromTldr(markdown: string): Flag[] {
  const out: Flag[] = [];
  const lines = markdown.split(/\r?\n/);
  let lastDesc = "";
  for (const line of lines) {
    const d = /^-\s+(.*):\s*$/.exec(line);
    if (d) { lastDesc = d[1].trim(); continue; }
    const code = /^`(.+)`$/.exec(line.trim());
    if (!code) continue;
    for (const tok of code[1].split(/\s+/)) {
      const clean = tok.replace(/[=\[].*$/, "");
      if (FLAG.test(clean)) out.push({ names: [clean], desc: lastDesc || undefined, from: "tldr" });
    }
  }
  return dedupe(out);
}

/** Merge sources: help wins for descriptions; aliases collapse to one entry. */
export function mergeFlags(...sources: Flag[][]): Flag[] {
  const byName = new Map<string, Flag>();
  for (const flag of sources.flat()) {
    // Reuse the entry any of these names already belongs to, else start one.
    let target: Flag | undefined;
    for (const n of flag.names) { const e = byName.get(n); if (e) { target = e; break; } }
    if (!target) target = { names: [], from: flag.from, desc: flag.desc };
    for (const n of flag.names) if (!target.names.includes(n)) target.names.push(n);
    if (flag.from === "help") {
      if (target.from !== "help") { target.from = "help"; target.desc = flag.desc ?? target.desc; }
      else if (!target.desc) target.desc = flag.desc;
    } else if (!target.desc) {
      target.desc = flag.desc;
    }
    for (const n of target.names) byName.set(n, target); // all aliases point at the one object
  }
  return [...new Set(byName.values())].sort((a, b) => shortest(a).localeCompare(shortest(b)));
}

export interface FlagAccept {
  /** Characters to APPEND to what's already typed. Never deletes; may be "". */
  insert: string;
  /** True when Enter should run the line instead of completing (exact / no match). */
  passthrough: boolean;
}

/**
 * Decide what pressing Enter on the highlighted flag does, given the live typed
 * `prefix` (the `-token` under the cursor) and that flag's `names`:
 *   - prefix exactly equals a spelling  → passthrough (the user wants to run it);
 *   - a spelling extends the prefix      → insert only the missing suffix
 *     (`--al` + `--all` ⇒ "l");
 *   - nothing matches (stale highlight)  → passthrough, never a corrupting edit.
 * The caller only ever appends `insert` and never sends a newline itself; on
 * passthrough it lets the Enter reach the shell.
 */
export function flagAccept(prefix: string, names: string[]): FlagAccept {
  if (names.includes(prefix)) return { insert: "", passthrough: true };
  const name = names.find((n) => n.startsWith(prefix));
  if (!name) return { insert: "", passthrough: true };
  return { insert: name.slice(prefix.length), passthrough: false };
}

export interface FlagAcceptHooks {
  /** Append these characters to the shell line (only ever the flag suffix). */
  send: (chars: string) => void;
  /** Return focus to the terminal. */
  focus: () => void;
  /** Dismiss the popup. */
  close: () => void;
}

/**
 * Perform an accept — whether triggered by Enter or a mouse click — on the flag
 * at `index`, given the live typed `prefix`. Sends ONLY the missing suffix
 * (never the name list, description, or any row text), then always closes the
 * popup and refocuses the terminal. Returns whether it inserted (the caller
 * swallows the Enter) vs passed through (let the Enter run the line). Both the
 * keyboard and mouse paths go through here, so their `sent` value is identical.
 */
export function runFlagAccept(
  prefix: string,
  flags: Flag[],
  index: number,
  h: FlagAcceptHooks,
): { consumed: boolean; insert: string; selected: string } {
  const flag = flags[index];
  if (!flag) {
    h.close();
    h.focus();
    return { consumed: false, insert: "", selected: "" };
  }
  const { insert, passthrough } = flagAccept(prefix, flag.names);
  const selected = flag.names.find((n) => n.startsWith(prefix)) ?? flag.names[0];
  if (insert && !passthrough) h.send(insert);
  h.close();
  h.focus();
  return { consumed: !!insert && !passthrough, insert, selected };
}

/** Flags whose first name starts with `prefix` (what the user has typed). */
export function matchFlags(flags: Flag[], prefix: string): Flag[] {
  if (prefix === "-" || prefix === "--") {
    // "-" shows short flags first; "--" shows only long flags.
    const long = prefix === "--";
    return flags.filter((f) => f.names.some((n) => (long ? n.startsWith("--") : true)));
  }
  return flags.filter((f) => f.names.some((n) => n.startsWith(prefix)));
}

function shortest(f: Flag): string {
  return [...f.names].sort((a, b) => a.length - b.length)[0];
}

function dedupe(flags: Flag[]): Flag[] {
  const seen = new Set<string>();
  return flags.filter((f) => {
    const key = f.names.join(",");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/** Decides whether the flag picker may evaluate. It only ever reacts to typed
 *  keystrokes: a paste suppresses it from the moment it begins until the next
 *  133;B prompt after the send, and any input that wasn't a single keypress
 *  (a paste, an IME burst) keeps it closed until the user types again. */
export class PickerGate {
  private suppressed = false;
  private sending = false;
  private last: "key" | "paste" | "bulk" = "key";   // kind of the last input event
  private logged = false;          // one log line per suppression

  /** A paste is starting (before the guard bar / queue / send). */
  pasteBegin(): void { this.suppressed = true; this.last = "paste"; this.logged = false; }
  /** Wrap the actual write so its onData echo isn't mistaken for a keypress. */
  send(fn: () => void): void {
    this.sending = true;
    try { fn(); } finally { this.sending = false; }
  }
  /** A paste was cancelled at the guard bar: nothing was sent, nothing to wait for. */
  pasteCancelled(): void { this.suppressed = false; }
  /** 133;B — a fresh prompt. Ends a paste's suppression once its send is done. */
  prompt(): void { if (!this.sending) this.suppressed = false; }
  /** Every xterm onData. Outside a paste send, classify it as keypress or bulk. */
  input(d: string): void {
    if (this.sending) return;
    this.last = isSingleKeypress(d) ? "key" : "bulk";
    if (this.last === "key") this.logged = false;
  }
  /** null → may evaluate; otherwise the reason it may not. */
  blocked(): "paste" | "bulk" | null {
    if (this.suppressed || this.last === "paste") return "paste";
    if (this.last === "bulk") return "bulk";
    return null;
  }
  /** True the first time a given suppression is hit (so the log isn't per-cursor-move). */
  shouldLog(): boolean { if (this.logged) return false; this.logged = true; return true; }
}

/** One key's worth of terminal input: a single character (incl. a surrogate
 *  pair) or one escape sequence (arrows, Alt+x, F-keys). */
export function isSingleKeypress(d: string): boolean {
  if ([...d].length === 1) return true;
  return /^\x1b(\[[0-9;?]*[ -\/]*[@-~]|O.|.)$/s.test(d);
}
