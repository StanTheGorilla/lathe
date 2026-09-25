<script>
  import { onMount, onDestroy, tick } from "svelte";
  import {
    platform,
    providerKeyStatus,
    setProviderKey,
    revealProviderKey,
    deleteProviderKey,
    testCloudModel,
    listProviderModels,
  } from "../api.js";
  import { TEMPLATES, LOCAL_SERVERS, displayName, incomplete } from "../providers.js";

  let { config = $bindable(), onchange, saveNow } = $props();

  // Per provider id: what the credential store says ({saved, last4}), the key typed
  // but not yet saved, the key shown after Show, and any error. The key itself is
  // only ever here after Save (typed) or Show (fetched), and Show wipes it again.
  let keys = $state({});
  let typed = $state({});
  let replacing = $state({});
  let shown = $state({});
  let keyError = $state({});
  // Per "provider/model": the answer to Test, or the error.
  let tests = $state({});
  let testing = $state({});
  // Per provider id: the models its own list offers ({loading, error, models}), whether
  // the picker is open, what is typed in its search, and the model typed by name.
  let lists = $state({});
  let picking = $state({});
  let search = $state({});
  let byName = $state({});
  let removing = $state({});
  let store = $state("the system credential store");

  const SHOW_FOR_SECS = 30;
  // A list of hundreds is searched, not scrolled; this many is plenty to scan.
  const SHOW_AT_MOST = 100;
  const timers = {};

  const STORES = {
    windows: "Windows Credential Manager",
    macos: "the macOS Keychain",
    linux: "the Secret Service (your desktop's keyring)",
  };

  onMount(async () => {
    try {
      store = STORES[(await platform()).os] ?? store;
    } catch {
      /* the generic name stands */
    }
    for (const p of config.providers ?? []) refreshKey(p.id);
  });

  // Leaving the page or closing the window takes any shown key with it.
  onDestroy(() => {
    for (const id of Object.keys(timers)) clearTimeout(timers[id]);
    shown = {};
    typed = {};
  });

  async function refreshKey(id) {
    try {
      keys[id] = await providerKeyStatus(id);
      keyError[id] = "";
    } catch (e) {
      keyError[id] = String(e);
    }
  }

  function newId() {
    const bytes = crypto.getRandomValues(new Uint8Array(4));
    return "p-" + Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  }

  async function addProvider(template) {
    const id = newId();
    config.providers = [
      ...(config.providers ?? []),
      {
        id,
        name: template.name,
        base_url: template.base_url,
        api: template.api,
        models: [],
        timeout_secs: 120,
      },
    ];
    keys[id] = { saved: false, last4: "" };
    onchange();
    await tick();
    // The first thing still to fill in: a name for "Other", the key for the rest.
    const next = template.name ? `key-${id}` : `name-${id}`;
    document.getElementById(next)?.focus();
  }

  function removeProvider(p) {
    removing[p.id] = false;
    config.providers = config.providers.filter((x) => x !== p);
    // No slot may point at a provider that is gone.
    for (const field of ["whisper_cloud", "cleanup_cloud", "cleanup_multilingual_cloud"]) {
      if (config.models[field]?.provider === p.id) config.models[field] = null;
    }
    hide(p.id);
    onchange();
  }

  function removeModel(p, name) {
    p.models = p.models.filter((x) => x.name !== name);
    // A slot using it would point at nothing.
    for (const field of ["whisper_cloud", "cleanup_cloud", "cleanup_multilingual_cloud"]) {
      const c = config.models[field];
      if (c?.provider === p.id && c.model === name) config.models[field] = null;
    }
    onchange();
  }

  const isChosen = (p, id) => p.models.some((m) => m.name === id);

  function choose(p, listed, on) {
    if (!on) return removeModel(p, listed.id);
    if (isChosen(p, listed.id)) return;
    const model = { name: listed.id, kind: listed.kind };
    if (listed.label && listed.label !== listed.id) model.label = listed.label;
    p.models = [...p.models, model];
    onchange();
  }

  function addByName(p) {
    const entry = byName[p.id] ?? {};
    const name = (entry.name ?? search[p.id] ?? "").trim();
    if (!name || isChosen(p, name)) return;
    const kind = p.api === "anthropic" ? "cleanup" : (entry.kind ?? "cleanup");
    p.models = [...p.models, { name, kind }];
    byName[p.id] = { name: "", kind };
    onchange();
  }

  // The list comes from the saved address with the stored key, so whatever is typed
  // here is saved first.
  async function fetchModels(p) {
    lists[p.id] = { loading: true, error: "", models: lists[p.id]?.models ?? null };
    if (!(await saveNow())) {
      lists[p.id] = { loading: false, error: "This page could not be saved; see the message below.", models: null };
      return;
    }
    try {
      const models = await listProviderModels($state.snapshot(p));
      lists[p.id] = { loading: false, error: "", models };
    } catch (e) {
      lists[p.id] = { loading: false, error: String(e), models: null };
    }
  }

  function togglePicker(p) {
    picking[p.id] = !picking[p.id];
    if (picking[p.id] && !lists[p.id]?.models && !lists[p.id]?.loading) fetchModels(p);
  }

  function matches(p) {
    const words = (search[p.id] ?? "").toLowerCase().split(/\s+/).filter(Boolean);
    return (lists[p.id]?.models ?? []).filter((m) => {
      const text = `${m.label} ${m.id}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
  }

  async function saveKey(id) {
    keyError[id] = "";
    try {
      keys[id] = await setProviderKey(id, typed[id] ?? "");
      typed[id] = "";
      replacing[id] = false;
      // A list refused for want of a key is worth asking for again.
      if (lists[id]?.error) lists[id] = undefined;
    } catch (e) {
      keyError[id] = String(e);
    }
  }

  async function removeKey(id) {
    keyError[id] = "";
    hide(id);
    try {
      await deleteProviderKey(id);
      keys[id] = { saved: false, last4: "" };
    } catch (e) {
      keyError[id] = String(e);
    }
  }

  async function show(id) {
    keyError[id] = "";
    try {
      shown[id] = await revealProviderKey(id);
      clearTimeout(timers[id]);
      timers[id] = setTimeout(() => hide(id), SHOW_FOR_SECS * 1000);
    } catch (e) {
      keyError[id] = String(e);
    }
  }

  function hide(id) {
    clearTimeout(timers[id]);
    delete timers[id];
    shown[id] = "";
  }

  async function test(p, m) {
    const k = `${p.id}/${m.name}`;
    testing[k] = true;
    tests[k] = null;
    try {
      if (!(await saveNow())) throw "This page could not be saved; see the message below.";
      tests[k] = { ok: true, text: await testCloudModel($state.snapshot(p), m.name, m.kind) };
    } catch (e) {
      tests[k] = { ok: false, text: String(e) };
    } finally {
      testing[k] = false;
    }
  }

  function setField(p, field, value) {
    p[field] = value;
    // A list from another address, or in another shape, is not this provider's.
    if (field !== "name") lists[p.id] = undefined;
    onchange();
  }

  const insecure = (url) => {
    const u = url.trim().toLowerCase();
    return (
      u.startsWith("http://") &&
      !/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])([:/?#]|$)/.test(u)
    );
  };

  const missing = (p) =>
    [!p.name?.trim() && "a name", !p.base_url?.trim() && "an address"].filter(Boolean).join(" and ");

  const refused = (error) => /HTTP 40[13]/.test(error);
</script>

<h1>Providers</h1>
<p class="subtitle">
  Cloud models, from OpenRouter, OpenAI, Anthropic, DeepSeek, or any service with an
  OpenAI-compatible API, including LM Studio and Ollama on this computer. Nothing here is
  used until you pick one of its models on the Models page, and then what you dictate is
  sent to that service.
</p>

<div class="status info">
  API keys are kept in {store}, never in config.toml. They are sent only to the
  provider's own address, and only over https (plain http is allowed to this computer
  alone). Any program running as you can still ask {store} for them; that is how every
  password store works.
</div>

{#each config.providers ?? [] as p (p.id)}
  {@const status = keys[p.id]}
  {@const list = lists[p.id]}
  <section class="provider" class:unfinished={incomplete(p)}>
    <h2 class="provider-name">{displayName(p)}</h2>
    {#if incomplete(p)}
      <div class="status bad">
        Not finished: this provider needs {missing(p)}. Nothing can use it until it has
        both.
      </div>
    {/if}

    <div class="field">
      <label for="name-{p.id}">Name</label>
      <input
        id="name-{p.id}"
        type="text"
        value={p.name}
        oninput={(e) => setField(p, "name", e.currentTarget.value)}
      />
    </div>

    <div class="field">
      <label for="url-{p.id}">Address</label>
      <input
        id="url-{p.id}"
        class="mono"
        type="text"
        spellcheck="false"
        value={p.base_url}
        oninput={(e) => setField(p, "base_url", e.currentTarget.value)}
      />
      {#if insecure(p.base_url)}
        <p class="hint" style="color:var(--clay)">
          Plain http to another computer: a key will not be sent to this address. Use
          https, or leave the key empty if the server needs none.
        </p>
      {:else if !p.base_url.trim()}
        <p class="hint">
          The address from the service's API documentation, often ending in /v1. On this
          computer:
          {#each LOCAL_SERVERS as s}
            <button
              class="inline"
              onclick={() => {
                if (!p.name.trim()) p.name = s.name;
                setField(p, "base_url", s.base_url);
              }}>{s.name} <span class="mono">{s.base_url}</span></button
            >{" "}
          {/each}
        </p>
      {/if}
    </div>

    <div class="field">
      <label for="api-{p.id}">Speaks</label>
      <select
        id="api-{p.id}"
        value={p.api ?? "openai"}
        onchange={(e) => setField(p, "api", e.currentTarget.value)}
      >
        <option value="openai">the OpenAI-compatible API (most services, LM Studio, Ollama)</option>
        <option value="anthropic">Anthropic's API</option>
      </select>
    </div>

    <div class="field">
      <span class="pseudo-label">API key</span>
      {#if status?.saved && !replacing[p.id]}
        <div class="row">
          <span class="mono key" aria-label="saved key">
            {shown[p.id] || `••••••••••••${status.last4}`}
          </span>
          {#if shown[p.id]}
            <button onclick={() => hide(p.id)}>Hide</button>
          {:else}
            <button onclick={() => show(p.id)}>Show</button>
          {/if}
          <button onclick={() => { hide(p.id); replacing[p.id] = true; }}>Replace</button>
          <button onclick={() => removeKey(p.id)}>Remove</button>
        </div>
        <p class="hint">
          {shown[p.id]
            ? `Hidden again after ${SHOW_FOR_SECS} seconds, or when you leave this page.`
            : `Saved in ${store}.`}
        </p>
      {:else}
        <div class="row">
          <input
            id="key-{p.id}"
            class="mono"
            type="password"
            autocomplete="off"
            spellcheck="false"
            aria-label="API key"
            bind:value={typed[p.id]}
          />
          <button class="primary" disabled={!typed[p.id]?.trim()} onclick={() => saveKey(p.id)}>
            Save key
          </button>
          {#if replacing[p.id]}
            <button onclick={() => { replacing[p.id] = false; typed[p.id] = ""; }}>Cancel</button>
          {/if}
        </div>
        <p class="hint">
          Paste the key and press Save key: it goes straight to {store}, never into the
          settings file. Leave it empty for a server on this computer that needs none.
        </p>
      {/if}
      {#if keyError[p.id]}<div class="status bad" style="margin-top:8px">{keyError[p.id]}</div>{/if}
    </div>

    <div class="field">
      <span class="pseudo-label">Models</span>
      {#if p.models.length === 0}
        <p class="hint" style="margin-top:0">None chosen yet. Choose the ones you want to use.</p>
      {/if}
      {#each p.models as m (m.name)}
        {@const k = `${p.id}/${m.name}`}
        <div class="row model">
          <span class="model-name">
            {m.label || m.name}
            {#if m.label}<span class="mono dim">{m.name}</span>{/if}
          </span>
          <span class="hint kind" style="margin:0">{m.kind === "speech" ? "Speech" : "Cleanup"}</span>
          <button disabled={testing[k]} onclick={() => test(p, m)}>
            {testing[k] ? "Testing" : "Test"}
          </button>
          <button onclick={() => removeModel(p, m.name)}>Remove</button>
        </div>
        {#if tests[k]}
          <p class="hint" style="color:{tests[k].ok ? 'var(--moss)' : 'var(--clay)'}">
            {tests[k].text}
          </p>
        {/if}
      {/each}

      <div class="row" style="margin-top:8px">
        <button aria-expanded={!!picking[p.id]} onclick={() => togglePicker(p)} disabled={!p.base_url.trim()}>
          {picking[p.id] ? "Done choosing" : "Choose models"}
        </button>
        {#if !p.base_url.trim()}
          <span class="hint" style="margin:0">Add the address first.</span>
        {/if}
      </div>

      {#if picking[p.id]}
        {@const found = matches(p)}
        <div class="picker-panel">
          <div class="row">
            <input
              type="search"
              aria-label="Search models"
              spellcheck="false"
              value={search[p.id] ?? ""}
              oninput={(e) => (search[p.id] = e.currentTarget.value)}
            />
            <button onclick={() => fetchModels(p)} disabled={list?.loading}>
              {list?.loading ? "Loading" : "Refresh"}
            </button>
          </div>
          {#if list?.loading && !list.models}
            <p class="hint">Asking {displayName(p)} which models it has.</p>
          {:else if list?.error}
            <p class="hint" style="color:var(--clay)">
              {#if refused(list.error)}
                The list needs the API key: save it above, then Refresh.
              {:else}
                The list could not be fetched.
              {/if}
              {list.error}
            </p>
          {:else if list?.models}
            <p class="hint" style="margin:6px 0">
              {list.models.length} models{search[p.id]?.trim() ? `, ${found.length} matching` : ""}.
              Tick the ones to offer on the Models page.
            </p>
            <ul class="found">
              {#each found.slice(0, SHOW_AT_MOST) as m (m.id)}
                <li>
                  <label class="check">
                    <input
                      type="checkbox"
                      checked={isChosen(p, m.id)}
                      onchange={(e) => choose(p, m, e.currentTarget.checked)}
                    />
                    <span>
                      {m.label}
                      {#if m.label !== m.id}<span class="mono dim">{m.id}</span>{/if}
                      <span class="hint found-note">
                        {m.kind === "speech" ? "Speech" : "Cleanup"}{m.price ? `, ${m.price}` : ""}
                      </span>
                    </span>
                  </label>
                </li>
              {/each}
            </ul>
            {#if found.length > SHOW_AT_MOST}
              <p class="hint">And {found.length - SHOW_AT_MOST} more: type to narrow the list.</p>
            {/if}
          {/if}

          {#if list && !list.loading && (list.error || found.length === 0)}
            <div class="row" style="margin-top:8px">
              <input
                class="mono"
                type="text"
                spellcheck="false"
                aria-label="Model name"
                value={byName[p.id]?.name ?? search[p.id] ?? ""}
                oninput={(e) => (byName[p.id] = { ...byName[p.id], name: e.currentTarget.value })}
              />
              {#if p.api !== "anthropic"}
                <select
                  aria-label="What the model does"
                  value={byName[p.id]?.kind ?? "cleanup"}
                  onchange={(e) => (byName[p.id] = { ...byName[p.id], kind: e.currentTarget.value })}
                >
                  <option value="cleanup">Cleanup</option>
                  <option value="speech">Speech</option>
                </select>
              {/if}
              <button onclick={() => addByName(p)}>Add</button>
            </div>
            <p class="hint">
              Not in the list? Add it by its exact name, as the provider's documentation
              writes it.
            </p>
          {/if}
        </div>
      {/if}
      <p class="hint">
        Cleanup models answer chat requests; speech models take audio. Test sends one short
        request (it may cost a fraction of a cent) with the saved key.
      </p>
    </div>

    {#if removing[p.id]}
      <div class="status bad">
        Remove {displayName(p)}? Its key is deleted from {store}, and any slot on the
        Models page using one of its models goes back to the local model. This cannot be
        undone.
        <div class="row" style="margin-top:8px">
          <button class="primary" onclick={() => removeProvider(p)}>Remove</button>
          <button onclick={() => (removing[p.id] = false)}>Cancel</button>
        </div>
      </div>
    {:else}
      <button onclick={() => (removing[p.id] = true)}>Remove this provider</button>
    {/if}
  </section>
{/each}

<div class="field">
  <span class="pseudo-label">Add a provider</span>
  <div class="row templates">
    {#each TEMPLATES as t}
      <button onclick={() => addProvider(t)}>{t.label}</button>
    {/each}
  </div>
  <p class="hint">
    Each fills in its service's name and address. Other starts empty, for any other
    service, or LM Studio or Ollama on this computer.
  </p>
</div>

<style>
  .provider {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px 16px;
    margin-bottom: 14px;
  }

  .provider.unfinished {
    border-color: var(--clay);
  }

  .provider-name {
    margin-top: 0;
  }

  .key {
    min-width: 16ch;
    overflow-wrap: anywhere;
  }

  .model-name {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .dim {
    color: var(--text-dim);
    font-size: 12px;
    margin-left: 6px;
  }

  .kind {
    min-width: 7ch;
  }

  .picker-panel {
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-sunken);
  }

  .picker-panel input[type="search"],
  .picker-panel input[type="text"] {
    flex: 1;
  }

  .found {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 320px;
    overflow-y: auto;
  }

  .found li .check {
    margin: 0;
    padding: 4px 0;
  }

  .found-note {
    display: block;
    margin: 0;
  }

  .templates {
    flex-wrap: wrap;
  }
</style>
