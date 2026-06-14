//! Secure persistent credential storage via Windows Credential Manager.
//!
//! Each field lives as its own keyring entry under the service name
//! `"InterPrep"`. The Credential Manager encrypts entries with DPAPI bound
//! to the current Windows user account, so other users on the same machine
//! can't read them.
//!
//! Missing entries are treated as empty strings — there's no distinction
//! between "never set" and "blank" at the UI layer. Save failures bubble up
//! as `Err(String)` so Tauri commands can surface them to the frontend.

use serde::{Deserialize, Serialize};

const SERVICE: &str = "InterPrep";

/// Retired keyring entries, deleted on every save so they don't linger in the
/// OS keychain after the upgrade. Glassdoor/Indeed: pre-multi-LLM browser-
/// scraping logins. Groq/Ollama: dropped when the provider set was narrowed
/// back to Gemini/OpenAI/Anthropic.
const LEGACY_FIELDS: &[&str] = &[
    "glassdoor_email",
    "glassdoor_password",
    "indeed_email",
    "indeed_password",
    "groq_api_key",
    "ollama_base_url",
    "ollama_model",
];

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    /// Which provider powers LLM calls: "gemini" | "openai" | "anthropic".
    /// Empty means gemini (back-compat default).
    pub llm_provider:      String,
    pub gemini_api_key:    String,
    pub openai_api_key:    String,
    pub anthropic_api_key: String,
}

impl Credentials {
    pub fn load() -> Self {
        Self {
            llm_provider:      get("llm_provider"),
            gemini_api_key:    get("gemini_api_key"),
            openai_api_key:    get("openai_api_key"),
            anthropic_api_key: get("anthropic_api_key"),
        }
    }

    /// Writes every field. Empty fields delete the corresponding keyring
    /// entry so clearing a field in the UI really removes it from the OS.
    pub fn save(&self) -> Result<(), String> {
        set("llm_provider",      &self.llm_provider)?;
        set("gemini_api_key",    &self.gemini_api_key)?;
        set("openai_api_key",    &self.openai_api_key)?;
        set("anthropic_api_key", &self.anthropic_api_key)?;
        // Best-effort cleanup of retired entries.
        for field in LEGACY_FIELDS {
            let _ = set(field, "");
        }
        Ok(())
    }

    /// The `llm` payload attached to every backend request (snake_case to
    /// match the Python `LLMConfig` pydantic model).
    pub fn llm_json(&self) -> serde_json::Value {
        let provider = if self.llm_provider.trim().is_empty() {
            "gemini"
        } else {
            self.llm_provider.trim()
        };
        serde_json::json!({
            "provider":          provider,
            "gemini_api_key":    self.gemini_api_key,
            "openai_api_key":    self.openai_api_key,
            "anthropic_api_key": self.anthropic_api_key,
        })
    }
}

fn entry(field: &str) -> Option<keyring::Entry> {
    match keyring::Entry::new(SERVICE, field) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("credentials: cannot open entry for {field}: {e}");
            None
        }
    }
}

fn get(field: &str) -> String {
    let Some(entry) = entry(field) else { return String::new(); };
    match entry.get_password() {
        Ok(value) => value,
        Err(keyring::Error::NoEntry) => String::new(),
        Err(e) => {
            eprintln!("credentials: read failed for {field}: {e}");
            String::new()
        }
    }
}

fn set(field: &str, value: &str) -> Result<(), String> {
    let Some(entry) = entry(field) else {
        return Err(format!("cannot open keyring entry for {field}"));
    };
    if value.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("delete {field} failed: {e}")),
        }
    } else {
        entry
            .set_password(value)
            .map_err(|e| format!("write {field} failed: {e}"))
    }
}
