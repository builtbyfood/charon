// Clickable URLs in the terminal. Pure: rows in, link ranges out; Terminal.svelte
// adapts the xterm buffer and registers a link provider.
//
// A URL may run over several rows. A soft wrap (xterm's isWrapped) continues at
// column 0. Programs that lay out their own text (Claude Code, other Ink/curses
// UIs) break long words with real newlines and indent the next row, so a URL that
// fills its row up to the right edge and is followed by a row starting with URL
// characters is joined across that break too.

/** One terminal row: its text and, per string index, the cell column it starts at. */
export interface Row {
  text: string;
  /** cols[i] = cell column of text[i] (wide characters take two cells). */
  cols: number[];
  /** xterm isWrapped: this row continues the previous one (soft wrap). */
  wrapped: boolean;
}

export interface Cell { row: number; col: number }
/** A link: inclusive start and end cells (row = index into the rows passed in). */
export interface FoundLink { url: string; start: Cell; end: Cell }

/** How close to the right edge a URL must end for a hard break to count as a wrap. */
export const EDGE_SLACK = 3;

const URL_START = /https?:\/\/[^\s<>"'`]/g;
const URL_CHAR = /[^\s<>"'`│┃|]/;
// Characters that may close a URL in prose and are rarely its last character.
const TRAILING = /[.,;:!?'"*_]+$/;
// Row decorations that are not text: borders of boxes drawn by TUIs.
const BOX = /[│┃║╭╮╰╯─━]/;

/** Index just past the URL that starts at `from` in `s` (stops at a space/quote/border). */
function urlEnd(s: string, from: number): number {
  let i = from;
  while (i < s.length && URL_CHAR.test(s[i])) i++;
  return i;
}

/** Last column holding real text on a row (ignores trailing spaces and box borders). */
function lastTextCol(r: Row): number {
  let i = r.text.length - 1;
  while (i >= 0 && (r.text[i] === " " || BOX.test(r.text[i]))) i--;
  return i < 0 ? -1 : r.cols[i];
}

/** Where a continuation fragment starts on `r`: after indentation and a left border. */
function continuationStart(r: Row): number {
  let i = 0;
  while (i < r.text.length && (r.text[i] === " " || BOX.test(r.text[i]))) i++;
  return i;
}

/** Drop prose punctuation at the end, and a `)` with no `(` inside the URL. */
export function trimUrl(u: string): string {
  let s = u.replace(TRAILING, "");
  while (s.endsWith(")") && (s.match(/\(/g)?.length ?? 0) < (s.match(/\)/g)?.length ?? 0)) {
    s = s.slice(0, -1).replace(TRAILING, "");
  }
  return s;
}

/** Find every URL in `rows` (a contiguous slice of the buffer), joining wrapped ones. */
export function findLinks(rows: Row[], width: number): FoundLink[] {
  const out: FoundLink[] = [];
  // Rows already consumed as a continuation, from this index on (skip their start).
  const consumedUntil: number[] = rows.map(() => 0);
  for (let r = 0; r < rows.length; r++) {
    const text = rows[r].text;
    URL_START.lastIndex = consumedUntil[r];
    let m: RegExpExecArray | null;
    while ((m = URL_START.exec(text))) {
      const startIdx = m.index;
      let url = "";
      let row = r;
      let from = startIdx;
      let endRow = r;
      let endIdx = startIdx;
      for (;;) {
        const e = urlEnd(rows[row].text, from);
        url += rows[row].text.slice(from, e);
        endRow = row;
        endIdx = e - 1;
        const next = rows[row + 1];
        if (!next || e < rows[row].text.length && rows[row].text[e] !== " " && !BOX.test(rows[row].text[e])) break;
        // The URL must reach the end of this row's text...
        if (lastTextCol(rows[row]) !== rows[row].cols[e - 1]) break;
        let nextFrom: number;
        if (next.wrapped) {
          nextFrom = 0; // soft wrap: continues in column 0
        } else {
          // ...and, for a hard break, the right edge of the terminal.
          if (rows[row].cols[e - 1] < width - 1 - EDGE_SLACK) break;
          nextFrom = continuationStart(next);
        }
        if (nextFrom >= next.text.length || !URL_CHAR.test(next.text[nextFrom])) break;
        // A new URL on the next row is its own link, not a continuation.
        if (/^https?:\/\//.test(next.text.slice(nextFrom))) break;
        row++;
        from = nextFrom;
      }
      const trimmed = trimUrl(url);
      // Walk the end back over what trimUrl removed (all on the last row segment).
      const cut = url.length - trimmed.length;
      endIdx -= cut;
      if (trimmed.length > "https://".length) {
        out.push({
          url: trimmed,
          start: { row: r, col: rows[r].cols[startIdx] },
          end: { row: endRow, col: rows[endRow].cols[Math.max(endIdx, 0)] },
        });
      }
      if (endRow > r) {
        // Rows inside the link are done; the last one is scanned on from its end.
        for (let k = r + 1; k < endRow; k++) consumedUntil[k] = rows[k].text.length;
        consumedUntil[endRow] = endIdx + 1 + cut;
        break;
      }
      URL_START.lastIndex = endIdx + 1 + cut;
    }
  }
  return out;
}

/** OSC 52 (`ESC ] 52 ; <targets> ; <base64> BEL`): the text a program asks to put
 *  on the clipboard, or null for a query (`?`) or anything malformed. Reading the
 *  clipboard back is never allowed. */
export function parseOsc52(data: string): string | null {
  const semi = data.indexOf(";");
  if (semi < 0) return null;
  const b64 = data.slice(semi + 1).trim();
  if (!b64 || b64 === "?") return null;
  try {
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    return null;
  }
}
