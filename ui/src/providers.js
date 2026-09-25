// What the Providers and Models pages both need to know about a provider.

// The services most people use, filled in as real values rather than as grey hints:
// a hint that looks like a value got a provider saved with neither a name nor an
// address. "Other" starts empty, for anything else, on this computer or not.
export const TEMPLATES = [
  { label: "OpenRouter", name: "OpenRouter", base_url: "https://openrouter.ai/api/v1", api: "openai" },
  { label: "OpenAI", name: "OpenAI", base_url: "https://api.openai.com/v1", api: "openai" },
  { label: "Anthropic", name: "Anthropic", base_url: "https://api.anthropic.com", api: "anthropic" },
  { label: "DeepSeek", name: "DeepSeek", base_url: "https://api.deepseek.com", api: "openai" },
  { label: "Other", name: "", base_url: "", api: "openai" },
];

// Servers that run on this computer, offered under an "Other" provider's address.
export const LOCAL_SERVERS = [
  { name: "LM Studio", base_url: "http://localhost:1234/v1" },
  { name: "Ollama", base_url: "http://localhost:11434/v1" },
];

const host = (url) =>
  (url ?? "").trim().split("://").pop().split(/[/?#]/)[0];

// A provider saved before it had a name still needs telling apart from the others.
export function displayName(p) {
  return p.name?.trim() || host(p.base_url) || "Unnamed provider";
}

export const incomplete = (p) => !p.name?.trim() || !p.base_url?.trim();
