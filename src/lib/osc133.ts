// OSC 133 command marks (FinalTerm / iTerm2 / WezTerm semantics):
//   A prompt start · B command input start · C output start · D;<exit> finished.
// The tracker records one CommandMark per prompt, draws a gutter bar per
// command (red when exit ≠ 0) and answers "where is the previous command /
// what did the last one print" for jump keys and the context menu.
import type { IDecoration, IMarker, Terminal } from "@xterm/xterm";

export interface CommandMark {
  /** Absolute buffer lines (scrollback + viewport). */
  promptLine: number;
  cmdStartLine?: number;
  cmdStartCol?: number;
  outputStartLine?: number;
  /** Last line of output (inclusive); set at D. */
  endLine?: number;
  exitCode?: number;
  commandText?: string;
  marker?: IMarker;
  decoration?: IDecoration;
}

/** A mark without its xterm objects (exportMarks / importMarks). */
export type SavedMark = Omit<CommandMark, "marker" | "decoration">;

export interface TrackerOptions {
  /** Session colour for the gutter bar. */
  color: string;
  /** Fired the first time any 133 sequence arrives (the remote already has integration). */
  onfirst?: () => void;
  /** Fired after every 133 sequence (mark count / state changed). */
  onchange?: () => void;
  /** Fired at each command-input start (133;B) with the full prompt text. */
  onprompt?: (promptText: string) => void;
}

const FAIL_COLOR = "#ff6b6b";

export class CommandTracker {
  readonly marks: CommandMark[] = [];
  private seen = false;
  private dispose: () => void;
  /** Index of the command the jump keys have landed on; undefined = the live prompt. */
  private focus: number | undefined;
  private flashDeco: IDecoration | undefined;
  private flashTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(private term: Terminal, private opts: TrackerOptions) {
    const h = term.parser.registerOscHandler(133, (data) => { this.handle(data); return true; });
    this.dispose = () => h.dispose();
  }

  destroy(): void {
    this.dispose();
    clearTimeout(this.flashTimer);
    this.flashDeco?.dispose();
    for (const m of this.marks) { m.decoration?.dispose(); m.marker?.dispose(); }
    this.marks.length = 0;
  }

  /** The marks as plain data with current line numbers, for moving the tab to
   *  another window (the screen is serialised at the same size, so lines match). */
  exportMarks(): SavedMark[] {
    this.prune();
    return this.marks.map((m) => {
      const d = this.lineOf(m) - m.promptLine;
      const { marker: _m, decoration: _d, ...rest } = m;
      const sh = (n: number | undefined) => (n === undefined ? undefined : n + d);
      return { ...rest, promptLine: m.promptLine + d, cmdStartLine: sh(m.cmdStartLine), outputStartLine: sh(m.outputStartLine), endLine: sh(m.endLine) };
    });
  }

  /** Restore exported marks after the screen was written back. Gutter bars need a
   *  marker in the normal buffer; with a full-screen program up (alternate
   *  buffer) the marks keep only their data (queries still work). */
  importMarks(saved: SavedMark[]): void {
    const b = this.term.buffer.active;
    const normal = b.type === "normal";
    const cursor = b.baseY + b.cursorY;
    for (const s of saved) {
      const m: CommandMark = { ...s };
      if (normal) m.marker = this.term.registerMarker(s.promptLine - cursor);
      this.marks.push(m);
      if (m.outputStartLine !== undefined) this.draw(m, m.endLine !== undefined ? m.exitCode : undefined);
    }
    if (saved.length) this.seen = true;
  }

  /** True once the session has produced any 133 sequence. */
  get active(): boolean { return this.seen; }

  private line(): number {
    const b = this.term.buffer.active;
    return b.baseY + b.cursorY;
  }

  private handle(data: string): void {
    if (!this.seen) { this.seen = true; this.opts.onfirst?.(); }
    const [kind, arg] = data.split(";", 2);
    const cur = this.marks.at(-1);
    switch (kind) {
      case "A": {
        // A new prompt. A prompt re-drawn on the same line (Ctrl+L, resize) replaces the open mark.
        if (cur && cur.outputStartLine === undefined && cur.promptLine === this.line()) return;
        const marker = this.term.registerMarker(0);
        this.marks.push({ promptLine: this.line(), marker });
        this.focus = undefined; // typing at a new prompt ends any jump navigation
        this.prune();
        break;
      }
      case "B":
        if (cur) {
          cur.cmdStartLine = this.line();
          cur.cmdStartCol = this.term.buffer.active.cursorX;
          if (this.opts.onprompt) {
            const b = this.term.buffer.active;
            let text = "";
            for (let i = this.lineOf(cur); i <= cur.cmdStartLine; i++) text += b.getLine(i)?.translateToString(true) ?? "";
            this.opts.onprompt(text);
          }
        }
        break;
      case "C":
        if (cur && cur.outputStartLine === undefined) {
          cur.commandText = this.readCommand(cur);
          cur.outputStartLine = this.line();
          this.draw(cur, undefined);
        }
        break;
      case "D": {
        if (!cur || cur.endLine !== undefined) break;
        const code = arg !== undefined && arg !== "" ? Number(arg) : undefined;
        cur.exitCode = Number.isFinite(code) ? code : undefined;
        // D is printed right before the next prompt: at column 0 of a fresh line when the
        // output ended with a newline, else at the end of the last output line.
        const l = this.line();
        cur.endLine = this.term.buffer.active.cursorX === 0 ? l - 1 : l;
        if (cur.outputStartLine === undefined) cur.outputStartLine = cur.endLine + 1; // no C (empty Enter)
        this.draw(cur, cur.exitCode);
        break;
      }
    }
    this.opts.onchange?.();
  }

  /** Text the user typed: from the B position to the end of the input, joining wrapped lines. */
  private readCommand(m: CommandMark): string {
    if (m.cmdStartLine === undefined) return "";
    const b = this.term.buffer.active;
    const last = b.cursorX === 0 ? this.line() - 1 : this.line();
    let out = "";
    for (let i = m.cmdStartLine; i <= last; i++) {
      const line = b.getLine(i);
      if (!line) break;
      const text = line.translateToString(true, i === m.cmdStartLine ? m.cmdStartCol : 0);
      out += (i > m.cmdStartLine && !line.isWrapped ? "\n" : "") + text;
    }
    return out.trim();
  }

  /** Gutter bar from the prompt to the end of the output (or just the prompt while running). */
  private draw(m: CommandMark, exit: number | undefined): void {
    m.decoration?.dispose();
    if (!m.marker || m.marker.isDisposed) return;
    const end = m.endLine ?? m.promptLine;
    const height = Math.max(1, end - m.marker.line + 1);
    const color = exit !== undefined && exit !== 0 ? FAIL_COLOR : this.opts.color;
    const d = this.term.registerDecoration({ marker: m.marker, x: 0, width: 1, height, layer: "bottom" });
    if (!d) return;
    d.onRender((el) => {
      // Lives in the terminal's left padding (see .term in Terminal.svelte).
      el.style.width = "3px";
      el.style.marginLeft = "-8px";
      el.style.borderRadius = "2px";
      el.style.background = color;
      el.style.opacity = exit === undefined ? "0.35" : "0.9";
      el.style.pointerEvents = "none";
      el.title = m.commandText ? `${m.commandText}${exit !== undefined ? `  → exit ${exit}` : ""}` : "";
    });
    m.decoration = d;
  }

  /** Drop marks whose lines scrolled out of the buffer. */
  private prune(): void {
    while (this.marks.length && this.marks[0].marker?.isDisposed) {
      this.marks.shift()!.decoration?.dispose();
      if (this.focus !== undefined) this.focus = this.focus > 0 ? this.focus - 1 : undefined;
    }
  }

  // ---------------------------------------------------------------- queries

  private lineOf(m: CommandMark): number {
    return m.marker && !m.marker.isDisposed ? m.marker.line : m.promptLine;
  }

  /**
   * Move the jump focus to the previous/next command and bring its prompt into
   * view, flashing the prompt line. Focus starts at the live prompt, so Ctrl+Up
   * from a fresh prompt lands on the last command even when nothing has scrolled.
   */
  jump(dir: -1 | 1): boolean {
    this.prune();
    if (!this.marks.length) return false;
    const last = this.marks.length - 1;
    const cur = this.focus ?? last;
    const next = Math.max(0, Math.min(last, cur + dir));
    if (next === cur) return false;
    this.focus = next === last ? undefined : next;
    const m = this.marks[next];
    const line = this.lineOf(m);
    const b = this.term.buffer.active;
    if (line < b.viewportY || line >= b.viewportY + this.term.rows) this.term.scrollToLine(line);
    this.flash(m);
    return true;
  }

  /** The command the jump keys are on, if any. */
  focused(): CommandMark | undefined {
    return this.focus !== undefined ? this.marks[this.focus] : undefined;
  }

  /** Briefly highlight a command's prompt line so a jump is visible. */
  private flash(m: CommandMark): void {
    clearTimeout(this.flashTimer);
    this.flashDeco?.dispose();
    if (!m.marker || m.marker.isDisposed) return;
    const d = this.term.registerDecoration({ marker: m.marker, x: 0, width: this.term.cols, height: 1, layer: "bottom" });
    if (!d) return;
    d.onRender((el) => { el.style.background = this.opts.color; el.style.opacity = "0.25"; el.style.pointerEvents = "none"; });
    this.flashDeco = d;
    this.flashTimer = setTimeout(() => { d.dispose(); if (this.flashDeco === d) this.flashDeco = undefined; }, 700);
  }

  /** The command whose prompt…output range contains `line` (default: the cursor line). */
  at(line = this.line()): CommandMark | undefined {
    this.prune();
    for (let i = this.marks.length - 1; i >= 0; i--) {
      const m = this.marks[i];
      if (this.lineOf(m) <= line) return m;
    }
    return undefined;
  }

  /** What Ctrl+Shift+Up acts on: the focused command, else the finished one under the cursor, else the last finished. */
  selectTarget(): CommandMark | undefined {
    const m = this.focused() ?? this.at();
    return m && m.endLine !== undefined ? m : this.last();
  }

  /** Text of the command being typed, from the input start to the cursor, or null when not at a live command. */
  inputToCursor(): string | null {
    const cur = this.marks.at(-1);
    if (!cur || cur.cmdStartLine === undefined || cur.outputStartLine !== undefined) return null;
    const b = this.term.buffer.active;
    const curLine = b.baseY + b.cursorY;
    let out = "";
    for (let i = cur.cmdStartLine; i <= curLine; i++) {
      const line = b.getLine(i);
      if (!line) break;
      const start = i === cur.cmdStartLine ? cur.cmdStartCol ?? 0 : 0;
      const end = i === curLine ? b.cursorX : undefined;
      out += line.translateToString(true, start, end);
    }
    return out;
  }

  /** Most recent command that has finished. */
  last(): CommandMark | undefined {
    this.prune();
    for (let i = this.marks.length - 1; i >= 0; i--) if (this.marks[i].endLine !== undefined) return this.marks[i];
    return undefined;
  }

  /** The last `n` finished commands with exit code and output (for MCP read_scrollback). */
  recent(n: number): { command: string; exit_code: number | null; output: string }[] {
    this.prune();
    const done = this.marks.filter((m) => m.endLine !== undefined);
    return done.slice(-n).map((m) => ({
      command: m.commandText ?? "",
      exit_code: m.exitCode ?? null,
      output: this.outputOf(m),
    }));
  }

  /** Output lines of a command, or "" when it produced none. */
  outputOf(m: CommandMark): string {
    if (m.outputStartLine === undefined || m.endLine === undefined || m.endLine < m.outputStartLine) return "";
    // Convert recorded absolute lines to current ones if the buffer trimmed scrollback.
    const shift = this.lineOf(m) - m.promptLine;
    const b = this.term.buffer.active;
    const lines: string[] = [];
    for (let i = m.outputStartLine + shift; i <= m.endLine + shift; i++) {
      const line = b.getLine(i);
      if (!line) break;
      const text = line.translateToString(true);
      if (line.isWrapped && lines.length) lines[lines.length - 1] += text; else lines.push(text);
    }
    return lines.join("\n");
  }

  /** Select a command's output in the terminal. */
  selectOutput(m: CommandMark): boolean {
    if (m.outputStartLine === undefined || m.endLine === undefined || m.endLine < m.outputStartLine) return false;
    const shift = this.lineOf(m) - m.promptLine;
    this.term.selectLines(m.outputStartLine + shift, m.endLine + shift);
    return true;
  }
}

// ---------------------------------------------------------------- shell detection + injection

// Pure prompt helpers live in prompt.ts so they can be unit-tested without the ?raw imports.
export { detectShell, looksLikeShellPrompt, looksLikeSudoPrompt, sudoUserOf, sudoAutoEligible, parseOsc7, promptUserOf, type Shell } from "./prompt";
import type { Shell } from "./prompt";

import bashHook from "../../shell-integration/bash.sh?raw";
import zshHook from "../../shell-integration/zsh.zsh?raw";
import fishHook from "../../shell-integration/fish.fish?raw";
import pwshHook from "../../shell-integration/pwsh.ps1?raw";

/** The hook script as shipped, for "Copy hook text" (source it from an rc file). */
export function hookTextFor(shell: Shell): string {
  switch (shell) {
    case "bash": return bashHook;
    case "zsh": return zshHook;
    case "fish": return fishHook;
    case "pwsh": return pwshHook;
  }
}

/**
 * Private OSC the injected block prints last. The terminal swallows everything
 * from the moment it starts typing the block (the tty echo precedes any output
 * the remote could emit, so the start of the pair is client-side) until this
 * arrives, so the user only sees the prompt come back.
 */
export const INJECT_END = "\x1b]7770;end\x07";
const INJECT_END_BYTES = Array.from(INJECT_END, (c) => c.charCodeAt(0));

/**
 * Keystrokes that install the hook without touching remote rc files. POSIX
 * shells get one `eval "$(cat <<'EOF' … EOF)"` command; fish and PowerShell take
 * the script line by line. A leading space keeps it out of history where
 * HISTCONTROL=ignorespace is set. Each ends by printing INJECT_END.
 */
export function injectionFor(shell: Shell): string {
  const strip = (s: string) => s.split("\n").filter((l) => l.trim() && !l.trim().startsWith("#")).join("\n");
  const end = String.raw` printf '\033]7770;end\a'`;
  switch (shell) {
    case "bash": return ` eval "$(cat <<'CHARON_EOF'\n${strip(bashHook)}\nCHARON_EOF\n)";${end}\n`;
    case "zsh": return ` eval "$(cat <<'CHARON_EOF'\n${strip(zshHook)}\nCHARON_EOF\n)";${end}\n`;
    case "fish": return strip(fishHook).split("\n").map((l) => ` ${l}\n`).join("") + `${end}\n`;
    // pwsh: a multi-line function typed line-by-line drops into `>>` continuation
    // and never cleanly completes, so send it as one base64 line via Invoke-Expression;
    // it then runs at once and the end sentinel fires so the whole thing is swallowed.
    case "pwsh": {
      const b64 = btoa(unescape(encodeURIComponent(strip(pwshHook))));
      return ` Invoke-Expression ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${b64}'))); [Console]::Write("$([char]27)]7770;end$([char]7)")\n`;
    }
  }
}

/**
 * Drops session output while an injection is being echoed, until INJECT_END
 * (which may straddle chunks) or a safety timeout. Bytes after the sentinel pass
 * through, preceded by CR + erase-line (`\r\x1b[2K`) so the freshly-hooked prompt
 * overwrites the original prompt still on screen — otherwise the injection leaves
 * two prompts stacked (the "double bash" on first connect), since the echo of the
 * typed hook is swallowed and never advanced xterm's cursor off that first line.
 */
export class InjectionSwallower {
  private active = false;
  private until = 0;
  private tail: number[] = [];

  start(timeoutMs = 5000): void { this.active = true; this.until = Date.now() + timeoutMs; this.tail = []; }
  get swallowing(): boolean { return this.active; }

  /** Bytes to display; empty while swallowing. */
  filter(b: Uint8Array): Uint8Array {
    if (!this.active) return b;
    if (Date.now() > this.until) { this.active = false; return b; }
    const buf = this.tail.concat(Array.from(b));
    const at = indexOf(buf, INJECT_END_BYTES);
    if (at < 0) {
      // Keep enough to recognise a sentinel split across chunks.
      this.tail = buf.slice(Math.max(0, buf.length - INJECT_END_BYTES.length + 1));
      return new Uint8Array(0);
    }
    this.active = false; this.tail = [];
    // \r \x1b [ 2 K : return to column 0 and clear the line the stale prompt is on,
    // so the new hooked prompt replaces it instead of appearing beneath it.
    return new Uint8Array([0x0d, 0x1b, 0x5b, 0x32, 0x4b, ...buf.slice(at + INJECT_END_BYTES.length)]);
  }
}

function indexOf(hay: number[], needle: number[]): number {
  outer: for (let i = 0; i + needle.length <= hay.length; i++) {
    for (let j = 0; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}
