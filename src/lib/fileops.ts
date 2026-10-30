// File-pane move/copy rules, kept pure for node tests: what may be dropped where,
// and the Cut/Copy → Paste clipboard shared by every file pane in this window.

/** Which filesystem a pane is on: the session (null = local disk) and its backend. */
export interface FsRef { session: string | null; via: string }

export interface FileClip extends FsRef { op: "copy" | "cut"; paths: string[] }

/** The pane clipboard (not the OS one: these are paths on a remote host). */
export const fileClip: { current: FileClip | null } = { current: null };

/** Paste works within one filesystem: same session and same backend. */
export function canPaste(c: FileClip | null, at: FsRef): c is FileClip {
  return !!c && c.paths.length > 0 && c.session === at.session && c.via === at.via;
}

const norm = (p: string) => p.replace(/\\/g, "/").replace(/(.)\/+$/, "$1");

/** `inner` is `outer` or lies under it. */
export function isWithin(outer: string, inner: string): boolean {
  const o = norm(outer), i = norm(inner);
  return i === o || i.startsWith(o.endsWith("/") ? o : o + "/");
}

/** Parent directory as the pane spells it ("/a/b" → "/a", "C:\\x\\y" → "C:\\x"). */
export function parentOf(p: string): string {
  const win = /^[A-Za-z]:\\/.test(p) || (p.includes("\\") && !p.includes("/"));
  const sep = win ? "\\" : "/";
  const t = p.length > 1 ? p.replace(/[\\/]+$/, "") : p;
  const i = t.lastIndexOf(sep);
  if (i < 0) return t;
  if (i === 0) return sep;
  const head = t.slice(0, i);
  return /^[A-Za-z]:$/.test(head) ? head + "\\" : head;
}

/**
 * Can `paths` be moved/copied into `target`? Not onto one of themselves, not into
 * a folder's own subtree, and (for a move) not into the folder they are already in.
 */
export function dropAllowed(paths: string[], target: string, op: "move" | "copy"): boolean {
  if (!paths.length) return false;
  for (const p of paths) {
    if (isWithin(p, target)) return false;
    if (op === "move" && norm(parentOf(p)) === norm(target)) return false;
  }
  return true;
}

/** Names from `names` that already exist in `existing` (destination listing). */
export function collisions(names: string[], existing: string[]): string[] {
  const have = new Set(existing);
  return names.filter((n) => have.has(n));
}
