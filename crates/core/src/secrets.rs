// API keys for cloud providers, kept in the operating system's credential store:
// Windows Credential Manager, the macOS Keychain, the Secret Service on Linux.
//
// Never in config.toml. That file is plain text, read by anything that can read the
// user's AppData, pasted into bug reports and synced by backup tools. The credential
// store is encrypted with the user's login and is where the OS keeps its own secrets.
// It is not a wall against a program already running as the user -- no store is --
// but it keeps the key out of every file Lathe writes.
//
// Nothing here logs a key, and `scrub` exists so that nothing else does either: some
// providers repeat the key back in their error text.

use anyhow::{Context as _, Result};

/// The service name every key is filed under in the credential store.
const SERVICE: &str = "Lathe";

/// Somewhere keys can be kept. The OS store in the app; a map in tests, so the
/// migration can be tested without touching a real keychain.
pub trait KeyStore {
    fn get(&self, provider: &str) -> Result<Option<String>>;
    fn set(&self, provider: &str, key: &str) -> Result<()>;
    fn delete(&self, provider: &str) -> Result<()>;
}

/// The operating system's credential store.
pub struct OsKeyStore;

fn entry(provider: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("provider:{provider}"))
        .context("opening the system credential store")
}

impl KeyStore for OsKeyStore {
    fn get(&self, provider: &str) -> Result<Option<String>> {
        match entry(provider)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("reading a key from the system credential store"),
        }
    }

    fn set(&self, provider: &str, key: &str) -> Result<()> {
        entry(provider)?
            .set_password(key)
            .context("saving a key to the system credential store")
    }

    fn delete(&self, provider: &str) -> Result<()> {
        match entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context("removing a key from the system credential store"),
        }
    }
}

/// Saves a key and reads it back, so a store that accepted the write but kept nothing
/// is found out now, while the caller still holds the key, not at the next dictation.
pub fn set_verified(store: &dyn KeyStore, provider: &str, key: &str) -> Result<()> {
    store.set(provider, key)?;
    match store.get(provider)? {
        Some(read) if read == key => Ok(()),
        _ => anyhow::bail!("the system credential store did not keep the key"),
    }
}

/// What the settings window may know about a key without holding it.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct KeyStatus {
    pub saved: bool,
    /// The last four characters, the way provider dashboards show a key, so two keys
    /// can be told apart. Empty for a key too short for four to be a small part of it.
    pub last4: String,
}

impl KeyStatus {
    pub fn of(key: Option<&str>) -> Self {
        match key {
            Some(k) if !k.is_empty() => {
                let chars: Vec<char> = k.chars().collect();
                let last4 = if chars.len() >= 16 {
                    chars[chars.len() - 4..].iter().collect()
                } else {
                    String::new()
                };
                Self { saved: true, last4 }
            }
            _ => Self { saved: false, last4: String::new() },
        }
    }
}

/// `text` with every copy of `key` blanked out. For anything that is about to be
/// logged or shown after a request that carried the key.
pub fn scrub(text: &str, key: Option<&str>) -> String {
    match key {
        // A very short "key" would blank out ordinary words; nobody's key is that short.
        Some(k) if k.len() >= 8 => text.replace(k, "\u{2022}\u{2022}\u{2022}\u{2022}"),
        _ => text.to_string(),
    }
}

/// Whether a key may be sent to `url`: over HTTPS anywhere, over plain HTTP only to
/// this computer. A key sent in the clear can be read by anyone on the network path;
/// a server on this machine never puts it on a network.
pub fn key_may_go_to(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") {
        return true;
    }
    let Some(rest) = lower.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Drop any user:password@ part, then the port. An IPv6 host is bracketed.
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A store that lives in memory, and can be told to lose what it is given.
    #[derive(Default)]
    pub struct MemoryStore {
        pub keys: RefCell<HashMap<String, String>>,
        pub forgetful: bool,
        pub broken: bool,
    }

    impl KeyStore for MemoryStore {
        fn get(&self, provider: &str) -> Result<Option<String>> {
            if self.broken {
                anyhow::bail!("no credential store");
            }
            Ok(self.keys.borrow().get(provider).cloned())
        }
        fn set(&self, provider: &str, key: &str) -> Result<()> {
            if self.broken {
                anyhow::bail!("no credential store");
            }
            if !self.forgetful {
                self.keys.borrow_mut().insert(provider.into(), key.into());
            }
            Ok(())
        }
        fn delete(&self, provider: &str) -> Result<()> {
            self.keys.borrow_mut().remove(provider);
            Ok(())
        }
    }

    #[test]
    fn a_store_that_keeps_nothing_is_caught_on_save() {
        let store = MemoryStore { forgetful: true, ..Default::default() };
        assert!(set_verified(&store, "p", "sk-0123456789abcdef").is_err());
        let store = MemoryStore::default();
        set_verified(&store, "p", "sk-0123456789abcdef").unwrap();
        assert_eq!(store.get("p").unwrap().as_deref(), Some("sk-0123456789abcdef"));
    }

    #[test]
    fn the_window_sees_only_the_last_four_characters() {
        let s = KeyStatus::of(Some("sk-or-v1-0123456789abcdefWXYZ"));
        assert_eq!(s, KeyStatus { saved: true, last4: "WXYZ".into() });
        // Four characters of a short key are too much of it.
        assert_eq!(KeyStatus::of(Some("short-key")).last4, "");
        assert!(!KeyStatus::of(None).saved);
        assert!(!KeyStatus::of(Some("")).saved);
    }

    #[test]
    fn a_key_is_blanked_out_of_error_text() {
        let key = "sk-0123456789abcdef";
        let text = format!("401: Incorrect API key provided: {key}. Check {key}.");
        let clean = scrub(&text, Some(key));
        assert!(!clean.contains(key));
        assert!(clean.contains("Incorrect API key provided"));
        assert_eq!(scrub("nothing here", None), "nothing here");
    }

    #[test]
    fn a_key_travels_in_the_clear_only_to_this_computer() {
        assert!(key_may_go_to("https://openrouter.ai/api/v1"));
        assert!(key_may_go_to("HTTPS://api.openai.com/v1"));
        assert!(key_may_go_to("http://localhost:1234/v1"));
        assert!(key_may_go_to("http://127.0.0.1:11434/v1"));
        assert!(key_may_go_to("http://[::1]:8080/v1"));
        assert!(!key_may_go_to("http://192.168.1.20:8000/v1"));
        assert!(!key_may_go_to("http://example.com/v1"));
        // A host that only starts like localhost is some other host.
        assert!(!key_may_go_to("http://localhost.evil.example/v1"));
        assert!(!key_may_go_to("http://localhost@evil.example/v1"));
        assert!(!key_may_go_to("ftp://localhost/"));
        assert!(!key_may_go_to("openrouter.ai/api/v1"));
    }
}
