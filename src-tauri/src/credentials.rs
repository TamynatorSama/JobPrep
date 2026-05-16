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

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub gemini_api_key:     String,
    pub glassdoor_email:    String,
    pub glassdoor_password: String,
    pub indeed_email:       String,
    pub indeed_password:    String,
}

impl Credentials {
    pub fn load() -> Self {
        Self {
            gemini_api_key:     get("gemini_api_key"),
            glassdoor_email:    get("glassdoor_email"),
            glassdoor_password: get("glassdoor_password"),
            indeed_email:       get("indeed_email"),
            indeed_password:    get("indeed_password"),
        }
    }

    /// Writes every field. Empty fields delete the corresponding keyring
    /// entry so clearing a field in the UI really removes it from the OS.
    pub fn save(&self) -> Result<(), String> {
        set("gemini_api_key",     &self.gemini_api_key)?;
        set("glassdoor_email",    &self.glassdoor_email)?;
        set("glassdoor_password", &self.glassdoor_password)?;
        set("indeed_email",       &self.indeed_email)?;
        set("indeed_password",    &self.indeed_password)?;
        Ok(())
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
