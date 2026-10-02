//! AI routing: which provider + model powers each part of the app.
//!
//! Settings offers two modes. **Same for everything** (the default) picks one
//! provider/model for every feature; **Per feature** gives each feature its own
//! provider, model and fallback. Choices aren't secret, so they live in
//! `%LOCALAPPDATA%\InterPrep\ai_routing.json` (atomic write, like jobs.json);
//! the API keys stay in Windows Credential Manager.
//!
//! `llm_for(feature)` builds the `llm` payload for every backend request. Rust
//! knows which feature each command serves, so the webview never handles API
//! keys for AI calls — it just triggers the command. Rules:
//!   * a provider must be *connected* (has a key) and *able to run* the feature
//!     (Anthropic has no embeddings API); otherwise the first provider that is
//!     stands in, and `substituted_from` lets the UI badge it;
//!   * a model id only makes sense for its own provider, so a stand-in uses
//!     "Auto" (the backend registry's pick for the feature's tier);
//!   * fallback "" = auto (the first other connected provider), "none" = off.
//!   * "chatgpt" is OpenAI through the user's ChatGPT plan (Sign in with
//!     ChatGPT, chatgpt_auth.rs) — connected when signed in, and limited to
//!     Responses API features: no company research (Chat Completions engine)
//!     and no embeddings.
//!   * embeddings resolve to "local" (the sidecar's on-device model) unless
//!     the per-feature Embeddings row names a provider.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::chatgpt_auth;
use crate::credentials::Credentials;

/// Every routable feature, in the order Settings lists them.
pub const FEATURES: &[&str] = &[
    "copilot",
    "coach",
    "interview",
    "resume_tailor",
    "cheatsheet",
    "role_fit",
    "company_research",
    "extension",
    "embeddings",
];

/// Provider preference order for stand-ins and auto fallbacks.
const PROVIDERS: &[&str] = &["gemini", "openai", "chatgpt", "anthropic"];

const FILE_NAME: &str = "ai_routing.json";

/// One routing choice. Empty `provider` = inherit (per-feature row → the
/// "same" choice → the legacy Settings toggle). Empty `model` = Auto.
/// `fallback`: "" = auto, "none" = no fallback, else a provider id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Route {
    pub provider: String,
    pub model: String,
    pub fallback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiRouting {
    /// "same" | "per_feature".
    pub mode: String,
    pub same: Route,
    pub features: BTreeMap<String, Route>,
}

impl Default for AiRouting {
    fn default() -> Self {
        Self { mode: "same".into(), same: Route::default(), features: BTreeMap::new() }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("InterPrep").join(FILE_NAME)
}

/// The saved routing, or the default ("same for everything", inheriting the
/// legacy provider toggle) on first run / unreadable file.
pub fn load() -> AiRouting {
    match fs::read(path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!("ai_routing: parse failed: {e}");
            AiRouting::default()
        }),
        Err(_) => AiRouting::default(),
    }
}

pub fn save(routing: &AiRouting) -> Result<(), String> {
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(routing).map_err(|e| format!("serialize failed: {e}"))?;
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("create {} failed: {e}", tmp.display()))?;
        f.write_all(&json).map_err(|e| format!("write {} failed: {e}", tmp.display()))?;
        f.sync_all().map_err(|e| format!("sync {} failed: {e}", tmp.display()))?;
    }
    fs::rename(&tmp, &path).map_err(|e| format!("rename failed: {e}"))
}

fn key_of<'a>(creds: &'a Credentials, provider: &str) -> &'a str {
    match provider {
        "gemini" => &creds.gemini_api_key,
        "openai" => &creds.openai_api_key,
        "anthropic" => &creds.anthropic_api_key,
        _ => "",
    }
}

/// What's connected: provider keys + whether Sign in with ChatGPT is active.
pub struct Conn<'a> {
    pub creds: &'a Credentials,
    pub chatgpt: bool,
}

impl Conn<'_> {
    fn connected(&self, provider: &str) -> bool {
        if provider == "chatgpt" {
            self.chatgpt
        } else {
            !key_of(self.creds, provider).trim().is_empty()
        }
    }
}

/// Can `provider` run `feature` at all?
fn supports(provider: &str, feature: &str) -> bool {
    match provider {
        "anthropic" => feature != "embeddings",
        // Plan usage covers Responses API calls only: the research engine
        // speaks Chat Completions, and embeddings aren't covered.
        "chatgpt" => feature != "embeddings" && feature != "company_research",
        _ => true,
    }
}

/// The provider/model/fallback a feature resolves to, before keys are added.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolved {
    pub provider: String,
    pub model: String,
    pub fallbacks: Vec<String>,
    /// The chosen provider when `provider` is a stand-in; "" otherwise.
    pub substituted_from: String,
}

/// Embeddings run on-device (backend/local_embed.py: no key, no API call)
/// unless the per-feature Embeddings row names a provider.
fn embeddings_on_device(routing: &AiRouting) -> bool {
    routing.mode != "per_feature"
        || routing.features.get("embeddings").map_or(true, |r| r.provider.trim().is_empty())
}

pub fn resolve(routing: &AiRouting, conn: &Conn, feature: &str) -> Resolved {
    if feature == "embeddings" && embeddings_on_device(routing) {
        return Resolved {
            provider: "local".into(),
            model: "bge-small-en-v1.5".into(),
            fallbacks: vec![],
            substituted_from: String::new(),
        };
    }
    let creds = conn.creds;
    let row = if routing.mode == "per_feature" {
        routing.features.get(feature).cloned().unwrap_or_default()
    } else {
        Route::default()
    };
    // Per-feature rows with no provider inherit the "same" choice wholesale.
    let base = if row.provider.trim().is_empty() { routing.same.clone() } else { row };
    let chosen = [base.provider.trim(), creds.llm_provider.trim(), "gemini"]
        .into_iter()
        .find(|p| !p.is_empty())
        .unwrap_or("gemini")
        .to_lowercase();

    let usable = |p: &str| conn.connected(p) && supports(p, feature);
    let (provider, substituted_from) = if usable(&chosen) {
        (chosen.clone(), String::new())
    } else {
        match PROVIDERS.iter().find(|p| usable(p)) {
            Some(p) => (p.to_string(), chosen.clone()),
            // Nothing usable: keep the choice so the backend reports the
            // missing key against what the user actually picked.
            None => (chosen.clone(), String::new()),
        }
    };
    let model = if provider == chosen { base.model.trim().to_string() } else { String::new() };

    let fallbacks = match base.fallback.trim() {
        "none" => vec![],
        "" => PROVIDERS
            .iter()
            .filter(|p| **p != provider && usable(p))
            .take(1)
            .map(|p| p.to_string())
            .collect(),
        fb => {
            let fb = fb.to_lowercase();
            if fb != provider && usable(&fb) { vec![fb] } else { vec![] }
        }
    };
    Resolved { provider, model, fallbacks, substituted_from }
}

/// The `llm` payload for one backend request serving `feature` (snake_case to
/// match the Python `LLMConfig`). Carries every key so fallbacks, company
/// research's extra lanes and Gemini search grounding keep working.
pub fn llm_for(feature: &str) -> Value {
    let creds = Credentials::load();
    let routing = load();
    let conn = Conn { creds: &creds, chatgpt: chatgpt_auth::is_signed_in() };
    let r = resolve(&routing, &conn, feature);
    let embeddings = resolve(&routing, &conn, "embeddings");
    // Only mint/refresh the plan token when this request can actually use it.
    let needs_plan = r.provider == "chatgpt" || r.fallbacks.iter().any(|p| p == "chatgpt");
    let token = if needs_plan { chatgpt_auth::access_token().unwrap_or_default() } else { String::new() };
    payload(&r, &creds, feature, &embeddings.provider, &token)
}

/// Like `llm_for`, but always carries the plan token when signed in — for the
/// Settings model list / speed test, which may target "chatgpt" directly.
pub fn llm_with_plan_token(feature: &str) -> Value {
    let mut v = llm_for(feature);
    if v["chatgpt_access_token"].as_str().unwrap_or("").is_empty() {
        if let Some(t) = chatgpt_auth::access_token() {
            v["chatgpt_access_token"] = Value::String(t);
        }
    }
    v
}

fn payload(r: &Resolved, creds: &Credentials, feature: &str, embeddings_provider: &str, token: &str) -> Value {
    json!({
        "provider":          r.provider,
        "model":             r.model,
        "gemini_api_key":    creds.gemini_api_key,
        "openai_api_key":    creds.openai_api_key,
        "anthropic_api_key": creds.anthropic_api_key,
        "feature":           feature,
        "fallbacks":         r.fallbacks.iter().map(|p| json!({ "provider": p, "model": "" })).collect::<Vec<_>>(),
        "substituted_from":  r.substituted_from,
        "embeddings_provider": embeddings_provider,
        "chatgpt_access_token": token,
    })
}

/// What every feature resolves to right now — for the Settings card, so it can
/// show "Auto" picks and stand-ins without the webview re-implementing rules.
pub fn resolve_all(routing: &AiRouting, conn: &Conn) -> BTreeMap<String, Resolved> {
    FEATURES.iter().map(|f| (f.to_string(), resolve(routing, conn, f))).collect()
}

/// Feature served by a chat stream, from its mode.
pub fn chat_feature(mode: &str) -> &'static str {
    match mode {
        "interviewer" => "interview",
        "copilot" => "copilot",
        _ => "coach",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(g: &str, o: &str, a: &str) -> Credentials {
        Credentials {
            llm_provider: String::new(),
            gemini_api_key: g.into(),
            openai_api_key: o.into(),
            anthropic_api_key: a.into(),
        }
    }

    fn conn(c: &Credentials) -> Conn<'_> {
        Conn { creds: c, chatgpt: false }
    }

    #[test]
    fn default_is_same_with_auto_fallback() {
        let c = creds("g", "o", "");
        let r = resolve(&AiRouting::default(), &conn(&c), "copilot");
        assert_eq!(r.provider, "gemini");
        assert_eq!(r.fallbacks, vec!["openai".to_string()]);
        assert!(r.substituted_from.is_empty());
    }

    #[test]
    fn unconnected_choice_gets_a_stand_in_with_auto_model() {
        let mut routing = AiRouting::default();
        routing.same = Route { provider: "anthropic".into(), model: "claude-x".into(), fallback: "none".into() };
        let c = creds("g", "", "");
        let r = resolve(&routing, &conn(&c), "coach");
        assert_eq!(r.provider, "gemini");
        assert_eq!(r.model, "");
        assert_eq!(r.substituted_from, "anthropic");
        assert!(r.fallbacks.is_empty());
    }

    #[test]
    fn embeddings_run_on_device_unless_a_row_picks_a_provider() {
        let mut routing = AiRouting::default();
        routing.same.provider = "anthropic".into();
        let c = creds("g", "", "a");
        assert_eq!(resolve(&routing, &conn(&c), "coach").provider, "anthropic");
        assert_eq!(resolve(&routing, &conn(&c), "embeddings").provider, "local");
        // An explicit row is honored — and Anthropic, which can't embed, gets a stand-in.
        routing.mode = "per_feature".into();
        routing.features.insert("embeddings".into(), Route { provider: "anthropic".into(), ..Default::default() });
        let e = resolve(&routing, &conn(&c), "embeddings");
        assert_eq!((e.provider.as_str(), e.substituted_from.as_str()), ("gemini", "anthropic"));
    }

    #[test]
    fn per_feature_row_overrides_and_empty_row_inherits() {
        let mut routing = AiRouting { mode: "per_feature".into(), ..Default::default() };
        routing.same = Route { provider: "openai".into(), ..Default::default() };
        routing.features.insert(
            "copilot".into(),
            Route { provider: "gemini".into(), model: "gemini-3.5-flash-lite".into(), fallback: "openai".into() },
        );
        let c = creds("g", "o", "");
        let cop = resolve(&routing, &conn(&c), "copilot");
        assert_eq!((cop.provider.as_str(), cop.model.as_str()), ("gemini", "gemini-3.5-flash-lite"));
        assert_eq!(cop.fallbacks, vec!["openai".to_string()]);
        assert_eq!(resolve(&routing, &conn(&c), "coach").provider, "openai");
    }

    #[test]
    fn chatgpt_plan_runs_chat_but_not_research_or_embeddings() {
        let mut routing = AiRouting::default();
        routing.same.provider = "chatgpt".into();
        let c = creds("g", "", "");
        let signed_in = Conn { creds: &c, chatgpt: true };
        let coach = resolve(&routing, &signed_in, "coach");
        assert_eq!(coach.provider, "chatgpt");
        assert_eq!(coach.fallbacks, vec!["gemini".to_string()]);
        let research = resolve(&routing, &signed_in, "company_research");
        assert_eq!((research.provider.as_str(), research.substituted_from.as_str()), ("gemini", "chatgpt"));
        // Signed out: Gemini stands in everywhere.
        let out = resolve(&routing, &conn(&c), "coach");
        assert_eq!((out.provider.as_str(), out.substituted_from.as_str()), ("gemini", "chatgpt"));
    }
}
