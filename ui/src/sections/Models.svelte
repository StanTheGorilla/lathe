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
  // The slot whose menu of alternatives is open, by config field; one at a time.
  let openSlot = $state(null);
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
      file: "ggml-large-v3-turbo-q5_0.bin",
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
      file: "s1-mini-q6_k-mixed.gguf",
      label: "S1-mini  Q6_K mixed",
      note: "21% smaller and 10% faster than Q8_0. Matches F16 on 6 dictations in 7; the rest differ in punctuation, never in content.",
      size: "0.59 GB",
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

  // General models that can clean English too, through the same prompt Polish uses.
  const ENGLISH_GENERAL = [
    {
      file: "gemma-4-E2B_q4_0-it.gguf",
      label: "Gemma 4 E2B  QAT Q4_0",
      note: "More accurate than S1-mini on English in testing (4.8% against 6.9% of words wrong) and knows names like Claude, but about four times slower. The same file as the other-languages model, so one download serves both.",
      size: "3.12 GB",
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
    {
      file: "Qwen3.5-4B-Q4_K_M.gguf",
      label: "Qwen3.5 4B  Q4_K_M",
      note: "Lists 201 languages; not yet measured on Polish. In English it cleaned no better than S1-mini and about twice as slowly as Gemma 4 E2B, but settled every \u201ccloud or Claude\u201d in testing.",
      size: "2.74 GB",
    },
  ];

  const present = (file) => available.some((m) => m.file === file && m.present);

  // A failed download is reported on its own row; anything else (a move, a file not
  // listed here) falls through to the block under the lists.
  const ROWS = [...SPEECH, ...CLEANUP, ...MULTILINGUAL].map((o) => o.file);
  const failed = (file) =>
    progress && progress.finished && progress.error && progress.file === file;
  const failureShownInline = $derived(
    !!progress && !!progress.error && ROWS.includes(progress.file) && !present(progress.file),
  );

  onMount(() => {
    load();
    return () => {
      if (poll) clearInterval(poll);
    };
  });

  async function load() {
    try {
      adapters = (await listDevices()).adapters;
      status = await modelStatus();
      await refreshDownloads();
      progress = await downloadProgress();
      // The download lives in the core, not in this window: it may have been started
      // from an earlier visit to this tab, and it carries on while the tab is closed.
      if (progress && !progress.finished) pollProgress(refreshModels);
    } catch (e) {
      error = String(e);
    }
  }

  async function refreshModels() {
    await refreshDownloads();
    status = await modelStatus();
  }

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
      pollProgress(refreshModels);
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

  // Picking a local model clears the slot's cloud choice; picking a cloud one leaves
  // the local file chosen underneath it, as the fallback.
  function choose(field, file) {
    config.models[field] = file;
    config.models[field + "_cloud"] = null;
    onchange();
  }

  function chooseCloud(field, cloud) {
    config.models[field + "_cloud"] = { provider: cloud.provider, model: cloud.model };
    onchange();
  }

  // The models added on the Providers page, as cards for a slot of this kind.
  function cloudOptions(kind) {
    return (config.providers ?? []).flatMap((p) =>
      (p.models ?? [])
        .filter((m) => m.kind === kind && m.name.trim())
        .map((m) => ({
          cloud: { provider: p.id, model: m.name },
          label: m.name,
          note: `${p.name || "Provider"}, in the cloud. Your text leaves this computer.`,
          size: "",
        })),
    );
  }

  const sameCloud = (a, b) => !!a && !!b && a.provider === b.provider && a.model === b.model;
</script>

<h1>Models</h1>
<p class="subtitle">
  Loaded on the first dictation, not at startup, and kept loaded from then on.
  Only the models the language you are speaking needs get loaded.
</p>

{#if error}<div class="status bad">{error}</div>{/if}

<svelte:window
  onmousedown={(e) => openSlot && !e.target.closest(".picker") && (openSlot = null)}
  onkeydown={(e) => e.key === "Escape" && (openSlot = null)}
/>

{#snippet card(field, o, onpick)}
  {@const current = config.models[field + "_cloud"]}
  {@const chosen = o.cloud ? sameCloud(current, o.cloud) : !current && config.models[field] === o.file}
  {@const have = o.cloud ? true : present(o.file)}
  {@const downloading = progress && progress.file === o.file && !progress.finished}
  <div class="choice" class:picked={chosen}>
    <button
      class="choice-pick"
      aria-pressed={chosen}
      onclick={() => { o.cloud ? chooseCloud(field, o.cloud) : choose(field, o.file); onpick?.(); }}
      disabled={!have && !chosen}
    >
      <span class="choice-head">
        <span class="choice-name">{#if o.cloud}{@render cloudIcon()}{/if}{o.label}</span>
        <span class="choice-size mono">{o.size}</span>
      </span>
      <span class="choice-note">{o.note}</span>
    </button>
    {#if !have}
      <p class="choice-missing">
        {downloading ? `Downloading -- ${pct(progress)}%` : "Not downloaded."}
        <button class="inline" disabled={busy} onclick={() => download(o.file)}>
          {downloading ? `${mb(progress.done)} of ${mb(progress.total)} MB` : failed(o.file) ? "Try again" : "Get it"}
        </button>
      </p>
      {#if failed(o.file)}
        <p class="choice-missing" style="color:var(--clay)">{progress.error}</p>
      {/if}
    {/if}
  </div>
{/snippet}

<!-- A slot shows the model in use as a picker: the card opens a menu of the others
     under it. A config that names a file not listed here has nothing to show on its
     own, so it lists them all. -->
{#snippet cloudIcon()}
  <!-- Lucide "cloud", ISC License; see App.svelte. -->
  <svg class="cloud" viewBox="0 0 24 24" aria-label="cloud model">
    <path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9Z" />
  </svg>
{/snippet}

{#snippet slot(title, field, localOptions, subtitle, kind)}
  {@const current = config.models[field + "_cloud"]}
  {@const local = localOptions.find((o) => o.file === config.models[field])}
  {@const clouds = cloudOptions(kind)}
  {@const options = [...localOptions, ...clouds]}
  {@const picked = current
    ? clouds.find((o) => sameCloud(o.cloud, current)) ?? {
        cloud: current,
        label: current.model,
        note: "This model is no longer on the Providers page. Pick another.",
        size: "",
      }
    : local}
  {@const others = options.filter((o) => o !== picked)}
  {@const open = openSlot === field}
  <h2>{title}</h2>
  <p class="hint" style="margin:-6px 0 10px">{subtitle}</p>
  {#if picked}
    {@const have = picked.cloud ? true : present(picked.file)}
    {@const downloading = progress && progress.file === picked.file && !progress.finished}
    <div class="picker">
      <div class="choice picked">
        <button
          class="choice-pick"
          aria-haspopup="listbox"
          aria-expanded={open}
          onclick={() => (openSlot = open ? null : field)}
        >
          <span class="choice-head">
            <span class="choice-name">{#if picked.cloud}{@render cloudIcon()}{/if}{picked.label}</span>
            <span class="choice-size mono">{picked.size}</span>
            <svg class="chevron" class:open viewBox="0 0 24 24" aria-hidden="true">
              <path d="m6 9 6 6 6-6" />
            </svg>
          </span>
          <span class="choice-note">{picked.note}</span>
        </button>
        {#if !have}
          <p class="choice-missing">
            {downloading ? `Downloading -- ${pct(progress)}%` : "Not downloaded."}
            <button class="inline" disabled={busy} onclick={() => download(picked.file)}>
              {downloading ? `${mb(progress.done)} of ${mb(progress.total)} MB` : failed(picked.file) ? "Try again" : "Get it"}
            </button>
          </p>
          {#if failed(picked.file)}
            <p class="choice-missing" style="color:var(--clay)">{progress.error}</p>
          {/if}
        {/if}
      </div>
      {#if open}
        <div class="menu" role="listbox" aria-label="{title} alternatives">
          {#each others as o}
            {@render card(field, o, () => (openSlot = null))}
          {/each}
        </div>
      {/if}
    </div>
  {:else}
    <div class="choices">
      {#each options as o}
        {@render card(field, o)}
      {/each}
    </div>
  {/if}
  {#if current}
    {#if kind === "speech"}
      <label class="check">
        <input
          type="checkbox"
          checked={config.models.speech_cloud_fallback}
          onchange={(e) => { config.models.speech_cloud_fallback = e.currentTarget.checked; onchange(); }}
        />
        <span>
          If the cloud fails, use {local ? local.label : "the local model"} instead
          <span class="hint" style="margin:0">
            Off by default, so a failure is reported instead of quietly giving a
            transcript from a different model. Speech detection always runs here, so
            silence is never sent.
          </span>
        </span>
      </label>
    {:else}
      <p class="hint" style="margin:-2px 0 10px">
        If the cloud fails, {local ? local.label : "the local model"} cleans it instead, and
        you get a notification.
      </p>
    {/if}
  {/if}
{/snippet}

{@render slot(
  "Speech",
  "whisper",
  SPEECH,
  "Turns what you said into text. The one choice that affects every dictation.",
  "speech",
)}

{@render slot(
  "Cleanup, English",
  "cleanup",
  [...CLEANUP, ...ENGLISH_GENERAL],
  "Punctuates and tidies English. S1-mini is built for exactly this and is the fastest; Gemma 4 E2B cleans more accurately and is slower.",
  "cleanup",
)}

{@render slot(
  "Cleanup, other languages and rewrites",
  "cleanup_multilingual",
  MULTILINGUAL,
  "A general model doing the same job for languages S1-mini does not cover, and the only one that can rewrite (Presets > Rewrite). Optional; without it non-English speech is recognised but pasted uncleaned.",
  "cleanup",
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
{:else if progress && progress.error && !failureShownInline}
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
  <table class="wide" style="margin-top:10px">
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
      Automatic{#if adapters.some((a) => a.preferred)}&nbsp;({adapters.find((a) => a.preferred).name}){/if}
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

<style>
  .cloud {
    width: 14px;
    height: 14px;
    margin-right: 6px;
    vertical-align: -2px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 8px;
  }

  .picker {
    position: relative;
    margin-bottom: 8px;
  }

  /* The menu sits over whatever follows, like a native dropdown; a border is the only
     edge it gets (brief section 8, no shadows). */
  .menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .chevron {
    width: 14px;
    height: 14px;
    flex: none;
    align-self: center;
    stroke: currentColor;
    fill: none;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 120ms;
  }

  .chevron.open {
    transform: rotate(180deg);
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
    flex: 1;
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
</style>
