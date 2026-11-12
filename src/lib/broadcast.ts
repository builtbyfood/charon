/**
 * MultiExec (broadcast input): the active tab's keystrokes and pastes go to every
 * selected tab's session too. The decisions live here, pure, so node tests can
 * drive them; `App.svelte` owns the selection and `Terminal.svelte` the input.
 */

export interface BroadcastTab { id: string; session: string | null }

/**
 * Tabs that receive the active tab's input besides itself, in tab-strip order:
 * the selected ones that have a live session, never the active tab, no repeats.
 */
export function broadcastTargets(tabs: BroadcastTab[], selected: ReadonlySet<string>, active: string | null): BroadcastTab[] {
  const seen = new Set<string>();
  const out: BroadcastTab[] = [];
  for (const t of tabs) {
    if (t.id === active || !selected.has(t.id) || !t.session || seen.has(t.session)) continue;
    seen.add(t.session);
    out.push(t);
  }
  return out;
}

/**
 * Write `data` to each session in order. Every write is started even if an
 * earlier one fails (a closed session must not stop the rest); the failures
 * are returned.
 */
export async function fanOut(sessions: string[], data: string, write: (session: string, data: string) => Promise<unknown> | unknown): Promise<{ session: string; error: string }[]> {
  const started = sessions.map((s) => {
    try { return Promise.resolve(write(s, data)).then(() => null, (e) => ({ session: s, error: String(e) })); }
    catch (e) { return Promise.resolve({ session: s, error: String(e) }); }
  });
  return (await Promise.all(started)).filter((r): r is { session: string; error: string } => r !== null);
}

/**
 * Input xterm produced on its own rather than from the user: answers to the
 * remote's queries (device attributes, cursor position, status), mouse and
 * focus reports, OSC/DCS replies. They describe the active terminal only, so
 * they are never broadcast.
 */
export function isTerminalReport(d: string): boolean {
  return /^\x1b\[[?>=]?[\d;]*[cRn]$/.test(d)        // DA1/DA2/DA3, CPR, DSR
    || /^\x1b\[[IO]$/.test(d)                          // focus in/out
    || /^\x1b\[M[\s\S]{3}$/.test(d)                    // X10 / normal mouse
    || /^\x1b\[<[\d;]+[Mm]$/.test(d)                   // SGR mouse
    || /^\x1b\[\??[\d;]*\$y$/.test(d)                  // DECRPM
    || /^\x1b\[[\d;]*t$/.test(d)                       // window reports
    || /^\x1b[\]P_^]/.test(d);                         // OSC / DCS / APC / PM replies
}

/**
 * Which of the source terminal's `onData` goes to the targets, decided by where
 * it came from, never by timing: keyboard input does; a paste's own echo through
 * `term.paste` doesn't (the paste is fanned out once, separately); xterm's replies
 * to the remote (cursor/status reports, mouse, focus) don't.
 */
export function fansOut(data: string, pasting: boolean): boolean {
  return !pasting && data.length > 0 && !isTerminalReport(data);
}

/**
 * The last line of output as it would sit on screen, for a tab that is not
 * rendering (a hidden broadcast target): escapes dropped, `\r` returns to the
 * line start, `\n` starts a new line. `prev` is the line so far.
 */
export function tailLine(prev: string, chunk: string): string {
  const text = chunk
    .replace(/\x1b\][^\x07\x1b]*(\x07|\x1b\\)/g, "")
    .replace(/\x1b\[[0-9;?<>=]*[ -\/]*[@-~]/g, "")
    .replace(/\x1b[()][0-9A-Za-z]|\x1b[=>78DEHM]/g, "");
  let line = prev;
  for (const part of text.split("\n").map((p, i) => ({ p, i }))) {
    if (part.i > 0) line = "";
    const segs = part.p.split("\r");
    // `\r` alone returns to the start; text after it overwrites from there.
    for (let j = 0; j < segs.length; j++) {
      const s = segs[j].replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "");
      if (j === 0) line += s;
      else if (s) line = s + line.slice(s.length);
    }
  }
  return line.length > 512 ? line.slice(-512) : line;
}

/**
 * The sudo button in broadcast mode: each tab that is at a password prompt
 * answers it with its own session's stored credential (`send` is called with
 * that tab's session id, which the backend resolves to that session's
 * password). Tabs not at a prompt get nothing — a password typed into a shell
 * would echo it.
 */
export async function fanSudo(targets: { session: string; prompted: boolean }[], send: (session: string) => Promise<boolean>): Promise<{ session: string; sent: boolean }[]> {
  const out: { session: string; sent: boolean }[] = [];
  for (const t of targets) {
    if (!t.prompted) { out.push({ session: t.session, sent: false }); continue; }
    let sent = false;
    try { sent = await send(t.session); } catch { sent = false; }
    out.push({ session: t.session, sent });
  }
  return out;
}

/** Commands that ask before a broadcast Enter sends them (profile `broadcast.confirm_prefixes`). */
export const DANGEROUS_DEFAULT = ["rm", "dd", "mkfs", "shutdown", "reboot", "poweroff", "halt", "systemctl stop"];

/**
 * The first prefix in `prefixes` that a command in `line` starts with, or null.
 * Every command in a list or pipeline counts (`cd /x && rm -rf *`), and leading
 * `sudo` / `env` / `VAR=x` / `command` / `exec` / `nohup` / `time` are looked
 * past. A prefix is matched by whole words (`rm` matches `rm -rf`, not `rmdir`;
 * `mkfs` also matches `mkfs.ext4`).
 */
export function dangerousCommand(line: string, prefixes: readonly string[]): string | null {
  const wrappers = new Set(["sudo", "env", "command", "exec", "nohup", "time", "doas"]);
  for (const segment of line.split(/\|\||&&|[;|&\n]/)) {
    let words = segment.trim().split(/\s+/).filter(Boolean);
    // sudo/doas options that take a value (`-u root`): drop the value with them.
    const withArg = new Set(["-u", "-g", "-h", "-p", "-C", "-D", "-r", "-t", "-U", "-T"]);
    while (words.length && (wrappers.has(words[0]) || /^[A-Za-z_]\w*=/.test(words[0]) || (words[0].startsWith("-") && words.length > 1))) {
      words = words.slice(withArg.has(words[0]) ? 2 : 1);
    }
    if (!words.length) continue;
    const bin = words[0].split("/").pop()!;
    for (const p of prefixes) {
      const want = p.trim().split(/\s+/).filter(Boolean);
      if (!want.length) continue;
      const first = want[0] === "mkfs" ? bin === "mkfs" || bin.startsWith("mkfs.") : bin === want[0];
      if (first && want.slice(1).every((w, i) => words[i + 1] === w)) return p.trim();
    }
  }
  return null;
}

/**
 * The line being typed, kept from keystrokes for when shell integration can't
 * read it off the screen: printable text appends, Backspace removes, Ctrl+U / C
 * clear, Enter ends it (returns ""). Cursor movement isn't modelled, so it is a
 * best effort; the OSC 133 reading is preferred.
 */
export function typedLine(prev: string, data: string): string {
  let line = prev;
  for (let i = 0; i < data.length; i++) {
    const c = data[i];
    if (c === "\x1b") { // skip an escape sequence (arrows etc.)
      const m = /^\x1b(\[[0-9;?]*[ -\/]*[@-~]|O.|.)/s.exec(data.slice(i));
      i += (m ? m[0].length : 1) - 1;
    } else if (c === "\r" || c === "\n" || c === "\x03" || c === "\x15") line = "";
    else if (c === "\x7f" || c === "\b") line = line.slice(0, -1);
    else if (c >= " ") line += c;
  }
  return line;
}

/** Broadcast has been idle this long (no input fanned out): switch it off. */
export function idleExpired(lastActivity: number, now: number, minutes: number): boolean {
  return minutes > 0 && now - lastActivity >= minutes * 60_000;
}
