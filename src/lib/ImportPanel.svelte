<script lang="ts">
  // Import picker: three sources, each with its default location or a file picker.
  import { open } from "@tauri-apps/plugin-dialog";
  import { importConnections, importPreview, type DupPolicy, type ImportKind, type ImportPreview, type ImportReport } from "./api";

  let { onclose }: { onclose: () => void } = $props();

  const isWindows = navigator.userAgent.includes("Windows");
  const sources: { kind: ImportKind; label: string; hint: string; browse?: { directory?: boolean; ext?: string[] }; needsPath?: boolean }[] = [
    { kind: "ssh_config", label: "OpenSSH config", hint: "~/.ssh/config", browse: {} },
    {
      kind: "putty", label: "PuTTY sessions",
      hint: isWindows ? "HKCU\\Software\\SimonTatham\\PuTTY\\Sessions" : "~/.putty/sessions/",
      browse: isWindows ? undefined : { directory: true },
    },
    { kind: "mobaxterm", label: "MobaXterm", hint: "pick a .mxtsessions file", browse: { ext: ["mxtsessions"] }, needsPath: true },
  ];

  let busy = $state<ImportKind | null>(null);
  // Two steps: parse → show new/duplicate counts and pick a duplicate policy → apply.
  let preview = $state<(ImportPreview & { kind: ImportKind; path?: string }) | null>(null);
  let policy = $state<DupPolicy>("skip");
  let report = $state<(ImportReport & { kind: ImportKind }) | null>(null);
  let error = $state("");

  async function run(kind: ImportKind, path?: string) {
    busy = kind; error = ""; report = null; preview = null; policy = "skip";
    try { preview = { kind, path, ...(await importPreview(kind, path)) }; }
    catch (e) { error = String(e); }
    finally { busy = null; }
  }
  async function applyImport() {
    if (!preview) return;
    const { kind, path } = preview;
    busy = kind; error = "";
    try { report = { kind, ...(await importConnections(kind, path, policy)) }; preview = null; }
    catch (e) { error = String(e); }
    finally { busy = null; }
  }
  async function browse(src: (typeof sources)[number]) {
    const picked = await open({
      multiple: false,
      directory: src.browse?.directory ?? false,
      filters: src.browse?.ext ? [{ name: src.label, extensions: src.browse.ext }] : undefined,
    });
    if (typeof picked === "string") await run(src.kind, picked);
  }
  function onkeydown(e: KeyboardEvent) { if (e.key === "Escape") onclose(); }
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="dialog">
    <h2>Import connections</h2>
    <p>Each source lands in its own folder. You get a summary before anything is written; hosts that already exist (same name, or same host and user) are skipped unless you say otherwise. Key files become vault entries; passwords are never imported.</p>
    {#each sources as src (src.kind)}
      <div class="src">
        <div class="label">{src.label}<span class="hint">{src.hint}</span></div>
        <div class="btns">
          {#if !src.needsPath}
            <button disabled={busy !== null} onclick={() => run(src.kind)}>{busy === src.kind ? "Importing…" : "Import default"}</button>
          {/if}
          {#if src.browse}
            <button disabled={busy !== null} onclick={() => browse(src)}>Browse…</button>
          {/if}
        </div>
      </div>
    {/each}
    {#if error}<p class="err">{error}</p>{/if}
    {#if preview}
      <div class="report">
        <strong>{preview.new.length} new, {preview.duplicates.length} {preview.duplicates.length === 1 ? "duplicate" : "duplicates"}</strong>
        {#if preview.duplicates.length}
          <ul class="dups">
            {#each preview.duplicates as d (d.name + d.host)}
              <li><span class="mono">{d.name}</span> <span class="dim">({d.host}) — {d.reason} as <span class="mono">{d.existing}</span></span></li>
            {/each}
          </ul>
          <div class="policy">
            <label><input type="radio" bind:group={policy} value="skip" /> Skip <span class="dim">leave existing nodes alone</span></label>
            <label><input type="radio" bind:group={policy} value="overwrite" /> Overwrite settings <span class="dim">host, port, user, key, jump host and (PuTTY) font of the existing node</span></label>
            <label><input type="radio" bind:group={policy} value="copy" /> Import as copy <span class="dim">add anyway, suffixed “(2)” on a name clash</span></label>
          </div>
        {/if}
        {#if preview.warnings.length}
          <ul>{#each preview.warnings as w, i (i)}<li>{w}</li>{/each}</ul>
        {/if}
      </div>
    {/if}
    {#if report}
      <div class="report">
        <strong>{report.added} added, {report.updated} updated, {report.skipped} skipped</strong>
        {#if report.warnings.length}
          <ul>{#each report.warnings as w, i (i)}<li>{w}</li>{/each}</ul>
        {/if}
      </div>
    {/if}
    <div class="row">
      <button onclick={onclose}>Close</button>
      {#if preview}
        <button class="primary" disabled={busy !== null || (preview.new.length === 0 && (preview.duplicates.length === 0 || policy === "skip"))} onclick={applyImport}>
          {busy ? "Importing…" : "Apply"}
        </button>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: var(--overlay); display: grid; place-content: center; z-index: 10; }
  .dialog { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 440px; max-height: 90vh; overflow: auto; display: flex; flex-direction: column; gap: 10px; }
  h2 { margin: 0; font-size: 15px; font-weight: 600; }
  p { margin: 0; color: var(--fg-dim); font-size: 12px; }
  .src { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 6px 0; border-top: 1px solid var(--line); }
  .label { display: flex; flex-direction: column; min-width: 0; }
  .hint { color: var(--fg-dim); font-family: var(--font-mono); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .btns { display: flex; gap: 4px; flex: none; }
  .err { color: var(--danger); }
  .report { font-size: 12px; border-top: 1px solid var(--line); padding-top: 8px; }
  .report ul { margin: 4px 0 0; padding-left: 18px; color: var(--fg-dim); max-height: 160px; overflow: auto; }
  .row { display: flex; justify-content: flex-end; gap: 6px; }
  .dups li { list-style: none; margin-left: -18px; }
  .mono { font-family: var(--font-mono); color: var(--fg); }
  .dim { color: var(--fg-dim); }
  .policy { display: flex; flex-direction: column; gap: 4px; margin-top: 8px; }
  .policy label { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  .primary { border-color: var(--accent); }
</style>
