<script>
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
  ];
</script>

<h1 class="wordmark">Lathe</h1>
<p class="subtitle">
  Local push-to-talk dictation. Nothing leaves this machine unless you turn on an
  external endpoint yourself.
</p>

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
  No account, no telemetry, no analytics, no crash reporting, no update checks. Audio is
  never written to disk on the normal path. Transcript history, if enabled, is a local
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
