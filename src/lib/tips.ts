// Tip of the day. `parseTips` is pure (tested); the DocsPanel feeds it the raw
// docs/tips.yaml (imported with Vite's ?raw). Community-extendable: add
// `- tip: … / note: …` entries to that file.
export interface Tip { tip: string; note: string }

export function parseTips(text: string): Tip[] {
  const tips: Tip[] = [];
  let cur: Partial<Tip> | null = null;
  const unquote = (s: string) => s.trim().replace(/^["']|["']$/g, "");
  for (const line of text.split(/\r?\n/)) {
    if (/^\s*#/.test(line) || !line.trim()) continue;
    const m = /^-\s+tip:\s*(.*)$/.exec(line);
    if (m) { if (cur?.tip) tips.push({ tip: cur.tip, note: cur.note ?? "" }); cur = { tip: unquote(m[1]) }; continue; }
    const n = /^\s+note:\s*(.*)$/.exec(line);
    if (n && cur) cur.note = unquote(n[1]);
  }
  if (cur?.tip) tips.push({ tip: cur.tip, note: cur.note ?? "" });
  return tips;
}

/** A tip that changes daily (stable within a day). */
export function tipForToday(tips: Tip[]): Tip | undefined {
  if (!tips.length) return undefined;
  return tips[Math.floor(Date.now() / 86_400_000) % tips.length];
}
