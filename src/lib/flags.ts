// Extract command-line flags from a tldr page and/or a `--help` dump, for the
// flag picker (P5). Pure and tested; the UI merges the two sources.

export interface Flag {
  /** All spellings that mean the same option, e.g. ["-l", "--long"]. */
  names: string[];
  /** One-line description, if known. */
  desc?: string;
  /** Where it came from (for the badge / ranking). */
  from: "help" | "tldr";
}

const FLAG = /^--?[A-Za-z0-9][\w-]*$/;

/** Split "-l, --long=FILE" style lead-ins into flag names (dropping =ARG / <ARG>). */
function namesIn(segment: string): string[] {
  const names: string[] = [];
  for (const raw of segment.split(/[,\s]+/)) {
    const tok = raw.replace(/[=\[].*$/, "").trim();
    if (FLAG.test(tok)) names.push(tok);
  }
  return names;
}

/**
 * Parse a `--help` dump. Recognises the common layouts:
 *   `  -l, --long        description`
 *   `  --long=FILE       description` (description may wrap to the next indented line)
 */
export function flagsFromHelp(text: string): Flag[] {
  const out: Flag[] = [];
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const m = /^\s{1,10}(--?[A-Za-z0-9][\w-]*(?:[=\[][^\s]*)?(?:[,\s]+--?[A-Za-z0-9][\w-]*(?:[=\[][^\s]*)?)*)(?:\s{2,}(.*))?$/.exec(line);
    if (!m) continue;
    const names = namesIn(m[1]);
    if (!names.length) continue;
    let desc = (m[2] ?? "").trim();
    // Description continues on the next line when this line was only the flag.
    if (!desc && /^\s{6,}\S/.test(lines[i + 1] ?? "")) desc = lines[i + 1].trim();
    out.push({ names, desc: desc || undefined, from: "help" });
  }
  return dedupe(out);
}

/** Pull flags out of a tldr page's example code spans, tagged with the example text. */
export function flagsFromTldr(markdown: string): Flag[] {
  const out: Flag[] = [];
  const lines = markdown.split(/\r?\n/);
  let lastDesc = "";
  for (const line of lines) {
    const d = /^-\s+(.*):\s*$/.exec(line);
    if (d) { lastDesc = d[1].trim(); continue; }
    const code = /^`(.+)`$/.exec(line.trim());
    if (!code) continue;
    for (const tok of code[1].split(/\s+/)) {
      const clean = tok.replace(/[=\[].*$/, "");
      if (FLAG.test(clean)) out.push({ names: [clean], desc: lastDesc || undefined, from: "tldr" });
    }
  }
  return dedupe(out);
}

/** Merge sources: help wins for descriptions; aliases collapse to one entry. */
export function mergeFlags(...sources: Flag[][]): Flag[] {
  const byName = new Map<string, Flag>();
  for (const flag of sources.flat()) {
    // Reuse the entry any of these names already belongs to, else start one.
    let target: Flag | undefined;
    for (const n of flag.names) { const e = byName.get(n); if (e) { target = e; break; } }
    if (!target) target = { names: [], from: flag.from, desc: flag.desc };
    for (const n of flag.names) if (!target.names.includes(n)) target.names.push(n);
    if (flag.from === "help") {
      if (target.from !== "help") { target.from = "help"; target.desc = flag.desc ?? target.desc; }
      else if (!target.desc) target.desc = flag.desc;
    } else if (!target.desc) {
      target.desc = flag.desc;
    }
    for (const n of target.names) byName.set(n, target); // all aliases point at the one object
  }
  return [...new Set(byName.values())].sort((a, b) => shortest(a).localeCompare(shortest(b)));
}

/** Flags whose first name starts with `prefix` (what the user has typed). */
export function matchFlags(flags: Flag[], prefix: string): Flag[] {
  if (prefix === "-" || prefix === "--") {
    // "-" shows short flags first; "--" shows only long flags.
    const long = prefix === "--";
    return flags.filter((f) => f.names.some((n) => (long ? n.startsWith("--") : true)));
  }
  return flags.filter((f) => f.names.some((n) => n.startsWith(prefix)));
}

function shortest(f: Flag): string {
  return [...f.names].sort((a, b) => a.length - b.length)[0];
}

function dedupe(flags: Flag[]): Flag[] {
  const seen = new Set<string>();
  return flags.filter((f) => {
    const key = f.names.join(",");
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}
