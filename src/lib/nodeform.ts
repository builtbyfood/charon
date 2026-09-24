// NodeEditor's form model ⇄ Settings. Pure so it can be unit-tested: every
// field the store returns as null must come back as "inherit" ("" in the form)
// and go out as undefined, never as a stringified "null" or a flipped boolean.
import type { Kind, Settings, SerialSettings } from "./api";

/** Strings for text inputs and selects; "" = inherit / unset. */
export interface NodeForm {
  host: string; port: string; user: string; credential: string; jump: string;
  color: string; tags: string; startup: string;
  shellInt: "" | "true" | "false"; mcp: string; mcpSudo: boolean;
  shell: string; cwd: string;
  serialPort: string; baud: string; dataBits: string; parity: string; stopBits: string;
}

export function formFromSettings(s: Settings): NodeForm {
  return {
    host: s.host ?? "",
    port: s.port ? String(s.port) : "",
    user: s.user ?? "",
    credential: s.credential ?? "",
    jump: s.jump_host ?? "",
    color: s.color ?? "",
    tags: (s.tags ?? []).join(", "),
    startup: (s.startup ?? []).join("\n"),
    // == null: the store serialises unset as null, not undefined.
    shellInt: s.shell_integration == null ? "" : s.shell_integration ? "true" : "false",
    mcp: s.mcp_tier ?? "",
    mcpSudo: s.mcp_sudo === true,
    shell: s.shell ?? "",
    cwd: s.cwd ?? "",
    serialPort: s.serial?.port ?? "",
    baud: String(s.serial?.baud ?? 9600),
    dataBits: String(s.serial?.data_bits ?? 8),
    parity: s.serial?.parity ?? "none",
    stopBits: String(s.serial?.stop_bits ?? 1),
  };
}

export type FormResult = { ok: true; settings: Settings } | { ok: false; error: string };

/** Validate and convert; fields the node kind does not use are dropped. */
export function settingsFromForm(f: NodeForm, kind: Kind): FormResult {
  const isFolder = kind === "folder", isLocal = kind === "local", isSerial = kind === "serial";
  const isSsh = !isFolder && !isLocal && !isSerial;

  let serial: SerialSettings | undefined;
  if (isSerial) {
    if (!f.serialPort.trim()) return { ok: false, error: "serial port is required" };
    const b = Number(f.baud);
    if (!Number.isInteger(b) || b <= 0) return { ok: false, error: "baud must be a positive integer" };
    serial = {
      port: f.serialPort.trim(), baud: b,
      data_bits: Number(f.dataBits) as SerialSettings["data_bits"],
      parity: f.parity as SerialSettings["parity"],
      stop_bits: Number(f.stopBits) as SerialSettings["stop_bits"],
    };
  }
  const p = f.port.trim() === "" ? undefined : Number(f.port);
  if (p !== undefined && (!Number.isInteger(p) || p < 1 || p > 65535)) return { ok: false, error: "port must be 1–65535" };
  const tagList = f.tags.split(",").map((t) => t.trim()).filter(Boolean);
  const startupList = f.startup.split("\n").map((l) => l.trimEnd()).filter(Boolean);

  const settings: Settings = {
    host: isSsh ? f.host.trim() || undefined : undefined,
    shell: isLocal ? f.shell.trim() || undefined : undefined,
    cwd: isLocal ? f.cwd.trim() || undefined : undefined,
    serial,
    port: p,
    user: f.user.trim() || undefined,
    credential: f.credential || undefined,
    jump_host: f.jump.trim() || undefined,
    color: f.color || undefined,
    tags: tagList.length ? tagList : undefined,
    startup: startupList.length ? startupList : undefined,
    shell_integration: f.shellInt === "" ? undefined : f.shellInt === "true",
    mcp_tier: (f.mcp || undefined) as Settings["mcp_tier"],
    // Only meaningful at the trusted tier; store true, else inherit/unset.
    mcp_sudo: f.mcpSudo ? true : undefined,
  };
  return { ok: true, settings };
}
