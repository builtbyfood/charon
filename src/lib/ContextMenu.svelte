<script lang="ts">
  // Minimal right-click menu. Positioned at the pointer, clamped to the window,
  // closed on click-away / Escape.
  export interface MenuItem { label: string; action?: () => void; disabled?: boolean; separator?: boolean }

  let { x, y, items, onclose }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  let el = $state<HTMLDivElement | null>(null);
  let size = $state({ w: 0, h: 0 });
  // Keep the menu on screen once we know its size.
  $effect(() => { if (el) { const r = el.getBoundingClientRect(); size = { w: r.width, h: r.height }; } });
  const left = $derived(Math.min(x, window.innerWidth - size.w - 4));
  const top = $derived(Math.min(y, window.innerHeight - size.h - 4));

  function run(item: MenuItem) {
    if (item.disabled || item.separator) return;
    onclose();
    item.action?.();
  }
  function onkeydown(e: KeyboardEvent) { if (e.key === "Escape") onclose(); }
</script>

<svelte:window {onkeydown} onmousedown={(e) => { if (el && !el.contains(e.target as Node)) onclose(); }} onblur={onclose} />

<div class="menu" bind:this={el} style:left="{left}px" style:top="{top}px" role="menu">
  {#each items as item, i (i)}
    {#if item.separator}
      <div class="sep"></div>
    {:else}
      <button type="button" role="menuitem" disabled={item.disabled} onclick={() => run(item)}>{item.label}</button>
    {/if}
  {/each}
</div>

<style>
  .menu { position: fixed; z-index: 20; min-width: 180px; background: var(--bg-raised); border: 1px solid var(--line); border-radius: 4px; padding: 4px; box-shadow: 0 6px 20px #0008; display: flex; flex-direction: column; }
  .menu button { text-align: left; border: 0; background: transparent; padding: 5px 10px; border-radius: 3px; }
  .menu button:hover:not(:disabled) { background: var(--bg); }
  .menu button:disabled { opacity: .4; cursor: default; }
  .sep { height: 1px; background: var(--line); margin: 4px 2px; }
</style>
