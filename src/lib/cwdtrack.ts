// OSC 7 → the tab's cwd. Kept out of Terminal.svelte so the real xterm parser
// path can be unit-tested (osc133.ts can't be: it imports the hooks with ?raw).
import { parseOsc7 } from "./prompt.ts";

/** The subset of xterm's Terminal this needs (headless or DOM). */
export interface OscHost {
  parser: { registerOscHandler(ident: number, cb: (data: string) => boolean): { dispose(): void } };
}

/** Register the OSC 7 handler. `onchange` fires only when the directory changes;
 *  it is where the tab stores the cwd, tells the backend, and the pane follows. */
export function attachOsc7(term: OscHost, current: () => string | undefined, onchange: (dir: string) => void): { dispose(): void } {
  return term.parser.registerOscHandler(7, (data) => {
    const dir = parseOsc7(data);
    if (!dir) { console.info(`[charon] cwd: osc7 unparsed data=${JSON.stringify(data.slice(0, 80))}`); return true; }
    if (dir !== current()) onchange(dir);
    return true;
  });
}
