<script>
  import { onMount, onDestroy } from "svelte";
  import { listDevices, inputLevel, startLevelMeter, stopLevelMeter } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let devices = $state({ inputs: [], outputs: [] });
  let level = $state({ peak_db: -120, rms_db: -120 });
  let metering = $state(false);
  let timer = null;

  onMount(async () => {
    try {
      devices = await listDevices();
    } catch (e) {
      devices = { inputs: [], outputs: [], error: String(e) };
    }
  });

  onDestroy(() => stop());

  // Brief 5.7: a live level meter is the only acceptable real-time visual, and it exists
  // only here. It is started explicitly so the microphone is not held open by default.
  async function start() {
    await startLevelMeter();
    metering = true;
    timer = setInterval(async () => {
      try {
        level = await inputLevel();
      } catch {
        /* the meter stopping is not worth an error banner */
      }
    }, 100);
  }

  function stop() {
    if (timer) clearInterval(timer);
    timer = null;
    if (metering) stopLevelMeter();
    metering = false;
    level = { peak_db: -120, rms_db: -120 };
  }

  // -60 dBFS at the left edge, 0 at the right.
  const width = $derived(Math.max(0, Math.min(100, ((level.peak_db + 60) / 60) * 100)));
</script>

<h1>Audio</h1>
<p class="subtitle">
  The cue output is chosen separately from the capture input, because listening on
  headphones while speaking into a desk microphone is the normal case.
</p>

<div class="field">
  <label for="input">Input device</label>
  <select
    id="input"
    value={config.audio.input_device}
    onchange={(e) => { config.audio.input_device = e.currentTarget.value; onchange(); }}
  >
    <option value="">System default</option>
    {#each devices.inputs as d}
      <option value={d.name}>{d.name} -- {d.sample_rate} Hz, {d.channels} ch</option>
    {/each}
  </select>
</div>

<div class="field">
  <span class="pseudo-label">Input level</span>
  <div class="meter"><div style="width:{width}%"></div></div>
  <div class="row" style="margin-top:8px">
    <button onclick={() => (metering ? stop() : start())}>
      {metering ? "Stop meter" : "Start meter"}
    </button>
    <span class="hint mono" style="margin:0">
      {metering ? `peak ${level.peak_db.toFixed(1)} dBFS` : "not running"}
    </span>
  </div>
  <p class="hint">Aim for peaks between -18 and -6 dBFS while speaking normally.</p>
</div>

<div class="field">
  <label for="gain">Input gain</label>
  <input
    id="gain"
    type="number"
    step="0.1"
    min="0.1"
    max="8"
    value={config.audio.input_gain}
    oninput={(e) => { config.audio.input_gain = +e.currentTarget.value; onchange(); }}
  />
</div>

<h2>Cues</h2>

<div class="field">
  <label for="output">Output device for cues</label>
  <select
    id="output"
    value={config.audio.output_device}
    onchange={(e) => { config.audio.output_device = e.currentTarget.value; onchange(); }}
  >
    <option value="">System default</option>
    {#each devices.outputs as d}
      <option value={d.name}>{d.name}</option>
    {/each}
  </select>
</div>

<div class="field">
  <label for="vol">Cue volume</label>
  <input
    id="vol"
    type="number"
    step="0.05"
    min="0"
    max="1"
    value={config.cues.volume}
    oninput={(e) => { config.cues.volume = +e.currentTarget.value; onchange(); }}
  />
</div>

<label class="check">
  <input
    type="checkbox"
    checked={config.cues.tick_on_paste}
    onchange={(e) => { config.cues.tick_on_paste = e.currentTarget.checked; onchange(); }}
  />
  <span>Tick when text is pasted</span>
</label>

<h2>While recording</h2>

<label class="check">
  <input
    type="checkbox"
    checked={config.audio.duck_others}
    onchange={(e) => { config.audio.duck_others = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Quieten other applications
    <span class="hint" style="margin:0">
      Otherwise the microphone hears your music and transcribes it.
    </span>
  </span>
</label>

{#if config.audio.duck_others}
  <div class="field">
    <label for="ducklevel">Leave them at</label>
    <select
      id="ducklevel"
      value={String(config.audio.duck_level)}
      onchange={(e) => { config.audio.duck_level = +e.currentTarget.value; onchange(); }}
    >
      <option value="0">Silent</option>
      <option value="0.05">5%</option>
      <option value="0.15">15%</option>
      <option value="0.3">30%</option>
      <option value="0.5">50%</option>
    </select>
    <p class="hint">
      Volumes are scaled, not set, so something already playing quietly does not get
      louder. Restored the moment recording stops.
    </p>
  </div>
{/if}

<h2>Speech detection</h2>

<div class="field">
  <label for="minspeech">Minimum speech before transcribing (ms)</label>
  <input
    id="minspeech"
    type="number"
    min="0"
    max="5000"
    value={config.audio.min_speech_ms}
    oninput={(e) => { config.audio.min_speech_ms = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">
    Clips with less detected speech than this are rejected and never reach the speech
    model, which is what stops it inventing text during silence.
  </p>
</div>

<h2>Output</h2>

<div class="field">
  <label for="thresh">Use the clipboard above this many characters</label>
  <input
    id="thresh"
    type="number"
    min="0"
    max="100000"
    value={config.output.clipboard_threshold}
    oninput={(e) => { config.output.clipboard_threshold = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">Shorter text is typed directly, which never touches the clipboard.</p>
</div>

<label class="check">
  <input
    type="checkbox"
    checked={config.output.keep_on_clipboard}
    onchange={(e) => { config.output.keep_on_clipboard = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Leave the dictated text on the clipboard
    <span class="hint" style="margin:0">
      When off, whatever was on the clipboard before is put back after pasting.
    </span>
  </span>
</label>
