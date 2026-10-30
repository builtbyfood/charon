<script lang="ts">
  import { onMount, tick } from "svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Terminal from "./lib/Terminal.svelte";
  import EditorPane from "./lib/EditorPane.svelte";
  import McpPanel from "./lib/McpPanel.svelte";
  import SettingsPanel from "./lib/SettingsPanel.svelte";
  import { diag } from "./lib/diag";
  import VaultUnlock from "./lib/VaultUnlock.svelte";
  import ContextMenu, { type MenuItem } from "./lib/ContextMenu.svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { appInfo, vaultStatus, getProfile, saveProfile, announceProfile, onProfileSaved, sessionWrite, mcpAnswer, onMcpApprove, onMcpOpenEditor,
    tabStash, tabTake, tabDropTarget, tabWindowOpen, tabSend, windowCloseSelf, tabsSaved, tabsSave, listConnections, onTabAdopt, editorOpenInMain, onEditorOpenReq, traceOutput,
    type AppInfo, type Node, type VaultStatus, type Profile, type FsVia, type McpApprove } from "./lib/api";
  import { prefsFromProfile, DEFAULTS, type Prefs, type SessionFont } from "./lib/prefs";
  import { applyTheme, resolveTheme, rememberTheme, type ThemePref } from "./lib/themes";
  import { dropIndex, reorder } from "./lib/tabdrag";
import { restorePlan, toSaved } from "./lib/opentabs";

  import type { OpenTarget, TabSnapshot } from "./lib/Terminal.svelte";
  /** Terminal tabs carry an OpenTarget; the single editor tab has none.
   *  `adopt` = the tab came from another window (see moveTab). */
  interface Tab { id: string; title: string; color?: string; font?: SessionFont; open?: OpenTarget; editor?: true; adopt?: TabSnapshot }

  // Every window runs this component. "main" has the sidebar, the editor and the
  // MCP prompts; tab windows ("tab-<token>", torn off the tab bar) hold terminals only.
  const appWindow = getCurrentWindow();
  const isMain = appWindow.label === "main";

  // One editor tab (Notepad++ style: documents are inner tabs), created on demand.
  let editorPane = $state<EditorPane | null>(null);
  let editorDirty = $state(false);
  const EDITOR_TAB = "editor";
  function ensureEditorTab(): void {
    if (!tabs.some((t) => t.id === EDITOR_TAB)) tabs.push({ id: EDITOR_TAB, title: "editor", editor: true });
    active = EDITOR_TAB;
  }
  /** From a terminal's file pane: `fs` is the session whose filesystem holds the file (null = local disk). */
  async function openInEditor(path: string, fs: string | null, origin: string | null, via: FsVia = "sftp",
    opts: { escalate?: boolean; interactive?: boolean } = {}) {
    ensureEditorTab();
    await tick();
    await editorPane?.openFile(path, fs, origin, via, opts);
  }
  /** User-initiated opens: a failure is already shown in the editor's status bar. */
  const openFromUi = (path: string, fs: string | null, origin: string | null, via: FsVia = "sftp") =>
    isMain
      ? openInEditor(path, fs, origin, via).catch((e) => console.info(`[charon] editor: open ${path} failed: ${String(e)}`))
      : editorOpenInMain({ path, fs, origin, via }); // the editor lives in the main window
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
    saveTimer = setTimeout(() => {
      if (profile) saveProfile($state.snapshot(profile)).then(() => announceProfile(appWindow.label)).catch((e) => console.warn("[charon] save profile", e));
    }, 400);
  }
  function setTimestamps(on: boolean) {
    if (!profile) return;
    profile.terminal.timestamps = on;
    prefs = prefsFromProfile(profile);
    persistProfile();
  }
  function setRenderer(r: "webgl" | "canvas" | "dom") {
    if (!profile) return;
    profile.appearance.renderer = r;
    prefs = prefsFromProfile(profile);
    persistProfile();
  }
  function setTrace(on: boolean) {
    if (!profile) return;
    profile.terminal.trace_output = on;
    persistProfile();
    traceOutput(on).then((v) => { diag.trace = v; console.info(`[charon] trace: output ${v ? "on" : "off"}`); }).catch(() => {});
  }
  // Theme: the profile's choice, or the OS's light/dark when it says "system".
  const darkMq = window.matchMedia("(prefers-color-scheme: dark)");
  function syncTheme() {
    const t = resolveTheme(profile?.appearance.theme, darkMq.matches);
    applyTheme(t);
    rememberTheme(t.id); // so the next start paints in it before the profile loads
  }
  darkMq.addEventListener("change", syncTheme);
  function setTheme(pref: ThemePref) {
    if (!profile) return;
    profile.appearance.theme = pref;
    syncTheme();
    persistProfile();
  }
  function setFont(family: string, size: number | undefined) {
    if (!profile) return;
    profile.appearance.font = family || "JetBrains Mono";
    if (size) profile.appearance.font_size = size;
    prefs = prefsFromProfile(profile);
    persistProfile();
  }
  function setRestoreTabs(on: boolean) {
    if (!profile) return;
    profile.ui.restore_tabs = on;
    persistProfile();
  }

  // Open tabs survive a restart: every window reports its tabs as they change
  // (once its own start-up tabs are in), the main window reopens them all at start.
  let tabsReady = $state(false);
  let tabsTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    if (!tabsReady) return;
    const w = JSON.parse(JSON.stringify(toSaved(tabs, active)));
    clearTimeout(tabsTimer);
    tabsTimer = setTimeout(() => tabsSave(w).catch((e) => console.warn("[charon] tabs: save failed", e)), 300);
  });
  async function restoreTabs() {
    try {
      const plan = restorePlan(await tabsSaved(), await listConnections());
      const opened: Tab[] = plan.tabs.map((t) => ({ id: crypto.randomUUID(), ...t }));
      if (opened.length) {
        tabs = [...opened, ...tabs];
        active = opened[plan.active]?.id ?? active;
        console.info(`[charon] tabs: reopened ${opened.length}`);
      }
    } catch (e) {
      console.warn("[charon] tabs: restore failed", e);
    } finally {
      tabsReady = true;
    }
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
  let settingsOpen = $state(false);
  // Docs sidebar view (Ctrl+Shift+D) + the command being typed in the active tab.
  let docsView = $state(false);
  let docsCommand = $state<string | null>(null);
  let docsSession = $state<string | null>(null);
  // MCP approval prompt (exec-approve tier) and the "always for this session" cache is backend-side.
  let approve = $state<McpApprove | null>(null);
  // Separate opt-in: let this run_command auto-answer the login user's sudo prompt.
  let allowSudo = $state(false);

  onMount(async () => {
    info = await appInfo();
    profile = await getProfile();
    prefs = prefsFromProfile(profile);
    syncTheme();
    if (profile.ui.sidebar_width >= 200) sidebarWidth = profile.ui.sidebar_width;
    // Off in the profile leaves CHARON_TRACE_OUTPUT=1 alone.
    traceOutput(profile.terminal.trace_output === true ? true : null).then((v) => (diag.trace = v)).catch(() => {});
    // Another window saved the profile (zoom, timestamps): pick it up so this
    // window's copy is not stale when it saves next.
    await onProfileSaved(async (from) => {
      if (from === appWindow.label) return;
      profile = await getProfile();
      prefs = prefsFromProfile(profile);
      syncTheme();
      diag.trace = await traceOutput(null).catch(() => diag.trace);
    });
    vault = await vaultStatus();
    // Tabs arriving from other windows; a new tab window starts with the one it was made for.
    await onTabAdopt((p) => adoptTab(p.token, p.x));
    if (!isMain) {
      await adoptTab(appWindow.label.slice("tab-".length), -1);
      tabsReady = true;
      return;
    }
    askMaster = vault.locked; // file vault in portable mode: ask once at startup
    // Reopen last run's tabs; with a locked vault, after the unlock prompt so
    // their connections can find their credentials.
    if (profile.ui.restore_tabs === false) tabsReady = true;
    else if (!askMaster) await restoreTabs();
    await onEditorOpenReq((r) => { openFromUi(r.path, r.fs, r.origin, r.via); getCurrentWindow().setFocus().catch(() => {}); });
    // MCP: exec-approval prompts, and open-in-editor requests from an agent.
    await onMcpApprove((p) => { approve = p; allowSudo = false; });
    await onMcpOpenEditor(async (p) => {
      try {
        // Agent opens never escalate on their own (no silent root-shell fallback) and
        // never prompt for a password; as_root arrives only after the backend's tier +
        // sudo opt-in gate, and uses a live sudo channel or the stored password.
        const opts = { escalate: false, interactive: false };
        if (p.path) await openInEditor(p.path, null, null, "sftp", opts);
        else if (p.session && p.remote_path) await openInEditor(p.remote_path, p.session, p.session, p.as_root ? "sudo" : "sftp", opts);
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
      for (const p of e.payload.paths) openFromUi(p, null, null);
    });
  });

  function openNode(n: Node, effColor?: string, font?: SessionFont) {
    const id = crypto.randomUUID();
    const kind = n.kind === "local" || n.kind === "serial" ? n.kind : "ssh";
    tabs.push({ id, title: n.name, color: effColor ?? n.settings.color, font, open: { kind, nodeId: n.id } });
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
    if (isMain && e.ctrlKey && e.shiftKey && (e.code === "KeyD")) { e.preventDefault(); docsView = !docsView; }
  }
  function closeTab(id: string) {
    if (id === EDITOR_TAB && editorPane?.hasUnsaved() && !confirm("The editor has unsaved changes. Close anyway?")) return;
    dropTab(id);
  }
  function dropTab(id: string) {
    tabs = tabs.filter((t) => t.id !== id);
    if (active === id) active = tabs.at(-1)?.id ?? null;
    // A tab window with no tabs left has nothing to show.
    if (!isMain && tabs.length === 0) windowCloseSelf().catch(() => {});
  }

  // ---- tab drag: reorder in the bar; drop outside the window → its own window;
  // drop on another Charon window → joins that window's tabs. Pointer events, not
  // HTML5 drag-and-drop: the native file-drop handler (files from Explorer) turns
  // that off inside the webview on Windows.
  let tabsEl = $state<HTMLDivElement | null>(null);
  let drag = $state<{ id: string; x0: number; y0: number; x: number; y: number; moved: boolean; out: boolean } | null>(null);
  let swallowClick = false;
  let moveError = $state("");
  let tabMenu = $state<{ x: number; y: number; id: string } | null>(null);
  const canMove = (t: Tab) => !!t.open;
  function tabRects(): { id: string; left: number; right: number }[] {
    return [...(tabsEl?.querySelectorAll<HTMLElement>("[data-tab]") ?? [])].map((el) => {
      const r = el.getBoundingClientRect();
      return { id: el.dataset.tab!, left: r.left, right: r.right };
    });
  }
  function tabDown(e: PointerEvent, id: string) {
    if (e.button !== 0) return;
    drag = { id, x0: e.clientX, y0: e.clientY, x: e.clientX, y: e.clientY, moved: false, out: false };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function tabMove(e: PointerEvent) {
    if (!drag) return;
    drag.x = e.clientX; drag.y = e.clientY;
    if (!drag.moved && Math.hypot(e.clientX - drag.x0, e.clientY - drag.y0) < 6) return;
    drag.moved = true;
    const bar = tabsEl?.getBoundingClientRect();
    const outside = e.clientX < 0 || e.clientY < 0 || e.clientX > window.innerWidth || e.clientY > window.innerHeight;
    const nearBar = !!bar && e.clientY >= bar.top - 30 && e.clientY <= bar.bottom + 30 && !outside;
    const t = tabs.find((x) => x.id === drag!.id);
    drag.out = !nearBar && !!t && canMove(t);
    if (nearBar) {
      const to = dropIndex(tabRects().filter((r) => r.id !== drag!.id), e.clientX);
      const next = reorder(tabs, drag.id, to);
      if (next !== tabs) tabs = next;
    }
  }
  async function tabUp() {
    if (!drag) return;
    const d = drag; drag = null;
    if (!d.moved) return;
    swallowClick = true; setTimeout(() => (swallowClick = false), 0);
    const outside = d.x < 0 || d.y < 0 || d.x > window.innerWidth || d.y > window.innerHeight;
    // Let go inside this window but away from the bar: cancel (the order change stays).
    if (d.out && outside) await moveTab(d.id, "drop");
  }

  /** Move a terminal tab to another window: "drop" = wherever the pointer is (another
   *  window, else a new one there), "new" = a new window, "main" = the main window. */
  async function moveTab(id: string, where: "drop" | "new" | "main") {
    const t = tabs.find((x) => x.id === id);
    const term = terms[id];
    if (!t || !t.open || !term) return;
    moveError = "";
    let snap: TabSnapshot;
    try { snap = await term.detach(); }
    catch (e) { moveError = String(e instanceof Error ? e.message : e); setTimeout(() => (moveError = ""), 4000); return; }
    const token = await tabStash(JSON.stringify({ id: t.id, title: t.title, color: t.color, font: t.font, open: t.open, adopt: snap } satisfies Tab));
    try {
      const tgt = await tabDropTarget(appWindow.label);
      const target = where === "main" ? "main" : where === "drop" ? tgt.window : null;
      if (target) await tabSend(target, token, where === "drop" ? tgt.x : -1, tgt.y);
      else await tabWindowOpen(token, tgt.x, tgt.y, window.innerWidth, window.innerHeight);
      console.info(`[charon] tab: moved ${id} to ${target ?? "a new window"}`);
    } catch (e) {
      console.warn("[charon] tab: move failed", e);
      await tabTake(token).catch(() => {});
      term.undetach();
      moveError = `could not move the tab: ${String(e)}`; setTimeout(() => (moveError = ""), 4000);
      return;
    }
    dropTab(id);
  }

  /** A tab handed to this window: take it from the stash and put it where it was
   *  dropped (`x` = physical screen x; negative = at the end). */
  async function adoptTab(token: string, x: number) {
    let t: Tab;
    try { t = JSON.parse(await tabTake(token)) as Tab; }
    catch (e) { console.warn("[charon] tab: adopt failed", e); if (!isMain && tabs.length === 0) windowCloseSelf().catch(() => {}); return; }
    let at = tabs.length;
    if (x >= 0) {
      const pos = await appWindow.innerPosition().catch(() => null);
      const scale = await appWindow.scaleFactor().catch(() => window.devicePixelRatio || 1);
      if (pos) at = dropIndex(tabRects(), (x - pos.x) / scale);
    }
    tabs.splice(at, 0, t);
    active = t.id;
    if (!isMain) appWindow.setTitle(`${t.title} — Charon`).catch(() => {});
  }
  function tabMenuItems(id: string): MenuItem[] {
    const t = tabs.find((x) => x.id === id);
    const movable = !!t && canMove(t);
    return [
      { label: "Move to new window", action: () => moveTab(id, "new"), disabled: !movable || (!isMain && tabs.length === 1) },
      ...(isMain ? [] : [{ label: "Move to main window", action: () => moveTab(id, "main"), disabled: !movable }]),
      { separator: true, label: "" },
      { label: "Close tab", action: () => closeTab(id) },
    ];
  }
</script>

<svelte:window onkeydown={onWindowKey} />
<div class="shell" class:detached={!isMain} style:--sidebar="{sidebarWidth}px">
  {#if isMain}
    <Sidebar onopen={openNode} onquick={quickConnect} onlocal={openLocal} oneditor={newInEditor} onmcp={() => (mcpOpen = true)} onsettings={() => (settingsOpen = true)}
      bind:docsView {docsCommand} {docsSession} {vault} onunlock={() => (askMaster = true)} />
    <div class="splitter" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" onpointerdown={startDrag}></div>
  {/if}
  <main>
    <div class="tabs" role="tablist" bind:this={tabsEl}>
      {#each tabs as t (t.id)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="tab" class:active={active === t.id} class:dragging={drag?.moved && drag.id === t.id} class:tearing={drag?.out && drag.id === t.id}
          data-tab={t.id} style:--tab-color={t.color ?? "var(--accent)"}
          oncontextmenu={(e) => { e.preventDefault(); tabMenu = { x: e.clientX, y: e.clientY, id: t.id }; }}>
          <button type="button" class="tab-main" role="tab" aria-selected={active === t.id}
            title={canMove(t) ? "Drag to reorder; drag out of the window to open it in its own window" : undefined}
            onpointerdown={(e) => tabDown(e, t.id)} onpointermove={tabMove} onpointerup={tabUp} onpointercancel={() => (drag = null)}
            onclick={() => { if (!swallowClick) active = t.id; }}>
            <span class="dot"></span>{t.title}{#if t.editor && editorDirty}<span class="unsaved">•</span>{/if}{#if t.open?.admin}<span class="badge">admin</span>{/if}
          </button>
          <button type="button" class="x" aria-label="Close tab" onclick={() => closeTab(t.id)}>×</button>
        </div>
      {/each}
      {#if moveError}<span class="move-error">{moveError}</span>{/if}
      {#if activeTerm}
        <button type="button" class="files-toggle" class:on={!!filesOpen[activeTerm]} title="File pane for this tab (Ctrl+Shift+E)"
          onclick={() => terms[activeTerm!]?.toggleFiles()}>Files</button>
      {/if}
    </div>
    <div class="panes">
      {#if tabs.length === 0}
        <div class="empty">
          <p>Pick a connection on the left, or type <code>user@host</code> in the quick-connect box.</p>
          {#if !isMain}<p>Drag a tab here, or move one from another window.</p>{/if}
          {#if info}<p class="meta">{info.portable ? "Portable mode" : "Installed"} · data in {info.data_dir}</p>{/if}
        </div>
      {/if}
      {#each tabs as t (t.id)}
        <div class="pane" hidden={active !== t.id}>
          {#if t.editor}
            <EditorPane bind:this={editorPane} onsend={sendToSession} ondirty={(d) => (editorDirty = d)} />
          {:else if t.open}
            <Terminal bind:this={terms[t.id]} open={t.open} adopt={t.adopt} color={t.color} font={t.font} {prefs} active={active === t.id} os={info?.os} osUser={info?.user} onfontsize={setFontSize} ontimestamps={setTimestamps} onclosed={() => {}} onabort={() => closeTab(t.id)}
              onopenfile={openFromUi} onfiles={(o) => (filesOpen[t.id] = o)}
              oncommand={(cmd, sess) => { if (isMain && active === t.id) { docsCommand = cmd; docsSession = sess; } }} />
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
{#if tabMenu}
  <ContextMenu x={tabMenu.x} y={tabMenu.y} items={tabMenuItems(tabMenu.id)} onclose={() => (tabMenu = null)} />
{/if}
{#if drag?.out}
  <div class="tear-hint">Release outside the window to open the tab in its own window, or over another Charon window to move it there</div>
{/if}
{#if askMaster && vault}
  <VaultUnlock status={vault} ondone={(s) => { if (s) vault = s; askMaster = false; if (!tabsReady && profile?.ui.restore_tabs !== false) restoreTabs(); }} />
{/if}
{#if settingsOpen && profile}
  <SettingsPanel theme={(profile.appearance.theme || "charon-dark") as ThemePref} font={profile.appearance.font} fontSize={profile.appearance.font_size}
    renderer={prefs.renderer} trace={profile.terminal.trace_output === true} restoreTabs={profile.ui.restore_tabs !== false}
    ontheme={setTheme} onfont={setFont} onrenderer={setRenderer} ontrace={setTrace} onrestoretabs={setRestoreTabs} onclose={() => (settingsOpen = false)} />
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
      {#if approve.tool === "run_command"}
        <label class="sudo-opt" title="Let this command auto-answer a '[sudo] password for {'<you>'}:' prompt with the saved password. The password is never shown or logged.">
          <input type="checkbox" bind:checked={allowSudo} /> allow sudo password
        </label>
      {/if}
      <div class="row">
        <button type="button" onclick={() => { mcpAnswer(approve!.req_id, { decision: "deny" }); approve = null; }}>Deny</button>
        <button type="button" onclick={() => { mcpAnswer(approve!.req_id, { decision: "always", allow_sudo: allowSudo }); approve = null; }}>Always for this session</button>
        <button type="button" class="primary" onclick={() => { mcpAnswer(approve!.req_id, { decision: "approve", allow_sudo: allowSudo }); approve = null; }}>Approve</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .shell { display: grid; grid-template-columns: var(--sidebar) 4px 1fr; height: 100%; }
  .shell.detached { grid-template-columns: 1fr; }
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
  .tab.dragging { opacity: .7; outline: 1px dashed var(--line); }
  .tab.tearing { opacity: .4; }
  .tab .tab-main { touch-action: none; }
  .move-error { color: var(--danger); font-size: 12px; padding: 0 8px; white-space: nowrap; }
  .tear-hint { position: fixed; left: 50%; bottom: 40px; transform: translateX(-50%); background: var(--bg-raised); border: 1px solid var(--accent); border-radius: 4px; padding: 6px 12px; font-size: 12px; z-index: 30; pointer-events: none; }
  .tab .x:hover { opacity: 1; color: var(--danger); }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--tab-color); }
  .unsaved { color: var(--warn); margin-left: -2px; }
  .badge { font-size: 10px; font-weight: 600; letter-spacing: .03em; color: #fff; background: var(--danger); border-radius: 3px; padding: 0 4px; }
  .panes { flex: 1; position: relative; min-height: 0; }
  .pane { position: absolute; inset: 0; }
  .empty { height: 100%; display: grid; place-content: center; text-align: center; color: var(--fg-dim); }
  .meta { font-size: 12px; }
  .appbar { display: flex; align-items: center; gap: 8px; font-size: 11px; color: var(--fg-dim); padding: 2px 8px; border-top: 1px solid var(--line); background: var(--bg-raised); }
  .appbar .mode { text-transform: uppercase; letter-spacing: .05em; font-weight: 600; }
  .appbar .mode.portable { color: var(--warn); }
  .appbar .mono { font-family: var(--font-mono); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .appbar .ver { margin-left: auto; }
  code { font-family: var(--font-mono); color: var(--fg); }
  .backdrop { position: fixed; inset: 0; background: var(--overlay); display: grid; place-content: center; z-index: 20; }
  .approve { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 460px; max-width: 92vw; display: flex; flex-direction: column; gap: 8px; }
  .approve h3 { margin: 0; font-size: 14px; }
  .approve .who { margin: 0; color: var(--fg-dim); font-size: 12px; }
  .approve .cmd { margin: 0; background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 4px; padding: 8px; font-family: var(--font-mono); font-size: 12px; white-space: pre-wrap; word-break: break-all; max-height: 160px; overflow: auto; }
  .approve .row { display: flex; justify-content: flex-end; gap: 6px; }
  .approve .sudo-opt { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--warn); }
  .approve .mono { font-family: var(--font-mono); }
  .approve .primary { border-color: var(--accent); }
</style>