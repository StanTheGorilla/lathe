// Cleanup through a cloud model. Two request shapes: the OpenAI-compatible
// `/chat/completions` nearly every provider speaks, and Anthropic's `/v1/messages`.
// Same prompt as the local instruction model, sent as one user message; the provider
// applies its model's own chat template.
//
// Every error leaving this module has had the key blanked out of it (see
// `secrets::scrub`), because errors end up in the log and in notifications.

use anyhow::{anyhow, bail, Result};
use std::time::Duration;

use crate::config::{Api, CloudKind, Provider};
use crate::secrets;

/// Anthropic's API version header. The one its documentation pins every example to.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic's current models think before answering unless told not to, and the
/// thinking is paid out of `max_tokens`. Below this a long dictation could spend the
/// whole budget thinking and come back with nothing.
const ANTHROPIC_MIN_TOKENS: u32 = 8192;

/// The HTTP agent every cloud request uses. Redirects are not followed at all: every
/// request here may carry a key, and a provider that bounces it somewhere else does
/// not get to hand the key to that somewhere else. ureq's own redirect rule removes
/// only the `Authorization` header, which would still send Anthropic's `x-api-key` on.
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .max_redirects(0)
        .redirect_auth_headers(ureq::config::RedirectAuthHeaders::Never)
        .build()
        .new_agent()
}

/// `base` with `path` under its `/v1`, accepting what people paste: the bare host,
/// the `/v1` address, or the full endpoint.
pub fn endpoint(base: &str, path: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if base.ends_with(path) {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}{path}")
    } else {
        format!("{base}/v1{path}")
    }
}

/// The key to send to `url`, refusing to send one over plain HTTP to another machine.
pub fn checked_key(url: &str, key: Option<String>) -> Result<Option<String>> {
    let key = key.filter(|k| !k.trim().is_empty());
    if key.is_some() && !secrets::key_may_go_to(url) {
        bail!(
            "not sending the API key to {url}: over plain http it would cross the network \
             unencrypted. Use the provider's https:// address."
        );
    }
    Ok(key)
}

fn no_address(provider: &Provider) -> anyhow::Error {
    anyhow!(
        "{} has no address. Add one on the Providers page.",
        provider.display_name()
    )
}

/// One request that may carry a key, and its answer as status and text. Any status
/// comes back rather than as an error, so a provider's own explanation can be read.
struct Call<'a> {
    api: Api,
    key: Option<&'a str>,
    timeout: Duration,
}

impl Call<'_> {
    fn scrub(&self, text: String) -> anyhow::Error {
        anyhow!(secrets::scrub(&text, self.key))
    }

    fn send(&self, url: &str, body: Option<&serde_json::Value>) -> Result<(u16, String)> {
        let agent = agent(self.timeout);
        let result = match body {
            Some(body) => {
                let mut request = agent
                    .post(url)
                    .header("Content-Type", "application/json");
                request = match (self.api, self.key) {
                    (Api::OpenAi, Some(key)) => request.header("Authorization", &format!("Bearer {key}")),
                    (Api::Anthropic, Some(key)) => request.header("x-api-key", key),
                    (_, None) => request,
                };
                if self.api == Api::Anthropic {
                    request = request.header("anthropic-version", ANTHROPIC_VERSION);
                }
                request
                    .config()
                    .http_status_as_error(false)
                    .build()
                    .send(body.to_string())
            }
            None => {
                let mut request = agent.get(url);
                request = match (self.api, self.key) {
                    (Api::OpenAi, Some(key)) => request.header("Authorization", &format!("Bearer {key}")),
                    (Api::Anthropic, Some(key)) => request.header("x-api-key", key),
                    (_, None) => request,
                };
                if self.api == Api::Anthropic {
                    request = request.header("anthropic-version", ANTHROPIC_VERSION);
                }
                request.config().http_status_as_error(false).build().call()
            }
        };
        let mut response =
            result.map_err(|e| self.scrub(format!("could not reach {url}: {e}")))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(self.scrub(format!(
                "{url} answered with a redirect (HTTP {status}), which is not followed so \
                 the key goes nowhere else. Check the provider's address."
            )));
        }
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| self.scrub(format!("reading the reply from {url}: {e}")))?;
        Ok((status, text))
    }

    /// What a status other than 200 means, in words, with the provider's own message
    /// when it sent one.
    fn refusal(&self, url: &str, status: u16, text: &str) -> anyhow::Error {
        let meaning = match status {
            401 | 403 => ": the API key was refused",
            404 => ": no such endpoint or model",
            429 => ": rate limited or out of credit",
            _ => "",
        };
        let said = provider_message(text)
            .map(|m| format!(" ({m})"))
            .unwrap_or_default();
        self.scrub(format!("{url} answered HTTP {status}{meaning}{said}"))
    }
}

/// The error message in a provider's JSON error body. OpenAI and Anthropic both put
/// it at `error.message`.
fn provider_message(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let m = v.pointer("/error/message")?.as_str()?.trim();
    // Some are pages long; the start says what went wrong.
    let m: String = m.chars().take(300).collect();
    (!m.is_empty()).then_some(m)
}

pub struct ChatClient {
    api: Api,
    url: String,
    model: String,
    key: Option<String>,
    timeout: Duration,
}

impl ChatClient {
    pub fn new(provider: &Provider, model: &str, key: Option<String>) -> Result<Self> {
        if provider.base_url.trim().is_empty() {
            return Err(no_address(provider));
        }
        let url = match provider.api {
            Api::OpenAi => endpoint(&provider.base_url, "/chat/completions"),
            Api::Anthropic => endpoint(&provider.base_url, "/messages"),
        };
        let key = checked_key(&url, key)?;
        Ok(Self {
            api: provider.api,
            url,
            model: model.to_string(),
            key,
            timeout: Duration::from_secs(provider.timeout_secs.clamp(5, 600)),
        })
    }

    fn is_openrouter(&self) -> bool {
        self.url.contains("openrouter.ai")
    }

    /// `request_body`, plus OpenRouter's reasoning control: a model that thinks before
    /// it answers is told not to think at all. Tidying a transcript needs no thinking,
    /// and thinking was nearly all of the wait. Measured on dots-3-note (free): with
    /// `effort: "low"` it thought for 800 to 3500 tokens and answered in 9 to 31 s;
    /// `effort: "minimal"` still thought for 1000 tokens (10 s), because OpenRouter sizes
    /// an effort as a share of `max_tokens`. Switched off it thinks for none and
    /// answers in 2 to 3 s whatever the length, with the same or better text.
    /// `exclude` only hides the thinking; it is still done and still waited for.
    /// OpenRouter ignores the switch for models that do not reason.
    fn request_body(&self, prompt: &str, max_tokens: u32) -> serde_json::Value {
        let mut body = request_body(&self.model, prompt, max_tokens);
        if self.is_openrouter() {
            body["reasoning"] = serde_json::json!({ "enabled": false });
        }
        body
    }

    /// The second try after an HTTP 400. OpenAI's reasoning models refuse `temperature`
    /// and `max_tokens` outright, so elsewhere the retry drops both. OpenRouter drops
    /// parameters a model does not take by itself, so its 400 more likely means a model
    /// that must think and refuses to be told not to; that one is asked to think
    /// briefly and keep the thinking to itself instead.
    fn retry_body(&self, prompt: &str, max_tokens: u32) -> serde_json::Value {
        if self.is_openrouter() {
            let mut body = request_body(&self.model, prompt, max_tokens);
            body["reasoning"] = serde_json::json!({ "effort": "low", "exclude": true });
            body
        } else {
            plain_request_body(&self.model, prompt)
        }
    }

    fn call(&self) -> Call<'_> {
        Call { api: self.api, key: self.key.as_deref(), timeout: self.timeout }
    }

    /// The model's reply to `prompt`, greedy where the provider allows it, at most
    /// `max_tokens` long.
    pub fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let call = self.call();
        match self.api {
            Api::OpenAi => {
                let (mut status, mut text) =
                    call.send(&self.url, Some(&self.request_body(prompt, max_tokens)))?;
                // Asked again in the shape `retry_body` gives, a model that refused the
                // first shape answers; anything else that was wrong with the request is
                // still wrong and says so the second time.
                if status == 400 {
                    (status, text) =
                        call.send(&self.url, Some(&self.retry_body(prompt, max_tokens)))?;
                }
                if status != 200 {
                    return Err(call.refusal(&self.url, status, &text));
                }
                parse_reply(&text).map_err(|e| call.scrub(format!("{e:#}")))
            }
            Api::Anthropic => {
                let body = anthropic_body(&self.model, prompt, max_tokens);
                let (status, text) = call.send(&self.url, Some(&body))?;
                if status != 200 {
                    return Err(call.refusal(&self.url, status, &text));
                }
                parse_anthropic_reply(&text).map_err(|e| call.scrub(format!("{e:#}")))
            }
        }
    }
}

pub fn request_body(model: &str, prompt: &str, max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        // Brief 4.2: cleanup is deterministic. Not every provider honours it.
        "temperature": 0,
        "max_tokens": max_tokens,
        "stream": false,
    })
}

/// The request with nothing a model could refuse: no sampling, no length limit.
pub fn plain_request_body(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": prompt }],
        "stream": false,
    })
}

/// Anthropic's shape. No `temperature` or `top_p`: its current models answer HTTP 400
/// to any sampling parameter. No `thinking` either, so each model does its default.
pub fn anthropic_body(model: &str, prompt: &str, max_tokens: u32) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": max_tokens.max(ANTHROPIC_MIN_TOKENS),
        "messages": [{ "role": "user", "content": prompt }],
    })
}

/// The text of the first choice. A reasoning model's `<think>` block, when a provider
/// leaves it in the content, is not part of the answer.
pub fn parse_reply(text: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|_| anyhow!("the provider did not answer with JSON"))?;
    if let Some(message) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        bail!("the provider answered with an error: {message}");
    }
    let content = v.pointer("/choices/0/message/content").and_then(|c| c.as_str());
    let finish = v.pointer("/choices/0/finish_reason").and_then(|f| f.as_str());
    let content = match (content, finish) {
        // A reasoning model that spent the whole budget thinking has nothing to say.
        (None | Some(""), Some("length")) => bail!(
            "the model used its whole answer budget thinking and wrote no text; \
             a model that does not reason, or reasons less, avoids this"
        ),
        (Some(c), _) => c,
        (None, finish) => bail!(
            "the provider's reply has no text in it (finish reason: {})",
            finish.unwrap_or("none given")
        ),
    };
    let content = match content.find("</think>") {
        Some(end) if content.trim_start().starts_with("<think>") => &content[end + 8..],
        _ => content,
    };
    Ok(content.trim().to_string())
}

/// The text blocks of an Anthropic reply, joined. Thinking blocks are the model's
/// working, not its answer.
pub fn parse_anthropic_reply(text: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|_| anyhow!("the provider did not answer with JSON"))?;
    if let Some(message) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        bail!("the provider answered with an error: {message}");
    }
    if v.get("stop_reason").and_then(|s| s.as_str()) == Some("refusal") {
        bail!("the model refused to clean this dictation");
    }
    let blocks = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("the provider's reply has no text in it"))?;
    let text: String = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        .collect();
    Ok(text.trim().to_string())
}

/// Room for the answer: generous, because a cloud model is billed for what it writes,
/// not for the limit, and a limit hit halfway loses the end of the dictation.
///
/// At least 8192 whatever the dictation's length: a reasoning model spends tokens
/// thinking before it writes a word, and with a budget sized to the text alone it
/// ran out before answering (dots-3-note on OpenRouter, a 21 s dictation). OpenRouter
/// now asks for no thinking, but a model that insists on it, or another provider's
/// model that thinks by default, still needs the room. A limit costs no time: the
/// model stops when the text is done.
pub fn max_tokens_for(raw: &str, factor: f32) -> u32 {
    let estimate = raw.chars().count() as f32 / 3.0 * factor;
    (estimate as u32 + 256).clamp(8192, 32768)
}

/// A model a provider offers, as the settings window lists it.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct Listed {
    pub id: String,
    /// The provider's display name for it, or the id.
    pub label: String,
    pub kind: CloudKind,
    /// Dollars per million tokens in and out, when the list says (OpenRouter's does).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
}

/// The models `provider` offers that Lathe could use, from its own list.
pub fn list_models(provider: &Provider, key: Option<String>) -> Result<Vec<Listed>> {
    if provider.base_url.trim().is_empty() {
        return Err(no_address(provider));
    }
    let url = endpoint(&provider.base_url, "/models");
    let key = checked_key(&url, key)?;
    let call = Call {
        api: provider.api,
        key: key.as_deref(),
        timeout: Duration::from_secs(provider.timeout_secs.clamp(5, 60)),
    };
    match provider.api {
        Api::OpenAi => {
            let (status, text) = call.send(&url, None)?;
            if status != 200 {
                return Err(call.refusal(&url, status, &text));
            }
            parse_openai_models(&text).map_err(|e| call.scrub(format!("{e:#}")))
        }
        Api::Anthropic => {
            let mut all = Vec::new();
            let mut after: Option<String> = None;
            // One page holds every model Anthropic offers today; the rest is in case
            // that changes, bounded so a server that always says "more" cannot loop.
            for _ in 0..10 {
                let page = match &after {
                    Some(id) => format!("{url}?limit=1000&after_id={id}"),
                    None => format!("{url}?limit=1000"),
                };
                let (status, text) = call.send(&page, None)?;
                if status != 200 {
                    return Err(call.refusal(&url, status, &text));
                }
                let (models, next) =
                    parse_anthropic_models(&text).map_err(|e| call.scrub(format!("{e:#}")))?;
                all.extend(models);
                match next {
                    Some(id) => after = Some(id),
                    None => break,
                }
            }
            Ok(all)
        }
    }
}

/// What an OpenAI-shape model id is for, from its name alone, which is all most
/// lists give. `None` for the models that neither recognise speech nor chat: they
/// would only fail when a dictation reached them.
pub fn kind_of(id: &str) -> Option<CloudKind> {
    let id = id.to_ascii_lowercase();
    if id.contains("whisper") || id.contains("transcribe") {
        return Some(CloudKind::Speech);
    }
    const NOT_CHAT: [&str; 8] =
        ["embed", "tts", "dall-e", "image", "moderation", "realtime", "sora", "audio"];
    if NOT_CHAT.iter().any(|w| id.contains(w)) {
        return None;
    }
    Some(CloudKind::Cleanup)
}

pub fn parse_openai_models(text: &str) -> Result<Vec<Listed>> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| anyhow!("the provider's model list is not JSON"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("the provider's model list has no models in it"))?;
    Ok(data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            // OpenRouter says what a model writes; one that writes no text cannot clean.
            if let Some(out) = m.pointer("/architecture/output_modalities").and_then(|o| o.as_array()) {
                if !out.iter().any(|o| o.as_str() == Some("text")) {
                    return None;
                }
            }
            let kind = kind_of(id)?;
            let label = m
                .get("name")
                .and_then(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(id);
            Some(Listed {
                id: id.to_string(),
                label: label.to_string(),
                kind,
                price: price_of(m),
            })
        })
        .collect())
}

/// OpenRouter's per-token prices as dollars per million tokens, in and out.
fn price_of(model: &serde_json::Value) -> Option<String> {
    let per_token = |field: &str| -> Option<f64> {
        let v = model.pointer(&format!("/pricing/{field}"))?;
        v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64())
    };
    let (input, output) = (per_token("prompt")?, per_token("completion")?);
    // OpenRouter marks a price decided per request (its router) as -1.
    if input < 0.0 || output < 0.0 {
        return None;
    }
    if input == 0.0 && output == 0.0 {
        return Some("free".into());
    }
    let money = |per_token: f64| {
        let per_million = per_token * 1_000_000.0;
        if per_million >= 10.0 {
            format!("${per_million:.0}")
        } else if per_million >= 0.1 {
            format!("${per_million:.2}")
        } else {
            format!("${per_million:.3}")
        }
    };
    Some(format!("{} in, {} out per million tokens", money(input), money(output)))
}

/// A page of Anthropic's model list, and the id to continue after when there is more.
pub fn parse_anthropic_models(text: &str) -> Result<(Vec<Listed>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| anyhow!("the provider's model list is not JSON"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| anyhow!("the provider's model list has no models in it"))?;
    let models = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            let label = m
                .get("display_name")
                .and_then(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(id);
            // Anthropic has no speech models: everything it lists answers messages.
            Some(Listed { id: id.to_string(), label: label.to_string(), kind: CloudKind::Cleanup, price: None })
        })
        .collect();
    let more = v.get("has_more").and_then(|m| m.as_bool()).unwrap_or(false);
    let next = v
        .get("last_id")
        .and_then(|l| l.as_str())
        .filter(|_| more)
        .map(str::to_string);
    Ok((models, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn provider(url: &str) -> Provider {
        Provider {
            id: "p".into(),
            name: "P".into(),
            base_url: url.into(),
            api: Api::OpenAi,
            models: vec![],
            timeout_secs: 30,
        }
    }

    /// A server on this machine that answers each request in turn with one of
    /// `replies` (status, extra headers, body), and hands back what it was sent.
    fn server(
        replies: Vec<(u16, &'static str, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, headers, body) in replies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                // Headers, then as much body as Content-Length says.
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                seen.push(String::from_utf8_lossy(&request).to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
            seen
        });
        (url, handle)
    }

    #[test]
    fn the_endpoint_comes_from_whatever_address_was_pasted() {
        let c = "/chat/completions";
        assert_eq!(endpoint("https://openrouter.ai/api/v1", c), "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(endpoint("https://api.openai.com/v1/", c), "https://api.openai.com/v1/chat/completions");
        assert_eq!(endpoint("http://localhost:11434", c), "http://localhost:11434/v1/chat/completions");
        assert_eq!(
            endpoint("https://api.groq.com/openai/v1/chat/completions", c),
            "https://api.groq.com/openai/v1/chat/completions"
        );
        assert_eq!(endpoint("https://api.anthropic.com", "/messages"), "https://api.anthropic.com/v1/messages");
        assert_eq!(endpoint("https://api.anthropic.com/v1", "/models"), "https://api.anthropic.com/v1/models");
        assert_eq!(endpoint("https://api.deepseek.com", "/models"), "https://api.deepseek.com/v1/models");
    }

    #[test]
    fn a_key_is_never_sent_over_plain_http_to_another_machine() {
        let key = Some("sk-0123456789abcdef".to_string());
        assert!(ChatClient::new(&provider("http://example.com/v1"), "m", key.clone()).is_err());
        assert!(ChatClient::new(&provider("https://example.com/v1"), "m", key.clone()).is_ok());
        assert!(ChatClient::new(&provider("http://localhost:1234/v1"), "m", key.clone()).is_ok());
        assert!(list_models(&provider("http://example.com/v1"), key).is_err());
        // No key, nothing to protect: a server on the local network is fine.
        assert!(ChatClient::new(&provider("http://192.168.1.5:8000/v1"), "m", None).is_ok());
        // And the refusal does not quote the key.
        let e = ChatClient::new(&provider("http://example.com/v1"), "m", Some("sk-0123456789abcdef".into()))
            .err()
            .unwrap();
        assert!(!format!("{e:#}").contains("sk-0123456789abcdef"));
    }

    #[test]
    fn a_provider_without_an_address_is_named_in_the_error() {
        let mut p = provider("");
        p.name = String::new();
        let e = ChatClient::new(&p, "m", None).err().unwrap();
        assert_eq!(format!("{e:#}"), "the unnamed provider has no address. Add one on the Providers page.");
    }

    #[test]
    fn a_budget_spent_on_thinking_is_named_as_such() {
        // What OpenRouter returned for dots-3-note with a budget sized to the text.
        let spent = r#"{"choices":[{"message":{"role":"assistant","content":null},"finish_reason":"length"}]}"#;
        let e = format!("{:#}", parse_reply(spent).unwrap_err());
        assert!(e.contains("thinking"), "{e}");
        let empty = r#"{"choices":[{"message":{"content":""},"finish_reason":"length"}]}"#;
        assert!(parse_reply(empty).is_err());
        let odd = r#"{"choices":[{"message":{"content":null},"finish_reason":"content_filter"}]}"#;
        assert!(format!("{:#}", parse_reply(odd).unwrap_err()).contains("content_filter"));
        // A short dictation still leaves a reasoning model room to think.
        assert_eq!(max_tokens_for("so um this is a test", 2.0), 8192);
        assert!(max_tokens_for(&"word ".repeat(20_000), 2.0) <= 32768);
    }

    #[test]
    fn openrouter_is_asked_not_to_think() {
        let or = ChatClient::new(&provider("https://openrouter.ai/api/v1"), "m", None).unwrap();
        let body = or.request_body("x", 8192);
        assert_eq!(body["reasoning"], serde_json::json!({ "enabled": false }));
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["max_tokens"], 8192);
        // A model that must think is asked again to think briefly, with its limits kept.
        let retry = or.retry_body("x", 8192);
        assert_eq!(retry["reasoning"], serde_json::json!({ "effort": "low", "exclude": true }));
        assert_eq!(retry["max_tokens"], 8192);
        // Other providers do not know the parameter and get neither.
        let other = ChatClient::new(&provider("https://api.deepseek.com"), "m", None).unwrap();
        assert!(other.request_body("x", 8192).get("reasoning").is_none());
        let plain = other.retry_body("x", 8192);
        assert!(plain.get("reasoning").is_none() && plain.get("max_tokens").is_none());
    }

    #[test]
    fn the_reply_is_the_first_choice_without_its_thinking() {
        let ok = r#"{"choices":[{"message":{"role":"assistant","content":"  Hello, world.\n"}}]}"#;
        assert_eq!(parse_reply(ok).unwrap(), "Hello, world.");
        let think = r#"{"choices":[{"message":{"content":"<think>hmm</think>\n\nHello."}}]}"#;
        assert_eq!(parse_reply(think).unwrap(), "Hello.");
        // Filler-only input is allowed to come back empty (brief 4.2).
        let empty = r#"{"choices":[{"message":{"content":""}}]}"#;
        assert_eq!(parse_reply(empty).unwrap(), "");
        let err = r#"{"error":{"message":"model not found"}}"#;
        assert!(format!("{:#}", parse_reply(err).unwrap_err()).contains("model not found"));
        assert!(parse_reply("<html>").is_err());
    }

    #[test]
    fn the_request_is_greedy_and_names_the_model() {
        let body = request_body("google/gemma-3-27b-it", "clean this", 300);
        assert_eq!(body["model"], "google/gemma-3-27b-it");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "clean this");
        assert_eq!(body["stream"], false);
        let plain = plain_request_body("o3-mini", "clean this");
        assert_eq!(plain.as_object().unwrap().len(), 3);
        assert!(plain.get("temperature").is_none() && plain.get("max_tokens").is_none());
    }

    #[test]
    fn anthropic_gets_no_sampling_and_room_to_think() {
        let body = anthropic_body("claude-sonnet-4-5", "clean this", 300);
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 8192);
        assert_eq!(body["messages"][0]["content"], "clean this");
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn an_anthropic_reply_is_its_text_blocks() {
        let reply = r#"{"content":[{"type":"thinking","thinking":"the user wants"},
            {"type":"text","text":"Hello, "},{"type":"text","text":"world.\n"}],"stop_reason":"end_turn"}"#;
        assert_eq!(parse_anthropic_reply(reply).unwrap(), "Hello, world.");
        let refused = r#"{"content":[],"stop_reason":"refusal"}"#;
        assert!(format!("{:#}", parse_anthropic_reply(refused).unwrap_err()).contains("refused"));
        let err = r#"{"type":"error","error":{"type":"not_found_error","message":"model: claude-x"}}"#;
        assert!(format!("{:#}", parse_anthropic_reply(err).unwrap_err()).contains("model: claude-x"));
        assert!(parse_anthropic_reply("{}").is_err());
    }

    #[test]
    fn anthropic_is_asked_in_its_own_shape_with_its_own_header() {
        let key = "sk-ant-0123456789abcdef";
        let (url, seen) = server(vec![(
            200,
            "",
            r#"{"content":[{"type":"text","text":"Cleaned."}],"stop_reason":"end_turn"}"#.into(),
        )]);
        let mut p = provider(&url);
        p.api = Api::Anthropic;
        let out = ChatClient::new(&p, "claude-haiku-4-5", Some(key.into())).unwrap().complete("x", 100).unwrap();
        assert_eq!(out, "Cleaned.");
        let request = &seen.join().unwrap()[0];
        assert!(request.starts_with("POST /v1/messages "), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains(&format!("x-api-key: {key}")));
        assert!(lower.contains("anthropic-version: 2023-06-01"));
        assert!(!lower.contains("authorization:"));
        assert!(!request.contains("temperature"));
    }

    #[test]
    fn a_model_that_refuses_sampling_is_asked_again_without_it() {
        let (url, seen) = server(vec![
            (400, "", r#"{"error":{"message":"Unsupported parameter: 'max_tokens'"}}"#.into()),
            (200, "", r#"{"choices":[{"message":{"content":"Done."}}]}"#.into()),
        ]);
        let out = ChatClient::new(&provider(&url), "o3-mini", None).unwrap().complete("x", 100).unwrap();
        assert_eq!(out, "Done.");
        let seen = seen.join().unwrap();
        assert!(seen[0].contains("\"temperature\""));
        assert!(!seen[1].contains("\"temperature\"") && !seen[1].contains("max_tokens"));
    }

    #[test]
    fn a_refusal_says_what_the_provider_said_without_the_key() {
        let key = "sk-0123456789abcdef";
        let (url, _) = server(vec![(
            401,
            "",
            format!(r#"{{"error":{{"message":"Incorrect API key provided: {key}"}}}}"#),
        )]);
        let e = ChatClient::new(&provider(&url), "m", Some(key.into())).unwrap().complete("x", 10).unwrap_err();
        let text = format!("{e:#}");
        assert!(text.contains("HTTP 401: the API key was refused (Incorrect API key provided:"), "{text}");
        assert!(!text.contains(key));
    }

    #[test]
    fn a_redirect_is_not_followed_with_the_key() {
        // A model list is a GET, which ureq would follow, taking x-api-key along.
        let key = "sk-ant-0123456789abcdef";
        let (url, seen) = server(vec![(
            302,
            "Location: http://127.0.0.1:9/v1/models\r\n",
            String::new(),
        )]);
        let mut p = provider(&url);
        p.api = Api::Anthropic;
        let e = list_models(&p, Some(key.into())).unwrap_err();
        assert!(format!("{e:#}").contains("redirect"), "{e:#}");
        assert_eq!(seen.join().unwrap().len(), 1);
        let (url, _) = server(vec![(307, "Location: http://127.0.0.1:9/x\r\n", String::new())]);
        let e = ChatClient::new(&provider(&url), "m", Some(key.into())).unwrap().complete("x", 10).unwrap_err();
        assert!(format!("{e:#}").contains("redirect"), "{e:#}");
    }

    #[test]
    fn model_ids_say_what_they_are_for() {
        assert_eq!(kind_of("whisper-1"), Some(CloudKind::Speech));
        assert_eq!(kind_of("gpt-4o-mini-transcribe"), Some(CloudKind::Speech));
        assert_eq!(kind_of("gpt-4o-transcribe-diarize"), Some(CloudKind::Speech));
        assert_eq!(kind_of("gpt-4.1-mini"), Some(CloudKind::Cleanup));
        assert_eq!(kind_of("deepseek-chat"), Some(CloudKind::Cleanup));
        for skip in [
            "text-embedding-3-small", "tts-1-hd", "gpt-4o-mini-tts", "dall-e-3", "gpt-image-1",
            "omni-moderation-latest", "gpt-4o-realtime-preview", "sora-2", "gpt-4o-audio-preview",
        ] {
            assert_eq!(kind_of(skip), None, "{skip}");
        }
    }

    #[test]
    fn an_openrouter_list_gives_names_prices_and_only_text_models() {
        let list = r#"{"data":[
            {"id":"google/gemma-3-27b-it","name":"Google: Gemma 3 27B",
             "architecture":{"output_modalities":["text"]},
             "pricing":{"prompt":"0.00000009","completion":"0.00000017"}},
            {"id":"google/gemini-2.5-flash-image","name":"Nano Banana",
             "architecture":{"output_modalities":["image","text"]},
             "pricing":{"prompt":"0.0000003","completion":"0.0000025"}},
            {"id":"black-forest-labs/flux","name":"FLUX",
             "architecture":{"output_modalities":["image"]}},
            {"id":"meta-llama/llama-3.3-70b-instruct:free","name":"Llama 3.3 70B (free)",
             "pricing":{"prompt":"0","completion":"0"}},
            {"id":"openrouter/auto","name":"Auto Router","pricing":{"prompt":"-1","completion":"-1"}},
            {"id":"anthropic/claude-sonnet-4.5","pricing":{"prompt":"0.000003","completion":"0.000015"}}
        ]}"#;
        let models = parse_openai_models(list).unwrap();
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "google/gemma-3-27b-it",
                "meta-llama/llama-3.3-70b-instruct:free",
                "openrouter/auto",
                "anthropic/claude-sonnet-4.5",
            ]
        );
        assert_eq!(models[0].label, "Google: Gemma 3 27B");
        assert_eq!(models[0].price.as_deref(), Some("$0.090 in, $0.17 out per million tokens"));
        assert_eq!(models[1].price.as_deref(), Some("free"));
        assert_eq!(models[2].price, None);
        assert_eq!(models[3].label, "anthropic/claude-sonnet-4.5");
        assert_eq!(models[3].price.as_deref(), Some("$3.00 in, $15 out per million tokens"));
        // OpenAI's own list: ids only.
        let openai = r#"{"object":"list","data":[{"id":"gpt-4.1","object":"model"},{"id":"whisper-1"},{"id":"tts-1"}]}"#;
        let models = parse_openai_models(openai).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!((models[1].id.as_str(), models[1].kind), ("whisper-1", CloudKind::Speech));
        assert!(parse_openai_models("<html>").is_err());
    }

    #[test]
    fn an_anthropic_list_is_all_cleanup_and_pages_on() {
        let page = r#"{"data":[{"type":"model","id":"claude-opus-4-1","display_name":"Claude Opus 4.1"},
            {"type":"model","id":"claude-haiku-4-5","display_name":""}],"has_more":true,"last_id":"claude-haiku-4-5"}"#;
        let (models, next) = parse_anthropic_models(page).unwrap();
        assert_eq!(models[0].label, "Claude Opus 4.1");
        assert_eq!(models[1].label, "claude-haiku-4-5");
        assert!(models.iter().all(|m| m.kind == CloudKind::Cleanup));
        assert_eq!(next.as_deref(), Some("claude-haiku-4-5"));
        let last = r#"{"data":[],"has_more":false,"last_id":null}"#;
        assert_eq!(parse_anthropic_models(last).unwrap().1, None);
    }

    #[test]
    fn the_model_list_is_asked_for_with_the_key_in_each_shape() {
        let key = "sk-ant-0123456789abcdef";
        let (url, seen) = server(vec![
            (200, "", r#"{"data":[{"id":"claude-a","display_name":"A"}],"has_more":true,"last_id":"claude-a"}"#.into()),
            (200, "", r#"{"data":[{"id":"claude-b","display_name":"B"}],"has_more":false,"last_id":"claude-b"}"#.into()),
        ]);
        let mut p = provider(&url);
        p.api = Api::Anthropic;
        let models = list_models(&p, Some(key.into())).unwrap();
        assert_eq!(models.len(), 2);
        let seen = seen.join().unwrap();
        assert!(seen[0].starts_with("GET /v1/models?limit=1000 "), "{}", seen[0]);
        assert!(seen[1].starts_with("GET /v1/models?limit=1000&after_id=claude-a "), "{}", seen[1]);
        assert!(seen[0].to_ascii_lowercase().contains(&format!("x-api-key: {key}")));

        let (url, seen) = server(vec![(200, "", r#"{"data":[{"id":"gpt-4.1"}]}"#.into())]);
        let models = list_models(&provider(&format!("{url}/v1")), Some("sk-0123456789abcdef".into())).unwrap();
        assert_eq!(models[0].id, "gpt-4.1");
        let request = &seen.join().unwrap()[0];
        assert!(request.starts_with("GET /v1/models "));
        assert!(request.to_ascii_lowercase().contains("authorization: bearer sk-0123456789abcdef"));
    }
}
