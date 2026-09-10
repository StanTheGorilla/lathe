<script>
  let { config = $bindable(), onchange } = $props();

  let index = $state(0);
  const preset = $derived(config.presets[index]);

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
  // else uses the multilingual model.
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
  A preset bundles the three cleanup settings, the language, and whether cleanup runs at
  all. Switch between presets from the tray menu.
</p>

<div class="preset-tabs">
  {#each config.presets as p, i}
    <button aria-pressed={index === i} onclick={() => (index = i)}>{p.name}</button>
  {/each}
</div>

{#if preset.name === "Prompt"}
  <div class="status info">
    Cleanup punctuates and removes fillers. It does not restructure rambling into a
    well-formed prompt -- that needs a second, general-purpose model.
  </div>
{/if}

{#if !preset.cleanup}
  <div class="status info">
    This preset bypasses the cleanup model entirely. You get raw speech recognition.
  </div>
{/if}

<div class="field">
  <label for="tone">Tone</label>
  <div class="stops" id="tone">
    {#each STOPS as stop}
      <button
        aria-pressed={preset.styling === stop}
        disabled={!preset.cleanup}
        onclick={() => set("styling", stop)}
      >
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
    disabled={!preset.cleanup}
    onchange={(e) => set("structure", e.currentTarget.value)}
  >
    <option value="prose">prose</option>
    <option value="lists">lists</option>
  </select>
  <p class="hint">
    Lists only produces bullets when what you said actually contains items.
  </p>
</div>

<div class="field">
  <label for="context">Context</label>
  <select
    id="context"
    value={preset.context}
    disabled={!preset.cleanup}
    onchange={(e) => set("context", e.currentTarget.value)}
  >
    <option value="general">general</option>
    <option value="email">email</option>
  </select>
  <p class="hint">Email adds a greeting, paragraph breaks and a sign-off.</p>
</div>

<h2>Behaviour</h2>

<label class="check">
  <input
    type="checkbox"
    checked={preset.cleanup}
    onchange={(e) => set("cleanup", e.currentTarget.checked)}
  />
  <span>
    Run the cleanup model
    <span class="hint" style="margin:0">
      English uses S1-mini; other languages use the multilingual model.
    </span>
  </span>
</label>

<label class="check">
  <input
    type="checkbox"
    checked={preset.auto_paste}
    onchange={(e) => set("auto_paste", e.currentTarget.checked)}
  />
  <span>
    Paste automatically
    <span class="hint" style="margin:0">
      When off, the text only goes to the clipboard.
    </span>
  </span>
</label>

<div class="field">
  <label for="lang">Speech language</label>
  <select
    id="lang"
    value={preset.lang}
    onchange={(e) => set("lang", e.currentTarget.value)}
  >
    {#each LANGUAGES as l}
      <option value={l.code}>{l.name}</option>
    {/each}
  </select>
  <p class="hint">
    {#if preset.lang === "en"}
      English is cleaned by S1-mini, a model built for exactly this job.
    {:else}
      Cleaned by the multilingual model, which is a separate download under Models. Without
      it, {LANGUAGES.find((l) => l.code === preset.lang)?.name ?? "this language"} is still
      recognised but pasted uncleaned.
    {/if}
  </p>
</div>

<h2>Vocabulary</h2>

<div class="field">
  <span class="pseudo-label">Sets this preset uses</span>
  {#each config.vocabulary.sets as s}
    <label class="check">
      <input
        type="checkbox"
        checked={preset.vocabulary_sets.length === 0 || preset.vocabulary_sets.includes(s.name)}
        disabled={preset.vocabulary_sets.length === 0}
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
  {#if preset.vocabulary_sets.length === 0}
    <p class="hint">
      Every enabled set is used. <button
        style="padding:2px 8px"
        onclick={() => set("vocabulary_sets", config.vocabulary.sets.map((s) => s.name))}
        >Choose specific sets</button
      >
    </p>
  {:else}
    <p class="hint">
      <button style="padding:2px 8px" onclick={() => set("vocabulary_sets", [])}>
        Use every enabled set
      </button>
    </p>
  {/if}
</div>

<h2>Replacements</h2>
<p class="hint" style="margin-bottom:10px">
  Applied last, after cleanup. Deterministic, no model involved. One per line as
  <span class="mono">find =&gt; replace</span>; prefix with <span class="mono">re:</span>
  to treat the left side as a regular expression. Use <span class="mono">\n</span> for a
  line break.
</p>

<div class="field" style="max-width:100%">
  <label for="rules">Rules ({preset.replacements.length})</label>
  <textarea id="rules" style="min-height:90px" onchange={(e) => setRules(e.currentTarget.value)}
    >{rulesText(preset.replacements)}</textarea
  >
</div>
