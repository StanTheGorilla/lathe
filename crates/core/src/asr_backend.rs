// Speech recognition backends. Brief section 4.3.
//
// Two implementations behind one trait: the local whisper.cpp path, and an HTTP client
// for any server exposing an OpenAI-compatible `/v1/audio/transcriptions` endpoint.
//
// The second was first written because Cohere Transcribe could not run locally. It
// now does, through CrispASR; the endpoint stays for anything bigger than the card can
// hold -- a machine on the network, a rented GPU, or a hosted API.

use anyhow::{anyhow, bail, Context as _, Result};
use std::time::Duration;

/// Brief 4.3, verbatim in shape: the seam every recognition backend fits through.
pub trait AsrBackend {
    fn transcribe(&self, pcm: &[f32], lang: &str, hints: &[String]) -> Result<String>;
    fn languages(&self) -> &[&str];
}

/// POSTs audio to an OpenAI-compatible transcription endpoint.
///
/// Works with Cohere's hosted API, a local vLLM instance, `cohere_transcribe_rs`'s
/// server, faster-whisper-server, LocalAI, or OpenAI itself. The shape of the request is
/// the same for all of them; only the base URL and model name differ.
pub struct OpenAiCompatBackend {
    base_url: String,
    model: String,
    api_key: Option<String>,
    timeout: Duration,
}

impl OpenAiCompatBackend {
    pub fn new(base_url: &str, model: &str, api_key: Option<String>, timeout_secs: u64) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            // An empty key is the same as no key, which is the normal case for a local
            // server and a mistake worth not sending as a literal empty header.
            api_key: api_key.filter(|k| !k.trim().is_empty()),
            timeout: Duration::from_secs(timeout_secs.max(5)),
        }
    }

    fn endpoint(&self) -> String {
        // Accept both "https://host" and "https://host/v1", since both are what people
        // paste in.
        if self.base_url.ends_with("/audio/transcriptions") {
            self.base_url.clone()
        } else if self.base_url.ends_with("/v1") {
            format!("{}/audio/transcriptions", self.base_url)
        } else {
            format!("{}/v1/audio/transcriptions", self.base_url)
        }
    }
}

/// A 16-bit PCM wav, because every server accepts it and it avoids depending on an
/// encoder. Roughly 32KB per second of audio, which is irrelevant on a LAN and fine over
/// the internet for dictation-length clips.
fn wav_bytes(pcm: &[f32], rate: u32) -> Vec<u8> {
    let data_len = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + data_len);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());

    for sample in pcm {
        let clamped = sample.clamp(-1.0, 1.0);
        out.extend_from_slice(&((clamped * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// Hand-built because the body has one file and three short text fields. Pulling in a
/// multipart crate for that is not worth the dependency.
fn multipart_body(boundary: &str, wav: &[u8], model: &str, lang: &str, prompt: &str) -> Vec<u8> {
    let mut body = Vec::new();
    let mut field = |name: &str, value: &str| {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    };

    field("model", model);
    if !lang.is_empty() {
        field("language", lang);
    }
    if !prompt.is_empty() {
        field("prompt", prompt);
    }
    field("response_format", "json");

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

enum Rejected {
    Status(u16),
    Other(anyhow::Error),
}

impl Rejected {
    fn into_error(self) -> anyhow::Error {
        match self {
            // Brief section 10: fail loudly and say why. A dictation tool that silently
            // produces nothing when a server is unreachable is worse than one that says
            // the server is unreachable.
            Rejected::Status(code) => anyhow!("the transcription endpoint returned HTTP {code}"),
            Rejected::Other(e) => e,
        }
    }
}

impl OpenAiCompatBackend {
    fn post(&self, wav: &[u8], lang: &str, prompt: &str) -> Result<String, Rejected> {
        let boundary = format!(
            "lathe{:x}",
            std::time::UNIX_EPOCH
                .elapsed()
                .map_err(|e| Rejected::Other(e.into()))?
                .as_nanos()
        );
        let body = multipart_body(&boundary, wav, &self.model, lang, prompt);

        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .new_agent();

        let mut request = agent
            .post(self.endpoint())
            .header(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            );
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", &format!("Bearer {key}"));
        }

        let mut response = request.send(&body[..]).map_err(|e| match e {
            ureq::Error::StatusCode(code) => Rejected::Status(code),
            other => Rejected::Other(anyhow!(
                "could not reach the transcription endpoint: {other}"
            )),
        })?;

        let text = response
            .body_mut()
            .read_to_string()
            .context("reading the transcription response")
            .map_err(Rejected::Other)?;

        // The documented response is {"text": "..."}; some servers nest it under a
        // segments array as well, but the top-level field is universal.
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .context("the endpoint did not return JSON")
            .map_err(Rejected::Other)?;
        parsed
            .get("text")
            .and_then(|t| t.as_str())
            .map(|t| t.trim().to_string())
            .ok_or_else(|| Rejected::Other(anyhow!("the response had no \"text\" field: {text}")))
    }
}

impl AsrBackend for OpenAiCompatBackend {
    fn transcribe(&self, pcm: &[f32], lang: &str, hints: &[String]) -> Result<String> {
        if pcm.is_empty() {
            bail!("no audio to transcribe");
        }

        let wav = wav_bytes(pcm, crate::audio::TARGET_RATE);
        let prompt = hints.join(", ");
        match self.post(&wav, lang, &prompt) {
            // `prompt` is in the OpenAI shape, but not every server that copies the
            // shape accepts it. A server that refuses the request over the vocabulary
            // is asked again without it: the words are a hint, the transcript is the job.
            Err(Rejected::Status(code @ (400 | 422))) if !prompt.is_empty() => {
                eprintln!(
                    "the transcription endpoint refused the vocabulary prompt (HTTP {code}); \
                     retrying without it"
                );
                self.post(&wav, lang, "").map_err(Rejected::into_error)
            }
            other => other.map_err(Rejected::into_error),
        }
    }

    fn languages(&self) -> &[&str] {
        // Unknowable without asking the server, and the endpoint has no capability
        // discovery. An empty list means "no claim made" rather than "none supported";
        // the language string is passed through untouched.
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    /// A server that answers each request with the next status in `statuses`, and hands
    /// back every request body it received.
    fn server(statuses: &'static [u16]) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for status in statuses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut buf = [0u8; 65536];
                // Read the headers, then as much body as Content-Length says.
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if raw.len() >= end + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                bodies.push(String::from_utf8_lossy(&raw).into_owned());
                let body = if *status == 200 { r#"{"text":"hello"}"# } else { "{}" };
                write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            bodies
        });
        (url, handle)
    }

    #[test]
    fn sends_the_vocabulary_as_the_prompt() {
        let (url, handle) = server(&[200]);
        let backend = OpenAiCompatBackend::new(&url, "m", None, 10);
        let hints = vec!["Claude".to_string(), "Vulkan".to_string()];
        assert_eq!(backend.transcribe(&[0.0; 1600], "en", &hints).unwrap(), "hello");
        let bodies = handle.join().unwrap();
        assert!(bodies[0].contains("name=\"prompt\"\r\n\r\nClaude, Vulkan\r\n"));
    }

    #[test]
    fn a_server_that_refuses_the_prompt_is_asked_again_without_it() {
        let (url, handle) = server(&[400, 200]);
        let backend = OpenAiCompatBackend::new(&url, "m", None, 10);
        let hints = vec!["Claude".to_string()];
        assert_eq!(backend.transcribe(&[0.0; 1600], "en", &hints).unwrap(), "hello");
        let bodies = handle.join().unwrap();
        assert!(bodies[0].contains("name=\"prompt\""));
        assert!(!bodies[1].contains("name=\"prompt\""));
    }

    #[test]
    fn other_failures_are_not_retried() {
        let (url, handle) = server(&[500]);
        let backend = OpenAiCompatBackend::new(&url, "m", None, 10);
        let err = backend
            .transcribe(&[0.0; 1600], "en", &["Claude".to_string()])
            .unwrap_err();
        assert!(format!("{err:#}").contains("HTTP 500"), "{err:#}");
        assert_eq!(handle.join().unwrap().len(), 1);
    }
}
