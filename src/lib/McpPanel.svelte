<script lang="ts">
  // Settings → MCP: enable/disable the server, show/regenerate the token, copy a
  // client config, and watch the live call log with tier decisions.
  import { onMount, onDestroy } from "svelte";
  import { writeText } from "@tauri-apps/plugin-clipboard-manager";
  import { mcpStatus, mcpSetEnabled, mcpRegenToken, mcpConfig, mcpLog, type McpStatus, type McpLogLine } from "./api";

  let { onclose }: { onclose: () => void } = $props();

  let status = $state<McpStatus | null>(null);
  let showToken = $state(false);
  let busy = $state(false);
  let error = $state("");
  let copied = $state("");
  let log = $state<McpLogLine[]>([]);
  let timer: ReturnType<typeof setInterval> | undefined;

  onMount(async () => {
    try { status = await mcpStatus(); } catch (e) { error = String(e); }
    refreshLog();
    timer = setInterval(refreshLog, 1500);
  });
  onDestroy(() => clearInterval(timer));

  async function refreshLog() { try { log = await mcpLog(100); } catch { /* ignore */ } }

  async function toggle() {
    if (!status) return;
    busy = true; error = "";
    try { status = await mcpSetEnabled(!status.enabled); } catch (e) { error = String(e); } finally { busy = false; }
  }
  async function regen() {
    busy = true; error = "";
    try { status = await mcpRegenToken(); } catch (e) { error = String(e); } finally { busy = false; }
  }
  async function copy(kind: "http" | "stdio") {
    try { await writeText(await mcpConfig(kind)); copied = kind; setTimeout(() => (copied = ""), 1500); }
    catch (e) { error = String(e); }
  }
  const when = (ts: string) => { const n = Number(ts); return n ? new Date(n * 1000).toLocaleTimeString() : ts; };
</script>

<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="dialog">
    <h2>MCP server</h2>
    <p>Let an agent (Claude Code, Cursor…) drive your sessions. Per-connection tier
      (<span class="mono">read</span> / <span class="mono">exec-approve</span> / <span class="mono">trusted</span>) is set on each host or folder; the default for ad-hoc sessions is
      <span class="mono">{status?.default_tier ?? "read"}</span>.</p>

    {#if status}
      <div class="row">
        <label class="sw"><input type="checkbox" checked={status.enabled} disabled={busy} onchange={toggle} /> Enable server</label>
        <span class="state" class:on={status.running}>{status.running ? `listening on 127.0.0.1:${status.port}` : "stopped"}</span>
      </div>

      <div class="field">
        <span>Bearer token <span class="dim">(per launch)</span></span>
        <div class="tokrow">
          <input class="mono" readonly value={showToken ? status.token : "•".repeat(24)} />
          <button type="button" onclick={() => (showToken = !showToken)}>{showToken ? "hide" : "show"}</button>
          <button type="button" onclick={() => writeText(status!.token)}>copy</button>
          <button type="button" onclick={regen} disabled={busy}>regenerate</button>
        </div>
      </div>

      <div class="field">
        <span>Client config</span>
        <div class="tokrow">
          <button type="button" onclick={() => copy("http")}>{copied === "http" ? "copied!" : "Copy HTTP config"}</button>
          <button type="button" onclick={() => copy("stdio")}>{copied === "stdio" ? "copied!" : "Copy stdio config"}</button>
          <span class="dim">HTTP works while the app is open; stdio spawns a proxy.</span>
        </div>
      </div>

      <div class="field">
        <span>Call log</span>
        <div class="log mono">
          {#each log as l, i (l.ts + l.tool + i)}
            <div class="ll" class:err={!l.ok}>
              <span class="t">{when(l.ts)}</span>
              <span class="tool">{l.tool}</span>
              <span class="dec dec-{l.decision}">{l.decision}</span>
              <span class="dim">{l.tier}{l.session ? ` · ${l.session.slice(0, 8)}` : ""}</span>
              <span class="det">{l.exit_code !== undefined && l.exit_code !== null ? `exit ${l.exit_code} · ` : ""}{l.duration_ms}ms</span>
            </div>
          {/each}
          {#if !log.length}<div class="dim">no calls yet</div>{/if}
        </div>
      </div>
    {/if}
    {#if error}<p class="err">{error}</p>{/if}
    <div class="foot"><button type="button" onclick={onclose}>Close</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: #0009; display: grid; place-content: center; z-index: 10; }
  .dialog { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 560px; max-width: 92vw; max-height: 90vh; overflow: auto; display: flex; flex-direction: column; gap: 10px; }
  h2 { margin: 0; font-size: 15px; font-weight: 600; }
  p { margin: 0; color: var(--fg-dim); font-size: 12px; }
  .row { display: flex; align-items: center; gap: 10px; }
  .sw { display: flex; align-items: center; gap: 6px; }
  .state { color: var(--fg-dim); font-size: 12px; }
  .state.on { color: #5fd38a; }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--fg-dim); }
  .tokrow { display: flex; gap: 6px; align-items: center; }
  .tokrow input { flex: 1; min-width: 0; }
  .mono { font-family: var(--font-mono); }
  .dim { color: var(--fg-dim); }
  .log { background: var(--bg-sunken); border: 1px solid var(--line); border-radius: 4px; max-height: 220px; overflow: auto; padding: 4px 6px; font-size: 11px; }
  .ll { display: grid; grid-template-columns: 62px 110px 72px auto 1fr; gap: 6px; align-items: baseline; }
  .ll.err { color: var(--danger); }
  .ll .tool { color: var(--fg); }
  .dec-allow, .dec-approved { color: #5fd38a; }
  .dec-deny, .dec-denied { color: var(--danger); }
  .det { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--fg-dim); }
  .err { color: var(--danger); margin: 0; }
  .foot { display: flex; justify-content: flex-end; }
  button { padding: 2px 8px; }
</style>
