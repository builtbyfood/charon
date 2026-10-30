import { test } from "node:test";
import assert from "node:assert/strict";
import { mergeFonts, probeFonts } from "./fonts.ts";

test("probeFonts keeps fonts whose metrics differ from a generic fallback", () => {
  // Fake measurer: generic families have fixed widths; "Hack" is installed; "Gone"
  // falls back (same widths); "Mono Twin" equals generic monospace but not serif.
  const widths: Record<string, number> = { monospace: 100, serif: 120, "sans-serif": 110 };
  const measure = (css: string) => {
    const m = css.match(/^72px (?:"([^"]+)", )?(.+)$/)!;
    const [, font, generic] = m;
    if (font === "Hack") return 95;
    if (font === "Mono Twin") return 100;
    return widths[generic];
  };
  assert.deepEqual(probeFonts(["Hack", "Gone", "Mono Twin"], measure), ["Hack", "Mono Twin"]);
});

test("mergeFonts dedupes case-insensitively and sorts", () => {
  assert.deepEqual(mergeFonts(["Hack", "fira code"], ["Fira Code", "Adwaita Mono", " "]), ["Adwaita Mono", "fira code", "Hack"]);
});
