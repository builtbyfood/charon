<script lang="ts">
  // Notepad++-style editor tab: one Monaco instance, an inner tab strip of
  // documents (local files, remote files via SFTP, untitled). Remote saves go
  // back through file_write, which replaces atomically.
  import { onMount, onDestroy } from "svelte";
  import { open as pickOpen, save as pickSave } from "@tauri-apps/plugin-dialog";
  import { monaco, languageFor, languageIds, registerEditorExtras } from "./monaco";
  import { fileRead, fileWrite, isPermissionError, sftpSudoOpen, readText, type Encoding, type Eol, type FsVia } from "./api";
  import { canRetryAsSudo, ensureSudo } from "./sudoedit";
  import { cleanForCopy } from "./clipboard";
  import { basename } from "./paths";
  import { currentTheme } from "./themes";

  let { onsend, ondirty }: {
    /** Ctrl+Enter: selection (or current line) to the session the file came from. */
    onsend?: (text: string, session: string) => void;
    /** Any document changed dirty state (tab title indicator). */
    ondirty?: (anyDirty: boolean) => void;
  } = $props();

  interface Doc {
    id: string;
    /** Filesystem the file lives on: session id (SSH → SFTP) or null (local disk). */
    fs: string | null;
    /** Session to send selections to, if it was opened from a terminal tab. */
    origin: string | null;
    /** Backend for remote files: SFTP, or the interactive shell after sudo -i. */
    via: FsVia;
    path: string | null; // null = untitled
    name: string;
    language: string;
    encoding: Encoding;
    eol: Eol;
    dirty: boolean;
    error: string;
    /** The last save failed with EACCES: offer "Retry as sudo". */
    retrySudo: boolean;
  }
  let docs = $state<Doc[]>([]);
  let activeId = $state<string | null>(null);
  const active = $derived(docs.find((d) => d.id === activeId) ?? null);
  // Monaco objects stay out of $state (they must not be proxied).
  const models = new Map<string, monaco.editor.ITextModel>();
  const savedVersion = new Map<string, number>();
  const viewStates = new Map<string, monaco.editor.ICodeEditorViewState | null>();
  const eolGlyphs = new Map<string, monaco.editor.IEditorDecorationsCollection>();

  let host: HTMLDivElement;
  let editor: monaco.editor.IStandaloneCodeEditor;
  let showAll = $state(false);
  let wrap = $state(false);
  let minimap = $state(false);
  let cursor = $state("Ln 1, Col 1");
  let status = $state("");
  const languages = languageIds();
  let untitledN = 1;
  // Inline sudo password ask (window.prompt is not available in the webview).
  let sudoAsk = $state<{ session: string; resolve: (pw: string | null) => void } | null>(null);
  let sudoPw = $state("");
  let sudoPwEl = $state<HTMLInputElement | null>(null);

  onMount(() => {
    registerEditorExtras();
    editor = monaco.editor.create(host, {
      theme: currentTheme().id,
      automaticLayout: true,
      fontFamily: '"JetBrains Mono", "Cascadia Mono", Consolas, "DejaVu Sans Mono", monospace',
      fontSize: 13,
      minimap: { enabled: minimap },
      scrollBeyondLastLine: false,
      renderWhitespace: "selection",
      // P4: turn on Monaco's built-in smarts.
      wordBasedSuggestions: "allDocuments",
      quickSuggestions: { other: true, comments: false, strings: true },
      suggestOnTriggerCharacters: true,
      tabCompletion: "on",
      bracketPairColorization: { enabled: true },
      guides: { bracketPairs: true, indentation: true },
      multiCursorModifier: "alt",
      formatOnPaste: false,
      model: null,
    });
    editor.onDidChangeCursorPosition((e) => { cursor = `Ln ${e.position.lineNumber}, Col ${e.position.column}`; });
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => save());
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => sendSelection());
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyV, () => pasteClean());
    editor.addCommand(monaco.KeyMod.Alt | monaco.KeyCode.KeyZ, () => toggleWrap());
    // F1 opens Monaco's command palette by default; expose a Format action too.
    editor.addAction({ id: "charon.format", label: "Format Document", keybindings: [monaco.KeyMod.Shift | monaco.KeyMod.Alt | monaco.KeyCode.KeyF], contextMenuGroupId: "1_modification", run: () => format() });
    editor.addAction({ id: "charon.pasteClean", label: "Paste clean (sanitised)", contextMenuGroupId: "9_cutcopypaste", run: () => pasteClean() });
    editor.addAction({ id: "charon.send", label: "Send selection to terminal", contextMenuGroupId: "navigation", run: () => sendSelection() });
  });
  onDestroy(() => {
    editor?.dispose();
    for (const m of models.values()) m.dispose();
  });

  export function hasUnsaved(): boolean { return docs.some((d) => d.dirty); }

  /** Root SFTP for `session`: reuse/stored password, else ask here (unless `interactive` is false, e.g. MCP). */
  function sudoFor(session: string, interactive = true): Promise<void> {
    return ensureSudo(session, {
      open: sftpSudoOpen,
      ask: interactive ? () => new Promise<string | null>((resolve) => { sudoPw = ""; sudoAsk = { session, resolve }; queueMicrotask(() => sudoPwEl?.focus()); }) : undefined,
    });
  }
  function answerSudo(pw: string | null) { const a = sudoAsk; sudoAsk = null; sudoPw = ""; a?.resolve(pw); editor?.focus(); }

  /** Open (or focus) a file. `fs` null = local disk. via "sudo" opens it as root over
   *  sudo-sftp. `escalate` false (MCP) never falls back to the root shell on EACCES;
   *  `interactive` false never asks for a sudo password. Throws on failure. */
  export async function openFile(path: string, fs: string | null, origin: string | null = null, via: FsVia = "sftp",
    opts: { escalate?: boolean; interactive?: boolean } = {}): Promise<void> {
    try {
      if (via === "sudo" && fs) await sudoFor(fs, opts.interactive ?? true);
      const existing = docs.find((d) => d.path === path && d.fs === fs);
      if (existing) {
        // Same file asked for as root: rebind the open document to the sudo backend.
        if (via === "sudo" && existing.via !== "sudo") { existing.via = "sudo"; existing.retrySudo = false; }
        activate(existing.id); return;
      }
      status = `Opening ${basename(path)}${via === "sudo" ? " as root" : ""}…`;
      let f;
      try { f = await fileRead(fs, path, via); }
      catch (e) {
        // SFTP runs as the login user; after sudo -i the shell can read what it cannot.
        if (fs && via === "sftp" && opts.escalate !== false && isPermissionError(e)) { via = "shell"; f = await fileRead(fs, path, via); } else throw e;
      }
      const doc = addDoc({ fs, origin, via, path, name: basename(path), encoding: f.encoding, eol: f.eol }, f.text);
      status = `${f.size.toLocaleString()} bytes${via === "sudo" ? " · root" : ""}`;
      activate(doc.id);
    } catch (e) {
      status = String(e);
      throw e;
    }
  }

  export function openUntitled(origin: string | null = null): void {
    const doc = addDoc({ fs: null, origin, via: "sftp", path: null, name: `new ${untitledN++}`, encoding: "utf-8", eol: navigator.userAgent.includes("Windows") ? "crlf" : "lf" }, "");
    activate(doc.id);
  }

  function addDoc(d: Omit<Doc, "id" | "language" | "dirty" | "error" | "retrySudo">, text: string): Doc {
    const id = crypto.randomUUID();
    const language = languageFor(d.name);
    const model = monaco.editor.createModel(text, language);
    model.setEOL(d.eol === "crlf" ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
    models.set(id, model);
    savedVersion.set(id, model.getAlternativeVersionId());
    model.onDidChangeContent(() => {
      const doc = docs.find((x) => x.id === id);
      if (!doc) return;
      const dirty = model.getAlternativeVersionId() !== savedVersion.get(id);
      if (dirty !== doc.dirty) { doc.dirty = dirty; ondirty?.(hasUnsaved()); }
      if (showAll && id === activeId) scheduleGlyphs();
    });
    const doc: Doc = { id, language, dirty: false, error: "", retrySudo: false, ...d };
    docs.push(doc);
    return doc;
  }

  function activate(id: string) {
    if (activeId && editor.getModel()) viewStates.set(activeId, editor.saveViewState());
    activeId = id;
    const model = models.get(id);
    if (!model) return;
    editor.setModel(model);
    const vs = viewStates.get(id);
    if (vs) editor.restoreViewState(vs);
    editor.focus();
    if (showAll) scheduleGlyphs();
  }

  async function closeDoc(id: string) {
    const doc = docs.find((d) => d.id === id);
    if (!doc) return;
    if (doc.dirty && !confirm(`Discard unsaved changes to ${doc.name}?`)) return;
    const idx = docs.indexOf(doc);
    docs.splice(idx, 1);
    models.get(id)?.dispose(); models.delete(id); savedVersion.delete(id); viewStates.delete(id); eolGlyphs.delete(id);
    if (activeId === id) {
      const next = docs[idx] ?? docs[idx - 1];
      if (next) activate(next.id); else { activeId = null; editor.setModel(null); }
    }
    ondirty?.(hasUnsaved());
  }

  async function save(doc = active) {
    if (!doc) return;
    const model = models.get(doc.id)!;
    let path = doc.path;
    if (!path) {
      // Untitled: local Save As. (Remote files always have a path.)
      const picked = await pickSave({ defaultPath: doc.name, title: "Save as" });
      if (typeof picked !== "string") return;
      path = picked; doc.path = path; doc.name = basename(path); doc.fs = null;
      const lang = languageFor(doc.name);
      if (lang !== doc.language) { doc.language = lang; monaco.editor.setModelLanguage(model, lang); }
    }
    status = `Saving ${doc.name}…`; doc.error = ""; doc.retrySudo = false;
    try {
      // A sudo document re-opens its channel if the pane dropped it (reused when live).
      if (doc.via === "sudo" && doc.fs) await sudoFor(doc.fs);
      const n = await fileWrite(doc.fs, path, model.getValue(monaco.editor.EndOfLinePreference.LF), doc.encoding, doc.eol, doc.via);
      savedVersion.set(doc.id, model.getAlternativeVersionId());
      doc.dirty = false; ondirty?.(hasUnsaved());
      status = `Saved ${doc.name} (${n.toLocaleString()} bytes, ${doc.encoding}, ${doc.eol.toUpperCase()})${doc.via === "sudo" ? " as root" : ""}`;
    } catch (e) {
      doc.error = String(e); status = doc.error;
      doc.retrySudo = canRetryAsSudo(doc, e);
      if (doc.retrySudo) console.info(`[charon] editor: save EACCES on ${path}; offering retry as sudo`);
    }
  }
  /** "Retry as sudo": bind the document to sudo-sftp and save through it (the
   *  backend's write keeps the original's owner and mode). */
  async function retryAsSudo(doc = active) {
    if (!doc?.fs || !doc.retrySudo) return;
    try { await sudoFor(doc.fs); }
    catch (e) { doc.error = String(e); status = doc.error; return; }
    doc.via = "sudo";
    await save(doc);
  }

  async function openLocal() {
    const picked = await pickOpen({ multiple: true, title: "Open file" });
    const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
    for (const p of paths) await openFile(p, null);
  }

  // ---- status bar controls
  function setLanguage(id: string) {
    if (!active) return;
    active.language = id;
    monaco.editor.setModelLanguage(models.get(active.id)!, id);
  }
  function setEol(eol: Eol) {
    if (!active || active.eol === eol) return;
    active.eol = eol;
    models.get(active.id)!.setEOL(eol === "crlf" ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF);
    // setEOL is an edit: the model version moves, so the doc shows dirty until saved.
  }
  function setEncoding(enc: Encoding) {
    if (!active || active.encoding === enc) return;
    active.encoding = enc; active.dirty = true; ondirty?.(true);
  }
  function toggleWrap() { wrap = !wrap; editor.updateOptions({ wordWrap: wrap ? "on" : "off" }); }
  function toggleMinimap() { minimap = !minimap; editor.updateOptions({ minimap: { enabled: minimap } }); }
  async function format() {
    status = "Formatting…";
    await editor.getAction("editor.action.formatDocument")?.run();
    status = "";
    editor.focus();
  }
  function toggleShowAll() {
    showAll = !showAll;
    editor.updateOptions({ renderWhitespace: showAll ? "all" : "selection", renderControlCharacters: showAll });
    if (showAll) scheduleGlyphs(); else for (const c of eolGlyphs.values()) c.clear();
  }
  // EOL glyphs: a ::after pseudo-element on each line end (Monaco renders whitespace but has no EOL marks).
  let glyphTimer: ReturnType<typeof setTimeout> | undefined;
  function scheduleGlyphs() { clearTimeout(glyphTimer); glyphTimer = setTimeout(renderGlyphs, 150); }
  function renderGlyphs() {
    if (!active || !showAll) return;
    const model = models.get(active.id)!;
    const lines = model.getLineCount();
    if (lines > 50000) return; // keep huge logs snappy
    const cls = active.eol === "crlf" ? "eol-glyph eol-crlf" : "eol-glyph eol-lf";
    const decos: monaco.editor.IModelDeltaDecoration[] = [];
    for (let i = 1; i < lines; i++) {
      const col = model.getLineMaxColumn(i);
      decos.push({ range: new monaco.Range(i, col, i, col), options: { afterContentClassName: cls, showIfCollapsed: true, stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges } });
    }
    let c = eolGlyphs.get(active.id);
    if (!c) { c = editor.createDecorationsCollection(); eolGlyphs.set(active.id, c); }
    c.set(decos);
  }

  async function pasteClean() {
    const text = await readText();
    if (!text) return;
    const sel = editor.getSelection();
    if (!sel) return;
    editor.executeEdits("paste-clean", [{ range: sel, text: cleanForCopy(text, { trimTrailing: false }), forceMoveMarkers: true }]);
    editor.focus();
  }
  function sendSelection() {
    if (!active) return;
    if (!active.origin) { status = "No paired terminal: open the file from a terminal's file pane to send to it."; return; }
    const model = models.get(active.id)!;
    const sel = editor.getSelection();
    let text = sel && !sel.isEmpty() ? model.getValueInRange(sel) : model.getLineContent(editor.getPosition()?.lineNumber ?? 1);
    text = text.replace(/\r\n/g, "\n");
    if (!text.endsWith("\n")) text += "\n";
    onsend?.(text, active.origin);
    status = `Sent ${text.split("\n").length - 1} line(s) to the terminal`;
  }
</script>

<div class="editor">
  <div class="tabs">
    <button type="button" onclick={openLocal} title="Open a local file">Open…</button>
    <button type="button" onclick={() => openUntitled()} title="New untitled document">New</button>
    <div class="strip">
      {#each docs as d (d.id)}
        <div class="dtab" class:active={d.id === activeId} class:dirty={d.dirty} title={d.path ? `${d.fs ? "remote: " : ""}${d.path}` : "untitled"}>
          <button type="button" class="dmain" onclick={() => activate(d.id)}>{#if d.via === "sudo"}<span class="root" title="Opened as root (sudo-sftp)">root</span>{/if}{d.fs ? (d.via === "shell" ? "# " : d.via === "sudo" ? "" : "⇅ ") : ""}{d.name}{d.dirty ? " •" : ""}</button>
          <button type="button" class="x" aria-label="Close" onclick={() => closeDoc(d.id)}>×</button>
        </div>
      {/each}
    </div>
  </div>
  <div class="host" bind:this={host}></div>
  {#if !docs.length}
    <div class="empty"><p>Open a file, create a new one, or double-click a file in a terminal's file pane (Ctrl+Shift+E).<br />Drop files on the window to open them here.</p></div>
  {/if}
  <div class="status">
    {#if active}
      <button type="button" onclick={() => save()} disabled={!active.dirty && !!active.path} title="Save (Ctrl+S)">Save</button>
      {#if active.retrySudo && !sudoAsk}
        <button type="button" class="sudo" onclick={() => retryAsSudo()} title="Save again as root over sudo-sftp; the file keeps its owner and mode">Retry as sudo</button>
      {/if}
      {#if sudoAsk}
        <form class="sudo-ask" onsubmit={(e) => { e.preventDefault(); answerSudo(sudoPw); }}>
          <span>sudo password</span>
          <input bind:this={sudoPwEl} type="password" bind:value={sudoPw} autocomplete="off" spellcheck="false"
            onkeydown={(e) => { if (e.key === "Escape") answerSudo(null); }} />
          <button type="submit" disabled={!sudoPw}>Unlock</button>
          <button type="button" onclick={() => answerSudo(null)}>Cancel</button>
        </form>
      {/if}
      <select value={active.language} onchange={(e) => setLanguage(e.currentTarget.value)} title="Language">
        {#each languages as l (l.id)}<option value={l.id}>{l.label}</option>{/each}
      </select>
      <select value={active.encoding} onchange={(e) => setEncoding(e.currentTarget.value as Encoding)} title="Encoding (applied on save)">
        <option value="utf-8">UTF-8</option><option value="utf-8-bom">UTF-8 BOM</option><option value="latin-1">Latin-1</option>
      </select>
      <button type="button" onclick={() => setEol(active.eol === "lf" ? "crlf" : "lf")} title="Click to convert line endings">{active.eol.toUpperCase()}</button>
      <button type="button" class:on={showAll} onclick={toggleShowAll} title="Show all characters (spaces, tabs, control chars, line ends)">¶</button>
      <button type="button" class:on={wrap} onclick={toggleWrap} title="Word wrap (Alt+Z)">wrap</button>
      <button type="button" class:on={minimap} onclick={toggleMinimap} title="Minimap">map</button>
      <button type="button" onclick={format} title="Format document (Shift+Alt+F) — JSON, or tidy YAML/Markdown">Format</button>
      <span class="hint">F1 palette · Ctrl+F/H find/replace · Ctrl+G line · Ctrl+Space suggest · Ctrl+Enter → terminal · Ctrl+Shift+V paste clean</span>
      <span class="cursor">{cursor}</span>
    {/if}
    {#if status}<span class="msg" class:err={!!active?.error}>{status}</span>{/if}
  </div>
</div>

<style>
  .editor { height: 100%; display: flex; flex-direction: column; position: relative; }
  .tabs { display: flex; align-items: center; gap: 4px; padding: 3px 6px; background: var(--bg-raised); border-bottom: 1px solid var(--line); }
  .tabs > button { padding: 1px 8px; }
  .strip { display: flex; gap: 2px; overflow-x: auto; min-width: 0; flex: 1; margin-left: 6px; }
  .dtab { display: flex; align-items: center; border-radius: 4px; color: var(--fg-dim); white-space: nowrap; }
  .dtab.active { background: var(--bg); color: var(--fg); box-shadow: inset 0 -2px 0 var(--accent); }
  .dtab.dirty .dmain { font-style: italic; }
  .dtab button { border: 0; background: transparent; color: inherit; padding: 3px 6px; }
  .dtab .x { padding: 3px 5px 3px 0; opacity: .5; }
  .dtab .x:hover { opacity: 1; color: var(--danger); }
  .host { flex: 1; min-height: 0; }
  .empty { position: absolute; inset: 32px 0 26px; display: grid; place-content: center; text-align: center; color: var(--fg-dim); pointer-events: none; }
  .status { display: flex; align-items: center; gap: 6px; font-size: 11px; color: var(--fg-dim); padding: 2px 8px; border-top: 1px solid var(--line); }
  .status select { font-size: 11px; padding: 0 4px; max-width: 130px; }
  .status button { padding: 0 6px; font-size: 11px; }
  .status button.on { border-color: var(--accent); color: var(--fg); }
  .hint { margin-left: auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .cursor { font-variant-numeric: tabular-nums; }
  .msg { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 40%; }
  .msg.err { color: var(--danger); }
  .root { display: inline-block; margin-right: 5px; padding: 0 4px; border-radius: 3px; font-size: 10px; font-weight: 600; line-height: 14px; color: #fff; background: var(--danger); }
  .status button.sudo { border-color: var(--danger); color: var(--danger); }
  .sudo-ask { display: flex; align-items: center; gap: 4px; }
  .sudo-ask input { font-size: 11px; padding: 0 4px; width: 140px; }
  :global(.eol-glyph::after) { color: var(--muted); opacity: .9; }
  :global(.eol-lf::after) { content: "¶"; }
  :global(.eol-crlf::after) { content: "¤¶"; }
</style>
