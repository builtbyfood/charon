<script lang="ts">
  // Flag picker popup (P5): a filtered list of a command's options, from tldr +
  // `--help`. The terminal owns key handling (so Tab is never intercepted); this
  // is presentation only. Positioned near the cursor by the caller.
  import type { Flag } from "./flags";

  let { x, y, command, flags, selected, loading, onaccept }: {
    x: number; y: number;
    command: string;
    flags: Flag[];
    selected: number;
    loading: boolean;
    /** Accept the flag at this index (mouse click). */
    onaccept?: (index: number) => void;
  } = $props();

  let list = $state<HTMLDivElement | null>(null);
  // Keep the highlighted row visible as the user arrows through.
  $effect(() => {
    selected;
    list?.querySelector<HTMLElement>(".sel")?.scrollIntoView({ block: "nearest" });
  });
</script>

<!-- mousedown preventDefault keeps focus in the terminal (clicking a div would
     otherwise blur xterm); accept happens on click, which still fires. The popup
     is deliberately not focusable — the terminal keeps focus and owns the keys. -->
<!-- svelte-ignore a11y_interactive_supports_focus -->
<div class="popup" style:left="{x}px" style:top="{y}px" role="listbox" aria-label="{command} flags"
  onmousedown={(e) => e.preventDefault()}>
  <div class="head"><span class="cmd">{command}</span>{#if loading}<span class="dim">loading --help…</span>{/if}</div>
  <div class="rows" bind:this={list}>
    {#each flags as f, i (f.names.join())}
      <!-- svelte-ignore a11y_click_events_have_key_events a11y_interactive_supports_focus -->
      <div class="row" class:sel={i === selected} role="option" aria-selected={i === selected}
        onclick={() => onaccept?.(i)}>
        <span class="names">{f.names.join(", ")}</span>
        <span class="desc" title={f.desc ?? ""}>{f.desc ?? ""}</span>
        {#if f.from === "tldr"}<span class="src" title="from a tldr example">tldr</span>{/if}
      </div>
    {/each}
    {#if !flags.length && !loading}<div class="empty">no flags found</div>{/if}
  </div>
  <div class="foot"><span class="dim">↑↓ select · Enter insert · Esc close · Tab → shell</span></div>
</div>

<style>
  .popup { position: fixed; z-index: 30; width: 420px; max-width: 70vw; background: var(--bg-raised); border: 1px solid var(--line); border-radius: 5px; box-shadow: 0 8px 24px var(--shadow); font-size: 12px; overflow: hidden; }
  .head { display: flex; justify-content: space-between; align-items: center; padding: 3px 8px; border-bottom: 1px solid var(--line); }
  .head .cmd { font-family: var(--font-mono); color: var(--fg); font-weight: 600; }
  .rows { max-height: 240px; overflow: auto; }
  .row { display: grid; grid-template-columns: auto 1fr auto; gap: 8px; align-items: baseline; padding: 2px 8px; cursor: pointer; }
  .row:hover { background: var(--bg); }
  .row.sel { background: var(--accent); color: var(--on-accent); }
  .row.sel .names, .row.sel .desc, .row.sel .src { color: var(--on-accent); }
  .names { font-family: var(--font-mono); color: var(--warn); white-space: nowrap; }
  .desc { color: var(--fg-dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .src { color: var(--fg-dim); font-size: 10px; }
  .empty { padding: 6px 8px; color: var(--fg-dim); }
  .foot { padding: 2px 8px; border-top: 1px solid var(--line); }
  .dim { color: var(--fg-dim); }
</style>
