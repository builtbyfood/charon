<script lang="ts">
  // Settings → Appearance (theme, default terminal font), Tabs (reopen at start) and Terminal (renderer,
  // output-timing trace). Values live in the profile (App.svelte owns it); this
  // only shows them and reports changes.
  import FontPicker from "./FontPicker.svelte";
  import { THEMES, type ThemePref } from "./themes";

  let { theme, font, fontSize, renderer, trace, restoreTabs, ontheme, onfont, onrenderer, ontrace, onrestoretabs, onclose }: {
    theme: ThemePref;
    font: string;
    fontSize: number;
    renderer: "webgl" | "canvas" | "dom";
    trace: boolean;
    restoreTabs: boolean;
    ontheme: (t: ThemePref) => void;
    onfont: (family: string, size: number | undefined) => void;
    onrenderer: (r: "webgl" | "canvas" | "dom") => void;
    ontrace: (on: boolean) => void;
    onrestoretabs: (on: boolean) => void;
    onclose: () => void;
  } = $props();

  // Local copies for the picker; reported on change.
  // svelte-ignore state_referenced_locally
  let fam = $state(font);
  // svelte-ignore state_referenced_locally
  let size = $state<number | undefined>(fontSize);
  const choices: { id: ThemePref; label: string }[] = [
    ...Object.values(THEMES).map((t) => ({ id: t.id, label: t.label })),
    { id: "system", label: "Match system" },
  ];
</script>

<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="dialog" role="dialog" aria-label="Settings">
    <h2>Settings</h2>
    <h3>Appearance</h3>
    <div class="themes" role="radiogroup" aria-label="Theme">
      {#each choices as c (c.id)}
        <button type="button" role="radio" aria-checked={theme === c.id} class="theme" class:sel={theme === c.id} onclick={() => ontheme(c.id)}>
          <span class="swatch" data-t={c.id}>
            {#if c.id === "system"}
              <span class="half" style:background={THEMES["charon-light"].ui.bg}></span><span class="half" style:background={THEMES["charon-dark"].ui.bg}></span>
            {:else}
              {@const t = THEMES[c.id]}
              <span class="mini" style:background={t.ui["bg-raised"]} style:border-color={t.ui.line}>
                <span class="bar" style:background={t.term.background}>
                  <i style:color={t.term.green}>$</i><i style:color={t.term.foreground}> ls </i><i style:color={t.term.blue}>src </i><i style:color={t.term.red}>!</i>
                </span>
              </span>
            {/if}
          </span>
          {c.label}
        </button>
      {/each}
    </div>
    <div class="field">
      <span>Terminal font <span class="dim">the default; a connection can set its own (Edit → Terminal font)</span></span>
      <FontPicker bind:family={fam} bind:size placeholder="JetBrains Mono" onchange={(f, s) => onfont(f, s)} />
    </div>

    <h3>Tabs</h3>
    <label class="sw">
      <input type="checkbox" checked={restoreTabs} onchange={(e) => onrestoretabs((e.currentTarget as HTMLInputElement).checked)} />
      <span>Reopen tabs from last time</span>
    </label>
    <p>At start, the terminal tabs open when Charon was closed connect again, those in torn-off windows included (they come back in the main window). Elevated local shells and editor files are not reopened.</p>

    <h3>Terminal</h3>
    <label class="field row">
      <span>Renderer</span>
      <select value={renderer} onchange={(e) => onrenderer((e.currentTarget as HTMLSelectElement).value as "webgl" | "canvas" | "dom")}>
        <option value="webgl">WebGL (fastest)</option>
        <option value="canvas">Canvas</option>
        <option value="dom">DOM (no GPU)</option>
      </select>
    </label>
    <p>Try Canvas or DOM if text glitches, goes blank, or only updates after a key press. Applies to every tab at once.</p>
    <label class="sw">
      <input type="checkbox" checked={trace} onchange={(e) => ontrace((e.currentTarget as HTMLInputElement).checked)} />
      <span>Trace output timing</span>
    </label>
    <p>Diagnostics: logs when each chunk of terminal output is sent, received, parsed and painted (<code>[charon] out:</code> lines, also on stdout). Leave off normally.</p>
    <div class="foot"><button type="button" onclick={onclose}>Close</button></div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: var(--overlay); display: grid; place-content: center; z-index: 10; }
  .dialog { background: var(--bg-raised); border: 1px solid var(--line); border-radius: 6px; padding: 14px 16px; width: 480px; max-width: 92vw; max-height: 90vh; overflow: auto; display: flex; flex-direction: column; gap: 10px; box-shadow: 0 10px 30px var(--shadow); }
  h2 { margin: 0; font-size: 15px; font-weight: 600; }
  h3 { margin: 4px 0 0; font-size: 13px; font-weight: 600; color: var(--fg-dim); }
  p { margin: 0; color: var(--fg-dim); font-size: 12px; }
  .dim { color: var(--fg-dim); font-size: 11px; font-weight: normal; }
  .field { display: flex; flex-direction: column; gap: 4px; font-size: 12px; }
  .field.row { flex-direction: row; align-items: center; gap: 10px; }
  .sw { display: flex; align-items: center; gap: 6px; font-size: 12px; }
  .themes { display: flex; gap: 8px; }
  .theme { flex: 1; display: flex; flex-direction: column; align-items: center; gap: 6px; padding: 8px; font-size: 12px; background: var(--bg); }
  .theme.sel { border-color: var(--accent); box-shadow: inset 0 0 0 1px var(--accent); }
  .swatch { display: flex; width: 100%; height: 44px; border-radius: 4px; overflow: hidden; }
  .half { flex: 1; }
  .swatch[data-t="system"] { border: 1px solid var(--line); }
  .mini { flex: 1; display: flex; padding: 6px; border: 1px solid; border-radius: 4px; }
  .bar { flex: 1; display: flex; align-items: center; padding: 0 6px; border-radius: 2px; font-family: var(--font-mono); font-size: 11px; white-space: pre; }
  .bar i { font-style: normal; }
  .foot { display: flex; justify-content: flex-end; }
  button { padding: 2px 8px; }
</style>
