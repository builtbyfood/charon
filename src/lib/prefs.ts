// Terminal/clipboard preferences the UI reads at runtime, derived from the
// profile (data/profiles/default.toml). Defaults here only cover the moment
// before the profile has loaded.
import type { Profile } from "./api";
import { PANE_DEFAULT, PANE_MIN, DEFAULT_COLS, colsFromProfile, type ColLayout } from "./filecols";
import { rendererSetting, resolveRenderer, type RendererSetting } from "./renderer";
import { DANGEROUS_DEFAULT } from "./broadcast";
export type { RendererSetting } from "./renderer";

export interface Prefs {
  fontFamily: string;
  fontSize: number;
  scrollback: number;
  /** The renderer in use: the setting, with "auto" resolved for this platform. */
  renderer: "webgl" | "canvas" | "dom";
  /** What Settings → Terminal says: "auto" (default) or one of the renderers. */
  rendererSetting: RendererSetting;
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
  /** Broadcast switches off after this many idle minutes; 0 = never. */
  broadcastIdleMin: number;
  /** While broadcasting, Enter on a command with one of these prefixes asks first; [] = never. */
  broadcastConfirm: string[];
  /** Max files transferring at once during a recursive / multi-item transfer. */
  filesParallel: number;
  /** Gutter with the time each line was printed. */
  timestamps: boolean;
  /** File pane width (px) and its list columns. */
  filesWidth: number;
  filesColumns: ColLayout;
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
  renderer: resolveRenderer("auto"),
  rendererSetting: "auto",
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
  broadcastIdleMin: 10,
  broadcastConfirm: [...DANGEROUS_DEFAULT],
  timestamps: false,
  filesWidth: PANE_DEFAULT,
  filesColumns: DEFAULT_COLS,
};

export function prefsFromProfile(p: Profile): Prefs {
  return {
    fontFamily: fontStack(p.appearance.font ?? ""),
    fontSize: clamp(p.appearance.font_size, 6, 40, DEFAULTS.fontSize),
    scrollback: clamp(p.appearance.scrollback, 0, 1_000_000, DEFAULTS.scrollback),
    renderer: resolveRenderer(rendererSetting(p.appearance.renderer)),
    rendererSetting: rendererSetting(p.appearance.renderer),
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
    broadcastIdleMin: clamp(p.broadcast?.idle_off_minutes, 0, 24 * 60, DEFAULTS.broadcastIdleMin),
    broadcastConfirm: p.broadcast?.confirm === false ? [] : Array.isArray(p.broadcast?.confirm_prefixes) ? p.broadcast!.confirm_prefixes!.map(String) : [...DANGEROUS_DEFAULT],
    timestamps: p.terminal.timestamps === true,
    filesWidth: clamp(p.files?.pane_width, PANE_MIN, 10_000, DEFAULTS.filesWidth),
    filesColumns: colsFromProfile(p.files?.column_widths, p.files?.hidden_columns),
  };
}

function clamp(v: unknown, lo: number, hi: number, dflt: number): number {
  return typeof v === "number" && Number.isFinite(v) ? Math.max(lo, Math.min(hi, v)) : dflt;
}
