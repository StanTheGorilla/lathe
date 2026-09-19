<script>
  import { onMount } from "svelte";
  import {
    updateStatus,
    checkForUpdates,
    openReleasePage,
    downloadUpdate,
    installUpdate,
  } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  // Amendment A31. The daily check runs in the core whether or not this screen is
  // open; this shows what it last found and can ask again by hand, which works even
  // with the automatic check switched off.
  let update = $state({ current: "", checked: false, available: null, error: null });
  let checking = $state(false);
  let installError = $state("");
  let poll = null;

  onMount(() => {
    (async () => {
      try {
        update = await updateStatus();
      } catch {
        /* the version line stays empty */
      }
    })();
    // The installer download runs in the core; this just watches it. Cheap enough
    // to poll whether or not one is running, and the tray can start one at any time.
    poll = setInterval(async () => {
      try {
        update = await updateStatus();
      } catch {
        /* keep what we had */
      }
    }, 500);
    return () => clearInterval(poll);
  });

  async function startUpdate() {
    installError = "";
    try {
      await downloadUpdate();
    } catch (e) {
      installError = String(e);
    }
  }

  async function restart() {
    installError = "";
    try {
      await installUpdate();
    } catch (e) {
      installError = String(e);
    }
  }

  const mb = (n) => (n / 1048576).toFixed(0);

  async function checkNow() {
    checking = true;
    try {
      update = await checkForUpdates();
    } catch (e) {
      update = { ...update, checked: true, error: String(e) };
    } finally {
      checking = false;
    }
  }

  function setCheck(on) {
    config.updates.check = on;
    onchange();
  }

  // Brief 4.2 requires crediting "S1-mini" by "Superwhisper" with that exact
  // capitalisation, and shipping its LICENSE and NOTICE. The other credits are here
  // because the same courtesy applies to everything else doing the work.
  const MODELS = [
    {
      name: "Cohere Transcribe",
      by: "Cohere Labs",
      role: "Speech recognition",
      licence: "CC-BY-NC 4.0",
    },
    {
      name: "S1-mini",
      by: "Superwhisper",
      role: "Transcript cleanup",
      licence: "Apache 2.0, plus a naming clause",
    },
    {
      name: "Silero VAD",
      by: "Silero Team",
      role: "Speech detection",
      licence: "MIT",
    },
  ];

  const RUNTIMES = [
    { name: "CrispASR", role: "Speech runtime", licence: "MIT" },
    { name: "llama.cpp", role: "Cleanup runtime", licence: "MIT" },
    { name: "ggml", role: "Tensor library and Vulkan backend", licence: "MIT" },
    { name: "Tauri", role: "Settings window", licence: "MIT / Apache 2.0" },
    { name: "Svelte", role: "Settings interface", licence: "MIT" },
    { name: "Lucide", role: "Interface icons", licence: "ISC" },
  ];
</script>

<h1 class="wordmark">Lathe</h1>
<p class="subtitle">
  Local push-to-talk dictation. Nothing leaves this machine unless you turn on an
  external endpoint yourself.
</p>

<h2>Version</h2>
{#if update.available}
  {@const download = update.download}
  <div class="status ok">
    <strong>Lathe {update.available.version} is available.</strong> You have
    {update.current}.
    {#if !update.available.installer}
      The installers are on the release page.
    {:else if download?.path}
      Downloaded and checked. Restarting closes Lathe, installs the new version and
      starts it again; a settings window does not come back by itself.
    {:else if download && !download.error}
      Downloading the installer
      {#if download.total > 0}
        &mdash; {mb(download.done)} of {mb(download.total)} MB
      {/if}
    {:else}
      Update downloads the installer here and restarts Lathe into the new version.
    {/if}
    <p style="margin:8px 0 0">
      {#if !update.available.installer}
        <button class="primary" onclick={() => openReleasePage(update.available.url)}>
          Open the download page
        </button>
      {:else if download?.path}
        <button class="primary" onclick={restart}>Restart to update</button>
      {:else if download && !download.error}
        <progress max={download.total || 1} value={download.done}></progress>
      {:else}
        <button class="primary" onclick={startUpdate}>
          {download?.error ? "Try again" : `Update to ${update.available.version}`}
        </button>
        <button onclick={() => openReleasePage(update.available.url)}>Release page</button>
      {/if}
    </p>
    {#if download?.error || installError}
      <p class="hint" style="margin:6px 0 0">{download?.error || installError}</p>
    {/if}
  </div>
{/if}
<div class="field">
  <span class="pseudo-label">Installed</span>
  <p class="mono" style="margin:0">
    {update.current || "unknown"}
    {#if !update.available && update.checked && !update.error}&mdash; up to date{/if}
  </p>
  {#if update.error}
    <p class="hint">Could not check: {update.error}</p>
  {/if}
  <p class="hint" style="margin-top:8px">
    <button style="padding:2px 8px" onclick={checkNow} disabled={checking}>
      {checking ? "Checking" : "Check now"}
    </button>
  </p>
</div>
<label class="check">
  <input
    type="checkbox"
    checked={config.updates.check}
    onchange={(e) => setCheck(e.currentTarget.checked)}
  />
  <span>
    Check for a new version once a day
    <span class="hint" style="margin:0">
      One anonymous request to GitHub's releases list. Nothing is downloaded until you
      press Update, and nothing about this machine is sent. When there is a newer
      version, a notification says so and the tray menu gets an item for it.
    </span>
  </span>
</label>

<h2>Models</h2>
<table>
  <thead>
    <tr><th>Model</th><th>By</th><th>Role</th><th>Licence</th></tr>
  </thead>
  <tbody>
    {#each MODELS as m}
      <tr>
        <td>{m.name}</td>
        <td>{m.by}</td>
        <td>{m.role}</td>
        <td>{m.licence}</td>
      </tr>
    {/each}
  </tbody>
</table>

<div class="status info" style="margin-top:14px">
  &ldquo;S1-mini&rdquo; by &ldquo;Superwhisper&rdquo;. Its licence requires that the model
  keep this name, in this capitalisation, wherever it is used or redistributed. The
  LICENSE and NOTICE files ship alongside the model in the models directory.
</div>

<h2>Runtimes</h2>
<table>
  <thead>
    <tr><th>Component</th><th>Role</th><th>Licence</th></tr>
  </thead>
  <tbody>
    {#each RUNTIMES as r}
      <tr>
        <td>{r.name}</td>
        <td>{r.role}</td>
        <td>{r.licence}</td>
      </tr>
    {/each}
  </tbody>
</table>

<h2>Privacy</h2>
<p class="subtitle">
  No account, no telemetry, no analytics, no crash reporting. The only thing that
  touches the network unasked is the daily version check above, which can be switched
  off; model downloads happen only when you click them. Audio is never written to disk
  on the normal path. Transcript history, if enabled, is a local
  SQLite file you can wipe from the History section.
</p>

<style>
  /* Brief section 8: one serif accent, used only for the app name here. */
  .wordmark {
    font-family: var(--font-serif);
    font-size: 26px;
    font-weight: 400;
    letter-spacing: 0;
    margin-bottom: 6px;
  }
</style>
