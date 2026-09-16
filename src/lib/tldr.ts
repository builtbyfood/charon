// Bundled tldr pages (public/tldr.json.gz, ~7000 entries), loaded and gunzipped
// once in the webview. Used by the flag picker and the docs sidebar.
import { tldrBundle } from "./api";
let cache: Record<string, string> | null = null;
let loading: Promise<Record<string, string>> | null = null;

async function gunzip(buf: ArrayBuffer): Promise<string> {
  const stream = new Response(buf).body!.pipeThrough(new DecompressionStream("gzip"));
  return new Response(stream).text();
}

async function load(): Promise<Record<string, string>> {
  if (cache) return cache;
  if (loading) return loading;
  loading = (async () => {
    // Loaded over IPC (data-dir override, else the embedded build-time bundle):
    // the webview CSP blocks fetching the static asset directly.
    const b64 = await tldrBundle();
    const text = await gunzip(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)).buffer);
    cache = JSON.parse(text) as Record<string, string>;
    return cache;
  })().catch((e) => { loading = null; throw e; }); // let a failed load be retried
  return loading;
}

/** Forget the loaded bundle so the next lookup reloads (after an update). */
export function resetTldr(): void { cache = null; loading = null; }

/** The tldr markdown for a command, or undefined. Pre-load with load() (idempotent). */
export async function tldrPage(cmd: string): Promise<string | undefined> {
  const pages = await load().catch(() => null);
  return pages?.[cmd];
}

/** Kick off loading so the first lookup is instant; ignores failure. */
export function preloadTldr(): void {
  void load().catch(() => {});
}
