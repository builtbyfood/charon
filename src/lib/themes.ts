// Colour themes: one palette per theme for the UI chrome (CSS variables), the
// terminal (xterm theme incl. the 16 ANSI colours) and the editor (Monaco). The
// profile's `appearance.theme` picks one, or "system" to follow the OS. Applied
// by setting CSS variables + `data-theme` on <html>; the terminal and Monaco
// watch that attribute, so no component has to thread the theme through props.

export type ThemeId = "charon-dark" | "charon-light";
export type ThemePref = ThemeId | "system";

/** CSS custom properties every theme defines (the names app.css and components use). */
export const UI_TOKENS = [
  "bg", "bg-raised", "bg-sunken", "line", "fg", "fg-dim", "accent", "on-accent",
  "danger", "warn", "ok", "overlay", "shadow", "term-bg", "muted",
] as const;
export type UiToken = (typeof UI_TOKENS)[number];

export const ANSI = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;

export interface TermColors {
  background: string; foreground: string; cursor: string; cursorAccent: string;
  selectionBackground: string; selectionInactiveBackground: string;
  black: string; red: string; green: string; yellow: string; blue: string; magenta: string; cyan: string; white: string;
  brightBlack: string; brightRed: string; brightGreen: string; brightYellow: string;
  brightBlue: string; brightMagenta: string; brightCyan: string; brightWhite: string;
}

export interface Theme {
  id: ThemeId;
  label: string;
  scheme: "dark" | "light";
  ui: Record<UiToken, string>;
  term: TermColors;
  /** Monaco: base theme + colour overrides. */
  editor: { base: "vs" | "vs-dark"; colors: Record<string, string> };
  /** Search-match highlights in the terminal. */
  search: { match: string; active: string; ruler: string };
}

const dark: Theme = {
  id: "charon-dark",
  label: "Dark",
  scheme: "dark",
  ui: {
    "bg": "#16181d",
    "bg-raised": "#1e2128",
    "bg-sunken": "#101215",
    "line": "#2c3039",
    "fg": "#d9dce2",
    "fg-dim": "#959aa5",
    "accent": "#6aa2ff",
    "on-accent": "#06101f",
    "danger": "#ff6b6b",
    "warn": "#ffb454",
    "ok": "#5fd38a",
    "overlay": "#00000099",
    "shadow": "#00000088",
    "term-bg": "#101215",
    "muted": "#5a6275",
  },
  term: {
    background: "#101215", foreground: "#d9dce2", cursor: "#6aa2ff", cursorAccent: "#101215",
    selectionBackground: "#3a5a9a80", selectionInactiveBackground: "#3a5a9a4d",
    black: "#1d2026", red: "#ef6b73", green: "#8fcf6f", yellow: "#e8c27c",
    blue: "#62aef0", magenta: "#c88ae6", cyan: "#58bfc9", white: "#c9cdd5",
    brightBlack: "#6a7180", brightRed: "#ff8a91", brightGreen: "#abe38f", brightYellow: "#f5d699",
    brightBlue: "#86c3ff", brightMagenta: "#dba6f5", brightCyan: "#7dd7df", brightWhite: "#f0f2f5",
  },
  editor: {
    base: "vs-dark",
    colors: {
      "editor.background": "#101215",
      "editor.foreground": "#d9dce2",
      "editorLineNumber.foreground": "#4b5160",
      "editorLineNumber.activeForeground": "#9aa3b2",
      "editor.lineHighlightBackground": "#16191e",
      "editorWhitespace.foreground": "#3a4150",
      "editorIndentGuide.background": "#22262d",
      "editor.selectionBackground": "#3a5a9a80",
    },
  },
  search: { match: "#ffb45455", active: "#ffb454aa", ruler: "#ffb454" },
};

const light: Theme = {
  id: "charon-light",
  label: "Light",
  scheme: "light",
  ui: {
    "bg": "#f4f5f7",
    "bg-raised": "#ffffff",
    "bg-sunken": "#e9ecf0",
    "line": "#d3d8df",
    "fg": "#1d2128",
    "fg-dim": "#59616d",
    "accent": "#2f6fdb",
    "on-accent": "#ffffff",
    "danger": "#c53434",
    "warn": "#9a5a00",
    "ok": "#1b7f45",
    "overlay": "#1d212859",
    "shadow": "#1d212833",
    "term-bg": "#fbfbfc",
    "muted": "#a3abb8",
  },
  term: {
    background: "#fbfbfc", foreground: "#24292f", cursor: "#2f6fdb", cursorAccent: "#fbfbfc",
    selectionBackground: "#2f6fdb38", selectionInactiveBackground: "#2f6fdb22",
    // "white" has to show on a white page, so the whites are greys here.
    black: "#24292f", red: "#c4232e", green: "#18733a", yellow: "#875400",
    blue: "#0b62c8", magenta: "#7d4ad1", cyan: "#16757d", white: "#6b7380",
    brightBlack: "#57606a", brightRed: "#9f1225", brightGreen: "#0f5c2b", brightYellow: "#6a4000",
    brightBlue: "#0a4ea3", brightMagenta: "#6234b5", brightCyan: "#0e5f66", brightWhite: "#8a929e",
  },
  editor: {
    base: "vs",
    colors: {
      "editor.background": "#fbfbfc",
      "editor.foreground": "#24292f",
      "editorLineNumber.foreground": "#a3abb8",
      "editorLineNumber.activeForeground": "#4b5462",
      "editor.lineHighlightBackground": "#f0f2f5",
      "editorWhitespace.foreground": "#c9cfd8",
      "editorIndentGuide.background": "#e3e7ec",
      "editor.selectionBackground": "#2f6fdb38",
    },
  },
  search: { match: "#f2b44966", active: "#e8950acc", ruler: "#c97a00" },
};

export const THEMES: Record<ThemeId, Theme> = { "charon-dark": dark, "charon-light": light };

/** The theme for a profile value; unknown names fall back to dark. */
export function resolveTheme(pref: string | undefined, prefersDark: boolean): Theme {
  if (pref === "system") return prefersDark ? dark : light;
  return THEMES[pref as ThemeId] ?? dark;
}

/** xterm theme for a tab: the theme's colours, cursor in the connection colour. */
export function termTheme(t: Theme, sessionColor?: string): TermColors {
  return { ...t.term, cursor: sessionColor || t.term.cursor };
}

// ---- contrast (WCAG relative luminance), used by the tests to keep palettes legible

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}
/** Luminance of an opaque #rgb / #rrggbb colour (alpha ignored). */
export function luminance(hex: string): number {
  let h = hex.replace("#", "");
  if (h.length === 3 || h.length === 4) h = [...h.slice(0, 3)].map((x) => x + x).join("");
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16));
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}
export function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

// ---- DOM side

/** Put `t` on the page: CSS variables, native control colours, `data-theme`. */
export function applyTheme(t: Theme, root: HTMLElement = document.documentElement): void {
  for (const k of UI_TOKENS) root.style.setProperty(`--${k}`, t.ui[k]);
  root.style.colorScheme = t.scheme; // native <select> popups, scrollbars, date pickers
  root.dataset.theme = t.id;
}

const STORE_KEY = "charon.theme";
/** Remember the resolved theme for the next start (per-window convenience only). */
export function rememberTheme(id: ThemeId): void {
  try { localStorage.setItem(STORE_KEY, id); } catch { /* storage unavailable: start dark */ }
}
/** Apply the theme remembered from last time, before the profile has loaded. */
export function applyRememberedTheme(): void {
  let id: string | null = null;
  try { id = localStorage.getItem(STORE_KEY); } catch { /* ignore */ }
  if (id && id in THEMES) applyTheme(THEMES[id as ThemeId]);
}

/** The theme currently on the page (dark before anything was applied). */
export function currentTheme(): Theme {
  return THEMES[document.documentElement.dataset.theme as ThemeId] ?? dark;
}

/** Call `cb` whenever applyTheme switches the page's theme; returns an unsubscribe. */
export function watchTheme(cb: (t: Theme) => void): () => void {
  const root = document.documentElement;
  let last = root.dataset.theme;
  const mo = new MutationObserver(() => {
    if (root.dataset.theme === last) return;
    last = root.dataset.theme;
    cb(currentTheme());
  });
  mo.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
  return () => mo.disconnect();
}
