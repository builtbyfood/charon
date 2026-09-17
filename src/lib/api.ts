// Typed wrappers around Tauri commands so the rest of the UI never spells command names.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Kind = "folder" | "ssh" | "serial" | "local" | "rdp";

export interface Settings {
  host?: string; port?: number; user?: string; credential?: string;
  jump_host?: string; color?: string; tags?: string[]; startup?: string[];
  shell_integration?: boolean; mcp_tier?: "read" | "exec-approve" | "trusted";
  /** Local sessions only. */
  shell?: string; cwd?: string;
  /** Serial sessions only. */
  serial?: SerialSettings;
}

export interface SerialSettings {
  port?: string; baud?: number; data_bits?: 5 | 6 | 7 | 8; parity?: "none" | "odd" | "even"; stop_bits?: 1 | 2;
}
export interface PortInfo { name: string; description: string }
export const serialListPorts = () => invoke<PortInfo[]>("serial_list_ports");
export interface OpenSerial { id?: string; node_id?: string; serial?: SerialSettings; cols: number; rows: number }
export const openSerial = (req: OpenSerial) => invoke<SessionInfo>("session_open_serial", { req });

export interface Node {
  id: string; parent_id: string | null; kind: Kind; name: string; sort: number; settings: Settings;
}

export interface SessionInfo { id: string; title: string; kind: string; shell_integration: boolean; shell: string | null; cwd: string | null }
export const sessionSetShell = (id: string, shell: string | null) => invoke<void>("session_set_shell", { id, shell });
export const sessionSetCwd = (id: string, cwd: string | null) => invoke<void>("session_set_cwd", { id, cwd });
export interface AppInfo { version: string; portable: boolean; data_dir: string }

export const appInfo = () => invoke<AppInfo>("app_info");

// Profile (data/profiles/default.toml). Unknown sections/keys are carried in
// the object as-is and written back unchanged.
export interface Profile {
  name: string;
  appearance: { font: string; font_size: number; theme: string; scrollback: number; renderer: "webgl" | "dom" | string; [k: string]: unknown };
  terminal: { copy_on_select: boolean; ctrl_c_copies_when_selected: boolean; middle_click_paste: boolean; right_click?: "moba" | "menu" | string; bracketed_paste: boolean; paste_guard_lines: number; [k: string]: unknown };
  files?: { auto_open?: boolean; [k: string]: unknown };
  clipboard: { strip_zero_width: boolean; nbsp_to_space: boolean; smart_quotes_to_ascii: boolean; trim_trailing_whitespace: boolean; crlf_to_lf: boolean; [k: string]: unknown };
  commands: { mark_gutter: boolean; tint_failed_commands: boolean; jump_keys: string[]; [k: string]: unknown };
  ssh: { keepalive_seconds: number; [k: string]: unknown };
  mcp: { enabled: boolean; port: number; default_tier: "read" | "exec-approve" | "trusted" | string; [k: string]: unknown };
  ui: { sidebar_width: number; [k: string]: unknown };
  [k: string]: unknown;
}
export const getProfile = () => invoke<Profile>("get_profile");
export const saveProfile = (profile: Profile) => invoke<void>("save_profile", { profile });
export const listConnections = () => invoke<Node[]>("list_connections");
export const saveConnection = (node: Node) => invoke<void>("save_connection", { node });
export const deleteConnection = (id: string) => invoke<void>("delete_connection", { id });

/** Set `settings.credential` on a saved node without the caller holding the tree. */
export async function linkCredential(nodeId: string, credential: string | undefined): Promise<void> {
  const n = (await listConnections()).find((x) => x.id === nodeId);
  if (!n) return;
  await saveConnection({ ...n, settings: { ...n.settings, credential } });
}

// Importers. `path` overrides the source's default location.
export type ImportKind = "ssh_config" | "putty" | "mobaxterm";
export interface ImportReport { added: number; skipped: number; updated: number; warnings: string[] }
/** What to do with parsed hosts that match an existing node (same name, or same host+user). */
export type DupPolicy = "skip" | "overwrite" | "copy";
export interface ImportPreview {
  new: string[];
  duplicates: { name: string; host: string; existing: string; reason: string }[];
  warnings: string[];
}
export const importPreview = (kind: ImportKind, path?: string) =>
  invoke<ImportPreview>("import_preview", { kind, path });
export const importConnections = (kind: ImportKind, path: string | undefined, policy: DupPolicy) =>
  invoke<ImportReport>("import_connections", { kind, path, policy });

// Vault: the UI only ever sees names and kinds, never values.
export type SecretKind = "password" | "key";
export interface Secret { kind: SecretKind; value: string; key_passphrase?: string }
export interface SecretMeta { name: string; kind: SecretKind }
export interface VaultStatus { backend: "keyring" | "file"; initialised: boolean; locked: boolean }

export const vaultStatus = () => invoke<VaultStatus>("vault_status");
export const vaultUnlock = (master: string) => invoke<VaultStatus>("vault_unlock", { master });
export const vaultList = () => invoke<SecretMeta[]>("vault_list");
export const vaultSet = (name: string, secret: Secret) => invoke<void>("vault_set", { name, secret });
export const vaultDelete = (name: string) => invoke<void>("vault_delete", { name });

// Cross-component nudges (sidebar refresh after a terminal saves a credential).
export const CHANGED = { connections: "charon:connections", vault: "charon:vault" } as const;
export const notifyChanged = (what: keyof typeof CHANGED) => window.dispatchEvent(new Event(CHANGED[what]));

export interface OpenSsh {
  /** Chosen by the caller so it can listen for `ssh:hostkey:{id}` before connecting. */
  id?: string;
  node_id?: string; host?: string; port?: number; user?: string;
  password?: string; key_path?: string; credential?: string; cols: number; rows: number;
}
/** `session_open_ssh` rejects with this shape; `need_credential` means "ask the user". */
export interface HostKeyChanged {
  host: string; port: number; key_type: string; fingerprint: string;
  file: string; line: number;
  /** true: the stale line is in Charon's own known_hosts and `hostkeyForget` can drop it. */
  own: boolean;
}
export interface OpenError { kind: "need_credential" | "hostkey_changed" | "error"; message: string; hostkey?: HostKeyChanged }
export const hostkeyForget = (host: string, port: number) => invoke<number>("hostkey_forget", { host, port });
/** The OpenSSH command that removes a host from ~/.ssh/known_hosts. */
export const sshKeygenRemove = (host: string, port: number) =>
  port === 22 ? `ssh-keygen -R ${host}` : `ssh-keygen -R "[${host}]:${port}"`;
export const isOpenError = (e: unknown): e is OpenError =>
  typeof e === "object" && e !== null && "kind" in e && "message" in e;
export const openSsh = (req: OpenSsh) => invoke<SessionInfo>("session_open_ssh", { req });

const enc = new TextEncoder();
const b64 = (bytes: Uint8Array) => btoa(String.fromCharCode(...bytes));
const unb64 = (s: string) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

export const sessionWrite = (id: string, text: string) =>
  invoke<void>("session_write", { id, dataB64: b64(enc.encode(text)) });
export const sessionResize = (id: string, cols: number, rows: number) =>
  invoke<void>("session_resize", { id, cols, rows });
export const sessionClose = (id: string) => invoke<void>("session_close", { id });

export interface OpenLocal { id?: string; node_id?: string; shell?: string; cwd?: string; admin?: boolean; cols: number; rows: number }
export const openLocal = (req: OpenLocal) => invoke<SessionInfo>("session_open_local", { req });

// Host key trust-on-first-use: backend asks, UI answers, connect resumes.
export interface HostKeyPrompt { session: string; host: string; port: number; key_type: string; fingerprint: string; known_hosts: string }
export function onHostKeyPrompt(id: string, cb: (p: HostKeyPrompt) => void): Promise<UnlistenFn> {
  return listen<HostKeyPrompt>(`ssh:hostkey:${id}`, (e) => cb(e.payload));
}
export const sshHostKeyAnswer = (id: string, accept: boolean) =>
  invoke<void>("ssh_hostkey_answer", { id, accept });

export function onSessionData(id: string, cb: (bytes: Uint8Array) => void): Promise<UnlistenFn> {
  return listen<string>(`term:data:${id}`, (e) => cb(unb64(e.payload)));
}
export function onSessionClosed(id: string, cb: (reason: string) => void): Promise<UnlistenFn> {
  return listen<string>(`term:closed:${id}`, (e) => cb(e.payload));
}

// Files: the SFTP pane and the editor. `session` undefined/null = the local disk;
// a local PTY session also means the local disk, an SSH session its SFTP subsystem.
export type FileKind = "file" | "dir" | "symlink" | "other";
export interface FsEntry {
  name: string; path: string; kind: FileKind; target_kind: FileKind | null;
  size: number; mtime: number; mode: number; user: string | null; group: string | null;
}
export interface FsListing { path: string; parent: string | null; entries: FsEntry[] }
export interface TransferProgress { id: string; done: number; total: number; state: "running" | "done" | "error" | "cancelled"; error: string | null }
/** Which backend serves a session's files: its own (SFTP / local disk) or the interactive shell (after sudo -i). */
export type FsVia = "sftp" | "shell" | "sudo";
type S = string | null | undefined;
export const sftpHome = (session: S, via?: FsVia) => invoke<string>("sftp_home", { session, via });
export const sftpList = (session: S, path: string, via?: FsVia) => invoke<FsListing>("sftp_list", { session, path, via });
export const sftpStat = (session: S, path: string, via?: FsVia) => invoke<FsEntry>("sftp_stat", { session, path, via });
export const sftpMkdir = (session: S, path: string, via?: FsVia) => invoke<void>("sftp_mkdir", { session, path, via });
export const sftpRename = (session: S, from: string, to: string, via?: FsVia) => invoke<void>("sftp_rename", { session, from, to, via });
export const sftpDelete = (session: S, path: string, isDir: boolean, via?: FsVia) => invoke<void>("sftp_delete", { session, path, isDir, via });
export const sftpChmod = (session: S, path: string, mode: number, via?: FsVia) => invoke<void>("sftp_chmod", { session, path, mode, via });
/** Resolves when the transfer ends; progress arrives on `sftp:progress` keyed by `transfer`. */
export const sftpGet = (session: S, transfer: string, remote: string, local: string, via?: FsVia) =>
  invoke<void>("sftp_get", { session, transfer, remote, local, via });
export const sftpPut = (session: S, transfer: string, local: string, remote: string, via?: FsVia) =>
  invoke<void>("sftp_put", { session, transfer, local, remote, via });
export const sftpCancel = (transfer: string) => invoke<boolean>("sftp_cancel", { transfer });
export function onTransferProgress(cb: (p: TransferProgress) => void): Promise<UnlistenFn> {
  return listen<TransferProgress>("sftp:progress", (e) => cb(e.payload));
}

// Editor: whole-file text with detected encoding/EOL; writes are atomic.
export type Encoding = "utf-8" | "utf-8-bom" | "latin-1";
export type Eol = "lf" | "crlf";
export interface TextFile { text: string; encoding: Encoding; eol: Eol; size: number }
export const fileRead = (session: S, path: string, via?: FsVia) => invoke<TextFile>("file_read", { session, path, via });
export const fileWrite = (session: S, path: string, text: string, encoding: Encoding, eol: Eol, via?: FsVia) =>
  invoke<number>("file_write", { session, path, text, encoding, eol, via });
/** Backend said the session's own file access may not read this (sudo territory). */
export const isPermissionError = (e: unknown) => /permission denied|EACCES/i.test(String(e));

// P6: root files over sudo-sftp (opened as an exec channel running `sudo -S sftp-server`).
export interface SudoOutcome { ok: boolean; probe: string | null; sudo: "ok" | "refused" | "timeout" | "skipped" | "error"; reason: string }
export const sftpSudoOpen = (session: string, password?: string) => invoke<SudoOutcome>("sftp_sudo_open", { session, password });
export const sftpSudoHasPassword = (session: string) => invoke<boolean>("sftp_sudo_has_password", { session });
export const sftpSudoClear = (session: string) => invoke<void>("sftp_sudo_clear", { session });

// P5: `<cmd> --help` over the session, cached per host in data/help-cache.
export const helpFetch = (session: string, cmd: string) => invoke<string>("help_fetch", { session, cmd });
export const manFetch = (session: string, cmd: string) => invoke<string>("man_fetch", { session, cmd });
export const tldrUpdate = () => invoke<number>("tldr_update");
export const tldrBundle = () => invoke<string>("tldr_bundle");

// MCP server (R2-4).
export interface McpStatus { enabled: boolean; running: boolean; port: number | null; token: string; default_tier: string }
export const mcpStatus = () => invoke<McpStatus>("mcp_status");
export const mcpSetEnabled = (enabled: boolean) => invoke<McpStatus>("mcp_set_enabled", { enabled });
export const mcpRegenToken = () => invoke<McpStatus>("mcp_regen_token");
export const mcpConfig = (kind: "http" | "stdio") => invoke<string>("mcp_config", { kind });
export const mcpAnswer = (reqId: string, value: unknown) => invoke<void>("mcp_answer", { reqId, value });
export const mcpLog = (lines?: number) => invoke<McpLogLine[]>("mcp_log", { lines });
export interface McpLogLine { ts: string; tool: string; session: string | null; tier: string; decision: string; ok: boolean; exit_code?: number; duration_ms: number }
export interface McpApprove { req_id: string; tool: string; session: string; command: string }
export interface McpScrollback { req_id: string; session: string; last_n_commands: number | null; last_n_lines: number | null }
export interface McpOpenEditor { req_id: string; path: string | null; session: string | null; remote_path: string | null }
export const onMcpApprove = (cb: (p: McpApprove) => void) => listen<McpApprove>("mcp:approve", (e) => cb(e.payload));
export const onMcpScrollback = (cb: (p: McpScrollback) => void) => listen<McpScrollback>("mcp:scrollback", (e) => cb(e.payload));
export const onMcpOpenEditor = (cb: (p: McpOpenEditor) => void) => listen<McpOpenEditor>("mcp:open_editor", (e) => cb(e.payload));
