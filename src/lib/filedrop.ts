// Files dragged in from the OS file manager (Tauri onDragDropEvent). One listener
// per window converts the drop point to CSS pixels, hit-tests it once and hands
// the paths to the zone under the pointer. Zones are elements marked `data-drop`:
// "files" (a file pane: upload into its directory) or "editor" (a terminal or the
// editor tab: open in the editor). The innermost zone wins, so a file pane inside
// a terminal tab takes its own drops; a drop outside every zone does nothing.
import { getCurrentWebview } from "@tauri-apps/api/webview";

export type DropZone = "files" | "editor";
export type Platform = "windows" | "macos" | "linux";

export interface DropTarget {
  zone: DropZone;
  /** Only drops on this element's zone (a file pane); omitted = any element of the zone. */
  el?: Element;
  /** A drag is over (true) or has left (false) this target. */
  hover?: (on: boolean) => void;
  drop: (paths: string[]) => void;
}

export function platformOf(ua: string): Platform {
  return ua.includes("Windows") ? "windows" : ua.includes("Mac") ? "macos" : "linux";
}

/** Tauri types every drop position as physical, but only WebView2 reports physical
 *  client pixels (ScreenToClient). WebKitGTK passes GTK widget coordinates and
 *  WKWebView view points, which are already CSS pixels: dividing those by the
 *  scale (2 on a HiDPI Fedora desktop) put the hit-test up and left of the pointer. */
export function toCssPoint(pos: { x: number; y: number }, platform: Platform, dpr: number): { x: number; y: number } {
  if (platform !== "windows") return { x: pos.x, y: pos.y };
  const s = dpr > 0 ? dpr : 1;
  return { x: pos.x / s, y: pos.y / s };
}

/** The target for a drop whose innermost zone element is `zoneEl` (null = no zone). */
export function pickTarget<T extends { zone: string; el?: unknown }>(targets: Iterable<T>, zone: string | null, zoneEl: unknown): T | null {
  if (!zone) return null;
  for (const t of targets) if (t.zone === zone && (!t.el || t.el === zoneEl)) return t;
  return null;
}

const targets = new Set<DropTarget>();
let listening = false;
const platform = platformOf(navigator.userAgent);

/** Take drops for a zone; returns the unregister function. */
export function addDropTarget(t: DropTarget): () => void {
  targets.add(t);
  if (!listening) { listening = true; listen().catch((e) => { listening = false; console.warn("[charon] drop: listen failed", e); }); }
  return () => { targets.delete(t); t.hover?.(false); };
}

function describe(el: Element | null): string {
  if (!el) return "none";
  const cls = typeof el.className === "string" && el.className.trim() ? "." + el.className.trim().split(/\s+/).join(".") : "";
  return `${el.tagName.toLowerCase()}${el.id ? "#" + el.id : ""}${cls}`;
}

async function listen() {
  let hovered: DropTarget | null = null;
  const setHover = (t: DropTarget | null) => {
    if (t === hovered) return;
    hovered?.hover?.(false);
    hovered = t;
    t?.hover?.(true);
  };
  await getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type === "leave") { setHover(null); return; }
    const dpr = window.devicePixelRatio || 1;
    const pt = toCssPoint(p.position, platform, dpr);
    const hit = document.elementFromPoint(pt.x, pt.y);
    const zoneEl = hit?.closest("[data-drop]") ?? null;
    const zone = zoneEl?.getAttribute("data-drop") ?? null;
    const t = pickTarget(targets, zone, zoneEl);
    if (p.type === "enter" || p.type === "drop") {
      console.info(`[charon] drop: ${p.type} raw=(${p.position.x},${p.position.y}) dpr=${dpr} platform=${platform} css=(${Math.round(pt.x)},${Math.round(pt.y)}) hit=${describe(hit)} zone=${zone ?? "none"}${p.type === "drop" ? ` paths=${p.paths.length}` : ""}`);
    }
    if (p.type !== "drop") { setHover(t); return; }
    setHover(null);
    if (t && p.paths.length) t.drop(p.paths);
  });
}
