// Monospace fonts for the pickers. A webview cannot list system fonts, so probe a
// curated list by measurement (a font is installed when text set in it measures
// differently from both generic fallbacks), and merge what the backend can list
// (Linux fontconfig, `font_families`).

export const MONO_FONTS = [
  "Adwaita Mono", "Anonymous Pro", "Cascadia Code", "Cascadia Mono", "CaskaydiaCove Nerd Font",
  "Comic Mono", "Commit Mono", "Consolas", "Courier New", "Cousine", "DejaVu Sans Mono",
  "Droid Sans Mono", "Fira Code", "Fira Mono", "FiraCode Nerd Font", "Geist Mono", "Hack",
  "Hasklig", "IBM Plex Mono", "Inconsolata", "Input Mono", "Intel One Mono", "Iosevka",
  "JetBrains Mono", "JetBrainsMono Nerd Font", "Liberation Mono", "Lucida Console", "Menlo",
  "MesloLGS NF", "Monaco", "Monaspace Neon", "Noto Sans Mono", "PT Mono", "Red Hat Mono",
  "Roboto Mono", "SF Mono", "Source Code Pro", "Space Mono", "Terminus", "Ubuntu Mono",
  "Ubuntu Sans Mono", "Victor Mono",
];

/** Width of a sample string in `family` (with a generic fallback) at a large size. */
export type Measure = (fontCss: string) => number;

/** Names from `names` the measurer says are installed (pure; tested with a fake). */
export function probeFonts(names: string[], measure: Measure): string[] {
  const generic = ["monospace", "serif", "sans-serif"] as const;
  const base = generic.map((g) => measure(`72px ${g}`));
  return names.filter((n) => {
    const q = `"${n.replace(/"/g, "")}"`;
    // Installed if it differs from at least one fallback: a font may happen to
    // match one generic family's metrics (it may *be* the generic monospace).
    return generic.some((g, i) => measure(`72px ${q}, ${g}`) !== base[i]);
  });
}

/** Sorted union, case-insensitively deduplicated (first spelling wins). */
export function mergeFonts(...lists: string[][]): string[] {
  const seen = new Map<string, string>();
  for (const l of lists) for (const f of l) { const k = f.trim().toLowerCase(); if (k && !seen.has(k)) seen.set(k, f.trim()); }
  return [...seen.values()].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
}

const SAMPLE = "mmmmmmmmmmlli10WQ@#{}|";
let cached: Promise<string[]> | null = null;

/** Installed monospace fonts: probed curated list + whatever the OS lists. Cached. */
export function installedMonoFonts(osList: () => Promise<string[]>): Promise<string[]> {
  if (cached) return cached;
  cached = (async () => {
    const ctx = document.createElement("canvas").getContext("2d");
    const probed = ctx
      ? probeFonts(MONO_FONTS, (css) => { ctx.font = css; return ctx.measureText(SAMPLE).width; })
      : [];
    const os = await osList().catch(() => [] as string[]);
    return mergeFonts(probed, os);
  })();
  return cached;
}
