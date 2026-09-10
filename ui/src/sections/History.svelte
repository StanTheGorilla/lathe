<script>
  import { onMount } from "svelte";
  import { historyRecent, historyStats, historyWipe, historyPaste } from "../api.js";

  let { config = $bindable(), onchange } = $props();

  let items = $state([]);
  let stats = $state(null);
  let search = $state("");
  let error = $state("");
  let confirmingWipe = $state(false);

  async function refresh() {
    error = "";
    try {
      items = await historyRecent(search, 200);
      stats = await historyStats();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(refresh);

  async function paste(id, raw) {
    error = "";
    try {
      await historyPaste(id, raw);
    } catch (e) {
      error = String(e);
    }
  }

  async function wipe() {
    try {
      await historyWipe();
      confirmingWipe = false;
      await refresh();
    } catch (e) {
      error = String(e);
    }
  }

  const when = (unix) => {
    const d = new Date(unix * 1000);
    const today = new Date();
    const sameDay = d.toDateString() === today.toDateString();
    return sameDay
      ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      : d.toLocaleDateString([], { month: "short", day: "numeric" }) +
          " " +
          d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const duration = (secs) => {
    if (secs < 60) return `${Math.round(secs)}s`;
    const m = Math.floor(secs / 60);
    return `${m}m ${Math.round(secs % 60)}s`;
  };
</script>

<h1>History</h1>
<p class="subtitle">
  The last {config.history.limit} dictations, kept on this machine and nowhere else.
</p>

{#if error}<div class="status bad">{error}</div>{/if}

{#if stats}
  <h2>Totals</h2>
  <table>
    <tbody>
      <tr><td>Dictations</td><td class="mono">{stats.dictations}</td></tr>
      <tr><td>Words</td><td class="mono">{stats.words.toLocaleString()}</td></tr>
      <tr>
        <td>Spoken</td>
        <td class="mono">{duration(stats.audio_secs)}</td>
      </tr>
      <tr>
        <td>Saved against typing at 40 WPM</td>
        <td class="mono">{duration(stats.seconds_saved)}</td>
      </tr>
      <tr>
        <td>Average latency</td>
        <td class="mono">{Math.round(stats.avg_latency_ms)} ms</td>
      </tr>
      <tr>
        <td>Changed by cleanup</td>
        <td class="mono">{(stats.avg_change_ratio * 100).toFixed(1)}%</td>
      </tr>
    </tbody>
  </table>
  <p class="hint">
    Totals cover what is still kept, so they fall when the retention limit trims old
    entries or you wipe. &ldquo;Changed by cleanup&rdquo; is how much the normaliser
    rewrote, not a measure of accuracy.
  </p>
{/if}

<h2>Transcripts</h2>

<div class="field" style="max-width:100%">
  <label for="search">Search</label>
  <div class="row">
    <input
      id="search"
      type="text"
      value={search}
      oninput={(e) => { search = e.currentTarget.value; refresh(); }}
    />
    <button onclick={refresh}>Refresh</button>
  </div>
</div>

{#if items.length === 0}
  <p class="hint">{search ? "Nothing matches." : "Nothing recorded yet."}</p>
{:else}
  {#each items as item}
    <div class="history-item">
      <div class="history-meta">
        <span>{when(item.at)}</span>
        <span>{item.preset}</span>
        <span class="mono">{duration(item.audio_secs)}</span>
        <span class="mono">{item.asr_ms + item.cleanup_ms} ms</span>
        <span class="history-actions">
          <button onclick={() => paste(item.id, false)}>Paste</button>
          {#if item.raw !== item.cleaned}
            <button onclick={() => paste(item.id, true)}>Paste raw</button>
          {/if}
        </span>
      </div>
      <p class="history-text">{item.cleaned || "(empty)"}</p>
      {#if item.raw !== item.cleaned}
        <p class="history-raw mono">{item.raw}</p>
      {/if}
    </div>
  {/each}
{/if}

<h2>Retention</h2>

<label class="check">
  <input
    type="checkbox"
    checked={config.history.enabled}
    onchange={(e) => { config.history.enabled = e.currentTarget.checked; onchange(); }}
  />
  <span>Keep a history of dictations</span>
</label>

<div class="field">
  <label for="limit">How many to keep</label>
  <input
    id="limit"
    type="number"
    min="0"
    max="10000"
    value={config.history.limit}
    oninput={(e) => { config.history.limit = +e.currentTarget.value; onchange(); }}
  />
</div>

{#if confirmingWipe}
  <div class="status bad">
    This deletes every stored transcript and resets the totals. It cannot be undone.
    <div class="row" style="margin-top:8px">
      <button class="primary" onclick={wipe}>Delete everything</button>
      <button onclick={() => (confirmingWipe = false)}>Cancel</button>
    </div>
  </div>
{:else}
  <button onclick={() => (confirmingWipe = true)}>Wipe history</button>
{/if}

<style>
  .history-item {
    border-bottom: 1px solid var(--border);
    padding: 10px 0;
  }
  .history-meta {
    display: flex;
    gap: 12px;
    align-items: center;
    color: var(--text-dim);
    font-size: 12px;
    margin-bottom: 4px;
  }
  .history-actions {
    margin-left: auto;
    display: flex;
    gap: 6px;
  }
  .history-actions button {
    padding: 2px 8px;
    font-size: 12px;
  }
  .history-text {
    margin: 0;
    user-select: text;
  }
  .history-raw {
    margin: 4px 0 0;
    color: var(--text-dim);
    font-size: 12px;
  }
</style>
