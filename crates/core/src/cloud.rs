// Cleanup through a cloud model: any server with an OpenAI-compatible
// `/chat/completions` endpoint. Same prompt as the local instruction model, sent as
// one user message; the provider applies its model's own chat template.
//
// Every error leaving this module has had the key blanked out of it (see
// `secrets::scrub`), because errors end up in the log.

use anyhow::{anyhow, bail, Result};
use std::time::Duration;

use crate::config::Provider;
use crate::secrets;

/// The HTTP agent every cloud request uses. The `Authorization` header is dropped on
/// any redirect: a provider that bounces the request somewhere else does not get to
/// hand the key to that somewhere else. (ureq's default, set here so it stays so.)
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
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

pub struct ChatClient {
    url: String,
    model: String,
    key: Option<String>,
    timeout: Duration,
}

impl ChatClient {
    pub fn new(provider: &Provider, model: &str, key: Option<String>) -> Result<Self> {
        if provider.base_url.trim().is_empty() {
            bail!("the provider '{}' has no address", provider.name);
        }
        let url = endpoint(&provider.base_url, "/chat/completions");
        let key = checked_key(&url, key)?;
        Ok(Self {
            url,
            model: model.to_string(),
            key,
            timeout: Duration::from_secs(provider.timeout_secs.clamp(5, 600)),
        })
    }

    /// The model's reply to `prompt`, greedy, at most `max_tokens` long.
    pub fn complete(&self, prompt: &str, max_tokens: u32) -> Result<String> {
        let body = request_body(&self.model, prompt, max_tokens);
        let mut request = agent(self.timeout)
            .post(&self.url)
            .header("Content-Type", "application/json");
        if let Some(key) = &self.key {
            request = request.header("Authorization", &format!("Bearer {key}"));
        }
        let scrub = |e: String| anyhow!(secrets::scrub(&e, self.key.as_deref()));
        let mut response = request.send(body.to_string()).map_err(|e| match e {
            ureq::Error::StatusCode(code) => scrub(format!(
                "{} answered HTTP {code}{}",
                self.url,
                match code {
                    401 | 403 => ": the API key was refused",
                    404 => ": no such endpoint or model",
                    429 => ": rate limited or out of credit",
                    _ => "",
                }
            )),
            other => scrub(format!("could not reach {}: {other}", self.url)),
        })?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|e| scrub(format!("reading the reply from {}: {e}", self.url)))?;
        parse_reply(&text).map_err(|e| scrub(format!("{e:#}")))
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

/// The text of the first choice. A reasoning model's `<think>` block, when a provider
/// leaves it in the content, is not part of the answer.
pub fn parse_reply(text: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|_| anyhow!("the provider did not answer with JSON"))?;
    if let Some(message) = v.pointer("/error/message").and_then(|m| m.as_str()) {
        bail!("the provider answered with an error: {message}");
    }
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| anyhow!("the provider's reply has no text in it"))?;
    let content = match content.find("</think>") {
        Some(end) if content.trim_start().starts_with("<think>") => &content[end + 8..],
        _ => content,
    };
    Ok(content.trim().to_string())
}

/// Room for the answer: generous, because a cloud model is billed for what it writes,
/// not for the limit, and a limit hit halfway loses the end of the dictation.
pub fn max_tokens_for(raw: &str, factor: f32) -> u32 {
    let estimate = raw.chars().count() as f32 / 3.0 * factor;
    (estimate as u32 + 256).min(8192)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(url: &str) -> Provider {
        Provider {
            id: "p".into(),
            name: "P".into(),
            base_url: url.into(),
            models: vec![],
            timeout_secs: 30,
        }
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
    }

    #[test]
    fn a_key_is_never_sent_over_plain_http_to_another_machine() {
        let key = Some("sk-0123456789abcdef".to_string());
        assert!(ChatClient::new(&provider("http://example.com/v1"), "m", key.clone()).is_err());
        assert!(ChatClient::new(&provider("https://example.com/v1"), "m", key.clone()).is_ok());
        assert!(ChatClient::new(&provider("http://localhost:1234/v1"), "m", key).is_ok());
        // No key, nothing to protect: a server on the local network is fine.
        assert!(ChatClient::new(&provider("http://192.168.1.5:8000/v1"), "m", None).is_ok());
        // And the refusal does not quote the key.
        let e = ChatClient::new(&provider("http://example.com/v1"), "m", Some("sk-0123456789abcdef".into()))
            .err()
            .unwrap();
        assert!(!format!("{e:#}").contains("sk-0123456789abcdef"));
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
    }
}
