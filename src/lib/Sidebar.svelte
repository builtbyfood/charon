<script lang="ts">
  import { onMount } from "svelte";
  import {
    listConnections, saveConnection, deleteConnection, vaultList, vaultSet, vaultDelete, vaultRename, CHANGED,
    type Node, type SecretKind, type SecretMeta, type VaultStatus,
  } from "./api";
  import { inheritedSettings } from "./tree";
  import NodeEditor from "./NodeEditor.svelte";
  import ImportPanel from "./ImportPanel.svelte";
  import DocsPanel from "./DocsPanel.svelte";

  let { onopen, onquick, onlocal, oneditor, onmcp, docsView = $bindable(false), docsCommand, docsSession, vault, onunlock }: {
    onopen: (n: Node, color?: string) => void;
    onquick: (t: string) => void;
    onlocal: (admin?: boolean) => void;
    oneditor: () => void;
    onmcp: () => void;
    docsView: boolean;
    docsCommand: string | null;
    docsSession: string | null;
    vault: VaultStatus | null;
    onunlock: () => void;
  } = $props();

  let nodes = $state<Node[]>([]);
  let filter = $state("");
  let quick = $state("");
  let collapsed = $state<Set<string>>(new Set());

  // Credentials section.
  let creds = $state<SecretMeta[]>([]);
  let credError = $state("");
  let adding = $state(false);
  let newCred = $state({ name: "", kind: "password" as SecretKind, value: "", passphrase: "" });

  onMount(() => {
    refresh();
    window.addEventListener(CHANGED.connections, refresh);
    window.addEventListener(CHANGED.vault, refreshCreds);
    return () => {
      window.removeEventListener(CHANGED.connections, refresh);
      window.removeEventListener(CHANGED.vault, refreshCreds);
    };
  });
  // Re-list whenever the vault unlocks.
  $effect(() => { if (vault && !vault.locked) refreshCreds(); else creds = []; });

  async function refresh() { nodes = await listConnections(); }
  async function refreshCreds() {
    try { creds = await vaultList(); credError = ""; } catch (e) { creds = []; credError = String(e); }
  }
  async function addCred(e: SubmitEvent) {
    e.preventDefault();
    const { name, kind, value, passphrase } = newCred;
    if (!name.trim() || !value) return;
    try {
      await vaultSet(name.trim(), { kind, value, key_passphrase: kind === "key" && passphrase ? passphrase : undefined });
      newCred = { name: "", kind: "password", value: "", passphrase: "" };
      adding = false;
      await refreshCreds();
    } catch (err) { credError = String(err); }
  }
  // Rename in place: double-click the name or the pencil; Enter saves, Esc cancels.
  let renaming = $state<{ from: string; to: string } | null>(null);
  async function renameCred() {
    if (!renaming) return;
    const { from, to } = renaming;
    if (!to.trim() || to.trim() === from) { renaming = null; return; }
    try {
      const n = await vaultRename(from, to.trim());
      renaming = null;
      credError = "";
      await refreshCreds();
      if (n) await refresh(); // connections now reference the new name
      console.info(`[charon] vault: renamed credential, ${n} connection(s) updated`);
    } catch (err) { credError = String(err); }
  }
  function renameKey(e: KeyboardEvent) {
    if (e.key === "Enter") { e.preventDefault(); renameCred(); }
    else if (e.key === "Escape") { renaming = null; }
  }
  async function removeCred(m: SecretMeta) {
    if (!confirm(`Delete credential ${m.name}? Hosts using it will ask for a password.`)) return;
    try { await vaultDelete(m.name); await refreshCreds(); } catch (err) { credError = String(err); }
  }

  const children = (pid: string | null): Node[] =>
    nodes.filter((n) => n.parent_id === pid && (!filter || matches(n)));
  const matches = (n: Node): boolean => {
    const f = filter.toLowerCase();
    return n.name.toLowerCase().includes(f) || n.settings.host?.toLowerCase().includes(f)
      || n.settings.tags?.some((t) => t.toLowerCase().includes(f)) || children(n.id).length > 0;
  };
  // Effective colour (own or nearest folder's) for the tab dot and row stripe.
  const inheritedColor = (n: Node): string | undefined => n.settings.color ?? inheritedFor(n).color;
  function toggle(id: string) {
    collapsed.has(id) ? collapsed.delete(id) : collapsed.add(id);
    collapsed = new Set(collapsed);
  }
  // Host/folder editor. `editing` holds a draft; save upserts it.
  let editing = $state<{ node: Node; isNew: boolean } | null>(null);
  let importing = $state(false);
  const inheritedFor = (n: Node) => inheritedSettings(nodes, n.parent_id);
  function startNew(kind: "folder" | "ssh" | "local" | "serial", parent: string | null) {
    editing = { isNew: true, node: { id: crypto.randomUUID(), parent_id: parent, kind, name: "", sort: 0, settings: {} } };
  }
  function startEdit(n: Node) { editing = { isNew: false, node: n }; }
  async function saveEdit(n: Node) {
    await saveConnection(n);
    editing = null;
    await refresh();
  }
  async function remove(n: Node) {
    if (!confirm(`Delete ${n.name}${n.kind === "folder" ? " and everything in it" : ""}?`)) return;
    await deleteConnection(n.id); await refresh();
  }
</script>

{#snippet tree(pid: string | null, depth: number)}
  {#each children(pid) as n (n.id)}
    <div class="row" style:padding-left="{8 + depth * 14}px" style:--c={inheritedColor(n) ?? "var(--line)"}>
      {#if n.kind === "folder"}
        <button class="name folder" onclick={() => toggle(n.id)}>
          <span class="chev" class:closed={collapsed.has(n.id)}>▾</span>{n.name}
        </button>
        <span class="acts">
          <button title="Add host" onclick={() => startNew("ssh", n.id)}>+</button>
          <button title="Add folder" onclick={() => startNew("folder", n.id)}>▣</button>
          <button title="Edit" onclick={() => startEdit(n)}>✎</button>
          <button title="Delete" onclick={() => remove(n)}>×</button>
        </span>
      {:else}
        {@const eff = { ...inheritedFor(n), ...n.settings }}
        {@const target = n.kind === "local"
          ? (eff.shell ?? "local shell")
          : n.kind === "serial"
          ? `${eff.serial?.port ?? "?"} ${eff.serial?.baud ?? 9600}`
          : `${eff.user ? eff.user + "@" : ""}${eff.host ?? ""}${eff.port && eff.port !== 22 ? ":" + eff.port : ""}`}
        <button class="name host" title="{n.name}  {target}" onclick={() => onopen(n, inheritedColor(n))}>
          <span class="swatch"></span><span class="label">{n.name}</span>
          <span class="dim">{target}</span>
        </button>
        <span class="acts">
          <button title="Edit" onclick={() => startEdit(n)}>✎</button>
          <button title="Delete" onclick={() => remove(n)}>×</button>
        </span>
      {/if}
    </div>
    {#if n.kind === "folder" && !collapsed.has(n.id)}
      {@render tree(n.id, depth + 1)}
    {/if}
  {/each}
{/snippet}

<aside>
  <div class="tabs">
    <button class:sel={!docsView} onclick={() => (docsView = false)}>Connections</button>
    <button class:sel={docsView} onclick={() => (docsView = true)} title="Docs for the command you're typing (Ctrl+Shift+D)">Docs</button>
  </div>
  {#if docsView}
    <DocsPanel command={docsCommand} session={docsSession} />
  {:else}
  <form class="quick" onsubmit={(e) => { e.preventDefault(); if (quick) { onquick(quick); quick = ""; } }}>
    <input placeholder="user@host:port  ↵ to connect" bind:value={quick} spellcheck="false" />
  </form>
  <input class="filter" placeholder="Filter by name, host, tag" bind:value={filter} spellcheck="false" />
  <div class="tree">
    {#if nodes.length === 0}
      <p class="hint">No connections yet. Add a folder to group hosts; folders can set a jump host, user or colour that every host inside inherits.</p>
    {/if}
    {@render tree(null, 0)}
  </div>
  <div class="foot">
    <button onclick={() => startNew("folder", null)}>New folder</button>
    <button onclick={() => startNew("ssh", null)}>New host</button>
    <button onclick={() => onlocal(false)} title="Open a local shell tab">Local</button>
    <button onclick={() => onlocal(true)} title="Elevated local shell: Windows sudo (inline mode) or sudo -i">Local (admin)</button>
    <button onclick={oneditor} title="Open the editor tab">Editor</button>
    <button onclick={onmcp} title="MCP server settings (let an agent drive your sessions)">MCP</button>
    <button onclick={() => (importing = true)}>Import…</button>
  </div>
  <section class="creds">
    <header>
      <span>Credentials</span>
      {#if vault?.locked}
        <button onclick={onunlock}>Unlock vault</button>
      {:else}
        <button onclick={() => (adding = !adding)}>{adding ? "Cancel" : "Add"}</button>
      {/if}
    </header>
    {#if adding && !vault?.locked}
      <form class="add" onsubmit={addCred}>
        <input placeholder="name" bind:value={newCred.name} spellcheck="false" />
        <select bind:value={newCred.kind}>
          <option value="password">password</option>
          <option value="key">private key</option>
        </select>
        {#if newCred.kind === "password"}
          <input type="password" placeholder="password" bind:value={newCred.value} autocomplete="off" />
        {:else}
          <input placeholder="path to private key (OpenSSH or PuTTY .ppk)" bind:value={newCred.value} spellcheck="false" />
          <input type="password" placeholder="key passphrase (optional)" bind:value={newCred.passphrase} autocomplete="off" />
        {/if}
        <button type="submit" disabled={!newCred.name.trim() || !newCred.value}>Save</button>
      </form>
    {/if}
    {#if credError}<p class="err">{credError}</p>{/if}
    <ul>
      {#each creds as c (c.name)}
        <li><span class="kind">{c.kind === "key" ? "key" : "pw"}</span>
          {#if renaming?.from === c.name}
            <!-- svelte-ignore a11y_autofocus -->
            <input class="rename" bind:value={renaming.to} onkeydown={renameKey} onblur={renameCred} spellcheck="false" autofocus aria-label="New credential name" />
          {:else}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <span class="cname" title="Double-click to rename" ondblclick={() => (renaming = { from: c.name, to: c.name })}>{c.name}</span>
            <button title="Rename (connections using it follow)" onclick={() => (renaming = { from: c.name, to: c.name })}>✎</button>
            <button title="Delete" onclick={() => removeCred(c)}>×</button>
          {/if}</li>
      {/each}
      {#if !creds.length && !vault?.locked}<li class="hint">None yet. Saved passwords and key paths live in the {vault?.backend === "file" ? "encrypted vault.age" : "OS keyring"}.</li>{/if}
    </ul>
  </section>
  {/if}
</aside>
{#if importing}
  <ImportPanel onclose={() => { importing = false; refresh(); refreshCreds(); }} />
{/if}
{#if editing}
  {#key editing.node.id}
    <NodeEditor node={editing.node} inherited={inheritedFor(editing.node)} {creds} isNew={editing.isNew}
      onsave={saveEdit} oncancel={() => (editing = null)} />
  {/key}
{/if}

<style>
  .tabs { display: flex; gap: 2px; padding: 4px 6px 0; }
  .tabs button { flex: 1; border: 0; background: transparent; color: var(--fg-dim); padding: 4px; border-bottom: 2px solid transparent; }
  .tabs button.sel { color: var(--fg); border-bottom-color: var(--accent); }

  aside { display: flex; flex-direction: column; background: var(--bg-raised); border-right: 1px solid var(--line); min-height: 0; }
  .quick { padding: 6px; } .quick input, .filter { width: 100%; font-family: var(--font-mono); font-size: 12px; }
  .filter { margin: 0 6px 6px; width: calc(100% - 12px); }
  .tree { flex: 1; overflow: auto; }
  .row { display: flex; align-items: center; gap: 4px; padding-right: 4px; border-left: 3px solid var(--c); }
  .row:hover { background: var(--bg); }
  .row:hover .acts { visibility: visible; }
  .acts { visibility: hidden; display: flex; gap: 2px; flex: none; }
  .acts button { padding: 0 5px; border: 0; background: transparent; color: var(--fg-dim); }
  .name { flex: 1; text-align: left; border: 0; background: transparent; padding: 4px 4px; display: flex; align-items: center; gap: 6px; min-width: 0; white-space: nowrap; overflow: hidden; }
  .label { overflow: hidden; text-overflow: ellipsis; flex: 0 1 auto; min-width: 3ch; }
  .folder { color: var(--fg-dim); }
  .chev { display: inline-block; transition: transform .12s; } .chev.closed { transform: rotate(-90deg); }
  .swatch { width: 7px; height: 7px; border-radius: 50%; background: var(--c); flex: none; }
  .dim { color: var(--fg-dim); font-family: var(--font-mono); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; min-width: 0; }
  .hint { color: var(--fg-dim); padding: 12px; font-size: 12px; }
  .foot { display: flex; flex-wrap: wrap; gap: 6px; padding: 6px; border-top: 1px solid var(--line); }
  .foot button { flex: 1; white-space: nowrap; }
  .creds { border-top: 1px solid var(--line); max-height: 40%; display: flex; flex-direction: column; }
  .creds header { display: flex; justify-content: space-between; align-items: center; padding: 4px 6px 4px 10px; color: var(--fg-dim); font-size: 12px; }
  .creds header button { padding: 2px 8px; font-size: 12px; }
  .creds .add { display: flex; flex-direction: column; gap: 4px; padding: 0 6px 6px; }
  .creds .add input, .creds .add select { width: 100%; font-family: var(--font-mono); font-size: 12px; }
  .creds ul { list-style: none; margin: 0; padding: 0 0 4px; overflow: auto; }
  .creds li { display: flex; align-items: center; gap: 6px; padding: 2px 6px 2px 10px; }
  .creds li:hover { background: var(--bg); }
  .creds li button { visibility: hidden; padding: 0 5px; border: 0; background: transparent; color: var(--fg-dim); }
  .creds li:hover button { visibility: visible; }
  .creds .kind { font-family: var(--font-mono); font-size: 10px; color: var(--fg-dim); width: 22px; }
  .creds .cname { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .creds .rename { flex: 1; min-width: 0; font-family: var(--font-mono); font-size: 12px; padding: 1px 4px; }
  .creds .hint, .creds .err { color: var(--fg-dim); font-size: 12px; padding: 2px 10px; margin: 0; }
  .creds .err { color: var(--danger); }
</style>
