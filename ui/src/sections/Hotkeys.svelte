<script>
  import { onMount, onDestroy } from "svelte";
  import { autostartEnabled, setAutostart, lastHotkey } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let autostart = $state(false);
  let autostartError = $state("");
  let seen = $state(null);
  let seenTimer = null;

  onMount(async () => {
    try {
      autostart = await autostartEnabled();
    } catch (e) {
      autostartError = String(e);
    }
    // Polled while this section is open only; it stops on destroy.
    seenTimer = setInterval(async () => {
      try {
        seen = await lastHotkey();
      } catch {
        /* nothing worth reporting */
      }
    }, 400);
  });

  onDestroy(() => {
    if (seenTimer) clearInterval(seenTimer);
  });

  async function toggleAutostart(on) {
    autostartError = "";
    try {
      await setAutostart(on);
      autostart = await autostartEnabled();
    } catch (e) {
      autostartError = String(e);
    }
  }

  // Brief 5.1: a capture-style field. Click, press the combination, it records it.
  let capturing = $state(null);
  let conflict = $state("");

  function describe(e) {
    const parts = [];
    if (e.ctrlKey) parts.push("Ctrl");
    if (e.altKey) parts.push("Alt");
    if (e.shiftKey) parts.push("Shift");
    if (e.metaKey) parts.push("Win");

    let key = e.key;
    if (key === " ") key = "Space";
    else if (["Control", "Alt", "Shift", "Meta"].includes(key)) return null;
    else if (key.length === 1) key = key.toUpperCase();

    if (!parts.length) return null;
    parts.push(key);
    return parts.join("+");
  }

  function onkey(field, e) {
    e.preventDefault();
    const combo = describe(e);
    if (!combo) return;

    const other = field === "hotkey" ? config.paste_raw_hotkey : config.hotkey;
    if (combo === other) {
      conflict = `${combo} is already bound to the other action.`;
      return;
    }
    conflict = "";
    config[field] = combo;
    capturing = null;
    onchange();
  }
</script>

<h1>Hotkeys</h1>

<div class="status info">
  <strong>Press a Lathe hotkey now.</strong> Whatever it receives appears here, so you can
  confirm which binding a key actually triggers.
  <p style="margin:6px 0 0" class="mono">
    {seen ? seen[0] + "  ->  " + seen[1] : "nothing received yet"}
  </p>
</div>
<p class="subtitle">
  One binding does both styles. A tap shorter than the threshold latches recording on
  until you tap again; holding it longer records only while held.
</p>

{#if conflict}
  <div class="status bad">{conflict}</div>
{/if}

<div class="status info">
  Changing a binding needs a restart. The hook that watches the keyboard is installed
  once at startup and cannot be rebound while running.
</div>

<div class="field">
  <label for="hotkey">Dictate</label>
  <button
    id="hotkey"
    class="mono"
    style="width:100%;text-align:left"
    onclick={() => { capturing = "hotkey"; conflict = ""; }}
    onkeydown={(e) => capturing === "hotkey" && onkey("hotkey", e)}
    onblur={() => (capturing = null)}
  >
    {capturing === "hotkey" ? "Press a combination" : config.hotkey}
  </button>
  <p class="hint">
    Avoid Ctrl+Space: it is the Windows IME toggle and the completion key in most editors.
  </p>
</div>

<div class="field">
  <label for="rawkey">Paste the last transcript without cleanup</label>
  <button
    id="rawkey"
    class="mono"
    style="width:100%;text-align:left"
    onclick={() => { capturing = "paste_raw"; conflict = ""; }}
    onkeydown={(e) => capturing === "paste_raw" && onkey("paste_raw_hotkey", e)}
    onblur={() => (capturing = null)}
  >
    {capturing === "paste_raw" ? "Press a combination" : config.paste_raw_hotkey}
  </button>
  <p class="hint">Rescues a dictation when cleanup mangles something.</p>
</div>

<h2>Timing</h2>

<div class="field">
  <label for="tap">Tap threshold (ms)</label>
  <input
    id="tap"
    type="number"
    min="100"
    max="1500"
    value={config.tap_threshold_ms}
    oninput={(e) => { config.tap_threshold_ms = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">Presses shorter than this latch. Longer presses are push-to-talk.</p>
</div>

<div class="field">
  <label for="cap">Maximum recording length (seconds)</label>
  <input
    id="cap"
    type="number"
    min="10"
    max="3600"
    value={config.max_record_secs}
    oninput={(e) => { config.max_record_secs = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">A hard cap, so a stuck key cannot fill memory.</p>
</div>

<h2>Per preset</h2>
<p class="hint" style="margin-bottom:10px">
  An optional binding that dictates with one specific preset, whatever the tray has
  selected. Leave empty for none.
</p>

{#each config.presets as p, i}
  <div class="field">
    <label for={`ph${i}`}>{p.name}</label>
    <button
      id={`ph${i}`}
      class="mono"
      style="width:100%;text-align:left"
      onclick={() => { capturing = `preset:${i}`; conflict = ""; }}
      onkeydown={(e) => {
        if (capturing !== `preset:${i}`) return;
        e.preventDefault();
        const combo = describe(e);
        if (!combo) return;
        const taken =
          combo === config.hotkey ||
          combo === config.paste_raw_hotkey ||
          config.presets.some((q, j) => j !== i && q.hotkey === combo);
        if (taken) { conflict = `${combo} is already bound.`; return; }
        conflict = "";
        config.presets[i].hotkey = combo;
        capturing = null;
        onchange();
      }}
      onblur={() => (capturing = null)}
    >
      {capturing === `preset:${i}` ? "Press a combination" : p.hotkey || "none"}
    </button>
    {#if p.hotkey}
      <p class="hint">
        <button
          style="padding:2px 8px"
          onclick={() => { config.presets[i].hotkey = null; onchange(); }}>Clear</button
        >
      </p>
    {/if}
  </div>
{/each}

<h2>Startup</h2>

<label class="check">
  <input type="checkbox" checked={autostart} onchange={(e) => toggleAutostart(e.currentTarget.checked)} />
  <span>
    Start Lathe when I sign in
    <span class="hint" style="margin:0">
      Written to the current user's Run key. No elevation, nothing system-wide.
    </span>
  </span>
</label>
{#if autostartError}<div class="status bad" style="margin-top:8px">{autostartError}</div>{/if}
