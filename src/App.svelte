<script lang="ts">
  import { onMount, tick } from "svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Terminal from "./lib/Terminal.svelte";
  import EditorPane from "./lib/EditorPane.svelte";
  import McpPanel from "./lib/McpPanel.svelte";
  import VaultUnlock from "./lib/VaultUnlock.svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { appInfo, vaultStatus, getProfile, saveProfile, sessionWrite, mcpAnswer, onMcpApprove, onMcpOpenEditor, type AppInfo, type Node, type VaultStatus, type Profile, type FsVia, type McpApprove } from "./lib/api";
  import { prefsFromProfile, DEFAULTS, type Prefs } from "./lib/prefs";

  import type { OpenTarget } from "./lib/Terminal.svelte";
  /** Terminal tabs carry an OpenTarget; the single editor tab has none. */
  interface Tab { id: string; title: string; color?: string; open?: OpenTarget; editor?: true }

  // One editor tab (Notepad++ style: documents are inner tabs), created on demand.
  let editorPane = $state<EditorPane | null>(null);
  let editorDirty = $state(false);
  const EDITOR_TAB = "editor";
  function ensureEditorTab(): void {
    if (!tabs.some((t) => t.id === EDITOR_TAB)) tabs.push({ id: EDITOR_TAB, title: "editor", editor: true });
    active = EDITOR_TAB;
  }
  /** From a terminal's file pane: `fs` is the session whose filesystem holds the file (null = local disk). */
  async function openInEditor(path: string, fs: string | null, origin: string | null, via: FsVia = "sftp") {
    ensureEditorTab();
    await tick();
    await editorPane?.openFile(path, fs, origin, via);
  }
  // Terminal instances by tab, for the tab-bar Files button.
  let terms = $state<Record<string, Terminal | undefined>>({});
  let filesOpen = $state<Record<string, boolean>>({});
  function newInEditor() { ensureEditorTab(); tick().then(() => editorPane?.openUntitled()); }
  function sendToSession(text: string, session: string) { sessionWrite(session, text); }

  let info = $state<AppInfo | null>(null);
  let tabs = $state<Tab[]>([]);
  let active = $state<string | null>(null);
  const activeTerm = $derived(active && tabs.find((t) => t.id === active && t.open && t.open.kind !== "serial") ? active : null);
  let vault = $state<VaultStatus | null>(null);

  // The profile is owned here; Terminal/Sidebar get derived prefs and report
  // changes (zoom, splitter) back so they land in data/profiles/default.toml.
  let profile = $state<Profile | null>(null);
  let prefs = $state<Prefs>(DEFAULTS);
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  function persistProfile() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => { if (profile) saveProfile($state.snapshot(profile)).catch((e) => console.warn("[charon] save profile", e)); }, 400);
  }
  function setFontSize(size: number) {
    if (!profile) return;
    profile.appearance.font_size = size;
    prefs = prefsFromProfile(profile);
    persistProfile();
  }

  // Sidebar width: drag the splitter; min 200, max half the window.
  let sidebarWidth = $state(260);
  function startDrag(e: PointerEvent) {
    const startX = e.clientX, startW = sidebarWidth;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      sidebarWidth = Math.max(200, Math.min(window.innerWidth / 2, startW + ev.clientX - startX));
    };
    const up = () => {
      el.removeEventListener("pointermove", move); el.removeEventListener("pointerup", up);
      if (profile) { profile.ui.sidebar_width = Math.round(sidebarWidth); persistProfile(); }
    };
    el.addEventListener("pointermove", move); el.addEventListener("pointerup", up);
  }
  let askMaster = $state(false);
  let mcpOpen = $state(false);
  // Docs sidebar view (Ctrl+Shift+D) + the command being typed in the active tab.
  let docsView = $state(false);
  let docsCommand = $state<string | null>(null);
  let docsSession = $state<string | null>(null);
  // MCP approval prompt (exec-approve tier) and the "always for this session" cache is backend-side.
  let approve = $state<McpApprove | null>(null);

  onMount(async () => {
    info = await appInfo();
    profile = await getProfile();
    prefs = prefsFromProfile(profile);
    if (profile.ui.sidebar_width >= 200) sidebarWidth = profile.ui.sidebar_width;
    vault = await vaultStatus();
    askMaster = vault.locked; // file vault in portable mode: ask once at startup
    // MCP: exec-approval prompts, and open-in-editor requests from an agent.
    await onMcpApprove((p) => { approve = p; });
    await onMcpOpenEditor(async (p) => {
      try {
        if (p.path) await openInEditor(p.path, null, null, "sftp");
        else if (p.session && p.remote_path) await openInEditor(p.remote_path, p.session, p.session, "sftp");
        mcpAnswer(p.req_id, { ok: true });
      } catch (e) { mcpAnswer(p.req_id, { ok: false, error: String(e) }); }
    });
    // Files dropped on the window open in the editor — unless they land on a file
    // pane, which uploads them itself (it marks its element data-drop="files").
    await getCurrentWebview().onDragDropEvent((e) => {
      if (e.payload.type !== "drop") return;
      const s = window.devicePixelRatio || 1;
      const target = document.elementFromPoint(e.payload.position.x / s, e.payload.position.y / s);
      if (target?.closest("[data-drop='files']")) return;
      for (const p of e.payload.paths) openInEditor(p, null, null);
    });
  });

  function openNode(n: Node, effColor?: string) {
    const id = crypto.randomUUID();
    const kind = n.kind === "local" || n.kind === "serial" ? n.kind : "ssh";
    tabs.push({ id, title: n.name, color: effColor ?? n.settings.color, open: { kind, nodeId: n.id } });
    active = id;
  }
  function openLocal(admin = false) {
    const id = crypto.randomUUID();
    tabs.push({ id, title: "local", open: { kind: "local", admin } });
    active = id;
  }
  function quickConnect(target: string) {
    const m = target.match(/^(?:([^@]+)@)?([^:]+)(?::(\d+))?$/);
    if (!m) return;
    const id = crypto.randomUUID();
    tabs.push({ id, title: target, open: { kind: "ssh", user: m[1], host: m[2], port: m[3] ? +m[3] : undefined } });
    active = id;
  }
  function onWindowKey(e: KeyboardEvent) {
    if (e.ctrlKey && e.shiftKey && (e.code === "KeyD")) { e.preventDefault(); docsView = !docsView; }
  }
  function closeTab(id: string) {
    if (id === EDITOR_TAB && editorPane?.hasUnsaved() && !confirm("The editor has unsaved changes. Close anyway?")) return;
    tabs = tabs.filter((t) => t.id !== id);
    if (active === id) active = tabs.at(-1)?.id ?? null;
  }
</script>

<svelte:window onkeydown={onWindowKey} />
<div class="shell" style:--sidebar="{sidebarWidth}px">
  <Sidebar onopen={openNode} onquick={quickConnect} onlocal={openLocal} oneditor={newInEditor} onmcp={() => (mcpOpen = true)}
    bind:docsView {docsCommand} {docsSession} {vault} onunlock={() => (askMaster = true)} />
  <div class="splitter" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" onpointerdown={startDrag}></div>
  <main>
    <div class="tabs" role="tablist">
      {#each tabs as t (t.id)}
        <div class="tab" class:active={active === t.id} style:--tab-color={t.color ?? "var(--accent)"}>
          <button type="button" class="tab-main" role="tab" aria-selected={active === t.id} onclick={() => (active = t.id)}>
            <span class="dot"></span>{t.title}{#if t.editor && editorDirty}<span class="unsaved">•</span>{/if}{#if t.open?.admin}<span class="badge">admin</span>{/if}
          </button>
          <button type="button" class="x" aria-label="Close tab" onclick={() => closeTab(t.id)}>×</button>
        </div>
      {/each}
      {#if activeTerm}
        <button type="button" class="files-toggle" class:on={!!filesOpen[activeTerm]} title="File pane for this tab (Ctrl+Shift+E)"
          onclick={() => terms[activeTerm!]?.toggleFiles()}>Files</button>
      {/if}
    </div>
    <div class="panes">
      {#if tabs.length === 0}
        <div class="empty">
          <p>Pick a connection on the left, or type <code>user@host</code> in the quick-connect box.</p>
          {#if info}<p class="meta">{info.portable ? "Portable mode" : "Installed"} · data in {info.data_dir}</p>{/if}
        </div>
      {/if}
      {#each tabs as t (t.id)}
        <div class="pane" hidden={active !== t.id}>
          {#if t.editor}
            <EditorPane bind:this={editorPane} onsend={sendToSession} ondirty={(d) => (editorDirty = d)} />
          {:else if t.open}
            <Terminal bind:this={terms[t.id]} open={t.open} color={t.color} {prefs} onfontsize={setFontSize} onclosed={() => {}} onabort={() => closeTab(t.id)}
              onopenfile={openInEditor} onfiles={(o) => (filesOpen[t.id] = o)}
              oncommand={(cmd, sess) => { if (active === t.id) { docsCommand = cmd; docsSession = sess; } }} />
          {/if}
        </div>
      {/each}
    </div>
    {#if info}
      <div class="appbar" title="Where charon.db, profiles/ and the vault live">
        <span class="mode" class:portable={info.portable}>{info.portable ? "portable" : "installed"}</span>
        <span class="mono">{info.data_dir}</span>
        <span class="ver">v{info.version}</span>
      </div>
    {/if}
  </main>
</div>
{#if askMaster && vault}
  <VaultUnlock status={vault} ondone={(s) => { if (s) vault = s; askMaster = false; }} />
{/if}
{#if mcpOpen}
  <McpPanel onclose={() => (mcpOpen = false)} />
{/if}
{#if approve}
  <div class="backdrop" role="presentation">
    <div class="approve">
      <h3>MCP wants to run a command</h3>
      <p class="who">tool <span class="mono">{approve.tool}</span> · session <span class="mono">{approve.session.slice(0, 8)}</span></p>
      <pre class="cmd">{approve.command}</pre>
      <div class="row">
        <button type="button" onclick={() => { mcpAnswer(approve!.req_id, { decision: "deny" }); approve = null; }}>Deny</button>
        <button type="button" onclick={() => { mcpAnswer(approve!.req_id, { decision: "always" }); approve = null; }}>Always for this session</button>
        <button type="button" class="primary" onclick={() => { mcpAnswer(approve!.req_id, { decision: "approve" }); approve = null; }}>Approve</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .shell { display: grid; grid-template-columns: var(--sidebar) 4px 1fr; height: 100%; }
  .splitter { cursor: col-resize; background: var(--line); }
  .splitter:hover, .splitter:active { background: var(--accent); }
  main { display: flex; flex-direction: column; min-width: 0; }
  .tabs { display: flex; gap: 2px; padding: 4px 6px 0; background: var(--bg-raised); border-bottom: 1px solid var(--line); overflow-x: auto; align-items: center; }
  .files-toggle { margin-left: auto; margin-bottom: 4px; padding: 3px 10px; flex: none; }
  .files-toggle.on { border-color: var(--accent); color: var(--fg); }
  .tab { display: flex; align-items: center; border-radius: 4px 4px 0 0; color: var(--fg-dim); }
  .tab.active { background: var(--bg); color: var(--fg); box-shadow: inset 0 2px 0 var(--tab-color); }
  .tab button { border: 0; background: transparent; color: inherit; display: flex; align-items: center; gap: 6px; padding: 5px 8px; }
  .tab .x { padding: 5px 6px 5px 0; opacity: .5; }
  .tab .x:hover { opacity: 1; color: var(--danger); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--tab-color); }
  .unsaved { color: #ffb454; margin-left: -2px; }
  .badge { font-size: 10px; font-weight: 600; letter-spacing: .03em; color: #fff; background: var(--danger); border-radius: 3px; padding: 0 4px; }
  .panes { flex: 1; position: relative; min-height: 0; }
  .pane { position: absolute; inset: 0; }
  .empty { height: 100%; display: grid; place-content: center; text-align: center; color: var(--fg-dim); }
  .meta { font-size: 12px; }
  .appbar { display: flex; align-items: center; gap: 8px; font-size: 11px; color: var(--fg-dim); padding: 2px 8px; border-top: 1px solid var(--line); background: var(--bg-raised); }
  .appbar .mode { text-transform: uppercase; letter-spacing: .05em; font-weight: 600; }
  .appbar .mode.portable { color: #ffb454; }
  .appbar .mono { font-family: var(--font-mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .appbar .ver { margin-left: auto; }
  code { font-family: var(--font-mono); color: var(--fg); }
  .backdrop { position: fixed; inset: 0; background: #0009; display: grid; place-content: center; z-index: 20; }
  .approve { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 460px; max-width: 92vw; display: flex; flex-direction: column; gap: 8px; }
  .approve h3 { margin: 0; font-size: 14px; }
  .approve .who { margin: 0; color: var(--fg-dim); font-size: 12px; }
  .approve .cmd { margin: 0; background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 4px; padding: 8px; font-family: var(--font-mono); font-size: 12px; white-space: pre-wrap; word-break: break-all; max-height: 160px; overflow: auto; }
  .approve .row { display: flex; justify-content: flex-end; gap: 6px; }
  .approve .mono { font-family: var(--font-mono); }
  .approve .primary { border-color: var(--accent); }
</style>