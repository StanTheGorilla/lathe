<script>
  import { onMount } from "svelte";
  import { defaultVocabularySets, modelStatus } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let index = $state(0);
  const set = $derived(config.vocabulary.sets[index]);

  // The core writes a term with no spoken forms as a bare string, so the list arrives
  // in two shapes. Make them one before anything reads `.write`; the file reads back
  // either, so nothing is marked dirty for it.
  $effect.pre(() => {
    for (const s of config.vocabulary.sets) {
      if (s.terms.some((t) => typeof t === "string")) {
        s.terms = s.terms.map((t) => (typeof t === "string" ? { write: t, heard: [] } : t));
      }
    }
  });

  // A set is read-only until Edit is pressed, so a stray keystroke cannot change a
  // list that is mostly looked at. Switching sets locks again.
  let editing = $state(false);
  let showAdvanced = $state(false);
  let lockedIndex = $state(-1);
  $effect(() => {
    if (lockedIndex !== index) {
      editing = false;
      lockedIndex = index;
    }
  });

  // The sets a fresh install starts with, from the core. A set with the same name can
  // be put back to that list.
  let shipped = $state([]);
  onMount(async () => {
    try {
      shipped = await defaultVocabularySets();
    } catch {
      shipped = [];
    }
  });
  const shippedFor = $derived(
    set && shipped.find((s) => s.name.toLowerCase() === set.name.toLowerCase()),
  );

  // Whether pass 1 does anything on the speech model in use. Most backends accept the
  // biasing call and drop it silently, so the answer has to come from the core.
  let biases = $state(null);
  let backend = $state("");
  onMount(async () => {
    try {
      const status = await modelStatus();
      biases = status.speech_biases;
      backend = status.speech_backend;
    } catch {
      biases = null;
    }
  });

  const heardText = (term) => (term.heard ?? []).join(", ");
  const parseHeard = (text) =>
    text
      .split(",")
      .map((h) => h.trim())
      .filter(Boolean);

  // Cells commit on change, not on every keystroke: "Lata," would otherwise be parsed
  // and written back as "Lata" while the comma was still being typed.
  function setWord(i, value) {
    const write = value.trim();
    if (!write) return removeTerm(i);
    config.vocabulary.sets[index].terms[i].write = write;
    onchange();
  }

  function setHeard(i, value) {
    config.vocabulary.sets[index].terms[i].heard = parseHeard(value);
    onchange();
  }

  function removeTerm(i) {
    config.vocabulary.sets[index].terms = set.terms.filter((_, j) => j !== i);
    onchange();
  }

  let newWord = $state("");
  let newHeard = $state("");
  let newWordInput = $state(null);

  function addTerm() {
    const write = newWord.trim();
    if (!write) return;
    config.vocabulary.sets[index].terms = [
      ...set.terms,
      { write, heard: parseHeard(newHeard) },
    ];
    newWord = "";
    newHeard = "";
    newWordInput?.focus();
    onchange();
  }

  function addSet() {
    const name = `set ${config.vocabulary.sets.length + 1}`;
    config.vocabulary.sets = [
      ...config.vocabulary.sets,
      { name, enabled: true, terms: [] },
    ];
    index = config.vocabulary.sets.length - 1;
    // An empty set is there to be typed into.
    lockedIndex = index;
    editing = true;
    onchange();
  }

  function restoreShipped() {
    config.vocabulary.sets[index].terms = $state.snapshot(shippedFor.terms);
    onchange();
  }

  function removeSet() {
    config.vocabulary.sets = config.vocabulary.sets.filter((_, i) => i !== index);
    index = Math.max(0, index - 1);
    onchange();
  }

  const count = $derived(
    config.vocabulary.sets.reduce((n, s) => n + (s.enabled ? s.terms.length : 0), 0),
  );
</script>

<h1>Vocabulary</h1>
<p class="subtitle">
  Words the recogniser should spell your way: names, products, jargon. Add the word. If
  one keeps coming out as something else, add what it is heard as and it is put right.
</p>

{#if biases === false}
  <div class="status info">
    The <span class="mono">{backend}</span> speech model cannot be given these words up
    front. They are corrected after recognition instead, which catches near misses and
    anything listed under &ldquo;Also heard as&rdquo;.
  </div>
{/if}

<h2>Sets</h2>

<div class="preset-tabs">
  {#each config.vocabulary.sets as s, i}
    <button aria-pressed={index === i} onclick={() => (index = i)}>
      {s.name} <span class="count">{s.terms.length}{#if !s.enabled} &middot; off{/if}</span>
    </button>
  {/each}
  <button onclick={addSet}>+ New set</button>
</div>

{#if set}
  <div class="set-bar">
    <input
      class="mono set-name"
      type="text"
      aria-label="Set name"
      value={set.name}
      disabled={!editing}
      oninput={(e) => { config.vocabulary.sets[index].name = e.currentTarget.value; onchange(); }}
    />
    <label class="check" style="margin:0">
      <input
        type="checkbox"
        checked={set.enabled}
        onchange={(e) => { config.vocabulary.sets[index].enabled = e.currentTarget.checked; onchange(); }}
      />
      <span>Use this set</span>
    </label>
    <button aria-pressed={editing} onclick={() => (editing = !editing)}>
      {editing ? "Done" : "Edit"}
    </button>
    {#if editing && shippedFor}
      <button onclick={restoreShipped}>Restore shipped words</button>
    {/if}
    {#if editing}
      <button onclick={removeSet} disabled={config.vocabulary.sets.length <= 1}>
        Delete this set
      </button>
    {/if}
  </div>

  {#if editing && shippedFor}
    <p class="hint" style="margin:-8px 0 12px">
      Restoring puts back the {shippedFor.terms.length} words this version of the app
      ships for &ldquo;{shippedFor.name}&rdquo; and drops anything you added. Revert at
      the bottom undoes it until you save.
    </p>
  {/if}

  <table class="terms wide" class:editing>
    <thead>
      <tr>
        <th>Word</th>
        <th>Also heard as</th>
        {#if editing}<th></th>{/if}
      </tr>
    </thead>
    <tbody>
      {#each set.terms as term, i (i)}
        <tr>
          {#if editing}
            <td>
              <input
                type="text"
                class="mono"
                value={term.write}
                onchange={(e) => setWord(i, e.currentTarget.value)}
              />
            </td>
            <td>
              <input
                type="text"
                class="mono"
                value={heardText(term)}
                placeholder="none"
                onchange={(e) => setHeard(i, e.currentTarget.value)}
              />
            </td>
            <td class="actions">
              <button class="quiet" onclick={() => removeTerm(i)}>Remove</button>
            </td>
          {:else}
            <td class="mono">{term.write}</td>
            <td class="mono">
              {#if term.heard?.length}{heardText(term)}{:else}<span class="dim">&ndash;</span>{/if}
            </td>
          {/if}
        </tr>
      {/each}
      {#if editing}
        <tr class="add">
          <td>
            <input
              type="text"
              class="mono"
              placeholder="New word"
              bind:this={newWordInput}
              bind:value={newWord}
              onkeydown={(e) => { if (e.key === "Enter") addTerm(); }}
            />
          </td>
          <td>
            <input
              type="text"
              class="mono"
              placeholder="heard as, comma separated"
              bind:value={newHeard}
              onkeydown={(e) => { if (e.key === "Enter") addTerm(); }}
            />
          </td>
          <td class="actions">
            <button class="primary" onclick={addTerm} disabled={!newWord.trim()}>Add</button>
          </td>
        </tr>
      {/if}
      {#if !set.terms.length && !editing}
        <tr><td colspan="2" class="dim">No words yet. Press Edit to add some.</td></tr>
      {/if}
    </tbody>
  </table>

  <p class="hint">
    A word on its own is only ever swapped in for something that sounds the same, so
    &ldquo;Cohere&rdquo; never touches &ldquo;coherent&rdquo;, and never outright for an
    everyday English word. When Claude is heard as <span class="mono">cloud</span>, a
    real word, the cleanup model reads the sentence both ways: &ldquo;ask cloud&rdquo;
    becomes Claude, &ldquo;the cloud server&rdquo; stays. Phrases are fine as words; only
    single words are corrected.
  </p>
{/if}

<button
  class="disclose"
  style="margin-top:26px"
  aria-expanded={showAdvanced}
  onclick={() => (showAdvanced = !showAdvanced)}
>
  {showAdvanced ? "Hide" : "Show"} advanced
  <span class="hint">
    Correction {config.vocabulary.correction_enabled ? "on" : "off"}, threshold
    {config.vocabulary.max_distance_ratio}
  </span>
</button>

{#if showAdvanced}
<label class="check" style="margin-top:14px">
  <input
    type="checkbox"
    checked={config.vocabulary.correction_enabled}
    onchange={(e) => { config.vocabulary.correction_enabled = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Correct the transcript after recognition
    <span class="hint" style="margin:0">
      When off, nothing is rewritten afterwards; the words still bias recognition on
      models that support it.
    </span>
  </span>
</label>

<label class="check">
  <input
    type="checkbox"
    checked={config.vocabulary.context}
    onchange={(e) => { config.vocabulary.context = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Let the sentence decide words that are also everyday English
    <span class="hint" style="margin:0">
      When off, a word listed under &ldquo;Also heard as&rdquo; is always replaced, and a
      word that only sounds like a term is always kept.
    </span>
  </span>
</label>

<label class="check">
  <input
    type="checkbox"
    disabled={!config.vocabulary.context}
    checked={config.vocabulary.context_with_instruction_model}
    onchange={(e) => { config.vocabulary.context_with_instruction_model = e.currentTarget.checked; onchange(); }}
  />
  <span>
    In English, decide with the instruction model
    <span class="hint" style="margin:0">
      Better with names: on the same test sentences Gemma 4 E2B settled every
      &ldquo;cloud or Claude&rdquo; and S1-mini two in three. Keeps a second model in
      memory, about 1 GB more graphics memory with Gemma 4 E2B.
    </span>
  </span>
</label>

<div class="field">
  <label for="margin">Context margin</label>
  <input
    id="margin"
    type="number"
    step="0.5"
    min="0"
    max="10"
    disabled={!config.vocabulary.context}
    value={config.vocabulary.context_margin}
    oninput={(e) => { config.vocabulary.context_margin = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">
    How much better an everyday word must read as a term before it is swapped, for words
    you did not list yourself. Higher swaps less. Listed words need only read better.
  </p>
</div>

<div class="field">
  <label for="ratio">Similarity threshold</label>
  <input
    id="ratio"
    type="number"
    step="0.02"
    min="0"
    max="0.6"
    value={config.vocabulary.max_distance_ratio}
    oninput={(e) => { config.vocabulary.max_distance_ratio = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">
    How different a word may be and still be corrected, as a fraction of its length.
    0.34 allows roughly one wrong letter in three. Higher corrects more and misfires more.
  </p>
</div>

<div class="field">
  <label for="boost">
    Recognition bias{biases === false ? " (unused by this model)" : ""}
  </label>
  <input
    id="boost"
    type="number"
    step="0.5"
    min="0"
    max="10"
    disabled={biases === false}
    value={config.vocabulary.hotword_boost}
    oninput={(e) => { config.vocabulary.hotword_boost = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">
    How hard to push the recogniser toward these words, where the model allows it. Too
    high and it hears them where they were not said. {count} word{count === 1 ? "" : "s"}
    active; the first 128 are sent.
  </p>
</div>
{/if}

<style>
  .count {
    color: var(--text-dim);
    font-size: 12px;
    margin-left: 4px;
  }

  .set-bar {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 14px;
  }

  .terms input {
    font-family: var(--font-mono);
  }

  .set-name {
    width: 180px;
  }

  .terms {
    margin-bottom: 8px;
  }

  /* Read: the word column hugs its content. Edit: two inputs share the row. */
  .terms:not(.editing) th:first-child,
  .terms:not(.editing) td:first-child {
    width: 1%;
    white-space: nowrap;
    padding-right: 40px;
  }

  .terms.editing th:first-child,
  .terms.editing td:first-child {
    width: 40%;
  }

  .terms td {
    vertical-align: middle;
  }

  .terms.editing td {
    padding: 4px 8px 4px 0;
    border-bottom: 0;
  }

  .terms .actions {
    width: 1%;
    white-space: nowrap;
  }

  .terms tr.add td {
    padding-top: 10px;
  }

  .dim {
    color: var(--text-dim);
  }
</style>
