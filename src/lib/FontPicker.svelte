<script lang="ts">
  // Terminal font + size, with the installed monospace fonts offered as a list
  // (any name can still be typed) and a live preview. Blank = "inherit": the
  // placeholders show what applies instead.
  import { onMount } from "svelte";
  import { fontFamilies } from "./api";
  import { installedMonoFonts } from "./fonts";
  import { fontStack } from "./prefs";

  let { family = $bindable(""), size = $bindable<number | undefined>(undefined), inheritFamily, inheritSize, placeholder, onchange }: {
    family?: string;
    size?: number;
    /** Shown (and previewed) while blank; omit where blank is not allowed. */
    inheritFamily?: string;
    inheritSize?: number;
    /** Placeholder for a blank family when nothing is inherited. */
    placeholder?: string;
    onchange?: (family: string, size: number | undefined) => void;
  } = $props();

  const listId = `fonts-${Math.random().toString(36).slice(2)}`;
  let fonts = $state<string[]>([]);
  onMount(async () => { fonts = await installedMonoFonts(fontFamilies); });

  const shown = $derived(family.trim() || inheritFamily || "");
  const shownSize = $derived(size || inheritSize || 13);
  const unknown = $derived(!!family.trim() && fonts.length > 0 && !fonts.some((f) => f.toLowerCase() === family.trim().toLowerCase()));
  const changed = () => onchange?.(family.trim(), size || undefined);
</script>

<div class="fp">
  <div class="inputs">
    <input class="family" list={listId} bind:value={family} spellcheck="false" aria-label="Font"
      placeholder={inheritFamily ? `inherit: ${inheritFamily}` : placeholder ?? "font"} onchange={changed} />
    <input class="size" type="number" min="6" max="40" step="1" aria-label="Font size"
      value={size ?? ""} placeholder={inheritSize ? String(inheritSize) : "13"}
      oninput={(e) => { const v = parseInt(e.currentTarget.value, 10); size = Number.isFinite(v) ? Math.max(6, Math.min(40, v)) : undefined; }}
      onchange={changed} />
    <span class="px">px</span>
  </div>
  <datalist id={listId}>{#each fonts as f (f)}<option value={f}></option>{/each}</datalist>
  <div class="preview" style:font-family={fontStack(shown)} style:font-size="{shownSize}px" title="Preview">
    user@host:~$ ls -la  0O 1lI {"{}"} [] =&gt; ✓
  </div>
  {#if unknown}<div class="hint">Not found among installed monospace fonts; the fallback is used if it is missing.</div>{/if}
</div>

<style>
  .fp { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .inputs { display: flex; gap: 4px; align-items: center; }
  .family { flex: 1; min-width: 0; }
  .size { width: 64px; }
  .px { color: var(--fg-dim); font-size: 11px; }
  .preview { padding: 4px 8px; border: 1px solid var(--line); border-radius: 3px; background: var(--term-bg); color: var(--fg); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; line-height: 1.3; }
  .hint { color: var(--warn); font-size: 11px; }
</style>
