// Terminal/clipboard preferences the UI reads at runtime, derived from the
// profile (data/profiles/default.toml). Defaults here only cover the moment
// before the profile has loaded.
import type { Profile } from "./api";

export interface Prefs {
  fontFamily: string;
  fontSize: number;
  scrollback: number;
  /** "webgl" (default), "canvas", or "dom" if the GPU renderers misbehave. */
  renderer: "webgl" | "canvas" | "dom";
  copyOnSelect: boolean;
  ctrlCCopiesWhenSelected: boolean;
  middleClickPaste: boolean;
  /** "moba": right-click copies the selection or pastes; Shift+right-click opens the menu. "menu": always the menu. */
  rightClick: "moba" | "menu";
  /** Warn when pasting at least this many lines. */
  pasteGuardLines: number;
  /** Bracketed paste ending in a newline: "auto" runs it, "wait" leaves it highlighted. */
  pasteRun: "auto" | "wait";
  smartQuotesToAscii: boolean;
  trimTrailingWhitespace: boolean;
  /** Open the file pane on every SSH/local connect (MobaXterm habit). */
  filesAutoOpen: boolean;
  /** Auto-send the saved password at a sudo/su prompt (debounced). */
  sudoAutoSend: boolean;
  /** Max files transferring at once during a recursive / multi-item transfer. */
  filesParallel: number;
  /** Gutter with the time each line was printed. */
  timestamps: boolean;
}

// xterm needs a literal font list; it cannot resolve CSS variables. The
// profile's font goes first, these are the fallbacks.
const FALLBACK_FONTS = '"Cascadia Mono", "Cascadia Code", "JetBrains Mono", "Fira Code", Consolas, "DejaVu Sans Mono", monospace';

/** A font name as an xterm font list: the font, then the fallbacks. */
export function fontStack(name: string): string {
  const n = name.replace(/"/g, "").trim();
  return n ? `"${n}", ${FALLBACK_FONTS}` : `"JetBrains Mono", ${FALLBACK_FONTS}`;
}

/** A connection's own terminal font (Settings.font / font_size); unset = the profile's. */
export interface SessionFont { family?: string; size?: number }

export const DEFAULTS: Prefs = {
  fontFamily: `"JetBrains Mono", ${FALLBACK_FONTS}`,
  fontSize: 13,
  scrollback: 50000,
  renderer: "webgl",
  copyOnSelect: false,
  ctrlCCopiesWhenSelected: true,
  middleClickPaste: true,
  rightClick: "moba",
  pasteGuardLines: 2,
  pasteRun: "auto",
  smartQuotesToAscii: true,
  trimTrailingWhitespace: true,
  filesAutoOpen: true,
  filesParallel: 4,
  sudoAutoSend: false,
  timestamps: false,
};

export function prefsFromProfile(p: Profile): Prefs {
  return {
    fontFamily: fontStack(p.appearance.font ?? ""),
    fontSize: clamp(p.appearance.font_size, 6, 40, DEFAULTS.fontSize),
    scrollback: clamp(p.appearance.scrollback, 0, 1_000_000, DEFAULTS.scrollback),
    renderer: p.appearance.renderer === "dom" || p.appearance.renderer === "canvas" ? p.appearance.renderer : "webgl",
    copyOnSelect: !!p.terminal.copy_on_select,
    ctrlCCopiesWhenSelected: p.terminal.ctrl_c_copies_when_selected !== false,
    middleClickPaste: p.terminal.middle_click_paste !== false,
    rightClick: p.terminal.right_click === "menu" ? "menu" : "moba",
    pasteGuardLines: clamp(p.terminal.paste_guard_lines, 1, 1000, DEFAULTS.pasteGuardLines),
    pasteRun: p.terminal.paste_run === "wait" ? "wait" : "auto",
    smartQuotesToAscii: p.clipboard.smart_quotes_to_ascii !== false,
    trimTrailingWhitespace: p.clipboard.trim_trailing_whitespace !== false,
    filesAutoOpen: p.files?.auto_open !== false,
    filesParallel: clamp(p.files?.parallel_transfers ?? 4, 1, 64, DEFAULTS.filesParallel),
    sudoAutoSend: p.sudo?.auto_send === true,
    timestamps: p.terminal.timestamps === true,
  };
}

function clamp(v: unknown, lo: number, hi: number, dflt: number): number {
  return typeof v === "number" && Number.isFinite(v) ? Math.max(lo, Math.min(hi, v)) : dflt;
}
