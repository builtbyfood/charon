// Phase 2 home for sanitised copy/paste. Wired in now so Terminal.svelte can call it.

const ZERO_WIDTH = /[\u200B-\u200D\u2060\uFEFF\u00AD]/g;   // ZWSP, ZWNJ, ZWJ, WJ, BOM, soft hyphen
const NBSP = /[\u00A0\u202F]/g;
const SMART = new Map<string, string>([
  ["\u2018", "'"], ["\u2019", "'"], ["\u201C", '"'], ["\u201D", '"'],
  ["\u2013", "-"], ["\u2014", "-"], ["\u2026", "..."],
]);

export interface CleanOptions { smartQuotes?: boolean; trimTrailing?: boolean }

export function cleanForCopy(text: string, o: CleanOptions = {}): string {
  let t = text.replace(/\r\n?/g, "\n").replace(ZERO_WIDTH, "").replace(NBSP, " ");
  if (o.smartQuotes !== false) t = t.replace(/[\u2018\u2019\u201C\u201D\u2013\u2014\u2026]/g, (c) => SMART.get(c) ?? c);
  if (o.trimTrailing !== false) t = t.split("\n").map((l) => l.replace(/[ \t]+$/, "")).join("\n");
  return t;
}

/** What the paste guard warns about before sending to the shell. */
export function pasteWarnings(text: string, minLines = 2): string[] {
  const w: string[] = [];
  // Count content lines, ignoring a single trailing newline (that just means
  // "run it"): a one-liner copied with a trailing \n is still one line, not two.
  const lines = text.replace(/\r\n/g, "\n").replace(/\n$/, "").split("\n").length;
  if (lines >= minLines) w.push(`${lines} lines`);
  if (ZERO_WIDTH.test(text)) w.push("hidden zero-width characters (will be removed)");
  ZERO_WIDTH.lastIndex = 0;
  if (/^\s*\$ /m.test(text)) w.push("lines start with '$ ' (doc snippet?)");
  return w;
}
