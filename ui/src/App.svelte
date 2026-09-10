<script>
  import { onMount } from "svelte";
  import { loadConfig, saveConfig, configPath } from "./api.js";
  import Presets from "./sections/Presets.svelte";
  import Vocabulary from "./sections/Vocabulary.svelte";
  import Hotkeys from "./sections/Hotkeys.svelte";
  import Audio from "./sections/Audio.svelte";
  import Models from "./sections/Models.svelte";
  import History from "./sections/History.svelte";
  import About from "./sections/About.svelte";

  // Brief section 8 lists five rail sections. History and About are added because the
  // brief itself puts features in settings that none of the five can hold: 6.4 asks for
  // searchable history, 6.7 for statistics, and 4.2 for a credits screen. See A17.
  const SECTIONS = [
    { id: "presets", label: "Presets", component: Presets },
    { id: "vocabulary", label: "Vocabulary", component: Vocabulary },
    { id: "hotkeys", label: "Hotkeys", component: Hotkeys },
    { id: "audio", label: "Audio", component: Audio },
    { id: "models", label: "Models", component: Models },
    { id: "history", label: "History", component: History },
    { id: "about", label: "About", component: About },
  ];

  let active = $state("presets");
  let config = $state(null);
  let path = $state("");
  let dirty = $state(false);
  let saving = $state(false);
  let error = $state("");

  const Current = $derived(SECTIONS.find((s) => s.id === active).component);

  onMount(async () => {
    try {
      config = await loadConfig();
      path = await configPath();
    } catch (e) {
      error = String(e);
    }
  });

  function touched() {
    dirty = true;
  }

  async function save() {
    saving = true;
    error = "";
    try {
      await saveConfig(config);
      dirty = false;
    } catch (e) {
      error = String(e);
    } finally {
      saving = false;
    }
  }

  async function revert() {
    try {
      config = await loadConfig();
      dirty = false;
      error = "";
    } catch (e) {
      error = String(e);
    }
  }
</script>

<div class="shell">
  <nav class="rail">
    {#each SECTIONS as section}
      <button
        class="rail-item"
        aria-current={active === section.id}
        onclick={() => (active = section.id)}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          {#if section.id === "presets"}
            <rect x="2.5" y="2.5" width="11" height="4" rx="1" />
            <rect x="2.5" y="9.5" width="11" height="4" rx="1" />
          {:else if section.id === "vocabulary"}
            <path d="M3 3h10v10H3z" />
            <path d="M5.5 6.5h5M5.5 9.5h3" />
          {:else if section.id === "hotkeys"}
            <rect x="1.5" y="4.5" width="13" height="7" rx="1" />
            <path d="M5 8h6" />
          {:else if section.id === "audio"}
            <path d="M8 2.5v11" />
            <path d="M5 5.5v5M11 5.5v5M2.5 7v2M13.5 7v2" />
          {:else if section.id === "models"}
            <path d="M8 2l5.5 3v6L8 14 2.5 11V5z" />
          {:else if section.id === "history"}
            <circle cx="8" cy="8" r="5.5" />
            <path d="M8 5v3.2l2 1.2" />
          {:else}
            <circle cx="8" cy="8" r="5.5" />
            <path d="M8 7.2v4M8 4.9v.1" />
          {/if}
        </svg>
        {section.label}
      </button>
    {/each}

    <div class="rail-spacer"></div>
    <div class="rail-foot mono" title={path}>
      {path.replace(/^.*[\\/]/, "") || ""}
    </div>
  </nav>

  <main class="pane">
    {#if error}
      <div class="status bad">{error}</div>
    {/if}

    {#if config}
      <Current bind:config onchange={touched} />

      {#if dirty}
        <div class="savebar">
          <button class="primary" onclick={save} disabled={saving}>
            {saving ? "Saving" : "Save changes"}
          </button>
          <button onclick={revert} disabled={saving}>Revert</button>
          <span class="hint" style="margin:0">
            The core reloads the file automatically. Hotkey changes need a restart.
          </span>
        </div>
      {/if}
    {:else if !error}
      <p class="subtitle">Loading configuration.</p>
    {/if}
  </main>
</div>
