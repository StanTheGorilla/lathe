<script>
  import { onMount, onDestroy } from "svelte";
  import {
    platform,
    providerKeyStatus,
    setProviderKey,
    revealProviderKey,
    deleteProviderKey,
    testCloudModel,
  } from "../api.js";

  let { config = $bindable(), onchange } = $props();

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
  let store = $state("the system credential store");

  const SHOW_FOR_SECS = 30;
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

  function addProvider() {
    const id = newId();
    config.providers = [
      ...(config.providers ?? []),
      { id, name: "", base_url: "", models: [], timeout_secs: 120 },
    ];
    keys[id] = { saved: false, last4: "" };
    onchange();
  }

  function removeProvider(p) {
    config.providers = config.providers.filter((x) => x !== p);
    // No slot may point at a provider that is gone.
    for (const field of ["whisper_cloud", "cleanup_cloud", "cleanup_multilingual_cloud"]) {
      if (config.models[field]?.provider === p.id) config.models[field] = null;
    }
    hide(p.id);
    onchange();
  }

  function addModel(p, kind) {
    p.models = [...p.models, { name: "", kind }];
    onchange();
  }

  function removeModel(p, m) {
    p.models = p.models.filter((x) => x !== m);
    for (const field of ["whisper_cloud", "cleanup_cloud", "cleanup_multilingual_cloud"]) {
      const c = config.models[field];
      if (c?.provider === p.id && c.model === m.name) config.models[field] = null;
    }
    onchange();
  }

  async function saveKey(id) {
    keyError[id] = "";
    try {
      keys[id] = await setProviderKey(id, typed[id] ?? "");
      typed[id] = "";
      replacing[id] = false;
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
      tests[k] = { ok: true, text: await testCloudModel($state.snapshot(p), m.name, m.kind) };
    } catch (e) {
      tests[k] = { ok: false, text: String(e) };
    } finally {
      testing[k] = false;
    }
  }

  const insecure = (url) => {
    const u = url.trim().toLowerCase();
    return (
      u.startsWith("http://") &&
      !/^http:\/\/(localhost|127\.0\.0\.1|\[::1\])([:/?#]|$)/.test(u)
    );
  };
</script>

<h1>Providers</h1>
<p class="subtitle">
  Cloud models, from any service with an OpenAI-compatible API: OpenRouter, OpenAI, Groq,
  or LM Studio and Ollama on this computer. Nothing here is used until you pick one of
  its models on the Models page, and then what you dictate is sent to that service.
</p>

<div class="status info">
  API keys are kept in {store}, never in config.toml. They are sent only to the
  provider's own address, and only over https (plain http is allowed to this computer
  alone). Any program running as you can still ask {store} for them; that is how every
  password store works.
</div>

{#each config.providers ?? [] as p (p.id)}
  {@const status = keys[p.id]}
  <section class="provider">
    <div class="field">
      <label for="name-{p.id}">Name</label>
      <input
        id="name-{p.id}"
        type="text"
        placeholder="OpenRouter"
        value={p.name}
        oninput={(e) => { p.name = e.currentTarget.value; onchange(); }}
      />
    </div>

    <div class="field">
      <label for="url-{p.id}">Address</label>
      <input
        id="url-{p.id}"
        class="mono"
        type="text"
        spellcheck="false"
        placeholder="https://openrouter.ai/api/v1"
        value={p.base_url}
        oninput={(e) => { p.base_url = e.currentTarget.value; onchange(); }}
      />
      {#if insecure(p.base_url)}
        <p class="hint" style="color:var(--clay)">
          Plain http to another computer: a key will not be sent to this address. Use
          https, or leave the key empty if the server needs none.
        </p>
      {:else}
        <p class="hint">
          OpenRouter <span class="mono">https://openrouter.ai/api/v1</span>, OpenAI
          <span class="mono">https://api.openai.com/v1</span>, LM Studio
          <span class="mono">http://localhost:1234/v1</span>, Ollama
          <span class="mono">http://localhost:11434/v1</span>.
        </p>
      {/if}
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
            class="mono"
            type="password"
            autocomplete="off"
            spellcheck="false"
            placeholder={status?.saved ? "New key" : "Paste the key"}
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
          Goes straight to {store} when you press Save key, not with the rest of the page.
          Leave it empty for a server on this computer that needs none.
        </p>
      {/if}
      {#if keyError[p.id]}<div class="status bad" style="margin-top:8px">{keyError[p.id]}</div>{/if}
    </div>

    <div class="field">
      <span class="pseudo-label">Models</span>
      {#each p.models as m}
        {@const k = `${p.id}/${m.name}`}
        <div class="row model">
          <input
            class="mono"
            type="text"
            spellcheck="false"
            placeholder={m.kind === "speech" ? "whisper-1" : "google/gemma-3-27b-it"}
            value={m.name}
            oninput={(e) => { m.name = e.currentTarget.value; onchange(); }}
          />
          <span class="hint kind" style="margin:0">{m.kind === "speech" ? "Speech" : "Cleanup"}</span>
          <button disabled={!m.name.trim() || testing[k]} onclick={() => test(p, m)}>
            {testing[k] ? "Testing" : "Test"}
          </button>
          <button onclick={() => removeModel(p, m)}>Remove</button>
        </div>
        {#if tests[k]}
          <p class="hint" style="color:{tests[k].ok ? 'var(--moss)' : 'var(--clay)'}">
            {tests[k].text}
          </p>
        {/if}
      {/each}
      <div class="row" style="margin-top:6px">
        <button onclick={() => addModel(p, "cleanup")}>Add a cleanup model</button>
        <button onclick={() => addModel(p, "speech")}>Add a speech model</button>
      </div>
      <p class="hint">
        The name exactly as the provider lists it. Cleanup models answer chat requests;
        speech models take audio. Test sends one short request (it may cost a fraction of
        a cent) to the saved address, with the saved key.
      </p>
    </div>

    <button onclick={() => removeProvider(p)}>Remove this provider</button>
  </section>
{/each}

<button class="primary" onclick={addProvider}>Add a provider</button>

<style>
  .provider {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 14px 16px;
    margin-bottom: 14px;
  }

  .key {
    min-width: 16ch;
    overflow-wrap: anywhere;
  }

  .model input {
    flex: 1;
  }

  .kind {
    min-width: 7ch;
  }
</style>
