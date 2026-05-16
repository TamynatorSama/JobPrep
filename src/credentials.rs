//! Secure persistent credential storage.
//!
//! Credentials (API keys, scraper logins) are stored in the OS keychain via the
//! `keyring` crate. On Windows this maps to the Credential Manager, which
//! encrypts entries with DPAPI tied to the current Windows user account.
//!
//! Each field is stored as its own keyring entry under the shared service name
//! `"InterPrep"`. Missing entries are treated as empty strings — there is no
//! distinction between "never set" and "blank" at the UI level.
//!
//! Failures (keyring unavailable, denied access) are logged to stderr and the
//! operation degrades to a no-op. The app keeps working with in-memory values;
//! the user just has to re-enter the key next launch.

const SERVICE: &str = "InterPrep";

/// All credentials persisted to the OS keychain.
#[derive(Default, Clone, Debug)]
pub struct Credentials {
    pub gemini_api_key:    String,
    pub glassdoor_email:   String,
    pub glassdoor_password:String,
    pub indeed_email:      String,
    pub indeed_password:   String,
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

    /// Persists every field. Empty fields delete the matching keyring entry so
    /// "clear it" works as expected.
    pub fn save(&self) {
        set("gemini_api_key",     &self.gemini_api_key);
        set("glassdoor_email",    &self.glassdoor_email);
        set("glassdoor_password", &self.glassdoor_password);
        set("indeed_email",       &self.indeed_email);
        set("indeed_password",    &self.indeed_password);
    }
}

fn entry(field: &str) -> Option<keyring::Entry> {
    match keyring::Entry::new(SERVICE, field) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("credentials: cannot open keyring entry for {field}: {e}");
            None
        }
    }
}

fn get(field: &str) -> String {
    let Some(entry) = entry(field) else { return String::new(); };
    match entry.get_password() {
        Ok(value)                        => value,
        Err(keyring::Error::NoEntry)     => String::new(),
        Err(e) => {
            eprintln!("credentials: read failed for {field}: {e}");
            String::new()
        }
    }
}

fn set(field: &str, value: &str) {
    let Some(entry) = entry(field) else { return; };
    let result = if value.is_empty() {
        // Delete the entry when the user clears the field.
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e),
        }
    } else {
        entry.set_password(value)
    };
    if let Err(e) = result {
        eprintln!("credentials: write failed for {field}: {e}");
    }
}
