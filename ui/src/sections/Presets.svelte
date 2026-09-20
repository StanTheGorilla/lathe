<script>
  let { config = $bindable(), onchange } = $props();

  let index = $state(0);
  const preset = $derived(config.presets[index]);
  let showAdvanced = $state(false);
  let confirmingDelete = $state(false);

  // Brief 5.4: four stops, because the model has four trained values, not a 0-100 bucketed
  // internally. The examples below are real S1-mini output for one fixed input, not
  // illustrative copy.
  const STOPS = ["casual", "semi-casual", "semi-formal", "formal"];
  const SOURCE =
    "so um i was thinking right we could maybe do the migration on friday and then like kick off the backfill over the weekend if the tests pass yeah";
  const EXAMPLES = {
    casual:
      "so um i was thinking right we could maybe do the migration on friday and then like kick off the backfill over the weekend if the tests pass yeah",
    "semi-casual":
      "so um I was thinking right, we could maybe do the migration on Friday and then kick off the backfill over the weekend if the tests pass yeah",
    "semi-formal":
      "So I was thinking, right, we could maybe do the migration on Friday and then kick off the backfill over the weekend if the tests pass. Yeah.",
    formal:
      "So I was thinking, right, we could maybe do the migration on Friday and then kick off the backfill over the weekend if the tests pass. Yeah.",
  };

  // The languages the speech model covers. English is cleaned by S1-mini; everything
  // else uses the multilingual model. Amendment A29: language is chosen here and in the
  // tray, not per preset.
  const LANGUAGES = [
    { code: "en", name: "English" },
    { code: "pl", name: "Polski (Polish)" },
    { code: "de", name: "Deutsch (German)" },
    { code: "fr", name: "Francais (French)" },
    { code: "es", name: "Espanol (Spanish)" },
    { code: "it", name: "Italiano (Italian)" },
    { code: "pt", name: "Portugues (Portuguese)" },
    { code: "nl", name: "Nederlands (Dutch)" },
    { code: "cs", name: "Cestina (Czech)" },
    { code: "uk", name: "Ukrainska (Ukrainian)" },
    { code: "ru", name: "Russkij (Russian)" },
    { code: "sv", name: "Svenska (Swedish)" },
    { code: "tr", name: "Turkce (Turkish)" },
    { code: "ja", name: "Nihongo (Japanese)" },
    { code: "ko", name: "Hangugeo (Korean)" },
    { code: "zh", name: "Zhongwen (Chinese)" },
    { code: "ar", name: "Arabic" },
    { code: "hi", name: "Hindi" },
  ];

  function set(field, value) {
    config.presets[index][field] = value;
    onchange();
  }

  // Brief 5.3: presets are editable and clonable. A new one starts from the shipped
  // Prompt defaults; a clone copies everything but the hotkey, which can only be held
  // by one preset at a time.
  function unusedName(base) {
    let name = base;
    for (let n = 2; config.presets.some((p) => p.name === name); n++) name = `${base} ${n}`;
    return name;
  }

  function addPreset(from) {
    const fresh = from
      ? { ...$state.snapshot(from), name: unusedName(`${from.name} copy`), hotkey: null }
      : {
          name: unusedName("New preset"),
          styling: "semi-formal",
          structure: "prose",
          context: "general",
          cleanup: true,
          auto_paste: true,
          vocabulary_sets: [],
          replacements: [],
          hotkey: null,
          rewrite: "off",
        };
    config.presets = [...config.presets, fresh];
    index = config.presets.length - 1;
    onchange();
  }

  function rename(name) {
    const was = preset.name;
    config.presets[index].name = name;
    // The tray selection follows the preset it named, not the name.
    if (config.active_preset === was) config.active_preset = name;
    onchange();
  }

  function removePreset() {
    confirmingDelete = false;
    const was = preset.name;
    config.presets = config.presets.filter((_, i) => i !== index);
    index = Math.max(0, index - 1);
    if (config.active_preset === was) config.active_preset = config.presets[index].name;
    onchange();
  }

  const nameClash = $derived(
    !preset.name.trim()
      ? "A preset needs a name."
      : config.presets.some((p, i) => i !== index && p.name === preset.name)
        ? "Another preset already has this name; the tray and hotkeys go by name."
        : "",
  );

  function setLanguage(field, value) {
    config.languages[field] = value;
    // The active one must be one of the two; follow the field that was edited if it was
    // the active one, otherwise leave the switch where it is.
    if (!["main", "secondary"].map((f) => config.languages[f]).includes(config.languages.active)) {
      config.languages.active = config.languages.main;
    }
    onchange();
  }

  // Rules round-trip through one line of text each. A real newline in a replacement is
  // shown as a literal \n, since the editor is line-based and a rule cannot span lines.
  const rulesText = (rules) =>
    rules
      .map((r) => `${r.regex ? "re:" : ""}${r.find} => ${r.replace.replaceAll("\n", "\\n")}`)
      .join("\n");

  function setRules(text) {
    set(
      "replacements",
      text
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => {
          const i = line.indexOf("=>");
          if (i === -1) return null;
          let find = line.slice(0, i).trim();
          const regex = find.startsWith("re:");
          if (regex) find = find.slice(3).trim();
          return {
            find,
            replace: line.slice(i + 2).trim().replaceAll("\\n", "\n"),
            regex,
          };
        })
        .filter(Boolean),
    );
  }
</script>

<h1>Presets</h1>
<p class="subtitle">
  A preset says how the text should come out: the three cleanup settings and whether
  cleanup runs at all. The language you are speaking is separate. Both are switched
  from the tray menu.
</p>

<h2>Language</h2>
<div class="row" style="align-items:flex-start">
  <div class="field" style="flex:1">
    <label for="lang-main">Main language</label>
    <select
      id="lang-main"
      value={config.languages.main}
      onchange={(e) => setLanguage("main", e.currentTarget.value)}
    >
      {#each LANGUAGES as l}
        <option value={l.code}>{l.name}</option>
      {/each}
    </select>
  </div>
  <div class="field" style="flex:1">
    <label for="lang-secondary">Second language</label>
    <select
      id="lang-secondary"
      value={config.languages.secondary}
      onchange={(e) => setLanguage("secondary", e.currentTarget.value)}
    >
      <option value="">None</option>
      {#each LANGUAGES as l}
        <option value={l.code}>{l.name}</option>
      {/each}
    </select>
  </div>
</div>
<p class="hint" style="margin:-8px 0 0">
  English is cleaned by S1-mini; anything else needs the multilingual model, under Models.
</p>

<h2>Preset</h2>

<div class="preset-tabs">
  {#each config.presets as p, i}
    <button aria-pressed={index === i} onclick={() => { index = i; confirmingDelete = false; }}>
      {p.name}
    </button>
  {/each}
  <button onclick={() => addPreset(null)}>+ New preset</button>
</div>

<div class="field">
  <label for="preset-name">Name</label>
  <div class="row">
    <input
      class="mono set-name"
      id="preset-name"
      type="text"
      value={preset.name}
      oninput={(e) => rename(e.currentTarget.value)}
    />
    <button class="quiet icon" data-tip="Clone this preset" aria-label="Clone this preset" onclick={() => addPreset(preset)}>
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <rect width="14" height="14" x="8" y="8" rx="2" />
        <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2" />
      </svg>
    </button>
    <button
      class="quiet icon"
      data-tip="Delete this preset"
      aria-label="Delete this preset"
      onclick={() => (confirmingDelete = true)}
      disabled={config.presets.length <= 1}
    >
      <svg viewBox="0 0 24 24" aria-hidden="true">
        <path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v6M14 11v6" />
      </svg>
    </button>
  </div>
</div>

{#if confirmingDelete}
  <div class="status bad">
    Delete the &ldquo;{preset.name}&rdquo; preset? Its hotkey and replacement rules go
    with it. Revert at the bottom undoes it until you save.
    <div class="row" style="margin-top:8px">
      <button class="primary" onclick={removePreset}>Delete</button>
      <button onclick={() => (confirmingDelete = false)}>Cancel</button>
    </div>
  </div>
{/if}

{#if nameClash}
  <div class="status bad">{nameClash}</div>
{/if}

<div class="row" style="gap:24px">
  <label class="check">
    <input
      type="checkbox"
      checked={preset.cleanup}
      onchange={(e) => set("cleanup", e.currentTarget.checked)}
    />
    <span>Run the cleanup model</span>
  </label>
  <label class="check">
    <input
      type="checkbox"
      checked={preset.auto_paste}
      onchange={(e) => set("auto_paste", e.currentTarget.checked)}
    />
    <span>Paste automatically</span>
  </label>
</div>

{#if !preset.cleanup}
  <div class="status info" style="margin-top:8px">
    This preset bypasses the cleanup model entirely. You get raw speech recognition.
  </div>
{:else}
  <div class="field" style="margin-top:8px">
    <label for="tone">Tone</label>
    <div class="stops" id="tone">
      {#each STOPS as stop}
        <button aria-pressed={preset.styling === stop} onclick={() => set("styling", stop)}>
          {stop}
        </button>
      {/each}
    </div>
    <p class="example">{EXAMPLES[preset.styling]}</p>
    <p class="hint">
      Spoken: &ldquo;{SOURCE}&rdquo;. The two formal stops often produce identical output on
      short input.
    </p>
  </div>

  <div class="field">
    <label for="structure">Structure</label>
    <select
      id="structure"
      value={preset.structure}
      disabled={preset.rewrite !== "off"}
      onchange={(e) => set("structure", e.currentTarget.value)}
    >
      <option value="prose">prose</option>
      <option value="lists">lists</option>
    </select>
    <p class="hint">
      {#if preset.rewrite !== "off"}
        Not used while Rewrite is on.
      {:else}
        Lists only produces bullets when what you said actually contains items.
      {/if}
    </p>
  </div>

  <div class="field">
    <label for="context">Context</label>
    <select
      id="context"
      value={preset.context}
      disabled={preset.rewrite !== "off"}
      onchange={(e) => set("context", e.currentTarget.value)}
    >
      <option value="general">general</option>
      <option value="email">email</option>
    </select>
    <p class="hint">
      {#if preset.rewrite !== "off"}
        Not used while Rewrite is on.
      {:else}
        Email adds a greeting, paragraph breaks and a sign-off.
      {/if}
    </p>
  </div>
{/if}

<button
  class="disclose"
  style="margin-top:26px"
  aria-expanded={showAdvanced}
  onclick={() => (showAdvanced = !showAdvanced)}
>
  {showAdvanced ? "Hide" : "Show"} advanced
  <span class="hint">
    {#if preset.cleanup}Rewrite {preset.rewrite ?? "off"},{/if}
    {preset.vocabulary_sets.length === 0
      ? "every vocabulary set"
      : `${preset.vocabulary_sets.length} vocabulary set${preset.vocabulary_sets.length === 1 ? "" : "s"}`},
    {preset.replacements.length} replacement rule{preset.replacements.length === 1 ? "" : "s"}
  </span>
</button>

{#if showAdvanced}
  {#if preset.cleanup}
    <div class="field" style="margin-top:14px">
      <label for="rewrite">Rewrite</label>
      <select
        id="rewrite"
        value={preset.rewrite ?? "off"}
        onchange={(e) => set("rewrite", e.currentTarget.value)}
      >
        <option value="off">off -- keep every word</option>
        <option value="prompt">prompt for an AI assistant</option>
        <option value="notes">structured notes</option>
        <option value="concise">the same thing in fewer words</option>
      </select>
      <p class="hint">
        Off, the cleanup model may only fix punctuation and drop fillers. A rewrite is free
        to change the words while keeping every point you made, and goes through the
        multilingual model in every language, so S1-mini and the Structure and Context
        settings above do not apply. Needs the multilingual model from Models; without it
        the text is cleaned the ordinary way.
      </p>
    </div>
  {/if}

  <div class="field" style="margin-top:14px">
    <span class="pseudo-label">Vocabulary sets</span>
    {#if preset.vocabulary_sets.length === 0}
      <p class="hint" style="margin:0">
        Every enabled set. <button
          style="padding:2px 8px"
          onclick={() => set("vocabulary_sets", config.vocabulary.sets.map((s) => s.name))}
          >Choose sets</button
        >
      </p>
    {:else}
      {#each config.vocabulary.sets as s}
        <label class="check">
          <input
            type="checkbox"
            checked={preset.vocabulary_sets.includes(s.name)}
            onchange={(e) => {
              const next = e.currentTarget.checked
                ? [...preset.vocabulary_sets, s.name]
                : preset.vocabulary_sets.filter((n) => n !== s.name);
              set("vocabulary_sets", next);
            }}
          />
          <span>{s.name}</span>
        </label>
      {/each}
      <p class="hint">
        <button style="padding:2px 8px" onclick={() => set("vocabulary_sets", [])}>
          Use every enabled set
        </button>
      </p>
    {/if}
  </div>

  <div class="field">
    <label for="rules">Replacement rules ({preset.replacements.length})</label>
    <textarea id="rules" style="min-height:90px" onchange={(e) => setRules(e.currentTarget.value)}
      >{rulesText(preset.replacements)}</textarea
    >
    <p class="hint">
      Applied last, after cleanup. Deterministic, no model involved. One per line as
      <span class="mono">find =&gt; replace</span>; prefix with <span class="mono">re:</span>
      to treat the left side as a regular expression. Use <span class="mono">\n</span> for a
      line break.
    </p>
  </div>
{/if}

<style>
  .set-name {
    width: 180px;
  }
</style>
