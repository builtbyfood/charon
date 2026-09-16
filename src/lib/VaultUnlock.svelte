<script lang="ts">
  // Master-password dialog for the portable (file) vault. Asked once per run; the
  // password only ever goes to `vault_unlock` and is never kept here.
  import { vaultUnlock, type VaultStatus } from "./api";

  let { status, ondone }: { status: VaultStatus; ondone: (s: VaultStatus | null) => void } = $props();

  let master = $state("");
  let confirm = $state("");
  let error = $state("");
  let busy = $state(false);
  const creating = $derived(!status.initialised);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (creating && master !== confirm) { error = "passwords do not match"; return; }
    busy = true; error = "";
    try { ondone(await vaultUnlock(master)); }
    catch (err) { error = String(err); }
    finally { busy = false; master = ""; confirm = ""; }
  }
</script>

<div class="backdrop">
  <form class="dialog" onsubmit={submit}>
    <h2>{creating ? "Create vault" : "Unlock vault"}</h2>
    <p>
      {#if creating}
        Portable mode keeps credentials in <code>vault.age</code> next to the exe, encrypted with a master password. Pick one now; it is asked for once per run.
      {:else}
        Enter the master password for <code>vault.age</code>.
      {/if}
    </p>
    <!-- svelte-ignore a11y_autofocus -->
    <input type="password" placeholder="Master password" bind:value={master} autofocus autocomplete="off" />
    {#if creating}<input type="password" placeholder="Confirm" bind:value={confirm} autocomplete="off" />{/if}
    {#if error}<p class="err">{error}</p>{/if}
    <div class="row">
      <button type="button" onclick={() => ondone(null)}>Skip for now</button>
      <button type="submit" class="primary" disabled={!master || busy}>{busy ? "Working…" : creating ? "Create" : "Unlock"}</button>
    </div>
  </form>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: #0009; display: grid; place-content: center; z-index: 10; }
  .dialog { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 16px; width: 360px; display: flex; flex-direction: column; gap: 8px; }
  h2 { margin: 0; font-size: 15px; font-weight: 600; }
  p { margin: 0; color: var(--fg-dim); font-size: 12px; }
  code { font-family: var(--font-mono); color: var(--fg); }
  input { font-family: var(--font-mono); }
  .err { color: var(--danger); }
  .row { display: flex; justify-content: flex-end; gap: 6px; margin-top: 4px; }
  .primary { border-color: var(--accent); }
</style>
