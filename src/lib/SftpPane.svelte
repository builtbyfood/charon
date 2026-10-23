<script lang="ts">
  // File pane beside a terminal: SFTP for SSH tabs, the local disk for local
  // tabs — same UI, the backend picks the filesystem from the session id.
  // Supports multi-select and recursive (directory / multi-item) transfers.
  import { onMount, onDestroy, untrack } from "svelte";
  import { open as pickOpen } from "@tauri-apps/plugin-dialog";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import {
    sftpHome, sftpList, sftpMkdir, sftpRename, sftpDelete, sftpChmod,
    sftpGetTree, sftpPutTree, sftpCancel, onTreeEvent,
    type FsEntry, type FsListing, type TreeEvent, type TreeOpts, type Conflict,
  } from "./api";
  import ContextMenu, { type MenuItem } from "./ContextMenu.svelte";
  import { joinPath, crumbsOf, osc7ToLocal, humanSize, modeString } from "./paths";
  import { followTarget, pushTarget } from "./panecwd";
  import { isPermissionError, sftpSudoOpen, sftpSudoHasPassword, sftpSudoClear, type FsVia } from "./api";
  import { ensureSudo } from "./sudoedit";

  let { session, cwd, color, sshUser, shellUser, isIdle, parallel, onopen, onchdir, onclose }: {
    /** Session id; null = local disk. */
    session: string | null;
    /** Shell cwd from OSC 7, followed when "follow" is on. */
    cwd?: string;
    color?: string;
    /** SSH login user (SFTP runs as this). */
    sshUser?: string;
    /** User at the shell prompt; differs from sshUser after sudo -i / su -. */
    shellUser?: string;
    /** The shell is at an idle prompt: safe to run hidden commands / cd. */
    isIdle?: () => boolean;
    /** Max files transferring at once (profile files.parallel_transfers); shown for reference. */
    parallel?: number;
    /** Open a file in the editor with the backend the pane is using. */
    onopen?: (path: string, via: FsVia) => void;
    /** "→ term" is on: tell the shell to cd here. */
    onchdir?: (path: string) => void;
    onclose: () => void;
  } = $props();

  let el = $state<HTMLDivElement | null>(null);
  let listing = $state<FsListing | null>(null);
  let error = $state("");
  let loading = $state(false);
  let showHidden = $state(false);
  let follow = $state(true);
  let pushCwd = $state(false);   // "→ term": navigating here cd's the shell
  // Which backend serves this pane: the session's own (SFTP) or the shell (after sudo).
  let via = $state<FsVia>("sftp");
  // Suppress the opposite direction for a moment after each cd/OSC-7 so the two toggles don't ping-pong.
  let suppressUntil = 0;
  let lastCwd: string | undefined;    // last shell cwd we followed
  let lastPushed: string | undefined; // last pane dir we pushed as cd
  // Root escalation (P6): try sudo-sftp first, ask for a password if none is saved,
  // fall back to the shell backend if sftp-server/sudo are unavailable.
  let askSudo = $state(false);
  let sudoPassword = $state("");
  let sudoHasSaved = $state<boolean | null>(null);
  let sudoBusy = $state(false);
  let sudoPwEl = $state<HTMLInputElement | null>(null);
  let sudoReason = $state("");  // why the last escalation fell back to the shell backend
  // "browse as root (sudo)" was chosen by hand: keep it when the shell user changes.
  let sudoOnDemand = false;
  // Set while the password form answers an ensureSudo() ask (Edit as root) rather
  // than switching the pane itself.
  let sudoResolve = $state<((pw: string | null) => void) | null>(null);
  let sortKey = $state<"name" | "size" | "mtime" | "mode">("name");
  let sortDir = $state<1 | -1>(1);
  // Multi-select: a set of entry paths, with an anchor path for Shift ranges.
  let selected = $state<Set<string>>(new Set());
  let anchor: string | null = null;
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let dropping = $state(false);
  // Inline prompt row (window.prompt is not available in the webview).
  let ask = $state<{ kind: "rename" | "chmod" | "mkdir"; entry?: FsEntry; value: string } | null>(null);
  let askEl = $state<HTMLInputElement | null>(null);

  // Per-file progress inside one tree transfer.
  interface FileProg { rel: string; done: number; total: number; state: "running" | "done" | "failed" | "skipped"; error?: string }
  // One top-level item (file or directory) being transferred recursively.
  interface Transfer {
    id: string; name: string; dir: "get" | "put";
    /** Destination directory: local dir for get, remote dir for put. */
    destDir: string;
    /** Source path (remote for get, local for put) — kept for retry. */
    src: string;
    opts: TreeOpts;
    filesTotal: number; filesDone: number; bytesTotal: number; bytesDone: number;
    state: "running" | "done" | "error" | "cancelled";
    error?: string; cancelled: boolean;
    failed: string[];              // rels that failed, for retry-failed-only
    files: Record<string, FileProg>;
    warnings: string[];
    expanded: boolean;
  }
  let transfers = $state<Transfer[]>([]);
  // Transfer options (shared by every transfer started from this pane).
  let followSymlinks = $state(true);   // follow symlinks; off = skip with a warning
  let preserveMtime = $state(true);
  let preserveMode = $state(false);
  // Conflict handling: asked once per batch; "remember" reuses the choice for the session.
  let conflictAsk = $state<null | { count: number; kind: "get" | "put"; run: (c: Conflict) => void }>(null);
  let rememberConflict = $state(false);
  let sessionConflict: Conflict | null = null;
  const unlisteners: Array<() => void> = [];

  const isLocal = $derived(session === null);
  const rootBackend = $derived(via === "shell" || via === "sudo");
  const differentUser = $derived(!!shellUser && !!sshUser && shellUser !== sshUser);
  /** Windows has no mode bits; drop the column rather than show blanks. */
  const hasModes = $derived(!!listing && listing.entries.some((e) => e.mode));
  const rows = $derived.by(() => {
    if (!listing) return [];
    const list = listing.entries.filter((e) => showHidden || !e.name.startsWith("."));
    const isDir = (e: FsEntry) => e.kind === "dir" || (e.kind === "symlink" && e.target_kind === "dir");
    return list.sort((a, b) => {
      const d = Number(isDir(b)) - Number(isDir(a));
      if (d) return d;
      let c = 0;
      if (sortKey === "name") c = a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
      else if (sortKey === "size") c = a.size - b.size;
      else if (sortKey === "mtime") c = a.mtime - b.mtime;
      else c = a.mode - b.mode;
      return c * sortDir || a.name.localeCompare(b.name);
    });
  });
  const selectedEntries = $derived(rows.filter((e) => selected.has(e.path)));

  onMount(async () => {
    unlisteners.push(await onTreeEvent(onTree));
    // OS drag-drop lands on the webview; take it if it was dropped over this pane.
    unlisteners.push(await getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "leave") { dropping = false; return; }
      const inside = el ? overThisPane(p.position.x, p.position.y) : false;
      if (p.type === "over" || p.type === "enter") { dropping = inside; return; }
      dropping = false;
      if (p.type === "drop" && inside && p.paths.length) startUpload(p.paths);
    }));
    try { await go(await sftpHome(session, via)); } catch (e) { error = String(e); }
  });
  onDestroy(() => unlisteners.forEach((u) => u()));

  // Tauri drag positions are physical pixels; hit-test with elementFromPoint (CSS px)
  // so this matches App.svelte's editor-drop routing exactly (they must agree).
  function overThisPane(px: number, py: number): boolean {
    const s = window.devicePixelRatio || 1;
    const hit = document.elementFromPoint(px / s, py / s);
    return !!hit && !!el && el.contains(hit);
  }

  async function go(path: string) {
    loading = true; error = ""; selected = new Set(); anchor = null; ask = null;
    try {
      try { listing = await sftpList(session, path, via); }
      catch (e) {
        // SFTP runs as the login user; after sudo -i the shell/root can read what it can't.
        if (session && via === "sftp" && isPermissionError(e) && differentUser) { pendingPath = path; await escalate(); return; }
        else throw e;
      }
    }
    catch (e) { error = String(e); }
    finally { loading = false; }
  }
  const refresh = () => listing && go(listing.path);
  const up = () => listing?.parent && go(listing.parent);

  // Follow the shell ("→ cwd"): act only when the shell's cwd actually changes, so
  // clicking around the pane (which changes listing, not cwd) is never yanked back (B3).
  $effect(() => {
    const dir = cwd;
    if (!follow) return;
    const target = followTarget(dir, lastCwd, isLocal, osc7ToLocal);
    lastCwd = dir;
    if (target === null) return;
    untrack(() => {
      const skip = Date.now() < suppressUntil ? "suppressed" : !listing ? "no-listing" : target === listing.path ? "already-there" : "";
      console.info(`[charon] files: follow cwd=${JSON.stringify(target)} ${skip || "go"}`);
      if (skip) return;
      suppressUntil = Date.now() + 500;
      go(target);
    });
  });
  // Push the pane's directory to the shell ("→ term"): act only when the pane moves.
  $effect(() => {
    const dir = listing?.path;
    if (!pushCwd || !onchdir) return;
    const target = pushTarget(dir, lastPushed);
    lastPushed = dir;
    if (target === null) return;
    untrack(() => {
      const skip = Date.now() < suppressUntil ? "suppressed" : isIdle && !isIdle() ? "shell-busy-or-no-133"
        : (isLocal ? osc7ToLocal(cwd ?? "") === target : cwd === target) ? "already-there" : "";
      console.info(`[charon] files: push cd=${JSON.stringify(target)} ${skip || "go"}`);
      if (skip) return;
      suppressUntil = Date.now() + 500;
      onchdir(target);
    });
  });
  // Backend follows the shell user, un-latched: back to the login user → SFTP again (B2).
  // Only on an actual change of shell user (exit from sudo -i): reading `via` here
  // would undo an on-demand root browse the moment it starts.
  let lastShellUser: string | undefined;
  $effect(() => {
    const su = shellUser;
    const was = lastShellUser;
    lastShellUser = su;
    if (!su || !sshUser || su !== sshUser || was === undefined || was === su) return;
    untrack(() => {
      if (via === "sftp" || sudoOnDemand) return;
      const wasSudo = via === "sudo";
      via = "sftp"; askSudo = false;
      if (wasSudo && session) sftpSudoClear(session).catch(() => {});
      refresh();
    });
  });

  // ---- selection
  const isDirEntry = (e: FsEntry) => e.kind === "dir" || (e.kind === "symlink" && e.target_kind === "dir");
  function onRowClick(ev: MouseEvent, e: FsEntry) {
    if (ev.shiftKey && anchor) {
      const paths = rows.map((r) => r.path);
      const a = paths.indexOf(anchor), b = paths.indexOf(e.path);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        const next = ev.ctrlKey || ev.metaKey ? new Set(selected) : new Set<string>();
        for (let i = lo; i <= hi; i++) next.add(paths[i]);
        selected = next;
      }
    } else if (ev.ctrlKey || ev.metaKey) {
      const next = new Set(selected);
      next.has(e.path) ? next.delete(e.path) : next.add(e.path);
      selected = next; anchor = e.path;
    } else {
      selected = new Set([e.path]); anchor = e.path;
    }
  }
  function selectAll() { selected = new Set(rows.map((r) => r.path)); anchor = rows[0]?.path ?? null; }
  function onListKey(k: KeyboardEvent) {
    if ((k.ctrlKey || k.metaKey) && k.key.toLowerCase() === "a") { k.preventDefault(); selectAll(); }
  }

  function activate(e: FsEntry) {
    if (isDirEntry(e)) go(e.path);
    else if (onopen) onopen(e.path, via);
    else error = "No editor yet: use Download… from the right-click menu.";
  }
  function sortBy(k: typeof sortKey) {
    if (sortKey === k) sortDir = sortDir === 1 ? -1 : 1; else { sortKey = k; sortDir = 1; }
  }

  // ---- root escalation
  let pendingPath: string | null = null;
  /** Switch the pane to root. `onDemand` = the user picked "browse as root (sudo)" /
   *  "Open as root" (not an EACCES while the shell is root). */
  async function escalate(onDemand = false) {
    if (!session) return;
    sudoOnDemand = onDemand;
    console.info(`[charon] sudo-sftp: escalating (${onDemand ? "on demand" : "EACCES"} as ${sshUser}, shell is ${shellUser})`);
    if (sudoHasSaved === null) { try { sudoHasSaved = await sftpSudoHasPassword(session); } catch { sudoHasSaved = false; } }
    if (sudoHasSaved) await openSudo(undefined);
    else { askSudo = true; queueMicrotask(() => { sudoPwEl?.focus(); }); }
  }
  async function openSudo(pw: string | undefined) {
    if (!session) return;
    sudoBusy = true; error = ""; sudoReason = "";
    try {
      const o = await sftpSudoOpen(session, pw);
      console.info(`[charon] sudo-sftp: probe=${o.probe ?? "none"} sudo=${o.sudo} reason=${o.reason || "-"}`);
      if (o.ok) {
        via = "sudo"; askSudo = false; sudoPassword = "";
        // Handshake succeeded; the readdir on the sudo session is the next thing that can fail.
        const target = pendingPath ?? listing?.path ?? await homeFor();
        try {
          const l = await sftpList(session, target, "sudo");
          listing = l;
          console.info(`[charon] sudo-sftp: first readdir ok at ${l.path} (${l.entries.length} entries)`);
        } catch (re) {
          console.info(`[charon] sudo-sftp: readdir after sudo=ok FAILED at ${target}: ${String(re)} — falling back to shell`);
          via = "shell"; sudoReason = `sudo handshake ok but readdir failed: ${String(re)}`;
          error = sudoReason;
          await go(target);
        }
      } else {
        // sftp-server missing or sudo refused/timeout → shell backend (base64 over the
        // pty) when the shell is root; otherwise there is nothing to fall back to.
        askSudo = false; sudoReason = o.reason;
        if (differentUser) {
          via = "shell";
          error = `sudo SFTP: ${o.sudo} — ${o.reason}. Using the shell backend.`;
          await go(pendingPath ?? listing?.path ?? await homeFor());
        } else {
          sudoOnDemand = false;
          error = `sudo SFTP: ${o.sudo} — ${o.reason}`;
        }
      }
    } catch (e) {
      askSudo = false; sudoReason = String(e);
      console.info(`[charon] sudo-sftp: call failed reason=${String(e)}`);
      if (differentUser) {
        via = "shell";
        error = `sudo SFTP failed (${String(e)}); using the shell backend`;
        await go(pendingPath ?? listing?.path ?? await homeFor());
      } else {
        sudoOnDemand = false;
        error = `sudo SFTP failed: ${String(e)}`;
      }
    } finally {
      sudoBusy = false; pendingPath = null;
    }
  }
  const homeFor = () => sftpHome(session, via);

  /** "Edit as root": make sure a root SFTP channel exists (asking for the password in
   *  this pane's form if nothing is stored), then open the file bound to it. The pane
   *  itself keeps its backend. */
  async function editAsRoot(e: FsEntry) {
    if (!session || !onopen) return;
    sudoBusy = true; error = "";
    try {
      await ensureSudo(session, {
        open: sftpSudoOpen,
        ask: () => new Promise<string | null>((resolve) => {
          sudoResolve = resolve; askSudo = true; sudoBusy = false;
          queueMicrotask(() => sudoPwEl?.focus());
        }),
      });
      console.info(`[charon] sudo-sftp: edit as root ${e.path}`);
      onopen(e.path, "sudo");
    } catch (err) {
      error = String(err);
    } finally {
      sudoBusy = false; askSudo = false; sudoResolve = null; sudoPassword = "";
    }
  }
  /** "Open as root" on a directory: browse it over sudo-sftp. */
  function openDirAsRoot(e: FsEntry) { pendingPath = e.path; escalate(true); }
  function submitSudo() {
    if (sudoResolve) { const r = sudoResolve; sudoResolve = null; sudoBusy = true; r(sudoPassword); }
    else openSudo(sudoPassword);
  }
  function skipSudo() {
    if (sudoResolve) { const r = sudoResolve; sudoResolve = null; r(null); return; }
    askSudo = false;
    if (differentUser) { via = "shell"; refresh(); } else sudoOnDemand = false;
  }

  // ---- transfers
  function optsFor(conflict: Conflict): TreeOpts {
    return { follow_symlinks: followSymlinks, preserve_mode: preserveMode, preserve_mtime: preserveMtime, conflict };
  }
  function onTree(ev: TreeEvent) {
    const t = transfers.find((x) => x.id === ev.transfer);
    if (!t) return;
    if (ev.event === "plan") {
      t.filesTotal = ev.files.length; t.bytesTotal = ev.bytes_total;
      if (ev.dest) t.name = ev.dest;
    } else if (ev.event === "file") {
      const f = t.files[ev.rel] ?? (t.files[ev.rel] = { rel: ev.rel, done: 0, total: ev.total, state: ev.state });
      f.done = ev.done; f.total = ev.total || f.total; f.state = ev.state; f.error = ev.error ?? undefined;
      recompute(t);
    } else if (ev.event === "warn") {
      t.warnings = [...t.warnings, ev.message].slice(-200);
    } else if (ev.event === "done") {
      t.filesDone = ev.files_done; t.filesTotal = ev.files_total;
      t.bytesDone = ev.bytes_done; t.bytesTotal = ev.bytes_total;
      t.failed = ev.failed; t.cancelled = ev.cancelled;
      t.state = ev.cancelled ? "cancelled" : ev.failed.length ? "error" : "done";
    }
  }
  function recompute(t: Transfer) {
    const vals = Object.values(t.files);
    t.bytesDone = vals.reduce((s, f) => s + (f.state === "done" ? f.total : f.done), 0);
    t.filesDone = vals.filter((f) => f.state === "done").length;
  }
  function trackTree(dir: "get" | "put", src: string, name: string, destDir: string, conflict: Conflict) {
    const id = crypto.randomUUID();
    const opts = optsFor(conflict);
    const t: Transfer = {
      id, name, dir, destDir, src, opts,
      filesTotal: 0, filesDone: 0, bytesTotal: 0, bytesDone: 0,
      state: "running", cancelled: false, failed: [], files: {}, warnings: [], expanded: false,
    };
    transfers = [t, ...transfers].slice(0, 50);
    const call = dir === "get"
      ? sftpGetTree(session, id, src, destDir, opts, via)
      : sftpPutTree(session, id, src, destDir, opts, via);
    call
      .then(() => { if (dir === "put") refresh(); })
      .catch((e) => { if (t.state === "running") { t.state = "error"; t.error = String(e); } });
  }
  function retryTree(t: Transfer) {
    const only = [...t.failed];
    if (!only.length) return;
    t.state = "running"; t.error = undefined; t.cancelled = false; t.failed = [];
    for (const rel of only) { const f = t.files[rel]; if (f) { f.state = "running"; f.done = 0; f.error = undefined; } }
    const opts = { ...t.opts, conflict: "overwrite" as Conflict };
    const call = t.dir === "get"
      ? sftpGetTree(session, t.id, t.src, t.destDir, opts, via, only)
      : sftpPutTree(session, t.id, t.src, t.destDir, opts, via, only);
    call
      .then(() => { if (t.dir === "put") refresh(); })
      .catch((e) => { if (t.state === "running") { t.state = "error"; t.error = String(e); } });
  }
  // Ask the conflict policy once, then run the batch; remember for the session if asked.
  function withConflict(kind: "get" | "put", count: number, run: (c: Conflict) => void) {
    if (sessionConflict) { run(sessionConflict); return; }
    conflictAsk = { kind, count, run: (c) => { if (rememberConflict) sessionConflict = c; conflictAsk = null; run(c); } };
  }

  async function downloadSelection() {
    const items = selectedEntries;
    if (!items.length) return;
    const destDir = await pickOpen({ directory: true, title: `Download ${items.length} item(s) to…` });
    if (typeof destDir !== "string") return;
    withConflict("get", items.length, (c) => { for (const e of items) trackTree("get", e.path, e.name, destDir, c); });
  }
  function startUpload(paths: string[]) {
    if (!listing || !paths.length) return;
    const dest = listing.path;
    withConflict("put", paths.length, (c) => {
      for (const p of paths) { const name = p.split(/[\\/]/).pop() || p; trackTree("put", p, name, dest, c); }
    });
  }
  async function pickUploadFiles() {
    const picked = await pickOpen({ multiple: true, title: "Upload files" });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    startUpload(paths);
  }
  async function pickUploadFolder() {
    const picked = await pickOpen({ directory: true, title: "Upload folder" });
    if (typeof picked === "string") startUpload([picked]);
  }
  const clearDone = () => (transfers = transfers.filter((t) => t.state === "running"));
  const pct = (t: Transfer) => (t.bytesTotal ? Math.floor((t.bytesDone / t.bytesTotal) * 100) + "%" : t.state === "done" ? "100%" : humanSize(t.bytesDone));

  // ---- inline prompts
  function startAsk(kind: NonNullable<typeof ask>["kind"], entry?: FsEntry) {
    ask = { kind, entry, value: kind === "rename" ? entry!.name : kind === "chmod" ? entry!.mode.toString(8).padStart(3, "0") : "" };
    queueMicrotask(() => { askEl?.focus(); askEl?.select(); });
  }
  async function submitAsk(e: SubmitEvent) {
    e.preventDefault();
    if (!ask || !listing) return;
    const { kind, entry, value } = ask;
    const v = value.trim();
    if (!v) { ask = null; return; }
    try {
      if (kind === "mkdir") await sftpMkdir(session, joinPath(listing.path, v), via);
      else if (kind === "rename" && entry) await sftpRename(session, entry.path, joinPath(listing.path, v), via);
      else if (kind === "chmod" && entry) {
        if (!/^[0-7]{3,4}$/.test(v)) { error = "mode must be octal, e.g. 644 or 0755"; return; }
        await sftpChmod(session, entry.path, parseInt(v, 8), via);
      }
      ask = null;
      await refresh();
    } catch (err) { error = String(err); }
  }
  async function removeSelection() {
    const items = selectedEntries;
    if (!items.length) return;
    const label = items.length === 1 ? items[0].name : `${items.length} items`;
    if (!confirm(`Delete ${label}? (directories must be empty)`)) return;
    try {
      for (const e of items) await sftpDelete(session, e.path, isDirEntry(e), via);
      await refresh();
    } catch (err) { error = String(err); }
  }

  function onRowMenu(ev: MouseEvent, e: FsEntry) {
    ev.preventDefault(); ev.stopPropagation();
    // Right-click on an unselected row selects just it; on a selected row keeps the set.
    if (!selected.has(e.path)) { selected = new Set([e.path]); anchor = e.path; }
    const many = selected.size > 1;
    const only = selectedEntries.length === 1 ? selectedEntries[0] : null;
    const isDir = only ? isDirEntry(only) : false;
    menu = { x: ev.clientX, y: ev.clientY, items: [
      { label: isDir ? "Open" : "Open in editor", action: () => only && activate(only), disabled: many || (!isDir && !onopen) },
      ...(!isLocal && via !== "sudo" ? [isDir
        ? { label: "Open as root", action: () => only && openDirAsRoot(only), disabled: many || !only || sudoBusy }
        : { label: "Edit as root", action: () => only && editAsRoot(only), disabled: many || !only || !onopen || sudoBusy }] : []),
      { label: many ? `Download ${selected.size} items…` : "Download…", action: downloadSelection },
      { separator: true, label: "" },
      { label: "Rename…", action: () => only && startAsk("rename", only), disabled: many || !only },
      { label: many ? `Delete ${selected.size}…` : "Delete…", action: removeSelection },
      { label: "Copy path", action: () => writeText(selectedEntries.map((x) => x.path).join("\n")) },
      { label: "chmod…", action: () => only && startAsk("chmod", only), disabled: many || !only || (isLocal && only.mode === 0) },
    ] };
  }
  function onPaneMenu(ev: MouseEvent) {
    ev.preventDefault(); ev.stopPropagation();
    menu = { x: ev.clientX, y: ev.clientY, items: [
      { label: "New folder…", action: () => startAsk("mkdir") },
      { label: "Upload files…", action: pickUploadFiles },
      { label: "Upload folder…", action: pickUploadFolder },
      { label: "Refresh", action: refresh },
      { separator: true, label: "" },
      { label: "Select all", action: selectAll, disabled: !rows.length },
      { label: "Copy path", action: () => listing && writeText(listing.path) },
    ] };
  }
  const when = (secs: number) => secs ? new Date(secs * 1000).toLocaleString(undefined, { dateStyle: "short", timeStyle: "short" }) : "";
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="pane" class:dropping bind:this={el} data-drop="files" style:--session-color={color ?? "var(--accent)"} oncontextmenu={onPaneMenu}>
  <div class="bar">
    <button type="button" title="Up" onclick={up} disabled={!listing?.parent}>↑</button>
    <button type="button" title="Refresh" onclick={refresh} disabled={!listing}>⟳</button>
    <div class="crumbs">
      {#if listing}
        {#each crumbsOf(listing.path) as c, i (c.path)}
          {#if i > 0}<span class="sep">›</span>{/if}
          <button type="button" class="crumb" onclick={() => go(c.path)} title={c.path}>{c.label}</button>
        {/each}
      {/if}
    </div>
    <button type="button" title="Download the selection" onclick={downloadSelection} disabled={!selected.size}>↓</button>
    <button type="button" title="Upload files here" onclick={pickUploadFiles} disabled={!listing}>↑file</button>
    <button type="button" title="Upload a folder here" onclick={pickUploadFolder} disabled={!listing}>↑dir</button>
    <button type="button" title="Toggle hidden files" class:on={showHidden} onclick={() => (showHidden = !showHidden)}>.*</button>
    <button type="button" title="Follow the shell's directory (OSC 7)" class:on={follow} onclick={() => (follow = !follow)}>→ cwd</button>
    {#if !isLocal}
      <button type="button" title="Send cd to the shell when navigating here" class:on={pushCwd} onclick={() => (pushCwd = !pushCwd)}>→ term</button>
    {/if}
    <button type="button" title="Close (Ctrl+Shift+E)" onclick={onclose}>×</button>
  </div>
  {#if !isLocal}
    <div class="backend" class:root={rootBackend}>
      {#if via === "sudo"}
        sftp as {shellUser ?? "root"} (sudo){#if differentUser} · SFTP is {sshUser}{/if}
        <button type="button" class="link" title="Back to plain SFTP as the login user" onclick={() => { via = "sftp"; sudoOnDemand = false; if (session) sftpSudoClear(session); refresh(); }}>use SFTP</button>
      {:else if via === "shell"}
        <span title={sudoReason ? `sudo-sftp fell back: ${sudoReason}` : "browsing via the terminal (base64 over the pty)"}>shell as {shellUser ?? "?"}{#if differentUser} · SFTP is {sshUser}{/if}</span>
        <button type="button" class="link" title={sudoReason ? `Retry sudo-sftp (last: ${sudoReason})` : "Retry root over sudo-sftp (faster)"} onclick={() => escalate()} disabled={sudoBusy}>{sudoBusy ? "…" : "try sudo"}</button>
        <button type="button" class="link" title="Back to plain SFTP as the login user" onclick={() => { via = "sftp"; refresh(); }}>use SFTP</button>
      {:else}
        sftp as {sshUser ?? "login user"}
        {#if differentUser}<button type="button" class="link" title="Browse as {shellUser} — sudo SFTP, or the terminal as a fallback" onclick={() => escalate()} disabled={sudoBusy}>browse as {shellUser}</button>{:else}<button type="button" class="link" title="Browse as root over sudo-sftp (asks for the sudo password once, or uses the stored credential)" onclick={() => escalate(true)} disabled={sudoBusy}>{sudoBusy ? "…" : "browse as root (sudo)"}</button>{/if}
      {/if}
    </div>
  {/if}
  {#if askSudo}
    <form class="ask" onsubmit={(e) => { e.preventDefault(); submitSudo(); }}>
      <span>sudo password for {sshUser ?? "the login user"}</span>
      <input bind:this={sudoPwEl} type="password" bind:value={sudoPassword} autocomplete="off" spellcheck="false"
        onkeydown={(e) => { if (e.key === "Escape") skipSudo(); }} />
      <button type="submit" disabled={sudoBusy || !sudoPassword}>{sudoBusy ? "…" : "Unlock"}</button>
      <button type="button" onclick={skipSudo} title={differentUser && !sudoResolve ? "Skip sudo, browse via the terminal" : "Cancel"}>{differentUser && !sudoResolve ? "use shell" : "cancel"}</button>
    </form>
  {/if}
  {#if conflictAsk}
    <div class="ask conflict">
      <span>If {conflictAsk.count > 1 ? "an item" : "it"} already exists at the destination:</span>
      <button type="button" onclick={() => conflictAsk!.run("overwrite")}>Overwrite</button>
      <button type="button" onclick={() => conflictAsk!.run("skip")}>Skip</button>
      <button type="button" onclick={() => conflictAsk!.run("rename")}>Keep both</button>
      <label class="pm" title="Use this choice for the rest of the session"><input type="checkbox" bind:checked={rememberConflict} /> remember</label>
      <button type="button" onclick={() => (conflictAsk = null)}>Cancel</button>
    </div>
  {/if}
  {#if ask}
    <form class="ask" onsubmit={submitAsk}>
      <span>{ask.kind === "mkdir" ? "New folder" : ask.kind === "rename" ? "Rename to" : "chmod (octal)"}</span>
      <input bind:this={askEl} bind:value={ask.value} spellcheck="false" class="mono" onkeydown={(e) => { if (e.key === "Escape") ask = null; }} />
      <button type="submit">OK</button>
      <button type="button" onclick={() => (ask = null)}>Cancel</button>
    </form>
  {/if}
  {#if error}<div class="err">{error}</div>{/if}
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="list" class:loading class:nomode={!hasModes} tabindex="0" onkeydown={onListKey}>
    <div class="head">
      <button type="button" onclick={() => sortBy("name")}>{selected.size ? `${selected.size} selected` : "Name"}{sortKey === "name" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      <button type="button" onclick={() => sortBy("size")}>Size{sortKey === "size" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      <button type="button" onclick={() => sortBy("mtime")}>Modified{sortKey === "mtime" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      {#if hasModes}<button type="button" onclick={() => sortBy("mode")}>Perm{sortKey === "mode" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>{/if}
    </div>
    {#each rows as e (e.path)}
      {@const isDir = isDirEntry(e)}
      <div class="row" class:sel={selected.has(e.path)} class:dir={isDir} role="row" tabindex="0"
        onclick={(ev) => onRowClick(ev, e)} ondblclick={() => activate(e)}
        onkeydown={(k) => { if (k.key === "Enter") activate(e); }}
        oncontextmenu={(ev) => onRowMenu(ev, e)}>
        <span class="name" title={e.path}>{isDir ? "▸ " : ""}{e.name}{e.kind === "symlink" ? " →" : ""}</span>
        <span class="size">{isDir ? "" : humanSize(e.size)}</span>
        <span class="mtime">{when(e.mtime)}</span>
        {#if hasModes}<span class="mode mono">{modeString(e.mode, isDir)}</span>{/if}
      </div>
    {/each}
    {#if listing && rows.length === 0}<div class="empty">empty</div>{/if}
  </div>
  {#if transfers.length}
    <div class="transfers">
      <div class="thead">
        <span>Transfers</span>
        <label class="pm" title="Follow symlinks (off: skip them with a warning)"><input type="checkbox" bind:checked={followSymlinks} /> follow links</label>
        <label class="pm" title="Preserve the source modification time"><input type="checkbox" bind:checked={preserveMtime} /> mtime</label>
        <label class="pm" title="Preserve the source permission bits (Unix)"><input type="checkbox" bind:checked={preserveMode} /> mode</label>
        {#if parallel}<span class="dim" title="Max files transferring at once (files.parallel_transfers)">×{parallel}</span>{/if}
        <button type="button" onclick={clearDone}>clear finished</button>
      </div>
      {#each transfers as t (t.id)}
        <div class="tr" class:err={t.state === "error"}>
          <button type="button" class="disc" title="Show per-file progress" onclick={() => (t.expanded = !t.expanded)}>{t.expanded ? "▾" : "▸"}</button>
          <span class="tname" title={`${t.destDir}${t.error ? ` · ${t.error}` : ""}`}>{t.dir === "get" ? "↓" : "↑"} {t.name}</span>
          <progress max={t.bytesTotal || 1} value={t.bytesDone}></progress>
          <span class="tpct">{pct(t)}</span>
          <span class="tcount" title="files done / total">{t.filesDone}/{t.filesTotal || "?"}{t.failed.length ? ` · ${t.failed.length} failed` : ""}</span>
          {#if t.state === "running"}
            <button type="button" onclick={() => sftpCancel(t.id)}>cancel</button>
          {:else}
            <span class="tstate">{t.state}</span>
            {#if t.failed.length}<button type="button" title="Retry only the failed files" onclick={() => retryTree(t)}>retry</button>{/if}
          {/if}
        </div>
        {#if t.expanded}
          <div class="files">
            {#each Object.values(t.files) as f (f.rel)}
              <div class="fr" class:ffail={f.state === "failed"}>
                <span class="frel" title={f.error ?? f.rel}>{f.rel}</span>
                <span class="fst">{f.state === "running" && f.total ? Math.floor((f.done / f.total) * 100) + "%" : f.state}</span>
              </div>
            {/each}
            {#each t.warnings as w}<div class="warn" title={w}>⚠ {w}</div>{/each}
          </div>
        {/if}
      {/each}
    </div>
  {/if}
  {#if dropping}<div class="drop">Drop to upload into {listing?.path ?? "…"}</div>{/if}
  {#if menu}
    <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
  {/if}
</div>

<style>
  .pane { position: relative; height: 100%; display: flex; flex-direction: column; min-width: 0; background: var(--bg-raised); border-left: 1px solid var(--line); font-size: 12px; }
  .pane.dropping { outline: 2px dashed var(--session-color); outline-offset: -2px; }
  .bar { display: flex; align-items: center; gap: 3px; padding: 4px; border-bottom: 1px solid var(--line); }
  .bar button { padding: 1px 6px; }
  .bar button.on { border-color: var(--session-color); color: var(--fg); }
  .backend { display: flex; align-items: center; gap: 6px; padding: 2px 8px; font-size: 11px; color: var(--fg-dim); border-bottom: 1px solid var(--line); }
  .backend.root { color: #ffb454; }
  .backend .link { border: 0; background: transparent; color: var(--session-color); padding: 0; text-decoration: underline; font-size: 11px; }
  .crumbs { flex: 1; min-width: 0; display: flex; align-items: center; overflow: hidden; white-space: nowrap; }
  .crumb { border: 0; background: transparent; padding: 1px 3px; color: var(--fg-dim); font-family: var(--font-mono); }
  .crumb:hover { color: var(--fg); }
  .crumbs .sep { color: var(--fg-dim); }
  .ask { display: flex; gap: 4px; align-items: center; padding: 4px; border-bottom: 1px solid var(--line); }
  .ask input { flex: 1; min-width: 0; }
  .ask.conflict { flex-wrap: wrap; }
  .ask.conflict span { flex: 1 1 100%; color: var(--fg-dim); }
  .ask.conflict button { flex: 0 0 auto; }
  .ask.conflict .pm { flex: 0 0 auto; }
  .err { color: var(--danger); padding: 4px 8px; border-bottom: 1px solid var(--line); }
  .list { flex: 1; overflow: auto; min-height: 0; outline: none; }
  .list.loading { opacity: .6; }
  .head, .row { display: grid; grid-template-columns: minmax(0, 1fr) 64px 118px 84px; gap: 6px; padding: 2px 8px; align-items: center; }
  .nomode .head, .nomode .row { grid-template-columns: minmax(0, 1fr) 64px 118px; }
  .head { position: sticky; top: 0; background: var(--bg-raised); border-bottom: 1px solid var(--line); color: var(--fg-dim); }
  .head button { border: 0; background: transparent; text-align: left; padding: 2px 0; color: inherit; }
  .row { cursor: default; user-select: none; }
  .row:hover { background: var(--bg); }
  .row.sel { background: var(--bg); box-shadow: inset 2px 0 0 var(--session-color); }
  .row.dir .name { color: var(--fg); font-weight: 600; }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .size { text-align: right; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .mtime, .mode { color: var(--fg-dim); white-space: nowrap; }
  .mono { font-family: var(--font-mono); }
  .empty { color: var(--fg-dim); padding: 12px; text-align: center; }
  .transfers { border-top: 1px solid var(--line); max-height: 200px; overflow: auto; padding: 4px 8px; }
  .thead { display: flex; align-items: center; gap: 8px; color: var(--fg-dim); margin-bottom: 2px; flex-wrap: wrap; }
  .thead span:first-child { margin-right: auto; }
  .thead .pm { display: flex; align-items: center; gap: 3px; }
  .thead .dim { color: var(--fg-dim); }
  .thead button { border: 0; background: transparent; color: var(--fg-dim); padding: 0; }
  .tr { display: grid; grid-template-columns: 16px minmax(0, 1fr) 80px 44px auto auto; gap: 6px; align-items: center; padding: 1px 0; }
  .tr.err { color: var(--danger); }
  .disc { border: 0; background: transparent; color: var(--fg-dim); padding: 0; cursor: pointer; }
  .tname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tcount { color: var(--fg-dim); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .tstate { color: var(--fg-dim); }
  .tr button:not(.disc) { padding: 0 6px; }
  progress { width: 80px; height: 8px; accent-color: var(--session-color); }
  .files { padding: 2px 0 4px 22px; color: var(--fg-dim); }
  .fr { display: grid; grid-template-columns: minmax(0, 1fr) 60px; gap: 6px; }
  .fr.ffail { color: var(--danger); }
  .frel { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fst { text-align: right; }
  .warn { color: #ffb454; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .drop { position: absolute; inset: 0; display: grid; place-content: center; background: #0008; color: var(--fg); pointer-events: none; }
</style>
