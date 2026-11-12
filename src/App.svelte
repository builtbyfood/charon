<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import Sidebar from "./lib/Sidebar.svelte";
  import Terminal from "./lib/Terminal.svelte";
  import EditorPane from "./lib/EditorPane.svelte";
  import McpPanel from "./lib/McpPanel.svelte";
  import SettingsPanel from "./lib/SettingsPanel.svelte";
  import { diag } from "./lib/diag";
  import VaultUnlock from "./lib/VaultUnlock.svelte";
  import ContextMenu, { type MenuItem } from "./lib/ContextMenu.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { appInfo, vaultStatus, getProfile, saveProfile, announceProfile, onProfileSaved, sessionWrite, mcpAnswer, onMcpApprove, onMcpOpenEditor,
    tabStash, tabTake, tabDropTarget, tabWindowOpen, tabSend, windowCloseSelf, tabsSaved, tabsSave, listConnections, onTabAdopt, editorOpenInMain, onEditorOpenReq, traceOutput,
    type AppInfo, type LinuxRepaint, type Node, type VaultStatus, type Profile, type FsVia, type McpApprove } from "./lib/api";
  import { prefsFromProfile, DEFAULTS, type Prefs, type SessionFont } from "./lib/prefs";
  import { applyTheme, resolveTheme, rememberTheme, type ThemePref } from "./lib/themes";
  import { dropIndex, reorder } from "./lib/tabdrag";
  import type { ColLayout } from "./lib/filecols";
  import { addDropTarget } from "./lib/filedrop";
  import { restorePlan, toSaved } from "./lib/opentabs";
  import { startTicker } from "./lib/ticker.svelte";
  import { broadcastTargets, fanOut, fanSudo, idleExpired } from "./lib/broadcast";
  import { broadcastCount, filesAllTarget, gridLayout, syncSplitFiles } from "./lib/splitview";
  import { sudoSendPassword, broadcastRoles, bcPublish, onBcState, bcHello, onBcHello, bcInput, onBcInput, focusWindow, type BcInput } from "./lib/api";

  import type { OpenTarget, TabSnapshot } from "./lib/Terminal.svelte";
  /** Terminal tabs carry an OpenTarget; the single editor tab has none.
   *  `adopt` = the tab came from another window (see moveTab).
   *  `popped` = a placeholder in the main window for a tab shown in a Split pop-out
   *  (that window's label). In a stashed tab on its way: `filesBefore` = its Files
   *  state from before Split, `popout` = it opens a Split pop-out window, carrying
   *  the `bundle` of the other tabs' stash tokens. */
  interface Tab { id: string; title: string; color?: string; font?: SessionFont; open?: OpenTarget; editor?: true; adopt?: TabSnapshot;
    popped?: string; filesBefore?: boolean; popout?: true; bundle?: string[] }

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

  // ---- MultiExec (broadcast input). Tick tabs (or "Broadcast to folder"), then
  // Ctrl+Shift+B: what is typed or pasted in the source tab also goes to every
  // ticked tab's session. The source is the terminal that last had the keyboard,
  // in any window: the state is shared between windows (`bc:state`), and each
  // window repeats the source's input on its own ticked tabs (`bc:input`).
  let bc = $state<{ on: boolean; selected: Set<string>; source: string | null; rev: number }>({ on: false, selected: new Set(), source: null, rev: 0 });
  let bcHint = $state("");
  const isTermTab = (t: Tab) => !!t.open && !t.editor;
  const bcCount = $derived(broadcastCount(bc.selected, bc.source));
  const broadcasting = $derived(bc.on && !!bc.source && bcCount > 1);
  /** Change the shared state here and tell the other windows (newest `rev` wins). */
  function publish(p: { on?: boolean; selected?: Set<string>; source?: string | null }) {
    bc = { on: p.on ?? bc.on, selected: p.selected ?? bc.selected, source: p.source !== undefined ? p.source : bc.source, rev: Math.max(Date.now(), bc.rev + 1) };
    bcPublish({ on: bc.on, selected: [...bc.selected], source: bc.source, rev: bc.rev, from: appWindow.label }).catch(() => {});
  }
  /** A terminal got the keyboard: it is the broadcast source now (the amber border follows it). */
  function claimSource(id: string) {
    if (bc.source !== id && tabs.some((t) => t.id === id && isTermTab(t))) publish({ source: id });
  }
  function toggleSelected(id: string, on: boolean) {
    const s = new Set(bc.selected);
    if (on) s.add(id); else s.delete(id);
    publish({ selected: s, on: bc.on && broadcastCount(s, bc.source) > 1 });
  }
  function flashHint(msg: string) { bcHint = msg; setTimeout(() => { if (bcHint === msg) bcHint = ""; }, 4000); }
  function setBroadcast(on: boolean) {
    if (!on) { publish({ on: false }); console.info("[charon] broadcast: off"); return; }
    // Pressed here: this window's terminal is the source.
    const here = active && tabs.some((t) => t.id === active && isTermTab(t)) ? active : null;
    const source = here ?? bc.source;
    if (!source) { flashHint("Broadcast: switch to a terminal tab first"); return; }
    if (broadcastCount(bc.selected, source) < 2) { flashHint("Broadcast: tick the tabs to send to first (checkbox on each tab, or Broadcast to folder)"); return; }
    publish({ on: true, source });
    console.info(`[charon] broadcast: on → ${broadcastCount(bc.selected, source)} sessions`);
    if (here) terms[here]?.focusTerm();
  }
  type FanEvent = { kind: "data"; data: string } | { kind: "paste"; text: string } | { kind: "sudo" };
  /** Repeat the source's input on this window's ticked tabs. */
  function fanLocal(from: string, e: FanEvent) {
    const targets = broadcastTargets(tabs.filter(isTermTab).map((t) => ({ id: t.id, session: terms[t.id]?.sessionOf() ?? null })), bc.selected, from);
    if (!targets.length) return;
    if (e.kind === "data") {
      for (const t of targets) terms[t.id]?.noteInput(e.data);
      fanOut(targets.map((t) => t.session!), e.data, sessionWrite)
        .then((errs) => errs.forEach((x) => console.warn(`[charon] broadcast: write to ${x.session} failed: ${x.error}`)));
    } else if (e.kind === "paste") {
      // The source's paste guard has been answered once; each target brackets it for its own shell.
      for (const t of targets) terms[t.id]?.pasteBroadcast(e.text);
      console.info(`[charon] broadcast: paste bytes=${e.text.length} to ${targets.length} session(s) in ${appWindow.label}`);
    } else {
      fanSudo(targets.map((t) => ({ session: t.session!, prompted: terms[t.id]?.atSudoPrompt() ?? false })), sudoSendPassword)
        .then((r) => console.info(`[charon] broadcast: sudo ${r.map((x) => `${x.session.slice(0, 8)}=${x.sent ? "sent" : "skipped"}`).join(" ")}`));
    }
  }
  // Idle auto-off: input in any window counts as activity.
  let bcActivity = Date.now();
  $effect(() => { if (bc.on) untrack(() => (bcActivity = Date.now())); });
  $effect(() => {
    const t = setInterval(() => {
      if (bc.on && idleExpired(bcActivity, Date.now(), prefs.broadcastIdleMin)) {
        publish({ on: false });
        flashHint(`Broadcast turned off after ${prefs.broadcastIdleMin} min without input`);
        console.info("[charon] broadcast: off (idle)");
      }
    }, 15_000);
    return () => clearInterval(t);
  });
  function onFan(from: string, e: FanEvent) {
    if (!broadcasting || from !== bc.source) return;
    bcActivity = Date.now();
    fanLocal(from, e);
    bcInput({ ...e, from, window: appWindow.label } as BcInput).catch(() => {});
  }
  // Each window tells the backend its tabs' roles, for MCP list_sessions.
  let sessionOfTab = $state<Record<string, string | null>>({});
  $effect(() => {
    const roles: Record<string, "source" | "target"> = {};
    for (const t of tabs) {
      const sid = sessionOfTab[t.id], r = bcRole(t.id);
      if (sid && r) roles[sid] = r.role;
    }
    broadcastRoles(roles).catch(() => {});
  });
  function bcRole(id: string): { role: "source" | "target"; count: number } | null {
    if (!broadcasting) return null;
    if (id === bc.source) return { role: "source", count: bcCount };
    return bc.selected.has(id) ? { role: "target", count: bcCount } : null;
  }

  // ---- Split view: the active tab and the ticked ones side by side; in a Split
  // pop-out window, all of its tabs. Sized to the window (gridLayout); past the
  // minimum pane size the grid scrolls by rows.
  let gridOn = $state(false);
  let popout = $state(false);
  const gridIds = $derived(gridOn ? tabs.filter((t) => isTermTab(t) && (popout || bc.selected.has(t.id) || t.id === active)).map((t) => t.id) : []);
  const gridActive = $derived(gridIds.length > 1 || (popout && gridIds.length > 0));
  let panesEl = $state<HTMLDivElement | null>(null);
  let panesW = $state(0), panesH = $state(0);
  let cell = $state({ w: 8, h: 17 });
  const layout = $derived(gridLayout(gridIds.length, panesW, panesH, cell));
  const shown = (id: string) => (gridActive ? gridIds.includes(id) : active === id);
  function setGrid(on: boolean) {
    gridOn = on;
    // Every pane shown grabs the keyboard as it appears; hand it back to the active one.
    tick().then(() => requestAnimationFrame(() => terms[active ?? ""]?.focusTerm()));
  }
  // Character size for the grid: the active pane's (falls back to an estimate).
  $effect(() => {
    if (!gridActive) return;
    void prefs;
    tick().then(() => { const c = terms[active ?? ""]?.cellSize() ?? terms[gridIds[0]]?.cellSize(); if (c) cell = c; });
  });
  // Split collapses each pane's Files panel and gives it back on the way out
  // (or when the tab leaves the grid); `splitSaved` = state from before Split.
  let splitSaved: Record<string, boolean> = {};
  $effect(() => {
    const ids = gridActive ? gridIds : [];
    untrack(() => {
      const r = syncSplitFiles(splitSaved, ids, (id) => terms[id]?.filesShown() ?? false);
      splitSaved = r.saved;
      for (const [id, v] of Object.entries(r.set)) terms[id]?.setFiles(v);
    });
  });
  /** The top bar's Files button in Split: every pane at once. */
  function toggleAllFiles() {
    const on = filesAllTarget(gridIds.map((id) => !!filesOpen[id]));
    for (const id of gridIds) terms[id]?.setFiles(on);
  }
  // A scrolling grid keeps the pane with the keyboard in view.
  $effect(() => {
    const a = active;
    if (!gridActive || !layout.scroll) return;
    tick().then(() => panesEl?.querySelector<HTMLElement>(`[data-pane="${a}"]`)?.scrollIntoView({ block: "nearest" }));
  });

  /** ⧉: the Split panes move to a window of their own; the main window keeps a
   *  placeholder tab for each. Closing that window brings them back. */
  async function popOut() {
    const ids = [...gridIds];
    if (!isMain || !ids.length) return;
    moveError = "";
    // The first tab opens the window and carries the others' tokens.
    const order = [...ids.slice(1), ids[0]];
    const done: { id: string; token: string }[] = [];
    try {
      for (const id of order) {
        const t = tabs.find((x) => x.id === id)!;
        const snap = await terms[id]!.detach();
        const first = id === ids[0];
        const json: Tab = { id, title: t.title, color: t.color, font: t.font, open: t.open, adopt: snap, filesBefore: splitSaved[id],
          ...(first ? { popout: true as const, bundle: done.map((d) => d.token) } : {}) };
        done.push({ id, token: await tabStash(JSON.stringify(json)) });
      }
      const pos = await appWindow.outerPosition();
      const label = await tabWindowOpen(done.at(-1)!.token, pos.x + 140, pos.y + 70, window.innerWidth, window.innerHeight);
      for (const id of ids) delete splitSaved[id];
      tabs = tabs.map((t) => (ids.includes(t.id) ? { id: t.id, title: t.title, color: t.color, popped: label } : t));
      gridOn = false;
      active = tabs.find((t) => !t.popped)?.id ?? active;
      console.info(`[charon] split: popped out ${ids.length} pane(s) to ${label}`);
    } catch (e) {
      console.warn("[charon] split: pop-out failed", e);
      for (const d of done) { await tabTake(d.token).catch(() => {}); terms[d.id]?.undetach(); }
      moveError = `could not pop out: ${String(e)}`; setTimeout(() => (moveError = ""), 4000);
    }
  }

  /** "Broadcast to folder": open each host (reusing a tab already open on it) and tick them all. */
  function broadcastFolder(hosts: { node: Node; color?: string; font?: SessionFont }[]) {
    const ids: string[] = [];
    for (const h of hosts) {
      const open = tabs.find((t) => t.open?.nodeId === h.node.id);
      if (open) { ids.push(open.id); continue; }
      openNode(h.node, h.color, h.font);
      ids.push(active!);
    }
    if (!ids.length) { flashHint("Broadcast to folder: no hosts in it"); return; }
    publish({ selected: new Set([...bc.selected, ...ids]) });
    active = ids[0];
    flashHint(`Ticked ${ids.length} tab${ids.length === 1 ? "" : "s"}: Ctrl+Shift+B to broadcast`);
  }
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
  function setRenderer(r: "auto" | "webgl" | "canvas" | "dom") {
    if (!profile) return;
    profile.appearance.renderer = r;
    prefs = prefsFromProfile(profile);
    persistProfile();
  }
  function setLinuxRepaint(m: LinuxRepaint) {
    if (!profile) return;
    profile.terminal.linux_repaint = m;
    persistProfile(); // the backend applies it when the profile is saved
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

  /** MCP tier of the bare Local tab (MCP panel); read by the backend on each call once saved. */
  function setLocalMcpTier(tier: string) {
    if (!profile) return;
    (profile.local ??= {}).mcp_tier = tier;
    persistProfile();
  }
  /** File pane width / columns from any tab's pane; new panes and the other tabs follow. */
  function setFilesLayout(l: { width?: number; columns?: ColLayout }) {
    if (!profile) return;
    const files = (profile.files ??= {});
    if (l.width !== undefined) files.pane_width = l.width;
    if (l.columns) {
      files.column_widths = Object.fromEntries(Object.entries(l.columns.widths).filter((e): e is [string, number] => !!e[1]));
      files.hidden_columns = [...l.columns.hidden];
    }
    prefs = prefsFromProfile(profile);
    persistProfile();
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
    if (info.debug_ticker) startTicker();
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
    // Files dropped on a terminal or the editor tab open in the editor (the main
    // window's); a file pane inside a terminal takes its own drops (filedrop.ts).
    addDropTarget({ zone: "editor", drop: (paths) => { for (const p of paths) openFromUi(p, null, null); } });
    // Broadcast state shared between windows; the main window answers newcomers.
    await onBcState((s) => {
      if (s.from === appWindow.label || s.rev <= bc.rev) return;
      bc = { on: s.on, selected: new Set(s.selected), source: s.source, rev: s.rev };
    });
    await onBcInput((e) => {
      bcActivity = Date.now();
      if (e.window === appWindow.label || !broadcasting || e.from !== bc.source) return;
      fanLocal(e.from, e);
    });
    if (isMain) await onBcHello(() => publish({}));
    else bcHello().catch(() => {});
    // Tabs arriving from other windows; a new tab window starts with the one it was made for.
    await onTabAdopt((p) => adoptTab(p.token, p.x));
    if (!isMain) {
      // Closing a Split pop-out sends its panes back to the main window; closing a
      // torn-off window closes its tabs, which leave the broadcast.
      let leaving = false;
      await appWindow.onCloseRequested(async (e) => {
        if (leaving) return;
        leaving = true;
        if (popout) {
          e.preventDefault();
          for (const t of tabs.filter((x) => x.open)) await moveTab(t.id, "main");
          windowCloseSelf().catch(() => {});
        } else if (tabs.some((t) => bc.selected.has(t.id))) {
          publish({ selected: new Set([...bc.selected].filter((id) => !tabs.some((t) => t.id === id))) });
        }
      });
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
    if (e.ctrlKey && e.shiftKey && e.code === "KeyB" && !e.repeat) { e.preventDefault(); setBroadcast(!bc.on); }
  }
  function closeTab(id: string) {
    if (id === EDITOR_TAB && editorPane?.hasUnsaved() && !confirm("The editor has unsaved changes. Close anyway?")) return;
    dropTab(id);
  }
  /** `moved` = the tab went to another window: it stays ticked there. */
  function dropTab(id: string, moved = false) {
    tabs = tabs.filter((t) => t.id !== id);
    if (!moved && bc.selected.has(id)) toggleSelected(id, false);
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
    // Its pre-Split Files state goes with it (the pane arrives collapsed).
    const token = await tabStash(JSON.stringify({ id: t.id, title: t.title, color: t.color, font: t.font, open: t.open, adopt: snap, filesBefore: splitSaved[t.id] } satisfies Tab));
    delete splitSaved[t.id];
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
    dropTab(id, true);
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
    const bundle = t.bundle ?? [];
    const opensPopout = !!t.popout;
    if (t.filesBefore !== undefined) splitSaved[t.id] = t.filesBefore;
    delete t.bundle; delete t.popout; delete t.filesBefore;
    // Back from a Split pop-out: the placeholder becomes the tab again.
    const ph = tabs.findIndex((x) => x.id === t.id && x.popped);
    if (ph >= 0) tabs[ph] = t; else tabs.splice(at, 0, t);
    active = t.id;
    if (opensPopout) { popout = true; gridOn = true; }
    if (!isMain) appWindow.setTitle(popout ? "Split — Charon" : `${t.title} — Charon`).catch(() => {});
    for (const tok of bundle) await adoptTab(tok, -1);
    if (opensPopout) setGrid(true);
    // All placeholders back: show them split again, as they were.
    if (ph >= 0 && !tabs.some((x) => x.popped)) setGrid(true);
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
    <Sidebar onopen={openNode} onbroadcast={broadcastFolder} onquick={quickConnect} onlocal={openLocal} oneditor={newInEditor} onmcp={() => (mcpOpen = true)} onsettings={() => (settingsOpen = true)}
      bind:docsView {docsCommand} {docsSession} {vault} onunlock={() => (askMaster = true)} />
    <div class="splitter" role="separator" aria-orientation="vertical" aria-label="Resize sidebar" onpointerdown={startDrag}></div>
  {/if}
  <main>
    <div class="tabs" role="tablist" bind:this={tabsEl}>
      {#each tabs as t (t.id)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="tab" class:active={active === t.id} class:dragging={drag?.moved && drag.id === t.id} class:tearing={drag?.out && drag.id === t.id}
          class:bc-sel={bc.selected.has(t.id)} class:bc-live={broadcasting && (t.id === bc.source || bc.selected.has(t.id))} class:bc-any={bc.on || bc.selected.size > 0}
          class:popped={!!t.popped}
          data-tab={t.id} style:--tab-color={t.color ?? "var(--accent)"}
          oncontextmenu={(e) => { e.preventDefault(); tabMenu = { x: e.clientX, y: e.clientY, id: t.id }; }}>
          {#if isTermTab(t) || t.popped}
            <input type="checkbox" class="bc-check" checked={bc.selected.has(t.id)} aria-label="Broadcast to {t.title}"
              title="Include this tab in broadcast input (Ctrl+Shift+B toggles broadcasting)"
              onchange={(e) => toggleSelected(t.id, e.currentTarget.checked)} />
          {/if}
          <button type="button" class="tab-main" role="tab" aria-selected={active === t.id}
            title={canMove(t) ? "Drag to reorder; drag out of the window to open it in its own window" : undefined}
            onpointerdown={(e) => tabDown(e, t.id)} onpointermove={tabMove} onpointerup={tabUp} onpointercancel={() => (drag = null)}
            onclick={() => { if (swallowClick) return; if (t.popped) { focusWindow(t.popped).catch(() => {}); return; } active = t.id; if (gridActive) terms[t.id]?.focusTerm(); }}>
            <span class="dot"></span>{t.title}{#if t.editor && editorDirty}<span class="unsaved">•</span>{/if}{#if t.open?.admin}<span class="badge">admin</span>{/if}{#if t.popped}<span class="pop-badge" title="Shown in the Split window">⧉</span>{/if}
          </button>
          {#if !t.popped}<button type="button" class="x" aria-label="Close tab" onclick={() => closeTab(t.id)}>×</button>{/if}
        </div>
      {/each}
      {#if moveError}<span class="move-error">{moveError}</span>{/if}
      {#if bcHint}<span class="bc-hint">{bcHint}</span>{/if}
      {#if bc.selected.size > 0 || bc.on || popout}
        <button type="button" class="bc-toggle" class:on={broadcasting} onclick={() => setBroadcast(!bc.on)}
          title="Send what is typed or pasted in the source tab (the one with the keyboard) to every ticked tab, in any window (Ctrl+Shift+B)">{broadcasting ? `Broadcasting → ${bcCount}` : "Broadcast"}</button>
        {#if !popout}
          <button type="button" class="grid-toggle" class:on={gridActive} disabled={!gridOn && tabs.filter((t) => isTermTab(t) && (bc.selected.has(t.id) || t.id === active)).length < 2}
            onclick={() => setGrid(!gridOn)} title="Show the active tab and the ticked ones side by side">Split</button>
          {#if isMain && gridActive}
            <button type="button" class="pop-out" onclick={popOut} title="Open the Split panes in a window of their own (closing it brings them back)">⧉</button>
          {/if}
          <button type="button" class="bc-clear" onclick={() => { publish({ selected: new Set(), on: false }); gridOn = false; }} title="Untick every tab and stop broadcasting">Clear</button>
        {/if}
      {/if}
      {#if gridActive}
        <button type="button" class="files-toggle" class:on={gridIds.some((id) => !!filesOpen[id])} title="File pane in every Split pane (each pane's own files button opens just that one)"
          onclick={toggleAllFiles}>Files</button>
      {:else if activeTerm}
        <button type="button" class="files-toggle" class:on={!!filesOpen[activeTerm]} title="File pane for this tab (Ctrl+Shift+E)"
          onclick={() => terms[activeTerm!]?.toggleFiles()}>Files</button>
      {/if}
    </div>
    <div class="panes" class:grid={gridActive} class:scroll={gridActive && layout.scroll} bind:this={panesEl} bind:clientWidth={panesW} bind:clientHeight={panesH}
      style:--grid-cols={layout.cols} style:--pane-h="{layout.paneH}px">
      {#if tabs.length === 0}
        <div class="empty">
          <p>Pick a connection on the left, or type <code>user@host</code> in the quick-connect box.</p>
          {#if !isMain}<p>Drag a tab here, or move one from another window.</p>{/if}
          {#if info}<p class="meta">{info.portable ? "Portable mode" : "Installed"} · data in {info.data_dir}</p>{/if}
        </div>
      {/if}
      {#each tabs as t (t.id)}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="pane" class:focused={gridActive && active === t.id} hidden={!shown(t.id)} data-drop="editor" data-pane={t.id}
          onpointerdowncapture={() => { if (gridActive && active !== t.id) active = t.id; }} onfocusin={() => claimSource(t.id)}>
          {#if t.popped}
            <div class="empty"><p>This tab is in the Split window.</p>
              <p><button type="button" onclick={() => focusWindow(t.popped!).catch(() => {})}>Show it</button></p></div>
          {:else if t.editor}
            <EditorPane bind:this={editorPane} onsend={sendToSession} ondirty={(d) => (editorDirty = d)} />
          {:else if t.open}
            <Terminal bind:this={terms[t.id]} open={t.open} adopt={t.adopt} color={t.color} font={t.font} {prefs} active={shown(t.id)}
              broadcast={bcRole(t.id)} onfan={(e) => onFan(t.id, e)} onsession={(sid) => (sessionOfTab[t.id] = sid)} os={info?.os} osUser={info?.user} onfontsize={setFontSize} ontimestamps={setTimestamps} onclosed={() => { if (bc.selected.has(t.id)) { toggleSelected(t.id, false); flashHint(`Broadcast: ${t.title} closed, unticked`); } }} onabort={() => closeTab(t.id)}
              onopenfile={openFromUi} onfiles={(o) => (filesOpen[t.id] = o)} onfileslayout={setFilesLayout}
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
    renderer={prefs.rendererSetting} trace={profile.terminal.trace_output === true} restoreTabs={profile.ui.restore_tabs !== false}
    os={info?.os} linuxRepaint={(profile.terminal.linux_repaint || "off") as LinuxRepaint} onlinuxrepaint={setLinuxRepaint}
    ontheme={setTheme} onfont={setFont} onrenderer={setRenderer} ontrace={setTrace} onrestoretabs={setRestoreTabs} onclose={() => (settingsOpen = false)} />
{/if}
{#if mcpOpen}
  <McpPanel localTier={profile?.local?.mcp_tier ?? "read"} onlocaltier={setLocalMcpTier} onclose={() => (mcpOpen = false)} />
{/if}
{#if approve}
  <div class="backdrop" role="presentation">
    <div class="approve">
      {#if approve.targets?.length}
        {@const asked = approve.targets.filter((t) => t.decision === "approve").length}
        <h3>MCP wants to run a command on {approve.targets.length} sessions</h3>
        <pre class="cmd">{approve.command}</pre>
        <ul class="targets">
          {#each approve.targets as t (t.session)}
            <li class={t.decision}><span class="mono">{t.title ?? t.session.slice(0, 8)}</span>{#if t.host}<span class="dim mono">{t.host}</span>{/if}
              <span class="tier">{t.tier}</span>
              <span class="what">{t.decision === "approve" ? "needs your approval" : t.decision === "allow" ? "runs (trusted / always)" : "refused (read tier)"}</span></li>
          {/each}
        </ul>
        <p class="who">Approve applies to the {asked} session{asked === 1 ? "" : "s"} marked "needs your approval"; Deny runs none of them.</p>
      {:else}
        <h3>MCP wants to run a command</h3>
        <p class="who">tool <span class="mono">{approve.tool}</span> · session <span class="mono">{approve.session.slice(0, 8)}</span></p>
        <pre class="cmd">{approve.command}</pre>
      {/if}
      {#if approve.tool === "run_command"}
        <label class="sudo-opt" title="A command starting with sudo runs as root on its own channel; Charon gives the saved password to sudo directly (never to the command, never shown or logged). Without this, a sudo command is refused.">
          <input type="checkbox" bind:checked={allowSudo} /> may run as root (sudo)
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
  .panes.grid { display: grid; grid-template-columns: repeat(var(--grid-cols), minmax(0, 1fr)); grid-auto-rows: var(--pane-h); align-content: start; gap: 3px; background: var(--line); overflow-y: auto; }
  .panes.grid.scroll { scroll-snap-type: y mandatory; }
  .panes.grid .pane { position: relative; inset: auto; min-width: 0; min-height: 0; background: var(--bg); scroll-snap-align: start; }
  .panes.grid .pane.focused { box-shadow: 0 0 0 2px var(--accent); z-index: 1; }
  /* MultiExec: ticked tabs are striped; the live ones get an amber top edge. */
  .tab .bc-check { display: none; margin: 0 0 0 6px; accent-color: var(--warn); }
  .tab:hover .bc-check, .tab.bc-any .bc-check { display: block; }
  .tab.bc-sel { background-image: repeating-linear-gradient(-45deg, color-mix(in srgb, var(--warn) 22%, transparent) 0 5px, transparent 5px 10px); }
  .tab.bc-live { box-shadow: inset 0 3px 0 var(--warn); }
  .bc-toggle, .grid-toggle, .bc-clear, .pop-out { margin-bottom: 4px; padding: 3px 10px; flex: none; }
  .tab.popped { opacity: .6; font-style: italic; }
  .pop-badge { color: var(--accent); font-style: normal; }
  .bc-toggle { margin-left: auto; }
  .bc-toggle.on { border-color: var(--warn); background: var(--warn); color: var(--bg); font-weight: 700; }
  .grid-toggle.on { border-color: var(--accent); color: var(--fg); }
  .bc-toggle ~ .files-toggle { margin-left: 0; }
  .bc-hint { color: var(--warn); font-size: 12px; padding: 0 8px; white-space: nowrap; }
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
  .approve .targets { list-style: none; margin: 0; padding: 0; max-height: 180px; overflow: auto; border: 1px solid var(--line); border-radius: 4px; font-size: 12px; }
  .approve .targets li { display: flex; gap: 8px; align-items: baseline; padding: 3px 8px; border-left: 3px solid var(--warn); }
  .approve .targets li.allow { border-left-color: var(--ok); }
  .approve .targets li.deny { border-left-color: var(--danger); color: var(--fg-dim); }
  .approve .targets .dim { color: var(--fg-dim); }
  .approve .targets .tier { margin-left: auto; color: var(--fg-dim); }
  .approve .targets .what { min-width: 150px; text-align: right; }
</style>