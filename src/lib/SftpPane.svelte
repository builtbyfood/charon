<script lang="ts">
  // File pane beside a terminal: SFTP for SSH tabs, the local disk for local
  // tabs — same UI, the backend picks the filesystem from the session id.
  import { onMount, onDestroy, untrack } from "svelte";
  import { open as pickOpen, save as pickSave } from "@tauri-apps/plugin-dialog";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import {
    sftpHome, sftpList, sftpStat, sftpMkdir, sftpRename, sftpDelete, sftpChmod, sftpGet, sftpPut, sftpCancel, onTransferProgress,
    type FsEntry, type FsListing, type TransferProgress,
  } from "./api";
  import ContextMenu, { type MenuItem } from "./ContextMenu.svelte";
  import { joinPath, crumbsOf, osc7ToLocal, humanSize, modeString } from "./paths";
  import { followTarget, pushTarget } from "./panecwd";
  import { isPermissionError, sftpSudoOpen, sftpSudoHasPassword, sftpSudoClear, type FsVia } from "./api";

  let { session, cwd, color, sshUser, shellUser, isIdle, onopen, onchdir, onclose }: {
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
  let sortKey = $state<"name" | "size" | "mtime" | "mode">("name");
  let sortDir = $state<1 | -1>(1);
  let selected = $state<string | null>(null);
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let dropping = $state(false);
  // Inline prompt row (window.prompt is not available in the webview).
  let ask = $state<{ kind: "rename" | "chmod" | "mkdir"; entry?: FsEntry; value: string } | null>(null);
  let askEl = $state<HTMLInputElement | null>(null);

  interface Transfer { id: string; name: string; dest: string; dir: "get" | "put"; done: number; total: number; state: TransferProgress["state"]; error?: string; mode?: number; retry: () => void }
  let transfers = $state<Transfer[]>([]);
  let preserveMode = $state(false);  // apply the source file's mode after upload
  const unlisteners: Array<() => void> = [];

  const isLocal = $derived(session === null);
  const shellBackend = $derived(via === "shell");
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

  onMount(async () => {
    unlisteners.push(await onTransferProgress(onProgress));
    // OS drag-drop lands on the webview; take it if it was dropped over this pane.
    unlisteners.push(await getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "leave") { dropping = false; return; }
      const inside = el ? overThisPane(p.position.x, p.position.y) : false;
      if (p.type === "over" || p.type === "enter") { dropping = inside; return; }
      dropping = false;
      if (p.type === "drop" && inside) for (const path of p.paths) upload(path);
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
    loading = true; error = ""; selected = null; ask = null;
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
      if (Date.now() < suppressUntil || !listing || target === listing.path) return;
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
      if (Date.now() < suppressUntil) return;
      if (isIdle && !isIdle()) return;
      if (isLocal ? osc7ToLocal(cwd ?? "") === target : cwd === target) return;
      suppressUntil = Date.now() + 500;
      onchdir(target);
    });
  });
  // Backend follows the shell user, un-latched: back to the login user → SFTP again (B2).
  $effect(() => {
    const su = shellUser;
    if (su && sshUser && su === sshUser && via !== "sftp") untrack(() => {
      const wasSudo = via === "sudo";
      via = "sftp"; askSudo = false;
      if (wasSudo && session) sftpSudoClear(session).catch(() => {});
      refresh();
    });
  });

  function activate(e: FsEntry) {
    if (e.kind === "dir" || (e.kind === "symlink" && e.target_kind === "dir")) go(e.path);
    else if (onopen) onopen(e.path, via);
    else error = "No editor yet: use Download… from the right-click menu.";
  }
  function sortBy(k: typeof sortKey) {
    if (sortKey === k) sortDir = sortDir === 1 ? -1 : 1; else { sortKey = k; sortDir = 1; }
  }

  // ---- root escalation
  let pendingPath: string | null = null;
  async function escalate() {
    if (!session) return;
    console.info(`[charon] sudo-sftp: escalating (EACCES as ${sshUser}, shell is ${shellUser})`);
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
        // sftp-server missing or sudo refused/timeout → shell backend (base64 over the pty).
        via = "shell"; askSudo = false; sudoReason = o.reason;
        error = `sudo SFTP: ${o.sudo} — ${o.reason}. Using the shell backend.`;
        await go(pendingPath ?? listing?.path ?? await homeFor());
      }
    } catch (e) {
      via = "shell"; askSudo = false; sudoReason = String(e);
      console.info(`[charon] sudo-sftp: call failed reason=${String(e)}`);
      error = `sudo SFTP failed (${String(e)}); using the shell backend`;
      await go(pendingPath ?? listing?.path ?? await homeFor());
    } finally {
      sudoBusy = false; pendingPath = null;
    }
  }
  const homeFor = () => sftpHome(session, via);

  // ---- transfers
  function onProgress(p: TransferProgress) {
    const t = transfers.find((x) => x.id === p.id);
    if (!t) return;
    if (p.state === "running") { t.done = p.done; t.total = p.total; }
    t.state = p.state; t.error = p.error ?? undefined;
  }
  function track(name: string, dest: string, dir: Transfer["dir"], run: (id: string) => Promise<void>, after?: (t: Transfer) => Promise<void>) {
    const id = crypto.randomUUID();
    const t: Transfer = { id, name, dest, dir, done: 0, total: 0, state: "running", retry: () => track(name, dest, dir, run, after) };
    transfers = [t, ...transfers].slice(0, 50);
    run(id)
      .then(async () => { if (after) await after(t); if (dir === "put") refresh(); })
      .catch((e) => { t.state = t.state === "cancelled" ? "cancelled" : "error"; t.error = String(e); });
  }
  async function download(e: FsEntry) {
    const local = await pickSave({ defaultPath: e.name, title: `Download ${e.name}` });
    if (typeof local !== "string") return;
    track(e.name, local, "get", (id) => sftpGet(session, id, e.path, local, via));
  }
  async function upload(localPath: string) {
    if (!listing) return;
    const name = localPath.split(/[\\/]/).pop() || localPath;
    const dest = joinPath(listing.path, name);
    // Same-destination guard: don't let two uploads race onto one path.
    if (transfers.some((t) => t.dir === "put" && t.dest === dest && t.state === "running")) {
      if (!confirm(`An upload to ${name} is already in progress. Start another to the same path?`)) return;
    }
    // Preserve mode: read the source file's mode now (Unix only; Windows has none).
    let mode: number | undefined;
    if (preserveMode) { try { mode = (await sftpStat(null, localPath)).mode || undefined; } catch { mode = undefined; } }
    const after = mode
      ? async (t: Transfer) => { try { await sftpChmod(session, dest, mode!, via); t.mode = mode; } catch { /* best effort */ } }
      : undefined;
    track(name, dest, "put", (id) => sftpPut(session, id, localPath, dest, via), after);
  }
  async function pickUpload() {
    const picked = await pickOpen({ multiple: true, title: "Upload" });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    for (const p of paths) upload(p);
  }
  const clearDone = () => (transfers = transfers.filter((t) => t.state === "running"));

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
  async function remove(e: FsEntry) {
    const isDir = e.kind === "dir";
    if (!confirm(`Delete ${isDir ? "directory" : "file"} ${e.name}?${isDir ? " (must be empty)" : ""}`)) return;
    try { await sftpDelete(session, e.path, isDir, via); await refresh(); } catch (err) { error = String(err); }
  }

  function onRowMenu(ev: MouseEvent, e: FsEntry) {
    ev.preventDefault(); ev.stopPropagation();
    selected = e.path;
    const isDir = e.kind === "dir" || (e.kind === "symlink" && e.target_kind === "dir");
    menu = { x: ev.clientX, y: ev.clientY, items: [
      { label: isDir ? "Open" : "Open in editor", action: () => activate(e), disabled: !isDir && !onopen },
      { label: "Download…", action: () => download(e), disabled: isDir },
      { separator: true, label: "" },
      { label: "Rename…", action: () => startAsk("rename", e) },
      { label: "Delete…", action: () => remove(e) },
      { label: "Copy path", action: () => writeText(e.path) },
      { label: "chmod…", action: () => startAsk("chmod", e), disabled: isLocal && e.mode === 0 },
    ] };
  }
  function onPaneMenu(ev: MouseEvent) {
    ev.preventDefault(); ev.stopPropagation();
    menu = { x: ev.clientX, y: ev.clientY, items: [
      { label: "New folder…", action: () => startAsk("mkdir") },
      { label: "Upload…", action: pickUpload },
      { label: "Refresh", action: refresh },
      { separator: true, label: "" },
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
        <button type="button" class="link" title="Back to plain SFTP as the login user" onclick={() => { via = "sftp"; if (session) sftpSudoClear(session); refresh(); }}>use SFTP</button>
      {:else if via === "shell"}
        <span title={sudoReason ? `sudo-sftp fell back: ${sudoReason}` : "browsing via the terminal (base64 over the pty)"}>shell as {shellUser ?? "?"}{#if differentUser} · SFTP is {sshUser}{/if}</span>
        <button type="button" class="link" title={sudoReason ? `Retry sudo-sftp (last: ${sudoReason})` : "Retry root over sudo-sftp (faster)"} onclick={escalate} disabled={sudoBusy}>{sudoBusy ? "…" : "try sudo"}</button>
        <button type="button" class="link" title="Back to plain SFTP as the login user" onclick={() => { via = "sftp"; refresh(); }}>use SFTP</button>
      {:else}
        sftp as {sshUser ?? "login user"}
        {#if differentUser}<button type="button" class="link" title="Browse as {shellUser} — sudo SFTP, or the terminal as a fallback" onclick={escalate} disabled={sudoBusy}>browse as {shellUser}</button>{:else}<span class="dim" title="The shell is the same user as SFTP; nothing to switch to">login user</span>{/if}
      {/if}
    </div>
  {/if}
  {#if askSudo}
    <form class="ask" onsubmit={(e) => { e.preventDefault(); openSudo(sudoPassword); }}>
      <span>sudo password for {shellUser ?? "root"}</span>
      <input bind:this={sudoPwEl} type="password" bind:value={sudoPassword} autocomplete="off" spellcheck="false"
        onkeydown={(e) => { if (e.key === "Escape") askSudo = false; }} />
      <button type="submit" disabled={sudoBusy || !sudoPassword}>{sudoBusy ? "…" : "Unlock"}</button>
      <button type="button" onclick={() => { askSudo = false; via = "shell"; refresh(); }} title="Skip sudo, browse via the terminal">use shell</button>
    </form>
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
  <div class="list" class:loading class:nomode={!hasModes}>
    <div class="head">
      <button type="button" onclick={() => sortBy("name")}>Name{sortKey === "name" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      <button type="button" onclick={() => sortBy("size")}>Size{sortKey === "size" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      <button type="button" onclick={() => sortBy("mtime")}>Modified{sortKey === "mtime" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>
      {#if hasModes}<button type="button" onclick={() => sortBy("mode")}>Perm{sortKey === "mode" ? (sortDir > 0 ? " ▲" : " ▼") : ""}</button>{/if}
    </div>
    {#each rows as e (e.path)}
      {@const isDir = e.kind === "dir" || (e.kind === "symlink" && e.target_kind === "dir")}
      <div class="row" class:sel={selected === e.path} class:dir={isDir} role="row" tabindex="0"
        onclick={() => (selected = e.path)} ondblclick={() => activate(e)}
        onkeydown={(k) => { if (k.key === "Enter") activate(k.currentTarget && e); }}
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
        <label class="pm" title="Apply the source file's permission bits to the uploaded file (Unix)"><input type="checkbox" bind:checked={preserveMode} /> preserve mode</label>
        <button type="button" onclick={clearDone}>clear finished</button>
      </div>
      {#each transfers as t (t.id)}
        <div class="tr" class:err={t.state === "error"}>
          <span class="tname" title={`${t.dest}${t.mode ? ` · mode ${t.mode.toString(8).padStart(3, "0")}` : ""}${t.error ? ` · ${t.error}` : ""}`}>{t.dir === "get" ? "↓" : "↑"} {t.name}</span>
          {#if t.state === "running"}
            <progress max={t.total || 1} value={t.done}></progress>
            <span class="tpct">{t.total ? Math.floor((t.done / t.total) * 100) + "%" : humanSize(t.done)}</span>
            <button type="button" onclick={() => sftpCancel(t.id)}>cancel</button>
          {:else}
            <span class="tstate">{t.state}{t.error ? ` — ${t.error}` : ""}</span>
            {#if t.state !== "done"}<button type="button" onclick={t.retry}>retry</button>{/if}
          {/if}
        </div>
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
  .err { color: var(--danger); padding: 4px 8px; border-bottom: 1px solid var(--line); }
  .list { flex: 1; overflow: auto; min-height: 0; }
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
  .transfers { border-top: 1px solid var(--line); max-height: 160px; overflow: auto; padding: 4px 8px; }
  .thead { display: flex; align-items: center; gap: 8px; color: var(--fg-dim); margin-bottom: 2px; }
  .thead span:first-child { margin-right: auto; }
  .thead .pm { display: flex; align-items: center; gap: 3px; }
  .thead button { border: 0; background: transparent; color: var(--fg-dim); padding: 0; }
  .tr { display: grid; grid-template-columns: minmax(0, 1fr) 90px 48px auto; gap: 6px; align-items: center; padding: 1px 0; }
  .tr.err { color: var(--danger); }
  .tname { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tstate { grid-column: 2 / 4; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tr button { padding: 0 6px; }
  progress { width: 90px; height: 8px; accent-color: var(--session-color); }
  .drop { position: absolute; inset: 0; display: grid; place-content: center; background: #0008; color: var(--fg); pointer-events: none; }
</style>
