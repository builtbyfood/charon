// Path helpers for the file pane. Remote paths are POSIX; local ones are
// whatever the OS spells (Windows: `C:\Users\x`). Pure, tested.

export function isWindowsPath(p: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");
}

export function joinPath(dir: string, name: string): string {
  if (isWindowsPath(dir)) return dir.endsWith("\\") ? dir + name : `${dir}\\${name}`;
  return dir.endsWith("/") ? dir + name : `${dir}/${name}`;
}

export function basename(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
}

export interface Crumb { label: string; path: string }

/** `/home/admin` → `/`, `home`, `admin`; `C:\Users\x` → `C:\`, `Users`, `x`. */
export function crumbsOf(path: string): Crumb[] {
  if (isWindowsPath(path)) {
    const parts = path.split(/[\\/]/).filter(Boolean);
    const drive = parts.shift()!;
    const out: Crumb[] = [{ label: drive + "\\", path: drive + "\\" }];
    let cur = drive;
    for (const seg of parts) { cur = `${cur}\\${seg}`; out.push({ label: seg, path: cur }); }
    return out;
  }
  const out: Crumb[] = [{ label: "/", path: "/" }];
  let cur = "";
  for (const seg of path.split("/").filter(Boolean)) { cur += "/" + seg; out.push({ label: seg, path: cur }); }
  return out;
}

/** OSC 7 gives `/C:/Users/x` for PowerShell on Windows (see pwsh.ps1); turn it back into a local path. */
export function osc7ToLocal(p: string): string {
  const m = /^\/([A-Za-z]:)(\/.*)?$/.exec(p);
  if (m) return m[1] + (m[2] ?? "/").replace(/\//g, "\\");
  return p;
}

export function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

/** `0o755` → `rwxr-xr-x` (with a leading d for directories). */
export function modeString(mode: number, dir = false): string {
  const bits = "rwxrwxrwx";
  let s = dir ? "d" : "-";
  for (let i = 0; i < 9; i++) s += (mode >> (8 - i)) & 1 ? bits[i] : "-";
  if (mode & 0o4000) s = s.slice(0, 3) + (mode & 0o100 ? "s" : "S") + s.slice(4);
  if (mode & 0o2000) s = s.slice(0, 6) + (mode & 0o010 ? "s" : "S") + s.slice(7);
  if (mode & 0o1000) s = s.slice(0, 9) + (mode & 0o001 ? "t" : "T");
  return s;
}

/** POSIX single-quote for typing into a shell. */
export function sq(s: string): string {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}
