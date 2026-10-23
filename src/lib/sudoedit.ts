// Edit as root (sudo-sftp on demand): the pure decisions, kept free of Tauri so
// node:test can cover them. The pane and the editor supply the API calls.
import type { FsVia, SudoOutcome } from "./api.ts";

export const isPermissionError = (e: unknown) => /permission denied|EACCES/i.test(String(e));

/** The "a sudo password is required" refusal from sftp_sudo_open (nothing stored). */
export const needsSudoPassword = (e: unknown) => /password is required/i.test(String(e));

/** Offer "Retry as sudo" after a failed save: only a remote document, not already
 *  bound to sudo, and only when the failure was a permission error (EACCES). */
export function canRetryAsSudo(doc: { fs: string | null; via: FsVia; path: string | null }, err: unknown): boolean {
  return !!doc.fs && !!doc.path && doc.via !== "sudo" && isPermissionError(err);
}

export interface SudoDeps {
  /** sftp_sudo_open: reuses a live channel, else the stored password, else `pw`. */
  open: (session: string, pw?: string) => Promise<SudoOutcome>;
  /** Ask the user for the sudo password; null = cancelled. Omitted = never ask (MCP). */
  ask?: () => Promise<string | null>;
}

/** Make sure `session` has a root SFTP channel, asking for a password only when
 *  nothing is stored. Throws with a readable reason when it can't. */
export async function ensureSudo(session: string, deps: SudoDeps): Promise<void> {
  let o: SudoOutcome;
  try {
    o = await deps.open(session);
  } catch (e) {
    if (!needsSudoPassword(e) || !deps.ask) throw e;
    const pw = await deps.ask();
    if (pw === null) throw new Error("sudo cancelled");
    o = await deps.open(session, pw);
  }
  if (!o.ok) throw new Error(`sudo SFTP ${o.sudo}: ${o.reason}`);
}
