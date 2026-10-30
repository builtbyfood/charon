import { test } from "node:test";
import assert from "node:assert/strict";
import { ANSI, THEMES, UI_TOKENS, contrast, resolveTheme, termTheme } from "./themes.ts";

const hex = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

test("every theme defines every token and all 16 ANSI colours", () => {
  for (const t of Object.values(THEMES)) {
    for (const k of UI_TOKENS) assert.match(t.ui[k], hex, `${t.id} ui.${k}`);
    for (const k of ANSI) assert.match(t.term[k], hex, `${t.id} term.${k}`);
    assert.match(t.term.background, hex);
    assert.match(t.term.foreground, hex);
  }
});

test("text is legible on every surface (WCAG)", () => {
  for (const t of Object.values(THEMES)) {
    for (const surface of ["bg", "bg-raised", "bg-sunken"] as const) {
      assert.ok(contrast(t.ui.fg, t.ui[surface]) >= 7, `${t.id} fg on ${surface}`);
      assert.ok(contrast(t.ui["fg-dim"], t.ui[surface]) >= 4.5, `${t.id} fg-dim on ${surface}`);
    }
    for (const k of ["danger", "warn", "ok", "accent"] as const) {
      assert.ok(contrast(t.ui[k], t.ui["bg-raised"]) >= 3, `${t.id} ${k} on bg-raised`);
    }
    assert.ok(contrast(t.ui["on-accent"], t.ui.accent) >= 4.5, `${t.id} on-accent`);
    assert.ok(contrast(t.term.foreground, t.term.background) >= 7, `${t.id} terminal text`);
    // Coloured output (ls, git, compiler errors) must read on the terminal background,
    // except black-on-dark / bright-white-on-light which are meant to be subtle.
    for (const k of ANSI) {
      if ((t.scheme === "dark" && k === "black") || (t.scheme === "light" && k === "brightWhite")) continue;
      assert.ok(contrast(t.term[k], t.term.background) >= 3, `${t.id} ansi ${k}: ${contrast(t.term[k], t.term.background).toFixed(2)}`);
    }
  }
});

test("resolveTheme: names, system, and fallback", () => {
  assert.equal(resolveTheme("charon-light", true).id, "charon-light");
  assert.equal(resolveTheme("system", true).id, "charon-dark");
  assert.equal(resolveTheme("system", false).id, "charon-light");
  assert.equal(resolveTheme("nope", false).id, "charon-dark");
  assert.equal(resolveTheme(undefined, false).id, "charon-dark");
});

test("termTheme puts the connection colour on the cursor", () => {
  assert.equal(termTheme(THEMES["charon-dark"], "#ff00ff").cursor, "#ff00ff");
  assert.equal(termTheme(THEMES["charon-light"]).cursor, THEMES["charon-light"].term.cursor);
});
