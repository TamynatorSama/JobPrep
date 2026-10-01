//! Sign in with ChatGPT — use the user's ChatGPT plan (e.g. Plus) for eligible
//! Responses API calls instead of an OpenAI API key.
//!
//! OpenAI's "ChatGPT plan usage for open-source apps" flow (developers.openai.com/
//! siwc/token-sharing-open-source): open-source / locally hosted apps may use it
//! without approval. Public client, OpenID Connect + PKCE (S256):
//!
//!   1. First sign-in registers this install: `client_id=dynamic_agent_client`
//!      + `agent_name_hint`; the callback hands back an issued `oaiapp_…`
//!      client id, saved and reused (without the name hint) from then on.
//!   2. The browser redirects to a one-shot listener on
//!      `http://127.0.0.1:<free port>/auth/callback` (must be 127.0.0.1).
//!   3. The code is exchanged at the token endpoint; we require the
//!      `chatgpt.tokens.use.direct` scope and verify the ID token (RS256 against
//!      OpenAI's JWKS, issuer, audience = issued client id, nonce, expiry).
//!   4. Access tokens live 1h (memory only); refresh tokens live 30 days and
//!      ROTATE on every refresh (the new one is saved each time). A background
//!      thread refreshes ahead of expiry and re-seeds the sidecar.
//!
//! Every request carries `ext_agent_host_id`, a per-install `urn:uuid:` minted
//! once. Refresh token, issued client id, ID token and the account email live in
//! Windows Credential Manager (service "InterPrep") — never in the webview.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::credentials;

const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const REVOKE_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const AGENT_NAME: &str = "InterPrep";
const CALLBACK_PATH: &str = "/auth/callback";
/// How long the browser has to come back with the code.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);
/// Refresh this long before the access token expires.
const REFRESH_MARGIN: Duration = Duration::from_secs(10 * 60);

// Credential Manager fields (service "InterPrep").
const F_REFRESH: &str = "chatgpt_refresh_token";
const F_CLIENT_ID: &str = "chatgpt_client_id";
const F_ID_TOKEN: &str = "chatgpt_id_token";
const F_EMAIL: &str = "chatgpt_email";
const F_HOST_ID: &str = "chatgpt_host_id";

struct Session {
    access_token: String,
    expires_at: Instant,
}

/// The in-memory access token. One lock also single-flights refreshes.
fn session() -> &'static Mutex<Option<Session>> {
    static S: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

// ─── public surface ─────────────────────────────────────────────────────────

/// True when a refresh token is stored (the access token is minted on demand).
pub fn is_signed_in() -> bool {
    !credentials::get(F_REFRESH).trim().is_empty()
}

/// `{ signedIn, email }` for Settings.
pub fn status() -> Value {
    json!({ "signedIn": is_signed_in(), "email": credentials::get(F_EMAIL) })
}

/// A valid access token, refreshing (blocking) when it's missing or close to
/// expiry. None when signed out or the refresh token was rejected.
pub fn access_token() -> Option<String> {
    if !is_signed_in() {
        return None;
    }
    let mut guard = session().lock().unwrap();
    if let Some(s) = guard.as_ref() {
        if s.expires_at > Instant::now() + Duration::from_secs(60) {
            return Some(s.access_token.clone());
        }
    }
    match refresh() {
        Ok(s) => {
            let token = s.access_token.clone();
            *guard = Some(s);
            Some(token)
        }
        Err(e) => {
            eprintln!("[chatgpt] refresh failed: {e}");
            None
        }
    }
}

/// Run the whole browser sign-in. Blocking (up to SIGN_IN_TIMEOUT) — call it
/// from a worker thread / spawn_blocking.
pub fn sign_in() -> Result<Value, String> {
    let host_id = host_id();
    let saved_client = credentials::get(F_CLIENT_ID);
    let registering = saved_client.trim().is_empty();
    let client_id = if registering { DYNAMIC_CLIENT.to_string() } else { saved_client };

    let verifier = b64url(&random_bytes(64));
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = b64url(&random_bytes(24));
    let nonce = b64url(&random_bytes(24));

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot open callback port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    let mut params: Vec<(&str, String)> = vec![
        ("client_id", client_id.clone()),
        ("response_type", "code".into()),
        ("redirect_uri", redirect_uri.clone()),
        ("scope", SCOPE.into()),
        ("resource", RESOURCE.into()),
        ("state", state.clone()),
        ("nonce", nonce.clone()),
        ("code_challenge_method", "S256".into()),
        ("code_challenge", challenge),
        ("ext_agent_host_id", host_id.clone()),
    ];
    if registering {
        params.push(("agent_name_hint", AGENT_NAME.into()));
    } else {
        let id_token = credentials::get(F_ID_TOKEN);
        if !id_token.is_empty() {
            params.push(("id_token_hint", id_token));
        }
    }
    let url = reqwest::Url::parse_with_params(AUTHORIZE_URL, &params).map_err(|e| e.to_string())?;
    opener::open_browser(url.as_str()).map_err(|e| format!("cannot open the browser: {e}"))?;

    let query = wait_for_callback(&listener)?;
    let get = |k: &str| query.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    if get("state").as_deref() != Some(state.as_str()) {
        return Err("Sign-in response didn't match this attempt (state mismatch). Try again.".into());
    }
    if let Some(err) = get("error") {
        return Err(if err == "access_denied" { "Sign-in was cancelled.".into() } else { format!("Sign-in failed: {err}") });
    }
    let code = get("code").ok_or("Sign-in response had no code.")?;
    let client_id = match get("client_id") {
        Some(issued) if !issued.is_empty() => issued,
        _ if registering => return Err("OpenAI didn't issue a client id for this app.".into()),
        _ => client_id,
    };

    let tokens: TokenResponse = token_request(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("client_id", &client_id),
        ("code_verifier", &verifier),
        ("redirect_uri", &redirect_uri),
        ("resource", RESOURCE),
    ])?;
    let granted = tokens.scope.clone().or_else(|| get("scope")).unwrap_or_default();
    if !granted.split_whitespace().any(|s| s == PLAN_SCOPE) {
        return Err("ChatGPT plan usage wasn't granted — approve \"use your ChatGPT plan\" on the sign-in page.".into());
    }
    let id_token = tokens.id_token.clone().ok_or("Sign-in returned no ID token.")?;
    let claims = verify_id_token(&id_token, &client_id, &nonce)?;
    let refresh_token = tokens.refresh_token.clone().ok_or("Sign-in returned no refresh token.")?;

    credentials::set(F_CLIENT_ID, &client_id)?;
    credentials::set(F_REFRESH, &refresh_token)?;
    credentials::set(F_ID_TOKEN, &id_token)?;
    credentials::set(F_EMAIL, claims.email.as_deref().unwrap_or(""))?;
    *session().lock().unwrap() = Some(Session {
        access_token: tokens.access_token,
        expires_at: Instant::now() + Duration::from_secs(tokens.expires_in.unwrap_or(3600)),
    });
    Ok(status())
}

/// Revoke the refresh token at OpenAI (best-effort) and forget the session.
/// The issued client id and host id are kept: OpenAI expects them reused.
pub fn sign_out() -> Result<Value, String> {
    let refresh = credentials::get(F_REFRESH);
    let client_id = credentials::get(F_CLIENT_ID);
    if !refresh.is_empty() {
        let _ = http()
            .post(REVOKE_URL)
            .header("ext_agent_host_id", host_id())
            .form(&[
                ("token", refresh.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", client_id.as_str()),
            ])
            .timeout(Duration::from_secs(10))
            .send();
    }
    for f in [F_REFRESH, F_ID_TOKEN, F_EMAIL] {
        credentials::set(f, "")?;
    }
    *session().lock().unwrap() = None;
    Ok(status())
}

/// Keep the access token fresh in the background so requests never wait on a
/// refresh, and run `on_refresh` after each one (re-seeds the sidecar's
/// extension config, which holds a copy of the token).
pub fn start_refresher<F: Fn() + Send + 'static>(on_refresh: F) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5 * 60));
        if !is_signed_in() {
            continue;
        }
        let due = session()
            .lock()
            .unwrap()
            .as_ref()
            .map_or(true, |s| s.expires_at <= Instant::now() + REFRESH_MARGIN);
        if due {
            // access_token() refreshes when within 60s; force it here by
            // clearing a nearly-expired session first.
            *session().lock().unwrap() = None;
            if access_token().is_some() {
                on_refresh();
            }
        }
    });
}

// ─── internals ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct IdClaims {
    nonce: Option<String>,
    email: Option<String>,
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn token_request(form: &[(&str, &str)]) -> Result<TokenResponse, String> {
    let resp = http()
        .post(TOKEN_URL)
        .header("ext_agent_host_id", host_id())
        .form(form)
        .timeout(Duration::from_secs(20))
        .send()
        .map_err(|e| format!("token request failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        // Error bodies carry error/error_description — never tokens.
        return Err(format!("token endpoint HTTP {status}: {}", body.chars().take(300).collect::<String>()));
    }
    serde_json::from_str(&body).map_err(|e| format!("bad token response: {e}"))
}

/// Exchange the stored refresh token. Refresh tokens rotate: save the new one.
fn refresh() -> Result<Session, String> {
    let refresh = credentials::get(F_REFRESH);
    let client_id = credentials::get(F_CLIENT_ID);
    let tokens = token_request(&[
        ("grant_type", "refresh_token"),
        ("client_id", &client_id),
        ("refresh_token", &refresh),
        ("resource", RESOURCE),
    ])
    .map_err(|e| {
        // A rejected refresh token (expired after 30 idle days, or revoked)
        // can't recover — sign out so Settings shows the button again.
        if e.contains("invalid_grant") {
            let _ = credentials::set(F_REFRESH, "");
        }
        e
    })?;
    if let Some(new_refresh) = tokens.refresh_token.as_deref() {
        credentials::set(F_REFRESH, new_refresh)?;
    }
    if let Some(id_token) = tokens.id_token.as_deref() {
        let _ = credentials::set(F_ID_TOKEN, id_token);
    }
    Ok(Session {
        access_token: tokens.access_token,
        expires_at: Instant::now() + Duration::from_secs(tokens.expires_in.unwrap_or(3600)),
    })
}

/// Verify the ID token: RS256 signature against OpenAI's JWKS, issuer,
/// audience (our issued client id), expiry, and our nonce.
fn verify_id_token(id_token: &str, client_id: &str, nonce: &str) -> Result<IdClaims, String> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
    let header = decode_header(id_token).map_err(|e| format!("bad ID token: {e}"))?;
    let jwks: Value = http()
        .get(JWKS_URL)
        .timeout(Duration::from_secs(10))
        .send()
        .and_then(|r| r.json())
        .map_err(|e| format!("cannot fetch OpenAI signing keys: {e}"))?;
    let key = jwks["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter().find(|k| header.kid.is_none() || k["kid"].as_str() == header.kid.as_deref())
        })
        .ok_or("ID token signed with an unknown key")?;
    let decoding = DecodingKey::from_rsa_components(
        key["n"].as_str().unwrap_or_default(),
        key["e"].as_str().unwrap_or_default(),
    )
    .map_err(|e| format!("bad signing key: {e}"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    let claims = decode::<IdClaims>(id_token, &decoding, &validation)
        .map_err(|e| format!("ID token rejected: {e}"))?
        .claims;
    if claims.nonce.as_deref() != Some(nonce) {
        return Err("ID token nonce mismatch".into());
    }
    Ok(claims)
}

/// Accept connections until the real callback arrives (browsers also open
/// speculative / favicon connections). Returns the callback's query pairs.
fn wait_for_callback(listener: &TcpListener) -> Result<Vec<(String, String)>, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + SIGN_IN_TIMEOUT;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(query) = handle_connection(stream) {
                    return Ok(query);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("callback listener failed: {e}")),
        }
    }
    Err("Sign-in timed out — the browser didn't come back within 5 minutes.".into())
}

fn handle_connection(mut stream: TcpStream) -> Option<Vec<(String, String)>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).ok()?;
    // "GET /auth/callback?code=…&state=… HTTP/1.1"
    let target = line.split_whitespace().nth(1)?;
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != CALLBACK_PATH {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return None;
    }
    let query: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let ok = query.iter().any(|(k, _)| k == "code");
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>InterPrep</title>\
         <body style=\"font-family:system-ui;background:#0c0c0c;color:#fff;display:grid;place-items:center;height:100vh;margin:0\">\
         <p>{}</p></body>",
        if ok { "Signed in to InterPrep with ChatGPT. You can close this tab." }
        else { "Sign-in didn't complete. You can close this tab and try again from InterPrep." }
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    Some(query)
}

/// Per-install host id (`urn:uuid:` v4), minted once and reused forever.
fn host_id() -> String {
    let existing = credentials::get(F_HOST_ID);
    if !existing.is_empty() {
        return existing;
    }
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let id = format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32]
    );
    let _ = credentials::set(F_HOST_ID, &id);
    id
}

fn random_bytes(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    getrandom::getrandom(&mut b).expect("OS RNG unavailable");
    b
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(b64url(&Sha256::digest(verifier.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn callback_query_parses_from_request_line() {
        let url = reqwest::Url::parse("http://127.0.0.1/auth/callback?code=abc&state=xyz&client_id=oaiapp_1").unwrap();
        let q: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(url.path(), CALLBACK_PATH);
        assert!(q.contains(&("client_id".into(), "oaiapp_1".into())));
    }
}
