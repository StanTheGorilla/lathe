<script>
  import { onMount } from "svelte";
  import { modelStatus } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let index = $state(0);
  const set = $derived(config.vocabulary.sets[index]);

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

  // One term per line. A term that is reliably misheard names the words it is heard as,
  // after an arrow: `Claude <- cloud, klaud`. Everything else stays a bare word.
  const toText = (terms) =>
    (terms ?? [])
      .map((t) => (t.heard?.length ? `${t.write} <- ${t.heard.join(", ")}` : t.write))
      .join("\n");

  const fromText = (text) =>
    text
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const i = line.indexOf("<-");
        if (i === -1) return { write: line, heard: [] };
        return {
          write: line.slice(0, i).trim(),
          heard: line
            .slice(i + 2)
            .split(",")
            .map((h) => h.trim())
            .filter(Boolean),
        };
      })
      .filter((t) => t.write);

  // The textarea is always editable and bound to a draft, resynced whenever the selected
  // set changes. An earlier version rendered a read-only textarea from the terms
  // directly and showed nothing at all: `value` on a textarea, and a text child, are
  // both unreliable ways to populate one.
  let draft = $state("");
  let loadedIndex = $state(-1);

  $effect(() => {
    if (loadedIndex !== index && set) {
      draft = toText(set.terms);
      loadedIndex = index;
    }
  });

  function commit() {
    config.vocabulary.sets[index].terms = fromText(draft);
    onchange();
  }

  function addSet() {
    const name = `set ${config.vocabulary.sets.length + 1}`;
    config.vocabulary.sets = [
      ...config.vocabulary.sets,
      { name, enabled: true, terms: [] },
    ];
    index = config.vocabulary.sets.length - 1;
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
  Words you want spelled correctly. Add the word; that is all. If one is reliably heard
  as a different word, you can say so &mdash; see below.
</p>

{#if biases === false}
  <div class="status bad">
    The <span class="mono">{backend}</span> speech model does not support recogniser
    biasing, so words added here are <strong>not</strong> pushed into the recogniser
    before it decides. They are applied afterwards, by the repair pass, which is strict
    and cannot reach every word on its own.
  </div>
{:else if biases === true}
  <div class="status info">
    Words here are pushed into the <span class="mono">{backend}</span> recogniser before
    it decides anything, which is where almost all of the benefit is.
  </div>
{/if}

<div class="status info">
  A bare word can never replace something you actually said. Matching needs an exact
  phonetic match, so adding &ldquo;Cohere&rdquo; will not touch &ldquo;coherent&rdquo; --
  they sound different. A word after an arrow is the deliberate exception:
  <span class="mono">Claude &lt;- cloud</span> means you cannot dictate the word
  &ldquo;cloud&rdquo; while this set is on. For an unconditional substitution that
  ignores the vocabulary entirely, use Replacements on a preset.
</div>

{#if config.vocabulary.sets.length}
  <div class="preset-tabs">
    {#each config.vocabulary.sets as s, i}
      <button aria-pressed={index === i} onclick={() => (index = i)}>
        {s.name}{s.enabled ? "" : " (off)"}
      </button>
    {/each}
    <button onclick={addSet}>+ New set</button>
  </div>
{/if}

{#if set}
  <div class="field">
    <label for="setname">Set name</label>
    <input
      id="setname"
      class="mono"
      type="text"
      value={set.name}
      oninput={(e) => { config.vocabulary.sets[index].name = e.currentTarget.value; onchange(); }}
    />
    <p class="hint">
      Presets choose sets by name, so technical terms and personal names need not
      contaminate each other.
    </p>
  </div>

  <label class="check">
    <input
      type="checkbox"
      checked={set.enabled}
      onchange={(e) => { config.vocabulary.sets[index].enabled = e.currentTarget.checked; onchange(); }}
    />
    <span>Use this set</span>
  </label>

  <div class="field" style="max-width:100%">
    <label for="entries">Words ({set.terms.length})</label>
    <textarea id="entries" bind:value={draft} onchange={commit} onblur={commit}></textarea>
    <div class="row" style="margin-top:8px">
      <button onclick={removeSet} disabled={config.vocabulary.sets.length <= 1}>
        Delete this set
      </button>
    </div>
    <p class="hint">
      One per line. Names, jargon, product names, anything the recogniser has not met.
      Phrases are fine and help recognition, though only single words are repaired
      afterwards.
    </p>
    <p class="hint">
      For a word that keeps coming out wrong, write
      <span class="mono">Claude &lt;- cloud, klaud</span> &mdash; the word on the left is
      written whenever you say one of the words on the right. Use it only where the
      recogniser is reliably wrong: each one costs you that word.
    </p>
  </div>
{/if}

<h2>Matching</h2>

<label class="check">
  <input
    type="checkbox"
    checked={config.vocabulary.correction_enabled}
    onchange={(e) => { config.vocabulary.correction_enabled = e.currentTarget.checked; onchange(); }}
  />
  <span>
    Correct the transcript after recognition
    <span class="hint" style="margin:0">
      When off, terms still bias recognition but nothing is rewritten afterwards.
    </span>
  </span>
</label>

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
    value={config.vocabulary.hotword_boost}
    oninput={(e) => { config.vocabulary.hotword_boost = +e.currentTarget.value; onchange(); }}
  />
  <p class="hint">
    How hard to push the recogniser toward these terms. Too high and it hears them where
    they were not said. {count} term{count === 1 ? "" : "s"} are active; the first 128 are
    sent.
  </p>
</div>
