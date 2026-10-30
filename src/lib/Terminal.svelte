<script module lang="ts">
  /** What a tab opens: a saved node of either kind, an ad-hoc SSH target, or a bare local shell. */
  export interface OpenTarget {
    kind: "ssh" | "local" | "serial";
    nodeId?: string; host?: string; user?: string; port?: number;
    /** Local only: elevated shell (Windows sudo inline / sudo -i). */
    admin?: boolean;
  }
  /** A terminal moving to another window: its session (kept open), its screen
   *  serialised at its current size, and the state that is not on screen. */
  export interface TabSnapshot {
    sessionId: string | null;
    status: "up" | "closed";
    screen: string;
    cols: number; rows: number;
    marks: SavedMark[];
    osc133: boolean; integrationEnabled: boolean; injectDone: boolean; reason: string;
    shell?: Shell; cwd?: string; program: string; sshUser?: string; promptUser?: string;
    filesOpen: boolean; filesWidth: number;
    stamps: Stamp[]; startedShell: string;
  }
  import type { SavedMark, Shell } from "./osc133";
  import type { Stamp } from "./stamps";
</script>

<script lang="ts">
  import { onMount, onDestroy, untrack } from "svelte";
  import { Terminal, type IMarker } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebglAddon } from "@xterm/addon-webgl";
  import { CanvasAddon } from "@xterm/addon-canvas";
  import { diag, epochMs } from "./diag";
  import { SearchAddon } from "@xterm/addon-search";
  import { SerializeAddon } from "@xterm/addon-serialize";
  import {
    openSsh, openLocal, openSerial, sessionWrite, sessionSetShell, sessionSetCwd, sessionResize, sessionClose, attachSession,
    sessionHold, sessionRelease, isOpenError, vaultSet, linkCredential, notifyChanged, onHostKeyPrompt, sshHostKeyAnswer, hostkeyForget, sshKeygenRemove,
    type HostKeyPrompt, type HostKeyChanged,
  } from "./api";
  import { cleanForCopy, pasteWarnings } from "./clipboard";
  import type { FsVia } from "./api";
  import { sq } from "./paths";
  import { fontStack, type Prefs, type SessionFont } from "./prefs";
  import { currentTheme, termTheme, watchTheme, type Theme } from "./themes";
  import ContextMenu, { type MenuItem } from "./ContextMenu.svelte";
  import SftpPane from "./SftpPane.svelte";
  import { attachOsc7 } from "./cwdtrack";
  import { findLinks, parseOsc52, type Row } from "./links";
  import FlagPopup from "./FlagPopup.svelte";
  import { helpFetch, onMcpScrollback, mcpAnswer, openUrl, readText, writeText } from "./api";
  import { cmdContext, currentCommand } from "./cmdline";
  import { tldrPage, preloadTldr } from "./tldr";
  import { flagsFromHelp, flagsFromTldr, mergeFlags, matchFlags, runFlagAccept, PickerGate, type Flag } from "./flags";
  import { CommandTracker, InjectionSwallower, detectShell, injectionFor, hookTextFor, looksLikeShellPrompt, looksLikeSudoPrompt, sudoUserOf, sudoAutoEligible, promptUserOf } from "./osc133";
  import { sudoHasPassword, sudoSendPassword, sessionShells, connectionSetRemoteShell, type HostShells } from "./api";
  import { gutterRows, type GutterRow } from "./stamps";

  let { open, adopt, color, font, prefs, active = true, os, osUser, onfontsize, ontimestamps, onclosed, onabort, oncwd, onopenfile, onfiles, oncommand }: {
    open: OpenTarget;
    /** Arrived from another window: restore this instead of connecting. */
    adopt?: TabSnapshot;
    color?: string;
    /** The connection's own terminal font (PuTTY-style per session); unset parts follow the profile. */
    font?: SessionFont;
    prefs: Prefs;
    /** This tab is the visible one. Inactive tabs buffer output and drop their
     *  WebGL context (browsers lose contexts past ~16 live), flushing on show. */
    active?: boolean;
    /** Host OS ("windows"|"macos"|"linux") and current OS user, from app_info.
     *  Used to gate the local sudo button (Windows UAC takes no tty password) and
     *  as the login user for a local sudo prompt. */
    os?: string;
    osUser?: string;
    /** Ctrl+= / Ctrl+- changed the size; the owner persists it to the profile (tabs
     *  whose connection sets its own size zoom on their own instead). */
    onfontsize: (size: number) => void;
    /** The timestamps switch was flipped; the owner persists it (all tabs follow prefs). */
    ontimestamps?: (on: boolean) => void;
    onclosed: (reason: string) => void;
    /** The user rejected the host key: nothing to show, close the tab. */
    onabort: () => void;
    /** The shell reported a new working directory (OSC 7). */
    oncwd?: (cwd: string) => void;
    /** Open a file from the file pane in the editor: `fs` = filesystem session (null = local disk), `origin` = this session. */
    onopenfile?: (path: string, fs: string | null, origin: string | null, via: FsVia) => void;
    /** The file pane was shown/hidden (tab-bar button state). */
    onfiles?: (open: boolean) => void;
    /** The command being typed changed (for the Docs pane): (command|null, session|null). */
    oncommand?: (command: string | null, session: string | null) => void;
  } = $props();

  let el: HTMLDivElement;
  let term: Terminal;
  let fit: FitAddon;
  let search: SearchAddon;
  let serializer: SerializeAddon;
  // Moving to another window: the session stays open (onDestroy must not close it).
  let handedOff = false;
  // Resolves when the hold marker (an empty data event) arrives: all output before
  // the hold has been received, everything after waits in the backend.
  let holdSeen: (() => void) | null = null;
  let webgl: WebglAddon | null = null;
  let canvas: CanvasAddon | null = null;
  // Output trace (diag.trace): the newest parsed chunk waiting for its paint.
  let tracePaint: { seq: number; t: number } | null = null;
  let tracker: CommandTracker;
  // Hidden-tab buffering: while inactive we don't touch xterm's parser/renderer;
  // incoming bytes queue here and flush on activation. Capped so a runaway hidden
  // tab (`yes`) can't grow without bound — on overflow we reset + write the tail.
  let pendingWrites: Uint8Array[] = [];
  let pendingTimes: number[] = [];   // arrival time of each pending chunk (line timestamps)
  let pendingBytes = 0;
  let pendingOverflow = false;
  const HIDDEN_BUF_CAP = 8 * 1024 * 1024;
  // Perf counters (T2): events/bytes since the last 5 s sample.
  let evCount = 0, byteCount = 0;
  let perfTimer: ReturnType<typeof setInterval> | undefined;
  // Shell-integration injection: after each quiet moment following connect, read
  // the last line; once it looks like a prompt, type the hook in. Gives up at the
  // deadline. Skipped if the remote already speaks OSC 133.
  let injectTimer: ReturnType<typeof setTimeout> | undefined;
  let injectDeadline = 0;
  let injectDone = false;
  const swallower = new InjectionSwallower();
  // Nested shells (sudo -i, su -, ssh) start without the hook: a prompt-looking
  // line while a command is still "running" triggers a re-injection, rate-limited.
  let integrationEnabled = false;    // per-session switch (chip menu)
  let lastInjectAt = 0;
  let reinjects = 0;
  const MAX_REINJECTS = 10;
  // Shell-integration chip in the status bar: "bash · 133 on · 3 marks".
  let osc133 = $state(false);        // any 133 sequence seen
  let shell = $state<Shell | undefined>();
  let markCount = $state(0);
  let reason = $state("");           // why 133 is off, once known
  let cwd = $state<string | undefined>();
  let chipMenu = $state<{ x: number; y: number } | null>(null);
  // File pane (Ctrl+Shift+E): SFTP for SSH, local disk for local tabs; serial has none.
  let filesOpen = $state(false);
  let filesWidth = $state(420);
  const hasFiles = $derived(open.kind !== "serial");
  export function toggleFiles() { if (hasFiles) { filesOpen = !filesOpen; onfiles?.(filesOpen); if (!filesOpen) term?.focus(); } }
  export function filesShown(): boolean { return filesOpen; }
  // Who the shell is vs who SFTP is: differ after sudo -i / su -, so the pane switches backend.
  let sshUser = $state<string | undefined>();
  // The login user for a sudo prompt: SSH login user, or the OS user for a local shell.
  const loginUser = $derived(open.kind === "local" ? osUser : sshUser);
  // Windows local shells elevate via UAC (no tty password): never offer the button.
  const winLocalNoSudo = $derived(open.kind === "local" && os === "windows");
  let promptUser = $state<string | undefined>();
  let program = $state("");
  /** At a prompt with nothing running: safe for the pane to type hidden commands / cd. */
  function shellIdle(): boolean {
    if (!osc133) return false;
    const m = tracker.marks.at(-1);
    return !m || m.endLine !== undefined || m.outputStartLine === undefined;
  }
  /** `cd` into a directory the pane navigated to (→ term). Only when idle; never mid-command. */
  function chdir(path: string) {
    if (!sessionId || !shellIdle()) return;
    const win = /^[A-Za-z]:\\/.test(path);
    const cmd = win
      ? (/cmd(\.exe)?$/i.test(program) ? `cd /d "${path}"` : `cd "${path}"`)
      : `cd -- ${sq(path)}`;
    sessionWrite(sessionId, `${cmd}\n`);
  }
  function startPaneDrag(e: PointerEvent) {
    const startX = e.clientX, startW = filesWidth;
    const target = e.currentTarget as HTMLElement;
    target.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => { filesWidth = Math.max(240, Math.min(window.innerWidth * 0.7, startW - (ev.clientX - startX))); };
    const stop = () => { target.removeEventListener("pointermove", move); target.removeEventListener("pointerup", stop); fit?.fit(); };
    target.addEventListener("pointermove", move); target.addEventListener("pointerup", stop);
  }

  // Search overlay (Ctrl+Shift+F).
  let searchOpen = $state(false);
  let query = $state("");
  let searchEl = $state<HTMLInputElement | null>(null);
  const searchOpts = (t: Theme = currentTheme()) => ({ decorations: { matchBackground: t.search.match, activeMatchBackground: t.search.active, matchOverviewRuler: t.search.ruler, activeMatchColorOverviewRuler: t.search.ruler } });

  // Line timestamps: one marker per second in which a line started (see stamps.ts).
  // Always recorded, so switching the gutter on shows times for earlier output too.
  let stampMarks: { marker: IMarker; t: number }[] = [];
  let lastStampSec = -1;
  // While flushing a hidden tab's buffered output: the time those bytes arrived.
  let stampClock: number | null = null;
  let gutter = $state<GutterRow[]>([]);
  let cellH = $state(0);
  let gutterFrame = 0;
  // Shell menu (SSH): what the host offers, and the shell this session started.
  let hostShells = $state<HostShells | string | null>(null);   // string = why it failed
  let startedShell = $state("");                                 // "" = login shell
  let shellOverride: string | undefined;                         // picked from the menu for the next connect

  // Right-click menu.
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);

  // Floating "Copy" button shown near a settled text selection.
  let copyBtn = $state<{ x: number; y: number } | null>(null);
  /** Short hint shown when a click goes to a program that has the mouse (Claude Code, vim, htop). */
  let mouseHint = $state(false);
  let mouseHintShown = false;
  let mouseHintTimer: ReturnType<typeof setTimeout> | undefined;
  /** Hovered link, shown as a small "Ctrl+click to open" tip. */
  let linkTip = $state<{ x: number; y: number; url: string } | null>(null);
  let selTimer: ReturnType<typeof setTimeout> | undefined;

  // Paste guard: an inline, non-modal bar (Enter/Esc or buttons) shown when a paste
  // trips a warning, so xterm never loses focus to a confirm() dialog.
  let pasteGuard = $state<{ cleaned: string; warns: string[]; route: string } | null>(null);
  // Pastes deferred while a hidden/injected command is in flight; flushed after.
  let pendingPaste: { cleaned: string; route: string }[] = [];

  // Sudo/su password prompt: detected on the live line while a command is open.
  let sudoPrompt = $state<{ user: string | null } | null>(null);
  let sudoHasPw = $state<boolean | null>(null);   // cached: does the session have a stored password?
  let sudoAutoTimer: ReturnType<typeof setTimeout> | undefined;

  // Flag picker (P5): a popup of a command's options as the user types `<cmd> -…`.
  let flagPopup = $state<{ x: number; y: number; command: string; token: string } | null>(null);
  let flagList = $state<Flag[]>([]);
  let flagSel = $state(0);
  let flagLoading = $state(false);
  let flagAll: Flag[] = [];                 // unfiltered flags for the current command
  const flagCache = new Map<string, { all: Flag[]; helpDone: boolean }>();
  let flagEvalTimer: ReturnType<typeof setTimeout> | undefined;
  // Typed keystrokes only: a paste suppresses the picker until the next prompt.
  const pickerGate = new PickerGate();

  let sessionId = $state<string | null>(null);
  let status = $state<"connecting" | "auth" | "hostkey" | "up" | "closed">("connecting");
  // Host key prompt: payload carries the session id to answer with.
  let hostkey = $state<HostKeyPrompt | null>(null);
  let hkAnswered = $state<"accept" | "reject" | null>(null);
  let hkError = $state("");
  // Host key mismatch: offer the fix that matches where the stale entry lives.
  let changed = $state<HostKeyChanged | null>(null);
  let lastPw: string | undefined; // password of the attempt in flight, for "replace key" retries
  let error = $state("");
  const unlisteners: Array<() => void> = [];

  // Inline credential form, shown only when nothing in the vault matches.
  let password = $state("");
  let saveAs = $state("");
  let authError = $state("");
  let passwordEl = $state<HTMLInputElement | null>(null);
  const HIDE_CURSOR = "\x1b[?25l";
  const SHOW_CURSOR = "\x1b[?25h";
  const defaultSaveName = () => `${open.user ?? ""}${open.user ? "@" : ""}${open.host ?? open.nodeId ?? "host"}`;

  onMount(async () => {
    term = new Terminal({
      fontFamily: fontFamily(),
      fontSize: fontSize(),
      scrollback: prefs.scrollback,
      allowProposedApi: true,
      // macOS: Option+drag selects even when a program has the mouse (Shift does elsewhere).
      macOptionClickForcesSelection: true,
      // OSC 8 hyperlinks (programs that mark their links explicitly).
      linkHandler: {
        activate: (e, uri) => activateLink(e, uri),
        hover: (e, uri) => showLinkTip(e, uri),
        leave: () => (linkTip = null),
        allowNonHttpProtocols: false,
      },
      theme: termTheme(currentTheme(), color),
      // A moved tab restores its screen at the size it was serialised at, then fits.
      ...(adopt ? { cols: adopt.cols, rows: adopt.rows } : {}),
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    search = new SearchAddon();
    term.loadAddon(search);
    serializer = new SerializeAddon();
    term.loadAddon(serializer);
    term.open(el);
    // Plain-text URLs, including ones broken over several rows.
    term.registerLinkProvider({ provideLinks: (y, cb) => cb(linksOnRow(y)) });
    // OSC 52: a program (Claude Code, tmux, vim with osc52) puts text on the clipboard.
    // Writing only — a request to read the clipboard (`?`) is ignored.
    term.parser.registerOscHandler(52, (data) => {
      const text = parseOsc52(data);
      if (text !== null && text !== "") {
        writeText(text).catch(() => {});
        console.info(`[charon] clipboard: osc52 chars=${text.length}`);
      }
      return true;
    });
    if (active) enableWebgl(); // inactive tabs render on demand once shown
    if (!adopt) fit.fit();
    preloadTldr();
    term.onCursorMove(() => { clearTimeout(flagEvalTimer); flagEvalTimer = setTimeout(() => { evalFlagPicker(); reportCommand(); }, 60); });
    tracker = new CommandTracker(term, {
      color: color ?? currentTheme().ui.accent,
      onchange: () => { osc133 = true; markCount = tracker.marks.length; },
      // Re-evaluate who the shell is on every prompt (sudo -i / su - / exit), un-latched.
      onprompt: (line) => { pickerGate.prompt(); const u = promptUserOf(line); if (u) promptUser = u; },
    });
    // OSC 7: the hook reports the cwd after every command.
    attachOsc7(term, () => cwd, (dir) => {
      console.info(`[charon] cwd: osc7 ${JSON.stringify(cwd ?? null)} -> ${JSON.stringify(dir)} pane=${filesOpen}`);
      cwd = dir; if (sessionId) sessionSetCwd(sessionId, dir).catch(() => {}); oncwd?.(dir);
    });
    // Selection: show a floating Copy button and (if enabled) copy — both debounced
    // so dragging a selection doesn't fire a clipboard write / reposition per frame
    // (the "slight delay while selecting"). Deselect hides the button at once.
    term.onSelectionChange(() => {
      clearTimeout(selTimer);
      if (!term.hasSelection()) { copyBtn = null; return; }
      selTimer = setTimeout(() => {
        if (!term.hasSelection()) { copyBtn = null; return; }
        if (prefs.copyOnSelect) copySelection();
        copyBtn = selectionAnchor();
      }, 120);
    });
    term.onScroll(() => { if (copyBtn) copyBtn = null; }); // stale position once scrolled
    term.onLineFeed(recordStamp);
    term.onRender(scheduleGutter);
    term.onRender(() => {
      if (!tracePaint) return;
      console.info(`[charon] out: painted session=${sessionId} seq=${tracePaint.seq} lag=${(epochMs() - tracePaint.t).toFixed(1)}ms renderer=${rendererName()}`);
      tracePaint = null;
    });
    term.onScroll(scheduleGutter);
    if (!adopt) recordStamp();
    // No cursor while connecting/authenticating; the remote shell shows it once up.
    term.write(HIDE_CURSOR);

    // Copy/paste: Ctrl+Shift+C/V everywhere; Ctrl+C sends SIGINT unless text is selected (PuTTY habit).
    // Ctrl+Shift+F search, Ctrl+= / Ctrl+- / Ctrl+0 zoom.
    // This is the only custom key handler xterm allows; every shortcut lives here.
    // Returning false stops xterm from encoding the key for the shell, but not the
    // browser default (Ctrl+Shift+V would still fire a native paste, Ctrl+Shift+C
    // opens WebView2's inspector), hence the preventDefault.
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      const handled = (): false => { e.preventDefault(); return false; };
      // Paste guard is up: Enter confirms, Esc cancels; xterm keeps focus meanwhile.
      if (pasteGuard) {
        if (e.code === "Enter" || e.code === "NumpadEnter") { confirmPaste(); return handled(); }
        if (e.code === "Escape") { cancelPaste(); return handled(); }
      }
      // Flag picker: steer the popup. Space/Tab/Esc (and any non-matching char)
      // close it and pass through untouched — Tab must reach remote completion.
      if (flagPopup && !e.ctrlKey && !e.altKey) {
        if (e.code === "ArrowDown") { flagSel = Math.min(flagList.length - 1, flagSel + 1); return handled(); }
        if (e.code === "ArrowUp") { flagSel = Math.max(0, flagSel - 1); return handled(); }
        if (e.code === "Enter" || e.code === "NumpadEnter") {
          // Insert the missing suffix and swallow this Enter; or, on an exact/no
          // match, close and let the Enter run the line. Never sends a newline here.
          // (acceptFlagAt closes the popup and refocuses xterm.)
          return acceptFlagAt(flagSel) ? handled() : true;
        }
        if (e.code === "Space" || e.code === "Tab" || e.code === "Escape") { closeFlagPicker(); return true; }
      }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyC") { copySelection(); return handled(); }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyV") { pasteFromClipboard("ctrl-shift-v"); return handled(); }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyF") { openSearch(); return handled(); }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyE") { toggleFiles(); return handled(); }
      if (e.ctrlKey && e.shiftKey && e.code === "KeyP" && sudoPrompt) { sendSudo(); return handled(); }
      if (e.ctrlKey && !e.shiftKey && e.code === "KeyC" && prefs.ctrlCCopiesWhenSelected && term.hasSelection()) { copySelection(); return handled(); }
      if (e.ctrlKey && (e.code === "Equal" || e.code === "NumpadAdd")) { zoom(1); return handled(); }
      if (e.ctrlKey && (e.code === "Minus" || e.code === "NumpadSubtract")) { zoom(-1); return handled(); }
      if (e.ctrlKey && (e.code === "Digit0" || e.code === "Numpad0")) { zoom(0); return handled(); }
      // Command marks: Ctrl+Up/Down walk the prompts, Ctrl+Shift+Up selects the output of the one landed on.
      if (e.ctrlKey && !e.shiftKey && e.code === "ArrowUp") { tracker.jump(-1); return handled(); }
      if (e.ctrlKey && !e.shiftKey && e.code === "ArrowDown") { tracker.jump(1); return handled(); }
      if (e.ctrlKey && e.shiftKey && e.code === "ArrowUp") { const m = tracker.selectTarget(); if (m) tracker.selectOutput(m); return handled(); }
      return true;
    });
    // Native paste events (Shift+Insert, browser menu) would otherwise go through xterm's
    // own handler and bypass the sanitiser; capture them first and route them the same way.
    term.element!.addEventListener("paste", (e) => {
      e.preventDefault(); e.stopImmediatePropagation();
      pasteToSession(e.clipboardData?.getData("text/plain") ?? "", "native");
    }, { capture: true });
    // Middle-click paste (X11 habit; on by default only on Linux).
    el.addEventListener("auxclick", (e) => { if (e.button === 1 && prefs.middleClickPaste) { e.preventDefault(); pasteFromClipboard("middle-click"); } });
    el.addEventListener("mousedown", (e) => { if (e.button === 1 && prefs.middleClickPaste) e.preventDefault(); });
    // A program with mouse tracking on (Claude Code, vim, htop) gets clicks, so a
    // plain drag selects nothing. Right-click (menu / MobaXterm copy-paste) and
    // middle-click paste stay Charon's: stop them before xterm reports them.
    // Shift+drag (Option on macOS) still selects; say so on the first plain click.
    el.addEventListener("mousedown", (e) => {
      if (term.modes.mouseTrackingMode === "none") return;
      if (e.button === 2 || (e.button === 1 && prefs.middleClickPaste)) { e.stopPropagation(); return; }
      if (e.button === 0 && !e.shiftKey && !e.altKey && !mouseHintShown) {
        mouseHintShown = true; mouseHint = true;
        clearTimeout(mouseHintTimer); mouseHintTimer = setTimeout(() => (mouseHint = false), 5000);
        console.info(`[charon] mouse: program has mouse tracking (${term.modes.mouseTrackingMode}); Shift+drag selects`);
      }
    }, { capture: true });

    term.onData((d) => { pickerGate.input(d); if (sessionId) sessionWrite(sessionId, d); });
    term.onResize(({ cols, rows }) => { if (sessionId) sessionResize(sessionId, cols, rows); });
    perfTimer = setInterval(logPerf, 5000);

    if (adopt) await restore(adopt);
    else await connect();
  });

  /** Hand this tab to another window. Holds the session's output in the backend,
   *  waits until everything before the hold is on screen, and serialises it. The
   *  receiving window releases the hold; `undetach` undoes this if the move fails. */
  export async function detach(): Promise<TabSnapshot> {
    if (status !== "up" && status !== "closed") throw new Error("wait until the tab has connected");
    if (sessionId && status === "up") {
      const seen = new Promise<void>((r) => (holdSeen = r));
      await sessionHold(sessionId);
      await Promise.race([seen, new Promise((r) => setTimeout(r, 2000))]);
      holdSeen = null;
    }
    flushHidden();
    await new Promise<void>((r) => term.write("", r));
    handedOff = true;
    const snap: TabSnapshot = {
      sessionId, status: status as "up" | "closed",
      screen: serializer.serialize(), cols: term.cols, rows: term.rows,
      marks: tracker.exportMarks(),
      osc133, integrationEnabled, injectDone, reason, shell, cwd, program, sshUser, promptUser,
      filesOpen, filesWidth,
      stamps: stampMarks.filter((s) => !s.marker.isDisposed).map((s) => ({ line: s.marker.line, t: s.t })),
      startedShell,
    };
    console.info(`[charon] tab: detach session=${sessionId} screen=${snap.screen.length} marks=${snap.marks.length}`);
    return snap;
  }
  /** The move failed: keep the tab here and let the held output through. */
  export function undetach() {
    handedOff = false;
    if (sessionId) sessionRelease(sessionId).catch(() => {});
  }

  /** Arrived from another window: put the screen back, pick up the session. */
  async function restore(a: TabSnapshot) {
    await new Promise<void>((r) => term.write(a.screen, r));
    tracker.importMarks(a.marks);
    osc133 = a.osc133; markCount = tracker.marks.length; integrationEnabled = a.integrationEnabled;
    injectDone = a.injectDone; reason = a.reason; shell = a.shell; cwd = a.cwd; program = a.program;
    sshUser = a.sshUser; promptUser = a.promptUser; filesWidth = a.filesWidth; startedShell = a.startedShell;
    // Writing the screen back stamped its lines "now"; the real times come with it.
    for (const st of stampMarks) st.marker.dispose();
    stampMarks = [];
    if (term.buffer.active.type === "normal") {
      const cur = term.buffer.active.baseY + term.buffer.active.cursorY;
      for (const st of a.stamps) {
        const marker = term.registerMarker(st.line - cur);
        if (marker) stampMarks.push({ marker, t: st.t });
      }
      lastStampSec = Math.floor((a.stamps.at(-1)?.t ?? 0) / 1000);
    }
    if (a.filesOpen && hasFiles) { filesOpen = true; onfiles?.(true); }
    fit.fit(); // a different window size: the resize reaches the remote (SIGWINCH redraws TUIs)
    if (a.sessionId && a.status === "up") {
      sessionId = a.sessionId;
      status = "up";
      await listenSession(a.sessionId);
      await sessionRelease(a.sessionId);
      if (term.cols !== a.cols || term.rows !== a.rows) sessionResize(a.sessionId, term.cols, term.rows);
    } else {
      status = "closed";
    }
    console.info(`[charon] tab: adopted session=${a.sessionId} marks=${a.marks.length}`);
    focusShown();
  }

  /** Connect using the node's vault credential (or `pw` typed inline). */
  /** Re-open the session reusing the same node/host and credentials. */
  async function reconnect() {
    if (sessionId) sessionClose(sessionId);
    sessionId = null;
    error = ""; reason = ""; cwd = undefined;
    osc133 = false; markCount = 0; shell = undefined;
    injectDone = false; injectDeadline = 0; reinjects = 0; lastReported = null;
    sudoPrompt = null; sudoHasPw = null; clearTimeout(sudoAutoTimer);
    term.write("\r\n\x1b[2m[reconnecting…]\x1b[0m\r\n" + HIDE_CURSOR);
    await connect(lastPw);
  }

  async function connect(pw?: string) {
    status = "connecting";
    changed = null;
    lastPw = pw;
    // Pick the id up front so the host key prompt can be routed to this tab.
    const id = crypto.randomUUID();
    hkAnswered = null; hkError = "";
    const unHostkey = await onHostKeyPrompt(id, (p) => {
      console.info("[charon] host key prompt", p);
      hostkey = p; status = "hostkey";
    });
    console.info("[charon] connecting", { id, host: open.host, nodeId: open.nodeId, withPassword: pw !== undefined });
    try {
      const info = open.kind === "local"
        ? await openLocal({ id, node_id: open.nodeId, admin: open.admin, cols: term.cols, rows: term.rows })
        : open.kind === "serial"
        ? await openSerial({ id, node_id: open.nodeId, cols: term.cols, rows: term.rows })
        : await openSsh({
            id, node_id: open.nodeId, host: open.host, user: open.user, port: open.port,
            password: pw, cols: term.cols, rows: term.rows, remote_shell: shellOverride,
          });
      sessionId = info.id;
      status = "up";
      program = info.title;
      if (open.kind === "ssh") { sshUser = info.title.split("@")[0]; startedShell = info.remote_shell ?? ""; }
      if (hasFiles && prefs.filesAutoOpen && !filesOpen) { filesOpen = true; onfiles?.(true); }
      term.write(SHOW_CURSOR);
      integrationEnabled = !!info.shell_integration;
      if (info.shell_integration) injectDeadline = Date.now() + 8000;
      else { reason = "off in settings"; console.info("[charon] 133: detected=none injected=false reason=disabled by node/folder setting"); }
      await listenSession(info.id);
      term.focus();
      if (pw && saveAs.trim()) await saveTypedPassword(pw, saveAs.trim());
      password = "";
    } catch (e) {
      const oe = isOpenError(e) ? e : { kind: "error" as const, message: String(e) };
      console.info("[charon] connect failed", { id, ...oe });
      if (hkAnswered === "reject") { onabort(); return; }
      if (oe.kind === "need_credential" || (pw !== undefined && /authentication rejected/.test(oe.message))) {
        // Ask (again) inline. First time: no message; retry: say why.
        authError = pw !== undefined ? oe.message : "";
        status = "auth";
        password = "";
        queueMicrotask(() => passwordEl?.focus());
        return;
      }
      status = "closed"; error = oe.message;
      term.write(`\r\n\x1b[31m${error}\x1b[0m\r\n`);
      if (oe.kind === "hostkey_changed" && oe.hostkey) changed = oe.hostkey;
    } finally {
      unHostkey();
      hostkey = null;
    }
  }

  /** Output, close and MCP scrollback requests for session `id`. */
  async function listenSession(id: string) {
    unlisteners.push(await attachSession(id, { closed: (r) => { status = "closed"; sudoPrompt = null; clearTimeout(sudoAutoTimer); term.write(`
[2m[${r}][0m
${HIDE_CURSOR}`); onclosed(r); }, data: (raw, m) => {
      if (diag.trace) console.info(`[charon] out: recv session=${id} seq=${m.seq} bytes=${raw.length} lag=${(epochMs() - m.t).toFixed(1)}ms active=${active}`);
      if (!raw.length) { holdSeen?.(); return; } // hold marker (tab moving windows)
      evCount++; byteCount += raw.length;
      // While the injected hook is being echoed back, show nothing until its end sentinel.
      const b = swallower.filter(raw);
      if (!b.length) return;
      // Hidden tab: don't run the parser/renderer; buffer and flush on activation.
      if (!active) { bufferHidden(b); return; }
      // Leaving the alternate screen (vim, htop) can leave a stale cursor cell in
      // the WebGL atlas; redraw everything once the bytes are processed.
      const leftAlt = webgl && exitsAltScreen(b);
      // Run post-parse work in the write callback: term.write is async, so the
      // buffer (and thus lastLine()) only reflects `b` once this fires. Reading
      // it synchronously here would see the previous prompt, not a sudo one.
      term.write(b, () => {
        if (leftAlt) { webgl?.clearTextureAtlas(); term.refresh(0, term.rows - 1); }
        maybeSudoPrompt(b);
        if (diag.trace) { console.info(`[charon] out: parsed session=${id} seq=${m.seq} lag=${(epochMs() - m.t).toFixed(1)}ms`); tracePaint = { seq: m.seq, t: m.t }; }
      });
      // An injected/hidden command just stopped being swallowed: release queued pastes.
      if (pendingPaste.length && !swallower.swallowing) flushPendingPaste();
      if (integrationEnabled) scheduleQuietCheck(id);
    } }));
    // MCP read_scrollback: only the Terminal that owns this session answers.
    unlisteners.push(await onMcpScrollback((p) => {
      if (p.session !== sessionId || handedOff) return;
      flushHidden(); // materialise any output buffered while this tab was hidden
      if (p.last_n_lines) {
        const b = term.buffer.active;
        const lines: string[] = [];
        const start = Math.max(0, b.baseY + b.cursorY - p.last_n_lines + 1);
        for (let i = start; i <= b.baseY + b.cursorY; i++) lines.push(b.getLine(i)?.translateToString(true) ?? "");
        mcpAnswer(p.req_id, { lines: lines.join("\n") });
      } else {
        mcpAnswer(p.req_id, { commands: tracker.recent(p.last_n_commands ?? 10) });
      }
    }));
  }

  /** Answer the prompt with the id it carried; the panel stays up until connect resolves. */
  async function answerHostKey(accept: boolean) {
    if (!hostkey || hkAnswered) return;
    hkAnswered = accept ? "accept" : "reject";
    hkError = "";
    console.info("[charon] host key answer", { session: hostkey.session, accept });
    try {
      await sshHostKeyAnswer(hostkey.session, accept);
    } catch (e) {
      hkAnswered = null;
      hkError = String(e);
      console.error("[charon] host key answer failed", e);
    }
  }

  /** "Host was reinstalled": drop the stale line from Charon's known_hosts and reconnect. */
  async function replaceKey() {
    if (!changed) return;
    try {
      const n = await hostkeyForget(changed.host, changed.port);
      term.write(`\r\n\x1b[2m[removed ${n} stale known_hosts ${n === 1 ? "entry" : "entries"}; reconnecting]\x1b[0m\r\n`);
      error = "";
      await connect(lastPw);
    } catch (e) {
      term.write(`\r\n\x1b[31m${String(e)}\x1b[0m\r\n`);
    }
  }

  async function saveTypedPassword(pw: string, name: string) {
    try {
      await vaultSet(name, { kind: "password", value: pw });
      if (open.nodeId) { await linkCredential(open.nodeId, name); notifyChanged("connections"); }
      notifyChanged("vault");
    } catch (e) {
      term.write(`\r\n\x1b[33mcould not save credential: ${String(e)}\x1b[0m\r\n`);
    }
  }

  function submitAuth(e: SubmitEvent) {
    e.preventDefault();
    if (!password) return;
    connect(password);
  }

  // Follow Settings → Appearance → Theme live.
  const unwatchTheme = watchTheme((t) => { if (term) term.options.theme = termTheme(t, color); });
  onDestroy(() => {
    unwatchTheme();
    clearTimeout(injectTimer);
    clearTimeout(flagEvalTimer);
    clearTimeout(selTimer);
    clearInterval(perfTimer);
    unlisteners.forEach((u) => u());
    if (sessionId && !handedOff) sessionClose(sessionId);
    tracker?.destroy();
    term?.dispose();
  });

  // ---- docs: report the command being typed
  let lastReported: string | null = null;
  function reportCommand() {
    if (!oncommand || !active) return;
    const input = osc133 && sessionId ? tracker.inputToCursor() : null;
    const cmd = input !== null ? currentCommand(input) : null;
    if (cmd !== lastReported) { lastReported = cmd; oncommand(cmd, sessionId); }
  }

  // ---- flag picker (P5)
  function closeFlagPicker() { flagPopup = null; flagList = []; flagSel = 0; }
  function evalFlagPicker() {
    if (!osc133 || !sessionId || !active) { closeFlagPicker(); return; }
    const blocked = pickerGate.blocked();
    if (blocked) {
      if (pickerGate.shouldLog()) console.info(`[charon] flags: suppressed reason=${blocked}`);
      closeFlagPicker(); return;
    }
    // Never complete a token mid-line: the picker can't see what's to the right.
    if (!cursorAtLineEnd()) { closeFlagPicker(); return; }
    const input = tracker.inputToCursor();
    const ctx = input !== null ? cmdContext(input) : null;
    if (!ctx) { closeFlagPicker(); return; }
    const pos = cursorXY();
    flagPopup = { x: pos.x, y: pos.y, command: ctx.command, token: ctx.token };
    loadFlagsFor(ctx.command);
    refilterFlags(ctx.token);
  }
  function refilterFlags(token: string) {
    flagList = matchFlags(flagAll, token).slice(0, 200);
    flagSel = 0;
  }
  async function loadFlagsFor(cmd: string) {
    const cached = flagCache.get(cmd);
    if (cached) {
      flagAll = cached.all;
      if (flagPopup?.command === cmd) refilterFlags(flagPopup.token);
      if (!cached.helpDone) fetchHelpFlags(cmd);
      return;
    }
    const md = await tldrPage(cmd).catch(() => undefined);
    flagAll = mergeFlags(md ? flagsFromTldr(md) : []);
    flagCache.set(cmd, { all: flagAll, helpDone: false });
    if (flagPopup?.command === cmd) refilterFlags(flagPopup.token);
    fetchHelpFlags(cmd);
  }
  async function fetchHelpFlags(cmd: string) {
    if (!sessionId) return;
    flagLoading = true;
    try {
      const help = await helpFetch(sessionId, cmd);
      const merged = mergeFlags(flagsFromHelp(help), flagCache.get(cmd)?.all ?? []);
      flagCache.set(cmd, { all: merged, helpDone: true });
      if (flagPopup?.command === cmd) { flagAll = merged; refilterFlags(flagPopup.token); }
    } catch { /* no --help (or local session): tldr flags only */ }
    finally { flagLoading = false; }
  }
  /** Enter on the highlighted flag. Returns true if it inserted (swallow Enter),
   *  false to let the Enter run the line. Only ever appends the missing suffix. */
  /** Accept the flag at `index` (Enter uses flagSel; a click uses the clicked row).
   *  Both paths go through runFlagAccept, so the sent suffix is identical and xterm
   *  is refocused. Returns true if it inserted (swallow the Enter). */
  function acceptFlagAt(index: number): boolean {
    if (!sessionId) return false;
    // Recompute the prefix from the live line so a stale popup token can't misfire.
    const input = tracker.inputToCursor();
    const ctx = input !== null ? cmdContext(input) : null;
    const prefix = ctx?.token ?? flagPopup?.token ?? "";
    const r = runFlagAccept(prefix, flagList, index, {
      send: (s) => { if (sessionId) sessionWrite(sessionId, s); },
      focus: () => term.focus(),
      close: closeFlagPicker,
    });
    console.info(`[charon] flags: prefix=${JSON.stringify(prefix)} selected=${JSON.stringify(r.selected)} sent=${JSON.stringify(r.insert)}`);
    return r.consumed;
  }
  /** True when the cursor is at the end of the typed line (nothing to its right).
   *  The picker can't complete a middle token: it can't see what follows. */
  function cursorAtLineEnd(): boolean {
    const b = term.buffer.active;
    const cur = b.getLine(b.baseY + b.cursorY);
    if (!cur) return true;
    if (cur.translateToString(true, b.cursorX) !== "") return false;
    // Wrapped continuation rows are also "to the right".
    let y = b.baseY + b.cursorY + 1;
    for (let guard = 0; guard < 256; guard++) {
      const nxt = b.getLine(y);
      if (!nxt || !nxt.isWrapped) break;
      if (nxt.translateToString(true) !== "") return false;
      y++;
    }
    return true;
  }
  /** Pixel position of the terminal cursor, for anchoring the popup. */
  function cursorXY(): { x: number; y: number } {
    const rect = el.getBoundingClientRect();
    try {
      const cell = (term as unknown as { _core: { _renderService: { dimensions: { css: { cell: { width: number; height: number } } } } } })._core._renderService.dimensions.css.cell;
      const b = term.buffer.active;
      const x = Math.min(rect.left + 12 + b.cursorX * cell.width, window.innerWidth - 440);
      const y = rect.top + 4 + (b.cursorY + 1) * cell.height;
      return { x: Math.max(8, x), y };
    } catch {
      return { x: rect.left + 40, y: rect.bottom - 260 };
    }
  }

  function scheduleQuietCheck(id: string) {
    clearTimeout(injectTimer);
    // 400 ms of silence after output = the prompt is probably drawn.
    injectTimer = setTimeout(() => { if (injectDone) maybeReinject(id); else tryInjection(id); }, 400);
  }
  /** Last non-empty line on screen. */
  function lastLine(): string {
    const b = term.buffer.active;
    let line = "";
    for (let i = b.baseY + b.cursorY; i >= 0 && !line; i--) line = b.getLine(i)?.translateToString(true) ?? "";
    return line;
  }
  function inject(id: string, detected: Shell) {
    shell = detected;
    promptUser = promptUserOf(lastLine()) ?? promptUser;
    sessionSetShell(id, detected).catch(() => {});
    swallower.start();
    lastInjectAt = Date.now();
    sessionWrite(id, injectionFor(detected));
  }
  function tryInjection(id: string) {
    if (injectDone || !sessionId) return;
    const finish = (r: string, detected?: string, injected = false) => {
      injectDone = true; reason = injected ? "" : r;
      console.info(`[charon] 133: detected=${detected ?? "none"} injected=${injected} reason=${r}`);
    };
    if (tracker.active) { finish("remote already emits OSC 133", shell); return; }
    const prompt = lastLine();
    const detected = detectShell(prompt);
    if (!detected) {
      // Not a prompt yet (MOTD pause, banner, password prompt): try again after the next quiet window.
      if (Date.now() < injectDeadline) { console.info("[charon] 133: no prompt yet, last line", JSON.stringify(prompt)); return; }
      finish(`no shell prompt recognised within 8 s, last line ${JSON.stringify(prompt)}`);
      return;
    }
    inject(id, detected);
    finish("hook typed in", detected, true);
  }
  /**
   * `sudo -i` / `su -` / nested `ssh`: the hook lives in the outer shell, so the
   * new one prints bare prompts while the outer command (sudo) is still running.
   * Re-inject when the last line looks like a shell prompt in that state.
   */
  function maybeReinject(id: string) {
    if (!sessionId || swallower.swallowing || reinjects >= MAX_REINJECTS || Date.now() - lastInjectAt < 3000) return;
    const running = tracker.marks.at(-1);
    if (!running || running.outputStartLine === undefined || running.endLine !== undefined) return;
    const line = lastLine();
    const detected = looksLikeShellPrompt(line);
    if (!detected) return;
    reinjects++;
    console.info(`[charon] 133: re-inject #${reinjects} into nested ${detected} shell, prompt ${JSON.stringify(line)}`);
    inject(id, detected);
  }
  /** Chip menu: re-inject now, stop for this session, copy the hook text. */
  function reinjectNow() {
    if (!sessionId) return;
    const detected = detectShell(lastLine()) ?? shell ?? "bash";
    injectDone = true; integrationEnabled = true; reason = "";
    console.info(`[charon] 133: manual re-inject as ${detected}`);
    inject(sessionId, detected);
  }
  function disableIntegration() {
    integrationEnabled = false; injectDone = true; clearTimeout(injectTimer);
    reason = "disabled for this session";
  }
  function chipItems(): MenuItem[] {
    const which = shell ?? "bash";
    return [
      { label: "Re-inject hook", action: reinjectNow, disabled: !sessionId },
      { label: "Disable for this session", action: disableIntegration, disabled: !integrationEnabled },
      { separator: true, label: "" },
      { label: `Copy hook text (${which})`, action: () => writeText(hookTextFor(which)) },
      ...shellItems(),
    ];
  }
  function onChipClick(e: MouseEvent) {
    e.stopPropagation();
    chipMenu = { x: e.clientX, y: e.clientY - 4 };
    // The shells the host has, asked once per session (own exec channel).
    if (open.kind === "ssh" && sessionId && status === "up" && (hostShells === null || typeof hostShells === "string")) {
      hostShells = null;
      sessionShells(sessionId)
        .then((h) => { hostShells = h; console.info(`[charon] shell: host offers ${h.os} ${h.shells.join(", ")}`); })
        .catch((err) => (hostShells = String(err)));
    }
  }

  // CSI ? 1049 l / 1047 l / 47 l — alternate screen exits.
  const ALT_EXITS = ["\x1b[?1049l", "\x1b[?1047l", "\x1b[?47l"].map((s) => Array.from(s, (c) => c.charCodeAt(0)));
  function exitsAltScreen(b: Uint8Array): boolean {
    outer: for (let i = 0; i < b.length; i++) {
      if (b[i] !== 0x1b) continue;
      for (const seq of ALT_EXITS) {
        if (i + seq.length > b.length) continue;
        for (let j = 1; j < seq.length; j++) if (b[i + j] !== seq[j]) continue outer;
        return true;
      }
    }
    return false;
  }

  // The connection's own font wins over the profile's; a connection with its own
  // size zooms this tab only (not saved), others zoom every tab via the profile.
  let zoomedSize = $state<number | null>(null);
  const fontFamily = () => (font?.family ? fontStack(font.family) : prefs.fontFamily);
  const fontSize = () => zoomedSize ?? font?.size ?? prefs.fontSize;
  function zoom(dir: -1 | 0 | 1) {
    if (font?.size) {
      zoomedSize = dir === 0 ? null : Math.max(6, Math.min(40, term.options.fontSize! + dir));
      return;
    }
    const size = dir === 0 ? 13 : Math.max(6, Math.min(40, term.options.fontSize! + dir));
    onfontsize(size); // flows back in through prefs
  }
  // Live-apply font changes from the profile (zoom in any tab, or an edit on disk).
  $effect(() => {
    const fontSize_ = fontSize(), fontFamily_ = fontFamily();
    if (!term) return;
    let changed = false;
    if (term.options.fontSize !== fontSize_) { term.options.fontSize = fontSize_; changed = true; }
    if (term.options.fontFamily !== fontFamily_) { term.options.fontFamily = fontFamily_; changed = true; }
    if (changed) fit.fit();
  });

  function openSearch() {
    searchOpen = true;
    queueMicrotask(() => { searchEl?.focus(); searchEl?.select(); });
  }
  function closeSearch() {
    searchOpen = false;
    search.clearDecorations();
    term.focus();
  }
  function onSearchKey(e: KeyboardEvent) {
    if (e.key === "Escape") { closeSearch(); return; }
    if (e.key === "Enter") { e.preventDefault(); if (e.shiftKey) search.findPrevious(query, searchOpts()); else search.findNext(query, searchOpts()); }
  }
  function onSearchInput() {
    if (query) search.findNext(query, { ...searchOpts(), incremental: true }); else search.clearDecorations();
  }

  // ---- links
  const isMac = navigator.userAgent.includes("Mac");
  function activateLink(e: MouseEvent, url: string) {
    // Ctrl+click (Cmd on macOS) so clicking around in a terminal never opens a browser by accident.
    if (!(isMac ? e.metaKey : e.ctrlKey)) return;
    if (!/^https?:\/\//i.test(url)) return;
    console.info(`[charon] link: open ${url}`);
    openUrl(url).catch((err) => console.info(`[charon] link: open failed ${String(err)}`));
  }
  function showLinkTip(e: MouseEvent, url: string) {
    linkTip = { x: e.clientX + 12, y: e.clientY + 14, url };
  }
  /** Buffer row `y` (0-based) as text plus the cell column of every string index. */
  function rowAt(y: number): Row | null {
    const line = term.buffer.active.getLine(y);
    if (!line) return null;
    let text = "";
    const cols: number[] = [];
    for (let x = 0; x < term.cols; x++) {
      const cell = line.getCell(x);
      if (!cell) break;
      if (cell.getWidth() === 0) continue; // right half of a wide character
      const ch = cell.getChars() || " ";
      for (let i = 0; i < ch.length; i++) cols.push(x);
      text += ch;
    }
    return { text, cols, wrapped: line.isWrapped };
  }
  /** xterm link provider: links touching buffer row `y1` (1-based). Looks a few rows
   *  up and down so a URL broken over several rows is one link on each of them. */
  function linksOnRow(y1: number) {
    const y = y1 - 1, from = Math.max(0, y - 6), to = y + 6;
    const rows: Row[] = [];
    for (let i = from; i <= to; i++) { const r = rowAt(i); if (!r) break; rows.push(r); }
    return findLinks(rows, term.cols)
      .filter((l) => l.start.row + from <= y && l.end.row + from >= y)
      .map((l) => ({
        range: { start: { x: l.start.col + 1, y: l.start.row + from + 1 }, end: { x: l.end.col + 1, y: l.end.row + from + 1 } },
        text: l.url,
        decorations: { underline: true, pointerCursor: true },
        activate: (e: MouseEvent) => activateLink(e, l.url),
        hover: (e: MouseEvent) => showLinkTip(e, l.url),
        leave: () => (linkTip = null),
      }));
  }

  // MobaXterm habit: right-click copies the selection (and drops it) or pastes;
  // Shift+right-click always opens the menu.
  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    if (prefs.rightClick === "moba" && !e.shiftKey) {
      if (term.hasSelection()) { copySelection(); term.clearSelection(); } else pasteFromClipboard("right-click");
      return;
    }
    menu = { x: e.clientX, y: e.clientY, items: menuItems() };
  }
  function menuItems(): MenuItem[] {
    return [
      { label: "Copy", action: copySelection, disabled: !term.hasSelection() },
      { label: "Paste", action: () => pasteFromClipboard("menu"), disabled: !sessionId },
      { separator: true, label: "" },
      { label: "Select all", action: () => term.selectAll() },
      { label: "Clear scrollback", action: () => term.clear() },
      { separator: true, label: "" },
      { label: "Copy last output", disabled: !tracker.last(), action: () => { const m = tracker.last(); if (m) writeText(cleanForCopy(tracker.outputOf(m))); } },
      { label: "Copy last command", disabled: !tracker.last()?.commandText, action: () => { const m = tracker.last(); if (m?.commandText) writeText(cleanForCopy(m.commandText)); } },
      { separator: true, label: "" },
      { label: "Find…  Ctrl+Shift+F", action: openSearch },
      { label: `${filesOpen ? "Hide" : "Show"} files  Ctrl+Shift+E`, action: toggleFiles, disabled: !hasFiles },
      { label: `Timestamps: ${prefs.timestamps ? "on → turn off" : "off → turn on"}`, action: toggleTimestamps },
    ];
  }

  async function copySelection() {
    const sel = term.getSelection();
    if (!sel) return;
    try { await writeText(cleanForCopy(sel, { smartQuotes: prefs.smartQuotesToAscii, trimTrailing: prefs.trimTrailingWhitespace })); }
    catch (e) { console.warn(`[charon] clipboard: write failed: ${e}`); }
  }
  /** Pixel anchor (fixed coords) just below the end of the selection; falls back to
   *  the terminal's bottom-right when the end is scrolled off or internals move. */
  function selectionAnchor(): { x: number; y: number } {
    const rect = el.getBoundingClientRect();
    const corner = { x: rect.right - 72, y: rect.bottom - 34 };
    try {
      const pos = term.getSelectionPosition();
      if (!pos) return corner;
      const cell = (term as unknown as { _core: { _renderService: { dimensions: { css: { cell: { width: number; height: number } } } } } })._core._renderService.dimensions.css.cell;
      const row = pos.end.y - term.buffer.active.viewportY; // viewport-relative
      if (row < 0 || row >= term.rows) return corner;
      const x = Math.min(rect.left + 12 + pos.end.x * cell.width + 4, rect.right - 64);
      const y = Math.min(rect.top + 4 + (row + 1) * cell.height + 2, rect.bottom - 30);
      return { x: Math.max(8, x), y };
    } catch {
      return corner;
    }
  }
  function copyFromButton() {
    copySelection();
    term.clearSelection();
    copyBtn = null;
    term.focus();
  }
  /** Every paste route (Ctrl+Shift+V, menu, middle click, native paste event) ends here. */
  function pasteToSession(text: string, route: string) {
    if (!text || !sessionId) return;
    // The flag picker must never react to pasted text: close it now and keep it
    // shut until the next prompt (else a trailing `--all` pops it and eats Enter).
    pickerGate.pasteBegin();
    clearTimeout(flagEvalTimer);
    closeFlagPicker();
    const cleaned = cleanForCopy(text, { trimTrailing: false, smartQuotes: prefs.smartQuotesToAscii });
    // Guard only on a real warning: ≥ paste_guard_lines content lines, hidden
    // characters, or `$ `-prefixed doc lines. An ordinary one-line paste goes
    // straight through. Inline, non-modal bar (xterm keeps focus; Enter/Esc resolve).
    const warns = pasteWarnings(text, prefs.pasteGuardLines);
    if (warns.length) { pasteGuard = { cleaned, warns, route }; return; }
    sendPaste(cleaned, route);
  }
  /** Send a (sanitised) paste, or queue it behind an in-flight hidden/injected command. */
  function sendPaste(cleaned: string, route: string) {
    if (!sessionId) return;
    const bracketed = !!term.modes.bracketedPasteMode;
    // A hidden command's echo is being swallowed → don't interleave; queue and flush
    // when it ends (the backend also queues writes during its Capture window).
    const queued = swallower.swallowing;
    console.info(`[charon] paste: route=${route} bytes=${cleaned.length} bracketed=${bracketed} queued=${queued}`);
    if (queued) { pendingPaste.push({ cleaned, route }); return; }
    writePaste(cleaned, bracketed);
    term.focus();
  }
  function writePaste(cleaned: string, bracketed: boolean) {
    pickerGate.send(() => writePasteNow(cleaned, bracketed));
  }
  function writePasteNow(cleaned: string, bracketed: boolean) {
    // Bracketed + "auto" + trailing newline: paste the body inside the 200~/201~
    // markers, then send the newline *after* 201~ so the shell executes it instead
    // of leaving it highlighted. Otherwise xterm's paste keeps the newline inside.
    if (bracketed && prefs.pasteRun === "auto" && /\n$/.test(cleaned)) {
      term.paste(cleaned.replace(/\r?\n$/, ""));
      if (sessionId) sessionWrite(sessionId, "\r");
    } else {
      term.paste(cleaned);
    }
  }
  function flushPendingPaste() {
    if (!pendingPaste.length) return;
    const items = pendingPaste; pendingPaste = [];
    for (const it of items) {
      const bracketed = !!term.modes.bracketedPasteMode;
      console.info(`[charon] paste: route=${it.route} bytes=${it.cleaned.length} bracketed=${bracketed} queued=false (flushed)`);
      writePaste(it.cleaned, bracketed);
    }
    term.focus();
  }
  function confirmPaste() {
    if (!pasteGuard) return;
    const g = pasteGuard; pasteGuard = null;
    sendPaste(g.cleaned, g.route);
  }
  function cancelPaste() {
    if (!pasteGuard) return;
    console.info(`[charon] paste: route=${pasteGuard.route} bytes=${pasteGuard.cleaned.length} bracketed=${!!term.modes.bracketedPasteMode} queued=false (cancelled)`);
    pasteGuard = null; pickerGate.pasteCancelled(); term.focus();
  }
  async function pasteFromClipboard(route: string) {
    let text: string;
    try { text = await readText(); } catch (e) { console.warn(`[charon] clipboard: read failed route=${route}: ${e}`); return; }
    pasteToSession(text, route);
  }

  // ---- sudo password prompt (T3)
  /** Detect a sudo/su password prompt on the live line; colon-gated so a flood is cheap. */
  function maybeSudoPrompt(b: Uint8Array) {
    if (!sessionId || winLocalNoSudo) return;
    if (sudoPrompt) {
      // Clear once the sudo line is gone from screen (answered, scrolled, new prompt).
      // Not gated on OSC 133: a session without shell integration must still work.
      if (!looksLikeSudoPrompt(lastLine())) { sudoPrompt = null; clearTimeout(sudoAutoTimer); }
      return;
    }
    if (!b.includes(0x3a)) return;   // cheap gate: sudo/su prompts contain ':'
    const line = lastLine();
    if (looksLikeSudoPrompt(line)) onSudoPrompt(line);
  }
  async function onSudoPrompt(line: string) {
    const user = sudoUserOf(line);
    sudoPrompt = { user };
    if (sudoHasPw === null && sessionId) sudoHasPw = await sudoHasPassword(sessionId).catch(() => false);
    // Auto-send only for the login user's own sudo prompt; a bare `Password:` (su)
    // or another user's prompt shows the button but never auto-sends.
    const eligible = sudoAutoEligible(line, loginUser);
    const auto = !!sudoHasPw && prefs.sudoAutoSend && eligible;
    // One diagnostic line (no secret): hasPassword drives the button; eligible +
    // autoSend drive auto-send; loginUser is the credential owner we'd send.
    console.info(`[charon] sudo: prompt detected user=${user ?? "?"} loginUser=${loginUser ?? "?"} hasPassword=${sudoHasPw} autoSend=${prefs.sudoAutoSend} eligible=${eligible} button=${!!sudoHasPw} sent=${auto}`);
    if (auto) sudoAutoTimer = setTimeout(() => sendSudo(), 300);
  }
  async function sendSudo() {
    clearTimeout(sudoAutoTimer);
    if (!sessionId || !sudoPrompt) return;
    let ok = false;
    try { ok = await sudoSendPassword(sessionId); } catch { ok = false; }
    console.info(`[charon] sudo: password send requested sent=${ok}`);
    sudoPrompt = null;
  }

  /** Load the renderer prefs.renderer names (Settings → Terminal); none = DOM. */
  function enableWebgl() {
    if (prefs.renderer === "canvas" && !canvas) {
      try { canvas = new CanvasAddon(); term.loadAddon(canvas); } catch { canvas?.dispose(); canvas = null; }
      return;
    }
    if (webgl || prefs.renderer !== "webgl" || !webglUsable()) return;
    try {
      webgl = new WebglAddon();
      // A lost GL context (driver reset, sleep/wake, or a flaky WebKitGTK GL stack)
      // would leave a blank terminal; drop back to the DOM renderer instead.
      webgl.onContextLoss(() => { webgl?.dispose(); webgl = null; });
      term.loadAddon(webgl);
    } catch { webgl?.dispose(); webgl = null; } // no GL here → DOM renderer
  }
  function disableWebgl() {
    if (webgl) { webgl.dispose(); webgl = null; }
    if (canvas) { canvas.dispose(); canvas = null; }
  }
  const rendererName = () => (webgl ? "webgl" : canvas ? "canvas" : "dom");
  // The renderer setting changed: swap the addon on a shown tab.
  $effect(() => {
    const want = prefs.renderer;
    untrack(() => {
      // Hidden tabs pick it up when shown (enableWebgl on activation).
      if (!term || !active || rendererName() === want) return;
      disableWebgl(); enableWebgl(); term.refresh(0, term.rows - 1);
      console.info(`[charon] renderer: ${rendererName()} (setting ${want})`);
    });
  });

  /** Buffer output for a hidden tab; drop oldest whole chunks past the cap. */
  function bufferHidden(b: Uint8Array) {
    pendingWrites.push(b); pendingTimes.push(Date.now()); pendingBytes += b.length;
    while (pendingBytes > HIDDEN_BUF_CAP && pendingWrites.length > 1) {
      pendingBytes -= pendingWrites.shift()!.length; pendingTimes.shift(); pendingOverflow = true;
    }
  }
  /** Write everything buffered while hidden; reset first if we had to drop bytes. */
  function flushHidden() {
    if (!pendingWrites.length && !pendingOverflow) return;
    if (pendingOverflow) { term.reset(); pendingOverflow = false; }
    const items = pendingWrites, times = pendingTimes; pendingWrites = []; pendingTimes = []; pendingBytes = 0;
    // Each chunk's lines are stamped with when it arrived, not when it is shown:
    // a write callback runs once everything before it is parsed.
    items.forEach((b, i) => { term.write("", () => (stampClock = times[i])); term.write(b); });
    term.write("", () => (stampClock = null));
  }

  /** A new line started (line feed): stamp it if it is the first in this second. */
  function recordStamp() {
    if (term.buffer.active.type !== "normal") return; // full-screen programs: no stamps
    const now = stampClock ?? Date.now();
    const sec = Math.floor(now / 1000);
    if (sec === lastStampSec) return;
    lastStampSec = sec;
    while (stampMarks.length && stampMarks[0].marker.isDisposed) stampMarks.shift();
    const marker = term.registerMarker(0);
    if (marker) stampMarks.push({ marker, t: now });
  }
  function scheduleGutter() {
    if (!prefs.timestamps || !active || gutterFrame) return;
    gutterFrame = requestAnimationFrame(() => { gutterFrame = 0; drawGutter(); });
  }
  function drawGutter() {
    const b = term.buffer.active;
    if (b.type !== "normal") { gutter = []; return; }
    const screen = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
    if (screen) cellH = screen.getBoundingClientRect().height / term.rows;
    const stamps = stampMarks.filter((s) => !s.marker.isDisposed).map((s) => ({ line: s.marker.line, t: s.t }));
    gutter = gutterRows(stamps, b.viewportY, term.rows, (l) => {
      const line = b.getLine(l);
      return line ? { text: line.translateToString(true), wrapped: line.isWrapped } : undefined;
    });
  }
  function toggleTimestamps() {
    ontimestamps?.(!prefs.timestamps);
    term?.focus();
  }
  // Switched on (here or in another tab): draw now; the terminal narrows to make room.
  $effect(() => {
    const on = prefs.timestamps;
    if (!term) return;
    untrack(() => { if (on) requestAnimationFrame(drawGutter); else gutter = []; fit?.fit(); });
  });

  /** Reconnect into another shell; a saved connection remembers it. */
  async function chooseShell(sh: string | null) {
    shellOverride = sh ?? "";
    if (open.nodeId) {
      try { await connectionSetRemoteShell(open.nodeId, sh); notifyChanged("connections"); }
      catch (e) { console.warn("[charon] shell: could not save", e); }
    }
    console.info(`[charon] shell: reconnecting with ${sh ?? "the login shell"}`);
    reconnect();
  }
  function shellItems(): MenuItem[] {
    if (open.kind !== "ssh") return [];
    const mark = (s: string) => (s === startedShell ? "✓ " : "\u2003");
    const items: MenuItem[] = [
      { separator: true, label: "" },
      { label: open.nodeId ? "Start this connection in… (reconnects, saved)" : "Start this tab in… (reconnects)", disabled: true },
      { label: `${mark("")}login shell (account default)`, action: () => chooseShell(null), disabled: !sessionId && status !== "closed" },
    ];
    if (typeof hostShells === "string") items.push({ label: `could not list shells: ${hostShells}`, disabled: true });
    else if (hostShells) for (const s of hostShells.shells) items.push({ label: `${mark(s)}${s}`, action: () => chooseShell(s) });
    else items.push({ label: "listing shells…", disabled: true });
    return items;
  }
  /** Selecting a tab puts the keyboard in it: the password field while asking for one,
   *  otherwise the terminal. Waits a frame for the pane to lose `hidden`. */
  function focusShown() {
    requestAnimationFrame(() => {
      if (!active) return;
      if (status === "auth") passwordEl?.focus();
      else if (status !== "hostkey") term?.focus();
    });
  }
  // Show/hide: (re)create or drop the GL context and flush any buffered output.
  $effect(() => {
    const a = active;
    if (!term) return;
    untrack(() => {
      if (a) { enableWebgl(); flushHidden(); fit?.fit(); focusShown(); if (integrationEnabled && sessionId) scheduleQuietCheck(sessionId); }
      else { disableWebgl(); }
    });
  });

  function writeQueueDepth(): number {
    try {
      const wb = (term as unknown as { _core: { _writeBuffer: { _writeBuffer: unknown[]; _bufferOffset: number } } })._core._writeBuffer;
      if (wb?._writeBuffer) return wb._writeBuffer.length - (wb._bufferOffset ?? 0);
    } catch { /* internals moved */ }
    return pendingWrites.length;
  }
  function logPerf() {
    const evs = evCount, bs = byteCount; evCount = 0; byteCount = 0;
    // Stay quiet when the tab is idle; only interesting under load.
    if (evs === 0 && bs === 0 && pendingBytes === 0) return;
    const renderer = !active ? "buffered" : rendererName();
    const tab = sessionId ?? open.host ?? open.kind;
    console.info(`[charon] perf tab=${tab} events/s=${(evs / 5).toFixed(1)} bytes/s=${(bs / 5).toFixed(0)} wq=${writeQueueDepth()} pending=${pendingBytes} renderer=${renderer}`);
  }

  /** Probe for a usable WebGL2 context before loading the addon: WebKitGTK can
   * lack GL (software or headless), where the addon would throw or render blank. */
  function webglUsable(): boolean {
    try {
      const c = document.createElement("canvas");
      return !!(c.getContext("webgl2") || c.getContext("webgl"));
    } catch { return false; }
  }

  const ro = new ResizeObserver(() => fit?.fit());
  $effect(() => { ro.observe(el); return () => ro.disconnect(); });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="wrap" style:--session-color={color ?? "var(--accent)"}>
  <div class="split">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="termbox" oncontextmenu={onContextMenu}>
    {#if prefs.timestamps}
      <div class="stamps" style:--cell-h="{cellH}px" aria-hidden="true" title="When each line started (local time)">
        {#each gutter as g, i (i)}<div class:rep={g.repeat}>{g.text}</div>{/each}
      </div>
    {/if}
    <div class="term" bind:this={el}></div>
    {#if searchOpen}
      <div class="search">
        <input bind:this={searchEl} bind:value={query} placeholder="Find" spellcheck="false"
          onkeydown={onSearchKey} oninput={onSearchInput} />
        <button type="button" title="Previous (Shift+Enter)" onclick={() => search.findPrevious(query, searchOpts())}>▲</button>
        <button type="button" title="Next (Enter)" onclick={() => search.findNext(query, searchOpts())}>▼</button>
        <button type="button" title="Close (Esc)" onclick={closeSearch}>×</button>
      </div>
    {/if}
  </div>
  {#if filesOpen && hasFiles && sessionId}
    <div class="vsplit" role="separator" aria-orientation="vertical" aria-label="Resize file pane" onpointerdown={startPaneDrag}></div>
    <div class="files" style:width="{filesWidth}px">
      <SftpPane session={open.kind === "local" ? null : sessionId} {cwd} {color} {sshUser} shellUser={promptUser} isIdle={shellIdle} parallel={prefs.filesParallel}
        onopen={onopenfile ? (p, via) => onopenfile?.(p, open.kind === "local" ? null : sessionId, sessionId, via) : undefined}
        onchdir={chdir} onclose={toggleFiles} />
    </div>
  {/if}
  </div>
  {#if copyBtn}
    <!-- mousedown preventDefault keeps the selection while the click copies it. -->
    <button type="button" class="sel-copy" style:left="{copyBtn.x}px" style:top="{copyBtn.y}px"
      onmousedown={(e) => e.preventDefault()} onclick={copyFromButton} title="Copy selection (Ctrl+Shift+C)">Copy</button>
  {/if}
  {#if linkTip}
    <div class="link-tip" style:left="{linkTip.x}px" style:top="{linkTip.y}px">{linkTip.url}<span>{isMac ? "⌘" : "Ctrl"}+click to open</span></div>
  {/if}
  {#if mouseHint}
    <div class="mouse-hint" role="status">This program has the mouse. Hold <b>{isMac ? "Option" : "Shift"}</b> and drag to select text for copying.</div>
  {/if}
  {#if menu}
    <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
  {/if}
  {#if chipMenu}
    <ContextMenu x={chipMenu.x} y={chipMenu.y} items={chipItems()} onclose={() => (chipMenu = null)} />
  {/if}
  {#if flagPopup}
    <FlagPopup x={flagPopup.x} y={flagPopup.y} command={flagPopup.command} flags={flagList} selected={flagSel} loading={flagLoading}
      onaccept={(i) => acceptFlagAt(i)} />
  {/if}
  {#if status === "hostkey" && hostkey}
    <div class="hostkey">
      <div class="hk-title">Unknown host key for <span class="mono">{hostkey.host}:{hostkey.port}</span></div>
      <div class="hk-fp mono">{hostkey.key_type} {hostkey.fingerprint}</div>
      <div class="hk-note">Not in <span class="mono">{hostkey.known_hosts}</span> or ~/.ssh/known_hosts. Compare the fingerprint with the server (<span class="mono">ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub</span>) before trusting it. Answer within 2 minutes: sshd drops unauthenticated connections after its login grace time.</div>
      <div class="hk-btns">
        {#if hkError}<span class="err">{hkError}</span>{/if}
        {#if hkAnswered}<span class="dim">{hkAnswered === "accept" ? "accepted, continuing…" : "rejected, closing…"}</span>{/if}
        <button type="button" disabled={hkAnswered !== null} onclick={() => answerHostKey(false)}>Reject</button>
        <button type="button" class="primary" disabled={hkAnswered !== null} onclick={() => answerHostKey(true)}>Accept &amp; save</button>
      </div>
    </div>
  {/if}
  {#if changed}
    <div class="hostkey">
      <div class="hk-title">Host key changed for <span class="mono">{changed.host}:{changed.port}</span></div>
      <div class="hk-fp mono">{changed.key_type} {changed.fingerprint}</div>
      {#if changed.own}
        <div class="hk-note">The stale key is in Charon's own <span class="mono">{changed.file}</span> (line {changed.line}). Only do this if you know the host was reinstalled or its keys rotated.</div>
        <div class="hk-btns">
          <button type="button" class="primary" onclick={replaceKey}>Host was reinstalled — replace key</button>
        </div>
      {:else}
        {@const cmd = sshKeygenRemove(changed.host, changed.port)}
        <div class="hk-note">The stale key is in your <span class="mono">{changed.file}</span> (line {changed.line}), which Charon never edits. If the host was reinstalled, remove it and reconnect:</div>
        <div class="hk-btns">
          <code class="cmd">{cmd}</code>
          <button type="button" onclick={() => writeText(cmd)}>Copy</button>
        </div>
      {/if}
    </div>
  {/if}
  {#if pasteGuard}
    <div class="pasteguard" role="alert">
      <span class="pg-icon">⚠</span>
      <span class="pg-msg">Paste {pasteGuard.warns.join(", ")}?</span>
      <span class="pg-hint">Enter to paste · Esc to cancel</span>
      <button type="button" class="primary" onclick={confirmPaste}>Paste</button>
      <button type="button" onclick={cancelPaste}>Cancel</button>
    </div>
  {/if}
  {#if status === "auth"}
    <form class="auth" onsubmit={submitAuth}>
      <span class="who">{open.user ? open.user + "@" : ""}{open.host ?? "saved host"}</span>
      <input type="password" placeholder="Password" bind:value={password} bind:this={passwordEl}
        autocomplete="off" spellcheck="false" />
      <label class="save">
        <input type="checkbox" checked={saveAs !== ""} onchange={(e) => (saveAs = e.currentTarget.checked ? defaultSaveName() : "")} />
        save to vault as
      </label>
      <input class="name" placeholder="name" bind:value={saveAs} disabled={saveAs === ""} spellcheck="false" />
      <button type="submit" disabled={!password}>Connect</button>
      {#if authError}<span class="err">{authError}</span>{/if}
    </form>
  {/if}
  <div class="status" class:up={status === "up"} class:closed={status === "closed"}>
    <span class="dot"></span>{status}{error ? ` — ${error}` : ""}
    {#if status === "closed"}
      <button type="button" class="chip reconnect" onclick={reconnect} title="Reconnect reusing this connection's host and credentials">reconnect</button>
    {/if}
    {#if sudoPrompt && sudoHasPw && status === "up"}
      <button type="button" class="chip sudo" onclick={sendSudo}
        title="Send the saved password to the {sudoPrompt.user ? `sudo prompt for ${sudoPrompt.user}` : 'password prompt'} (Ctrl+Shift+P). Not echoed.">
        send saved password
      </button>
    {/if}
    <button type="button" class="chip stamps-btn" class:on={prefs.timestamps} onclick={toggleTimestamps}
      title="Show when each line was printed (all tabs; remembered)">time {prefs.timestamps ? "on" : "off"}</button>
    {#if hasFiles && status === "up"}
      <button type="button" class="chip files-btn" class:on={filesOpen} onclick={toggleFiles} title="File pane (Ctrl+Shift+E)">files</button>
    {/if}
    {#if cwd}<span class="cwd mono" title="Working directory (OSC 7)">{cwd}</span>{/if}
    {#if status === "up" || osc133}
      <button type="button" class="chip" onclick={onChipClick} title={"Shell integration: detected shell · OSC 133 seen · commands recorded. Click for options." + (reason ? `\n133 off: ${reason}` : "")}>
        {shell ?? "shell?"} · 133 {osc133 ? "on" : `off${reason ? ` (${reason.split(",")[0]})` : ""}`} · {markCount} {markCount === 1 ? "mark" : "marks"}
      </button>
    {/if}
  </div>
</div>

<style>
  .wrap { position: relative; height: 100%; display: flex; flex-direction: column; border-left: 3px solid var(--session-color); }
  .split { flex: 1; min-height: 0; display: flex; }
  .termbox { flex: 1; min-height: 0; min-width: 0; position: relative; display: flex; }
  .vsplit { width: 4px; cursor: col-resize; background: var(--line); }
  .vsplit:hover, .vsplit:active { background: var(--session-color); }
  .files { flex: none; min-height: 0; }
  /* Left padding is the command-mark gutter (osc133.ts draws bars at -8px). */
  .term { flex: 1; min-height: 0; min-width: 0; padding: 4px 4px 4px 12px; background: var(--term-bg); }
  .stamps { flex: none; padding: 4px 0 0 6px; background: var(--term-bg); color: var(--fg-dim); font-family: var(--font-mono); font-size: max(9px, calc(var(--cell-h) * 0.62)); overflow: hidden; user-select: none; border-right: 1px solid var(--line); }
  .stamps div { height: var(--cell-h); line-height: var(--cell-h); white-space: pre; padding-right: 6px; min-width: 8ch; }
  .stamps div.rep { opacity: .4; }
  .search { position: absolute; top: 6px; right: 18px; display: flex; gap: 2px; padding: 4px; background: var(--bg-raised); border: 1px solid var(--line); border-radius: 4px; box-shadow: 0 4px 14px var(--shadow); }
  .search input { width: 220px; font-family: var(--font-mono); }
  .search button { padding: 2px 7px; }
  /* Prominent, obviously-modal-looking bar so a held paste can't be missed. */
  .pasteguard { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 8px 10px; background: color-mix(in srgb, var(--warn) 14%, transparent); border-top: 2px solid var(--warn); font-size: 12px; color: var(--fg); }
  .pasteguard .pg-icon { color: var(--warn); font-size: 14px; }
  .pasteguard .pg-msg { font-weight: 600; }
  .pasteguard .pg-hint { margin-right: auto; color: var(--fg-dim); }
  .auth { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 6px 8px; background: var(--bg-raised); border-top: 1px solid var(--line); }
  .auth .who { font-family: var(--font-mono); color: var(--fg-dim); }
  .auth input[type="password"] { width: 180px; font-family: var(--font-mono); }
  .auth .save { display: flex; align-items: center; gap: 4px; color: var(--fg-dim); }
  .auth .name { width: 160px; font-family: var(--font-mono); }
  .auth .name:disabled { opacity: .4; }
  .auth .err { color: var(--danger); }
  .hostkey { padding: 8px 10px; background: var(--bg-raised); border-top: 1px solid var(--line); display: flex; flex-direction: column; gap: 4px; font-size: 12px; }
  .hk-title { color: var(--fg); font-weight: 600; }
  .hk-fp { color: var(--warn); font-size: 13px; }
  .cmd { font-family: var(--font-mono); background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 3px; padding: 3px 8px; user-select: all; margin-right: auto; }
  .hk-note { color: var(--fg-dim); }
  .hk-btns { display: flex; gap: 6px; justify-content: flex-end; align-items: center; }
  .hk-btns .dim { color: var(--fg-dim); }
  .hk-btns .err { color: var(--danger); }
  .mono { font-family: var(--font-mono); }
  .primary { border-color: var(--accent); }
  .status { font-size: 11px; color: var(--fg-dim); padding: 2px 8px; border-top: 1px solid var(--line); display: flex; align-items: center; gap: 6px; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--fg-dim); }
  .files-btn { margin-left: auto; }
  .files-btn.on { border-color: var(--session-color); color: var(--fg); }
  .files-btn + .cwd, .files-btn + .chip { margin-left: 0; }
  .stamps-btn { margin-left: auto; }
  .stamps-btn.on { border-color: var(--session-color); color: var(--fg); }
  .stamps-btn + .files-btn, .stamps-btn + .cwd, .stamps-btn + .chip { margin-left: 0; }
  .cwd { margin-left: auto; color: var(--fg-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 40%; }
  .cwd + .chip { margin-left: 0; }
  .chip { margin-left: auto; font: inherit; font-family: var(--font-mono); color: inherit; background: transparent; padding: 0 6px; border: 1px solid var(--line); border-radius: 8px; cursor: pointer; }
  .reconnect { margin-left: 6px; border-color: var(--accent); color: var(--fg); }
  .sudo { margin-left: 6px; border-color: var(--warn); color: var(--warn); }
  .sel-copy { position: fixed; z-index: 25; padding: 2px 10px; font: inherit; font-size: 12px; color: var(--fg); background: var(--bg-raised); border: 1px solid var(--accent); border-radius: 4px; box-shadow: 0 3px 10px var(--shadow); cursor: pointer; }
  .sel-copy:hover { background: var(--accent); color: var(--on-accent); }
  .link-tip { position: fixed; z-index: 26; max-width: 520px; padding: 3px 8px; font-size: 11px; color: var(--fg); background: var(--bg-raised); border: 1px solid var(--line); border-radius: 4px; box-shadow: 0 3px 10px var(--shadow); pointer-events: none; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .link-tip span { margin-left: 8px; color: var(--fg-dim); }
  .mouse-hint { position: absolute; z-index: 24; left: 50%; bottom: 14px; transform: translateX(-50%); padding: 5px 12px; font-size: 12px; color: var(--fg); background: var(--bg-raised); border: 1px solid var(--accent); border-radius: 4px; box-shadow: 0 3px 10px var(--shadow); pointer-events: none; }
  .chip:hover { border-color: var(--fg-dim); color: var(--fg); }
  .up .dot { background: var(--ok); } .closed .dot { background: var(--danger); }
</style>
