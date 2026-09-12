<script>
  import { onMount } from "svelte";
  import {
    listDevices,
    modelStatus,
    runBenchmark,
    downloadableModels,
    downloadProgress,
    startDownload,
    cancelDownload,
    loadConfig,
    pickModelsDir,
    planModelsMove,
    setModelsDir,
  } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let adapters = $state([]);
  let status = $state(null);
  let bench = $state(null);
  let running = $state(false);
  let error = $state("");
  let available = $state([]);
  let progress = $state(null);
  let showFiles = $state(false);
  let poll = null;
  let movePlan = $state(null);
  let movingTo = $state("");

  const busy = $derived(!!progress && !progress.finished);
  const pct = (p) => (p.total ? Math.round((p.done / p.total) * 100) : 0);
  const mb = (bytes) => (bytes / (1024 * 1024)).toFixed(0);
  const gb = (bytes) => (bytes / (1024 * 1024 * 1024)).toFixed(2);

  // Each slot lists the options worth choosing between, best first, with the reason.
  // Every figure here was measured on this machine; see the amendments for the runs.
  const SPEECH = [
    {
      file: "cohere-transcribe-q8_0.gguf",
      label: "Cohere Transcribe  Q8_0",
      note: "Recommended. Near-lossless, and measured no slower than Q5_0 here.",
      size: "2.26 GB",
    },
    {
      file: "cohere-transcribe-q5_0.gguf",
      label: "Cohere Transcribe  Q5_0",
      note: "0.6 GB smaller with no speed advantage. Only worth it if memory is tight.",
      size: "1.62 GB",
    },
    {
      file: "cohere-transcribe-f16.gguf",
      label: "Cohere Transcribe  F16",
      note: "Full precision, but 18% slower here and no better output. Not recommended.",
      size: "3.85 GB",
    },
    {
      file: "whisper-large-v3-turbo-q5_0.gguf",
      label: "Whisper large-v3-turbo  Q5_0",
      note: "Slower and less accurate on this hardware. Kept as an alternative.",
      size: "0.55 GB",
    },
  ];

  const CLEANUP = [
    {
      file: "s1-mini-q8_0.gguf",
      label: "S1-mini  Q8_0",
      note: "Recommended. Word-for-word identical to F16 on 19 dictations in 20, never dropped content, half the size and twice the decode speed.",
      size: "0.75 GB",
    },
    {
      file: "s1-mini-f16.gguf",
      label: "S1-mini  F16",
      note: "Full precision. Slower, and large enough that it can fail to fit beside the speech model on an 8 GB card.",
      size: "1.41 GB",
    },
    {
      file: "s1-mini-q4_k_m.gguf",
      label: "S1-mini  Q4_K_M",
      note: "Three times smaller and faster, but can silently drop meaning.",
      size: "0.45 GB",
    },
  ];

  const MULTILINGUAL = [
    {
      file: "gemma-4-E2B_q4_0-it.gguf",
      label: "Gemma 4 E2B  QAT Q4_0",
      note: "Recommended. Fewer errors than Gemma 3 4B on Polish at the same speed, and only about 1 GB of it sits in graphics memory; the rest stays in system RAM.",
      size: "3.12 GB",
    },
    {
      file: "gemma-3-4b-it-qat-Q4_0.gguf",
      label: "Gemma 3 4B  QAT Q4_0",
      note: "The previous default. Slightly more errors on Polish and 1.8 GB of graphics memory.",
      size: "2.35 GB",
    },
    {
      file: "gemma-3-4b-it-Q8_0.gguf",
      label: "Gemma 3 4B  Q8_0",
      note: "Higher precision, but 1.5 GB more and it pushes the non-English pair to 6.1 GB resident.",
      size: "3.85 GB",
    },
  ];

  const present = (file) => available.some((m) => m.file === file && m.present);

  onMount(async () => {
    try {
      adapters = (await listDevices()).adapters;
      status = await modelStatus();
      await refreshDownloads();
      progress = await downloadProgress();
    } catch (e) {
      error = String(e);
    }
  });

  async function refreshDownloads() {
    try {
      available = await downloadableModels();
    } catch (e) {
      error = String(e);
    }
  }

  async function benchmark() {
    running = true;
    error = "";
    try {
      bench = await runBenchmark();
    } catch (e) {
      error = String(e);
    } finally {
      running = false;
    }
  }

  function pollProgress(onFinished) {
    poll = setInterval(async () => {
      progress = await downloadProgress();
      if (progress && progress.finished) {
        clearInterval(poll);
        poll = null;
        await onFinished();
      }
    }, 400);
  }

  async function download(file) {
    error = "";
    try {
      await startDownload(file);
      pollProgress(async () => {
        await refreshDownloads();
        status = await modelStatus();
      });
    } catch (e) {
      error = String(e);
    }
  }

  async function cancel() {
    try {
      await cancelDownload();
    } catch (e) {
      error = String(e);
    }
  }

  async function refreshAfterDirChange() {
    config = await loadConfig();
    status = await modelStatus();
    await refreshDownloads();
  }

  async function changeDir() {
    error = "";
    try {
      const chosen = await pickModelsDir(status.dir);
      if (!chosen) return;
      const plan = await planModelsMove(chosen);
      if (plan.same) return;
      if (plan.files === 0) {
        await setModelsDir(chosen, false);
        await refreshAfterDirChange();
        return;
      }
      movingTo = chosen;
      movePlan = plan;
    } catch (e) {
      error = String(e);
    }
  }

  async function moveFiles(move) {
    error = "";
    const to = movingTo;
    movePlan = null;
    movingTo = "";
    try {
      await setModelsDir(to, move);
      if (move) {
        pollProgress(refreshAfterDirChange);
      } else {
        await refreshAfterDirChange();
      }
    } catch (e) {
      error = String(e);
    }
  }

  function choose(field, file) {
    config.models[field] = file;
    onchange();
  }
</script>

<h1>Models</h1>
<p class="subtitle">
  Loaded on the first dictation, not at startup, and kept loaded from then on.
  Only the models the language you are speaking needs get loaded.
</p>

{#if error}<div class="status bad">{error}</div>{/if}

{#snippet slot(title, field, options, subtitle)}
  <h2>{title}</h2>
  <p class="hint" style="margin:-6px 0 10px">{subtitle}</p>
  <div class="choices">
    {#each options as o}
      {@const chosen = config.models[field] === o.file}
      {@const have = present(o.file)}
      {@const downloading = progress && progress.file === o.file && !progress.finished}
      <div class="choice" class:picked={chosen}>
        <button
          class="choice-pick"
          aria-pressed={chosen}
          onclick={() => choose(field, o.file)}
          disabled={!have && !chosen}
        >
          <span class="choice-head">
            <span class="choice-name">{o.label}</span>
            <span class="choice-size mono">{o.size}</span>
          </span>
          <span class="choice-note">{o.note}</span>
        </button>
        {#if !have}
          <p class="choice-missing">
            {downloading ? `Downloading -- ${pct(progress)}%` : "Not downloaded."}
            <button class="inline" disabled={busy} onclick={() => download(o.file)}>
              {downloading ? `${mb(progress.done)} of ${mb(progress.total)} MB` : "Get it"}
            </button>
          </p>
        {/if}
      </div>
    {/each}
  </div>
{/snippet}

{@render slot(
  "Speech",
  "whisper",
  SPEECH,
  "Turns what you said into text. The one choice that affects every dictation.",
)}

{@render slot(
  "Cleanup, English",
  "cleanup",
  CLEANUP,
  "Punctuates and tidies English. Purpose-built for exactly this, which is why it beats a general model at it.",
)}

{@render slot(
  "Cleanup, other languages",
  "cleanup_multilingual",
  MULTILINGUAL,
  "A general model doing the same job for languages S1-mini does not cover. Optional; without it non-English speech is recognised but pasted uncleaned.",
)}

{#if progress && !progress.finished}
  <div class="field" style="margin-top:14px">
    <span class="pseudo-label">Downloading {progress.file}</span>
    <div class="meter"><div style="width:{pct(progress)}%"></div></div>
    <div class="row" style="margin-top:8px">
      <button onclick={cancel}>Cancel</button>
      <span class="hint mono" style="margin:0">
        {mb(progress.done)} of {mb(progress.total)} MB
      </span>
    </div>
  </div>
{:else if progress && progress.error}
  <div class="status bad" style="margin-top:12px">{progress.error}</div>
{/if}

<h2>Storage</h2>
<p class="hint" style="margin:-6px 0 10px">
  Where the weights are kept. Several gigabytes of them, so a drive with room is a
  reasonable choice. Changing this offers to bring what you have already downloaded.
</p>

<div class="field">
  <span class="pseudo-label">Folder</span>
  <div class="row">
    <span class="mono" style="overflow-wrap:anywhere">{status ? status.dir : "—"}</span>
    <button onclick={changeDir}>Change&hellip;</button>
  </div>
</div>

{#if movePlan}
  <div class="status bad">
    Move {movePlan.files} file{movePlan.files === 1 ? "" : "s"} ({gb(movePlan.bytes)} GB) to
    the new folder?
    <div class="row" style="margin-top:8px">
      <button class="primary" onclick={() => moveFiles(true)}>Move</button>
      <button onclick={() => moveFiles(false)}>Leave them</button>
    </div>
  </div>
{/if}

<button class="disclose" aria-expanded={showFiles} onclick={() => (showFiles = !showFiles)}>
  {showFiles ? "Hide" : "Show"} files
  {#if status}
    <span class="hint" style="margin:0">
      {status.files.length} tracked, {gb(status.total_bytes)} GB
      {#if status.files.some((f) => !f.present)}
        <span style="color:var(--clay)">&mdash; one is missing</span>
      {/if}
    </span>
  {/if}
</button>

{#if showFiles && status}
  <table style="margin-top:10px">
    <thead>
      <tr><th>Role</th><th>File</th><th>Size</th><th>State</th></tr>
    </thead>
    <tbody>
      {#each status.files as f}
        <tr>
          <td>{f.role}</td>
          <td class="mono">{f.name}</td>
          <td>{f.present ? `${mb(f.size)} MB` : "--"}</td>
          <td style="color:{f.present ? 'var(--moss)' : 'var(--clay)'}">
            {f.present ? "present" : "missing"}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<h2>Hardware</h2>

<div class="field">
  <label for="gpu">Graphics adapter</label>
  <select
    id="gpu"
    value={config.models.gpu_device < 0 ? -1 : config.models.gpu_device}
    onchange={(e) => { config.models.gpu_device = +e.currentTarget.value; onchange(); }}
  >
    <option value={-1}>
      Automatic{#if adapters.some((a) => a.preferred)} ({adapters.find((a) => a.preferred).name}){/if}
    </option>
    {#each adapters as a}
      <option value={a.id}>{a.name} -- {a.kind}, {mb(a.vram_total)} MB</option>
    {/each}
  </select>
  <p class="hint">
    Discrete adapters are listed first. Picking the integrated one by accident is the most
    common reason for this being slow.
  </p>
</div>

<div class="field">
  <label for="threads">CPU threads</label>
  <input
    id="threads"
    type="number"
    min="1"
    max="64"
    value={config.models.threads}
    oninput={(e) => { config.models.threads = +e.currentTarget.value; onchange(); }}
  />
</div>

<div class="field">
  <label for="idle">Unload after idle (seconds)</label>
  <input
    id="idle"
    type="number"
    min="0"
    max="86400"
    value={config.models.idle_unload_secs}
    oninput={(e) => { config.models.idle_unload_secs = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">Zero never unloads. Unloading frees graphics memory.</p>
</div>

<label class="check">
  <input
    type="checkbox"
    checked={config.models.keep_loaded}
    onchange={(e) => { config.models.keep_loaded = e.currentTarget.checked; onchange(); }}
  />
  <span>Keep models loaded permanently</span>
</label>
<p class="hint">
  On by default. A model loaded while other applications hold the graphics memory lands in
  system memory instead and stays slow until it is reloaded; keeping the models resident
  means that happens once at most.
</p>

<h2>Benchmark</h2>
<p class="hint" style="margin:-6px 0 10px">
  Runs a reference clip through the current configuration and reports where the time goes.
</p>
<button onclick={benchmark} disabled={running}>
  {running ? "Running" : "Run benchmark"}
</button>

{#if bench}
  <table style="margin-top:14px">
    <tbody>
      <tr><td>Model load (both)</td><td class="mono">{bench.load_ms} ms</td></tr>
      <tr><td>Speech detection</td><td class="mono">{bench.vad_ms} ms</td></tr>
      <tr><td>Transcription</td><td class="mono">{bench.asr_ms} ms</td></tr>
      <tr><td>Cleanup</td><td class="mono">{bench.cleanup_ms} ms</td></tr>
      <tr><td>Realtime factor</td><td class="mono">{bench.realtime_factor.toFixed(1)}x</td></tr>
    </tbody>
  </table>
{/if}

<h2>External endpoint</h2>
<p class="hint" style="margin:-6px 0 12px">
  Sends audio to a server instead of transcribing here. For a stronger model on a machine
  with more memory, or a hosted API. Off by default, and the only setting that sends audio
  anywhere.
</p>

<label class="check">
  <input
    type="checkbox"
    checked={config.remote_asr.enabled}
    onchange={(e) => { config.remote_asr.enabled = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Use an external endpoint
    <span class="hint" style="margin:0">
      Speech detection still runs locally, so silence is never sent.
    </span>
  </span>
</label>

{#if config.remote_asr.enabled}
  <div class="field">
    <label for="url">Base URL</label>
    <input
      id="url"
      class="mono"
      type="text"
      placeholder="https://host:8000/v1"
      value={config.remote_asr.base_url}
      oninput={(e) => { config.remote_asr.base_url = e.currentTarget.value; onchange(); }}
    />
    <p class="hint">
      Any server exposing <span class="mono">/v1/audio/transcriptions</span>.
    </p>
  </div>

  <div class="field">
    <label for="rmodel">Model name</label>
    <input
      id="rmodel"
      class="mono"
      type="text"
      value={config.remote_asr.model}
      oninput={(e) => { config.remote_asr.model = e.currentTarget.value; onchange(); }}
    />
  </div>

  <div class="field">
    <label for="key">API key</label>
    <input
      id="key"
      class="mono"
      type="password"
      value={config.remote_asr.api_key}
      oninput={(e) => { config.remote_asr.api_key = e.currentTarget.value; onchange(); }}
    />
    <p class="hint">Leave empty for a local server. Stored as plain text in config.toml.</p>
  </div>

  <label class="check">
    <input
      type="checkbox"
      checked={config.remote_asr.fallback_to_local}
      onchange={(e) => { config.remote_asr.fallback_to_local = e.currentTarget.checked; onchange(); }}
    />
    <span>
      Fall back to the local model if the endpoint fails
      <span class="hint" style="margin:0">
        Off by default, so a failure is reported instead of quietly producing a worse
        transcript from a different model.
      </span>
    </span>
  </label>
{/if}

<style>
  .choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 8px;
  }

  /* A card, not a button: the "Get it" control lives inside it, and a button nested in
     a button is invalid HTML whose clicks the disabled outer one swallows. */
  .choice {
    padding: 9px 11px;
    background: var(--surface-sunken);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .choice.picked {
    border-color: var(--clay);
  }

  .choice-pick {
    display: block;
    width: 100%;
    text-align: left;
    padding: 0;
    border: 0;
    border-radius: 0;
    background: none;
  }

  .choice-pick:disabled {
    opacity: 0.65;
  }

  .choice-head {
    display: flex;
    justify-content: space-between;
    gap: 10px;
  }

  .choice-name {
    font-weight: 500;
  }

  .choice.picked .choice-name {
    color: var(--clay);
  }

  .choice-size {
    color: var(--text-dim);
    font-size: 12px;
  }

  .choice-note {
    display: block;
    color: var(--text-dim);
    font-size: 12px;
    margin-top: 2px;
  }

  .choice-missing {
    color: var(--text-dim);
    font-size: 12px;
    margin: 5px 0 0;
  }

  .inline {
    padding: 1px 7px;
    font-size: 12px;
  }

  .disclose {
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
</style>
