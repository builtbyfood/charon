// Parse the shell input line for the flag picker: what command is being run and
// what option token is under the cursor. Pure and tested.

export interface CmdContext {
  /** First bare word on the line (the command). */
  command: string;
  /** The option token the cursor is in (starts with '-'), or null. */
  token: string;
}

/**
 * Given the command-line text up to the cursor, return the command and the
 * option token being typed, or null when the cursor is not on a `-flag`.
 * Sudo/env prefixes are skipped so `sudo ls -` picks `ls`.
 */
export function cmdContext(lineToCursor: string): CmdContext | null {
  // Only the segment after the last pipe / separator matters: in `a | grep -x`
  // the command is grep, not a.
  const segment = lineToCursor.split(/\||;|&&|\|\||&|<|>/).pop() ?? "";
  const parts = segment.split(/\s+/).filter((p, i, a) => p !== "" || i === a.length - 1);
  const token = parts[parts.length - 1];
  if (!token || token[0] !== "-") return null;

  const words = parts.slice(0, -1).filter(Boolean);
  let i = 0;
  // Skip a leading `sudo`, `env`, and `VAR=value` assignments.
  while (i < words.length && (words[i] === "sudo" || words[i] === "env" || /^[A-Za-z_]\w*=/.test(words[i]))) i++;
  const command = words[i];
  if (!command || command.startsWith("-")) return null;
  return { command, token };
}

/**
 * The command name being typed on the current line (first word of the last
 * pipeline segment, skipping sudo/env/VAR=), for the docs pane. Null when there
 * is no plausible command yet.
 */
export function currentCommand(lineToCursor: string): string | null {
  const segment = lineToCursor.split(/\||;|&&|\|\||&|<|>/).pop() ?? "";
  const words = segment.trim().split(/\s+/).filter(Boolean);
  let i = 0;
  while (i < words.length && (words[i] === "sudo" || words[i] === "env" || /^[A-Za-z_]\w*=/.test(words[i]))) i++;
  const cmd = words[i];
  if (!cmd || !/^[A-Za-z][\w.+-]*$/.test(cmd)) return null;
  return cmd;
}
