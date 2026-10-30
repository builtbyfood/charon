<script lang="ts">
  // Create/edit panel for hosts and folders. Inherited values from the folder
  // chain show as grey placeholders so it is obvious what leaving a field blank means.
  import { onMount } from "svelte";
  import { serialListPorts, type Node, type Settings, type SecretMeta, type PortInfo } from "./api";
  import { formFromSettings, settingsFromForm } from "./nodeform";
  import { PALETTE } from "./tree";
  import FontPicker from "./FontPicker.svelte";

  let { node, inherited, creds, isNew, onsave, oncancel }: {
    node: Node;
    inherited: Settings;
    creds: SecretMeta[];
    isNew: boolean;
    onsave: (n: Node) => void;
    oncancel: () => void;
  } = $props();

  // The parent remounts this component per node ({#key}), so snapshotting the
  // initial props into local form state is intentional.
  // svelte-ignore state_referenced_locally
  const isFolder = node.kind === "folder";
  // svelte-ignore state_referenced_locally
  const s = node.settings;
  // svelte-ignore state_referenced_locally
  let kind = $state<Node["kind"]>(node.kind);
  const isLocal = $derived(kind === "local");
  const isSerial = $derived(kind === "serial");
  /** Fields the panel hides for the current type. */
  const isSsh = $derived(!isFolder && !isLocal && !isSerial);

  // Form state: strings for text inputs, "" = inherit. Mapping lives in nodeform.ts.
  // svelte-ignore state_referenced_locally
  let name = $state(node.name);
  const f = $state(formFromSettings(s));
  let ports = $state<PortInfo[]>([]);
  const BAUDS = [1200, 2400, 4800, 9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600];
  async function refreshPorts() { try { ports = await serialListPorts(); } catch { ports = []; } }
  onMount(() => { if (kind === "serial") refreshPorts(); });
  $effect(() => { if (kind === "serial" && !ports.length) refreshPorts(); });
  let error = $state("");

  const credMissing = $derived(f.credential !== "" && !creds.some((c) => c.name === f.credential));
  const inhCred = $derived(inherited.credential ? `inherit: ${inherited.credential}` : "none");
  const inhShell = $derived(`inherit: ${inherited.shell_integration === false ? "off" : "on"}`);
  const inhMcp = $derived(`inherit: ${inherited.mcp_tier ?? "read"}`);

  function submit(e: SubmitEvent) {
    e.preventDefault();
    error = "";
    const n = name.trim() || (isSsh ? f.host.trim() : isSerial ? f.serialPort.trim() : ""); // hosts default to their address
    if (!n) { error = "name is required"; return; }
    if (isSsh && !f.host.trim() && !inherited.host) { error = "host is required (nothing inherited)"; return; }
    const r = settingsFromForm(f, kind);
    if (!r.ok) { error = r.error; return; }
    onsave({ ...node, kind, name: n, settings: r.settings });
  }
  function onkeydown(e: KeyboardEvent) { if (e.key === "Escape") oncancel(); }
</script>

<svelte:window {onkeydown} />

<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) oncancel(); }}>
  <form class="dialog" onsubmit={submit}>
    <h2>{isNew ? "New" : "Edit"} {isFolder ? "folder" : isLocal ? "local terminal" : isSerial ? "serial console" : "host"}</h2>

    {#if isNew && !isFolder}
      <label>Type
        <select bind:value={kind}>
          <option value="ssh">SSH host</option>
          <option value="local">Local terminal</option>
          <option value="serial">Serial console</option>
        </select>
      </label>
    {/if}

    <label>Name
      <!-- svelte-ignore a11y_autofocus -->
      <input bind:value={name} autofocus spellcheck="false" placeholder={isSsh ? "defaults to host" : isSerial ? "defaults to port" : ""} />
    </label>

    {#if isSerial}
      <label>Port
        <div class="portrow">
          <input bind:value={f.serialPort} list="charon-serial-ports" placeholder={ports.length ? "pick or type" : "COM3, /dev/ttyUSB0"} spellcheck="false" class="mono" />
          <datalist id="charon-serial-ports">
            {#each ports as p (p.name)}<option value={p.name}>{p.description}</option>{/each}
          </datalist>
          <button type="button" onclick={refreshPorts} title="Rescan ports">↻</button>
        </div>
        {#if ports.length}
          <span class="dim">{ports.map((p) => p.description ? `${p.name} (${p.description})` : p.name).join(", ")}</span>
        {/if}
      </label>
      <div class="four">
        <label>Baud
          <input bind:value={f.baud} list="charon-bauds" inputmode="numeric" class="mono" />
          <datalist id="charon-bauds">{#each BAUDS as b (b)}<option value={b}></option>{/each}</datalist>
        </label>
        <label>Data bits
          <select bind:value={f.dataBits}>{#each ["8", "7", "6", "5"] as d (d)}<option value={d}>{d}</option>{/each}</select>
        </label>
        <label>Parity
          <select bind:value={f.parity}><option value="none">none</option><option value="even">even</option><option value="odd">odd</option></select>
        </label>
        <label>Stop bits
          <select bind:value={f.stopBits}><option value="1">1</option><option value="2">2</option></select>
        </label>
      </div>
    {/if}

    {#if isLocal}
      <label>Program <span class="dim">blank = pwsh → powershell → cmd (Windows), $SHELL → bash (Linux)</span>
        <input bind:value={f.shell} placeholder="e.g. wsl.exe, pwsh.exe, /bin/zsh" spellcheck="false" class="mono" />
      </label>
      <label>Start directory
        <input bind:value={f.cwd} placeholder="home" spellcheck="false" class="mono" />
      </label>
    {:else if !isFolder}
      <label>Host
        <input bind:value={f.host} placeholder={inherited.host ?? ""} spellcheck="false" class="mono" />
      </label>
    {/if}

    <div class="two" hidden={!isSsh && !isFolder}>
      <label>User
        <input bind:value={f.user} placeholder={inherited.user ?? "(current user)"} spellcheck="false" class="mono" />
      </label>
      <label>Port
        <input bind:value={f.port} placeholder={String(inherited.port ?? 22)} inputmode="numeric" class="mono" />
      </label>
    </div>

    <label hidden={!isSsh && !isFolder && !isLocal}>{isLocal ? "Credential (sudo password, Unix)" : "Credential"}
      <select bind:value={f.credential} class:inh={f.credential === ""}>
        <option value="">({inhCred})</option>
        {#each creds as c (c.name)}<option value={c.name}>{c.name}</option>{/each}
        {#if credMissing}<option value={f.credential}>{f.credential} (missing from vault)</option>{/if}
      </select>
    </label>

    <label hidden={!isSsh && !isFolder}>Shell <span class="dim">blank = the account's login shell; the tab's shell menu lists what the host has</span>
      <input bind:value={f.remoteShell} list="charon-shells" placeholder={inherited.remote_shell ?? "login shell"} spellcheck="false" class="mono" />
      <datalist id="charon-shells">{#each ["bash", "zsh", "fish", "sh", "ksh", "pwsh", "powershell", "cmd"] as s (s)}<option value={s}></option>{/each}</datalist>
    </label>

    <label hidden={!isSsh && !isFolder}>Jump host
      <input bind:value={f.jump} placeholder={inherited.jump_host ?? "saved host name, or user@bastion:port"} spellcheck="false" class="mono" />
    </label>

    <div class="field">
      <span>Colour</span>
      <div class="swatches">
        <button type="button" class="sw none" class:sel={f.color === ""} title={inherited.color ? `inherit ${inherited.color}` : "none"}
          style:--sw={inherited.color ?? "transparent"} onclick={() => (f.color = "")}>×</button>
        {#each PALETTE as c (c)}
          <button type="button" class="sw" class:sel={f.color === c} style:--sw={c} title={c} onclick={() => (f.color = c)} aria-label={c}></button>
        {/each}
        <input type="color" value={f.color || inherited.color || "#6aa2ff"} oninput={(e) => (f.color = e.currentTarget.value)} title="Custom" />
      </div>
    </div>

    <div class="field">
      <span>Terminal font <span class="dim">blank = {inherited.font || inherited.font_size ? "inherit from the folder" : "the profile's (Settings → Appearance)"}</span></span>
      <FontPicker bind:family={f.font} bind:size={f.fontSize} inheritFamily={inherited.font} inheritSize={inherited.font_size}
        placeholder={inherited.font ? undefined : "profile default"} />
    </div>

    <label>Tags
      <input bind:value={f.tags} placeholder={inherited.tags?.join(", ") ?? "prod, web"} spellcheck="false" />
    </label>

    <label>Startup commands <span class="dim">one per line, run after the shell is up</span>
      <textarea bind:value={f.startup} rows="2" placeholder={inherited.startup?.join("\n") ?? ""} spellcheck="false" class="mono"></textarea>
    </label>

    <div class="two">
      <label>Shell integration
        <select bind:value={f.shellInt} class:inh={f.shellInt === ""}>
          <option value="">({inhShell})</option>
          <option value="true">on</option>
          <option value="false">off</option>
        </select>
      </label>
      <label>MCP tier
        <select bind:value={f.mcp} class:inh={f.mcp === ""}>
          <option value="">({inhMcp})</option>
          <option value="read">read</option>
          <option value="exec-approve">exec-approve</option>
          <option value="trusted">trusted</option>
        </select>
      </label>
      {#if (f.mcp || inherited.mcp_tier) === "trusted"}
        <label class="check" title="Let a trusted MCP run_command auto-answer this host's '[sudo] password for <login user>:' prompt with the saved password. Never a bare 'Password:'. The password is never shown or logged.">
          <input type="checkbox" bind:checked={f.mcpSudo} /> allow MCP sudo password
        </label>
      {/if}
    </div>

    {#if error}<p class="err">{error}</p>{/if}
    <div class="row">
      <button type="button" onclick={oncancel}>Cancel</button>
      <button type="submit" class="primary">{isNew ? "Create" : "Save"}</button>
    </div>
  </form>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: var(--overlay); display: grid; place-content: center; z-index: 10; }
  .dialog { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 440px; max-height: 90vh; overflow: auto; display: flex; flex-direction: column; gap: 8px; }
  h2 { margin: 0 0 4px; font-size: 15px; font-weight: 600; }
  label, .field { display: flex; flex-direction: column; gap: 3px; font-size: 12px; color: var(--fg-dim); }
  label input, label select, label textarea { color: var(--fg); }
  label.check { flex-direction: row; align-items: center; gap: 6px; color: var(--warn); }
  label.check input { width: auto; }
  textarea { font: inherit; background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 3px; padding: 4px 8px; resize: vertical; }
  .mono { font-family: var(--font-mono); }
  .two { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
  .four { display: grid; grid-template-columns: 2fr 1fr 1fr 1fr; gap: 8px; }
  .portrow { display: flex; gap: 4px; } .portrow input { flex: 1; }
  .dim { font-weight: normal; opacity: .7; }
  select.inh { color: var(--fg-dim); }
  .swatches { display: flex; gap: 6px; align-items: center; }
  .sw { width: 22px; height: 22px; border-radius: 50%; border: 2px solid transparent; background: var(--sw); padding: 0; }
  .sw.sel { border-color: var(--fg); }
  .sw.none { background: var(--bg-sunken); color: var(--fg-dim); box-shadow: inset 0 0 0 3px var(--sw); font-size: 12px; line-height: 1; }
  input[type="color"] { width: 26px; height: 22px; padding: 0; border: 1px solid var(--line); background: none; }
  .err { color: var(--danger); margin: 0; }
  .row { display: flex; justify-content: flex-end; gap: 6px; margin-top: 4px; }
  .primary { border-color: var(--accent); }
</style>
