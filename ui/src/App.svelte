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
  //
  // The rail icons are Lucide's sliders-horizontal, book-a, keyboard, audio-lines, cpu,
  // history and info, inlined below (https://lucide.dev).
  //
  //   ISC License. Copyright (c) 2026 Lucide Icons and Contributors.
  //   Permission to use, copy, modify, and/or distribute this software for any purpose
  //   with or without fee is hereby granted, provided that the above copyright notice
  //   and this permission notice appear in all copies. THE SOFTWARE IS PROVIDED "AS IS"
  //   AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING
  //   ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
  //   AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR
  //   ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
  //   ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF OR IN
  //   CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
  //
  //   "info" derives from Feather: The MIT License (MIT), Copyright (c) 2013-present
  //   Cole Bemis. Permission is hereby granted, free of charge, to any person obtaining
  //   a copy of this software and associated documentation files (the "Software"), to
  //   deal in the Software without restriction, including without limitation the rights
  //   to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
  //   of the Software, and to permit persons to whom the Software is furnished to do
  //   so, subject to the following conditions: The above copyright notice and this
  //   permission notice shall be included in all copies or substantial portions of the
  //   Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
  //   OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
  //   FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
  //   AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
  //   WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
  //   CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
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
        <svg viewBox="0 0 24 24" aria-hidden="true">
          {#if section.id === "presets"}
            <path d="M10 5H3M12 19H3M14 3v4M16 17v4M21 12h-9M21 19h-5M21 5h-7M8 10v4M8 12H3" />
          {:else if section.id === "vocabulary"}
            <path d="M4 19.5v-15A2.5 2.5 0 0 1 6.5 2H19a1 1 0 0 1 1 1v18a1 1 0 0 1-1 1H6.5a1 1 0 0 1 0-5H20" />
            <path d="m8 13 4-7 4 7M9.1 11h5.7" />
          {:else if section.id === "hotkeys"}
            <rect width="20" height="16" x="2" y="4" rx="2" />
            <path d="M6 8h.01M10 8h.01M14 8h.01M18 8h.01M8 12h.01M12 12h.01M16 12h.01M7 16h10" />
          {:else if section.id === "audio"}
            <path d="M2 10v3M6 6v11M10 3v18M14 8v7M18 5v13M22 10v3" />
          {:else if section.id === "models"}
            <rect x="4" y="4" width="16" height="16" rx="2" />
            <rect x="8" y="8" width="8" height="8" rx="1" />
            <path d="M12 20v2M12 2v2M17 20v2M17 2v2M2 12h2M2 17h2M2 7h2M20 12h2M20 17h2M20 7h2M7 20v2M7 2v2" />
          {:else if section.id === "history"}
            <path d="M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8M3 3v5h5M12 7v5l4 2" />
          {:else}
            <circle cx="12" cy="12" r="10" />
            <path d="M12 16v-4M12 8h.01" />
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
