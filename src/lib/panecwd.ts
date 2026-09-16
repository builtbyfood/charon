// Pure decisions for the SFTP pane's two cwd toggles, so the ping-pong guard is
// testable. The Svelte effects hold the state (lastCwd, lastPushed) and call these.

/** "→ cwd": the shell moved — navigate the pane there. Null = no change to act on. */
export function followTarget(
  shellCwd: string | undefined,
  lastCwd: string | undefined,
  isLocal: boolean,
  toLocal: (p: string) => string,
): string | null {
  if (!shellCwd || shellCwd === lastCwd) return null; // only on a genuine cwd change
  return isLocal ? toLocal(shellCwd) : shellCwd;
}

/** "→ term": the pane moved — cd the shell there. Null = nothing to send. */
export function pushTarget(paneDir: string | undefined, lastPushed: string | undefined): string | null {
  if (!paneDir || paneDir === lastPushed) return null;
  return paneDir;
}
