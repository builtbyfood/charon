<script lang="ts">
  // Docs view (R2-5): live tldr for the command being typed in the active
  // session, a `man` button over that session, tip of the day, and an update
  // button for the bundled tldr pages.
  import { tldrPage, resetTldr } from "./tldr";
  import { manFetch, tldrUpdate } from "./api";
  import { parseTips, tipForToday } from "./tips";
  import tipsRaw from "../../docs/tips.yaml?raw";

  let { command, session }: {
    /** First token of what's being typed in the active terminal. */
    command: string | null;
    /** Active session id, for the man button. */
    session: string | null;
  } = $props();

  const tip = tipForToday(parseTips(tipsRaw));
  let page = $state<string | null>(null);
  let shown = $state<string | null>(null);
  let manText = $state<string | null>(null);
  let manCmd = $state<string | null>(null);
  let busy = $state("");
  let updateMsg = $state("");

  // Live tldr as the command changes.
  $effect(() => {
    const cmd = command;
    manText = null;
    if (!cmd) { page = null; shown = null; return; }
    if (cmd === shown) return;
    tldrPage(cmd).then((md) => { if (command === cmd) { page = md ?? null; shown = cmd; } });
  });

  async function showMan() {
    if (!command || !session) return;
    busy = "man"; manText = null;
    try { manText = await manFetch(session, command); manCmd = command; }
    catch (e) { manText = String(e); manCmd = command; }
    finally { busy = ""; }
  }
  async function update() {
    busy = "update"; updateMsg = "downloading…";
    try { const n = await tldrUpdate(); resetTldr(); if (shown) { const c = shown; shown = null; page = await tldrPage(c) ?? null; shown = c; } updateMsg = `updated: ${n.toLocaleString()} pages`; }
    catch (e) { updateMsg = `update failed: ${e}`; }
    finally { busy = ""; }
  }

  // Very light tldr markdown → parts for rendering (title, description, examples).
  interface Ex { text: string; code: string }
  const parsed = $derived.by(() => {
    if (!page) return null;
    const lines = page.split(/\r?\n/);
    let title = command ?? "", desc: string[] = [];
    const examples: Ex[] = [];
    let pendingText = "";
    for (const l of lines) {
      if (l.startsWith("# ")) title = l.slice(2).trim();
      else if (l.startsWith("> ")) desc.push(l.slice(2).trim());
      else if (l.startsWith("- ")) pendingText = l.slice(2).replace(/:$/, "").trim();
      else if (l.startsWith("`") && l.endsWith("`")) { examples.push({ text: pendingText, code: l.slice(1, -1) }); pendingText = ""; }
    }
    return { title, desc, examples };
  });
</script>

<div class="docs">
  {#if tip}
    <div class="tip">
      <span class="tag">tip</span>
      <code>{tip.tip}</code>
      <span class="note">{tip.note}</span>
    </div>
  {/if}

  <div class="body">
    {#if manText !== null}
      <div class="manhead"><strong>man {manCmd}</strong><button type="button" onclick={() => (manText = null)}>tldr</button></div>
      <pre class="man">{manText}</pre>
    {:else if parsed}
      <div class="head">
        <strong class="mono">{parsed.title}</strong>
        {#if session}<button type="button" onclick={showMan} disabled={busy === "man"}>{busy === "man" ? "…" : `man ${command}`}</button>{/if}
      </div>
      {#if parsed.desc.length}<p class="desc">{parsed.desc.join(" ")}</p>{/if}
      <div class="ex">
        {#each parsed.examples as e (e.code)}
          <div class="exi">
            <div class="et">{e.text}</div>
            <code class="ec mono">{e.code}</code>
          </div>
        {/each}
      </div>
    {:else if command}
      <p class="empty">No tldr page for <span class="mono">{command}</span>.{#if session}<button type="button" onclick={showMan} disabled={busy === "man"}>try man</button>{/if}</p>
    {:else}
      <p class="empty">Start typing a command in a terminal — its tldr appears here.</p>
    {/if}
  </div>

  <div class="foot">
    <button type="button" onclick={update} disabled={busy === "update"}>{busy === "update" ? "…" : "Update tldr pages"}</button>
    <span class="dim">{updateMsg}</span>
  </div>
</div>

<style>
  .docs { display: flex; flex-direction: column; height: 100%; min-height: 0; font-size: 12px; }
  .tip { display: flex; align-items: baseline; gap: 6px; padding: 6px 8px; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  .tip .tag { font-size: 10px; text-transform: uppercase; color: var(--accent); }
  .tip code { font-family: var(--font-mono); color: #ffb454; }
  .tip .note { color: var(--fg-dim); }
  .body { flex: 1; overflow: auto; min-height: 0; padding: 8px; }
  .head, .manhead { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 6px; }
  .head strong { font-size: 14px; }
  .desc { margin: 0 0 8px; color: var(--fg-dim); }
  .exi { margin-bottom: 8px; }
  .et { color: var(--fg-dim); margin-bottom: 2px; }
  .ec { display: block; background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 4px; padding: 4px 8px; color: var(--fg); white-space: pre-wrap; word-break: break-all; }
  .man { margin: 0; white-space: pre-wrap; word-break: break-word; font-family: var(--font-mono); font-size: 11px; color: var(--fg); }
  .mono { font-family: var(--font-mono); }
  .empty { color: var(--fg-dim); }
  .empty button, .head button, .manhead button { margin-left: 6px; padding: 1px 6px; }
  .foot { display: flex; align-items: center; gap: 8px; padding: 4px 8px; border-top: 1px solid var(--line); }
  .dim { color: var(--fg-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
