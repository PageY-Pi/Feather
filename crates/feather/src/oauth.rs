//! OAuth login (pi parity): browser PKCE flow + the `auth` document in `~/.feather/config.redb`.
//!
//! pi stores OAuth accounts as
//! `{"openai-codex": {"type":"oauth","access":...,"refresh":...,"expires":...,
//!  "accountId":...}}`; feather keeps the same shape in FEATHER_HOME so the
//! token store stays tool-agnostic. A legacy `auth.json` is imported once (initial migration).
//!
//! The login flow opens the system browser, listens on a localhost callback
//! port, exchanges the authorization code with PKCE, and persists the tokens.
//! Pure-logic pieces (PKCE math, callback-URL parsing, store IO) are tested.

use feather_core::provider::transport;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};

// ---------------------------------------------------------------------------
// Provider definitions
// ---------------------------------------------------------------------------

/// Login flow shape (mirrors pi's auth/oauth/* implementations).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flow {
    /// browser + localhost callback + code exchange (PKCE)
    Pkce,
    /// RFC 8628 device authorization grant
    Device,
    /// OpenRouter: PKCE-ish code -> permanent API key
    OpenRouter,
    /// Radius gateway: PKCE with a fixed callback port, discovered authorize endpoint
    Radius,
    /// user pastes an API key (pi's `/login zai` semantics: stored in the
    /// `auth` document of `config.redb`)
    ApiKey,
}

/// One OAuth-capable provider (values mirror pi's provider implementations).
pub struct OAuthProvider {
    /// key in the `auth` document of `config.redb`, e.g. "openai-codex"
    pub id: &'static str,
    /// login flow to run
    pub kind: Flow,
    /// PKCE/Radius: authorize endpoint; Device: device-code endpoint
    pub authorize_url: &'static str,
    /// PKCE/Radius/Device: token endpoint
    pub token_url: &'static str,
    pub client_id: &'static str,
    /// localhost port the callback server binds (PKCE/Radius)
    pub redirect_port: u16,
    /// path the provider appends the code to
    pub redirect_path: &'static str,
    pub scopes: &'static str,
}

pub const OPENAI_CODEX: OAuthProvider = OAuthProvider {
    id: "openai-codex",
    kind: Flow::Pkce,
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    // Codex CLI's public client id
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    redirect_port: 1455,
    redirect_path: "/auth/callback",
    scopes: "openid profile email offline_access",
};

pub const ANTHROPIC: OAuthProvider = OAuthProvider {
    id: "anthropic",
    kind: Flow::Pkce,
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://platform.claude.com/v1/oauth/token",
    // Claude Code's public client id
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    redirect_port: 53692,
    redirect_path: "/callback",
    scopes: "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
};

pub const GITHUB_COPILOT: OAuthProvider = OAuthProvider {
    id: "github-copilot",
    kind: Flow::Device,
    authorize_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    // Copilot Chat's public client id
    client_id: "Iv1.b507a08c87ecfe98",
    redirect_port: 0,
    redirect_path: "",
    scopes: "read:user",
};

pub const XAI: OAuthProvider = OAuthProvider {
    id: "xai",
    kind: Flow::Device,
    authorize_url: "https://auth.x.ai/oauth2/device/code",
    token_url: "https://auth.x.ai/oauth2/token",
    client_id: "b1a00492-073a-47ea-816f-4c329264a828",
    redirect_port: 0,
    redirect_path: "",
    scopes: "openid profile email offline_access grok-cli:access api:access",
};

pub const KIMI_CODING: OAuthProvider = OAuthProvider {
    id: "kimi-coding",
    kind: Flow::Device,
    authorize_url: "https://auth.kimi.com/api/oauth/device_authorization",
    token_url: "https://auth.kimi.com/api/oauth/token",
    client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
    redirect_port: 0,
    redirect_path: "",
    scopes: "",
};

pub const META: OAuthProvider = OAuthProvider {
    id: "meta",
    kind: Flow::Device,
    authorize_url: "https://auth.meta.com/oidc/device/authorization/",
    token_url: "https://auth.meta.com/oidc/device/token/",
    client_id: "1031625952748946",
    redirect_port: 0,
    redirect_path: "",
    scopes: "",
};

pub const OPENROUTER: OAuthProvider = OAuthProvider {
    id: "openrouter",
    kind: Flow::OpenRouter,
    authorize_url: "https://openrouter.ai/auth",
    // OpenRouter exchanges the code for a permanent user-controlled API key
    token_url: "https://openrouter.ai/api/v1/auth/keys",
    client_id: "",
    redirect_port: 0,
    redirect_path: "/oauth/callback",
    scopes: "",
};

pub const RADIUS: OAuthProvider = OAuthProvider {
    id: "radius",
    kind: Flow::Radius,
    authorize_url: "https://radius.pi.dev/v1/oauth",
    token_url: "https://radius.pi.dev/v1/oauth/token",
    client_id: "pi-gateway",
    redirect_port: 1456,
    redirect_path: "/oauth/callback",
    scopes: "gateway offline_access",
};

pub const ZAI: OAuthProvider = OAuthProvider {
    id: "zai",
    kind: Flow::ApiKey,
    authorize_url: "https://z.ai/manage-apikey/apikey-list",
    token_url: "",
    client_id: "",
    redirect_port: 0,
    redirect_path: "",
    scopes: "",
};

pub const ZAI_CODING_CN: OAuthProvider = OAuthProvider {
    id: "zai-coding-cn",
    kind: Flow::ApiKey,
    authorize_url: "https://open.bigmodel.cn/usercenter/apikeys",
    token_url: "",
    client_id: "",
    redirect_port: 0,
    redirect_path: "",
    scopes: "",
};

/// Volcengine Ark Coding Plan uses an API key and OpenAI-compatible Chat Completions.
pub const VOLCENGINE_PLAN: OAuthProvider = OAuthProvider {
    id: "volcengine-plan",
    kind: Flow::ApiKey,
    authorize_url: "https://ark.volcengine.com/region:cn-beijing/apikey",
    token_url: "",
    client_id: "",
    redirect_port: 0,
    redirect_path: "",
    scopes: "",
};

pub const PROVIDERS: &[&OAuthProvider] = &[
    &OPENAI_CODEX,
    &ANTHROPIC,
    &GITHUB_COPILOT,
    &XAI,
    &KIMI_CODING,
    &META,
    &OPENROUTER,
    &RADIUS,
    &ZAI,
    &VOLCENGINE_PLAN,
    &ZAI_CODING_CN,
];

pub fn provider(id: &str) -> Option<&'static OAuthProvider> {
    PROVIDERS.iter().copied().find(|p| p.id == id)
}

// ---------------------------------------------------------------------------
// Credential store: the `auth` document of `config.redb` (pi-compatible shape)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAccount {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh: String,
    /// Epoch millis, like pi. Absent for API-key entries.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub expires: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Preserve provider-specific metadata when another account is updated.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

pub const REFRESH_SKEW_MS: u64 = 300_000;

impl OAuthAccount {
    pub fn is_live(&self) -> bool {
        self.credential().is_some()
            && (self.kind == "api_key"
                || (self.kind == "oauth" && self.expires > now_millis() + 10_000))
    }

    pub fn needs_refresh_at(&self, now: u64) -> bool {
        self.kind == "oauth" && self.expires <= now.saturating_add(REFRESH_SKEW_MS)
    }

    pub fn credential(&self) -> Option<&str> {
        match self.kind.as_str() {
            "api_key" => self.key.as_deref().filter(|k| !k.trim().is_empty()),
            "oauth" => Some(self.access.as_str()).filter(|k| !k.is_empty()),
            _ => None,
        }
    }

    /// Exactly pi's {"type":"api_key","key":"..."} shape.
    pub fn api_key(key: impl Into<String>) -> Self {
        Self {
            kind: "api_key".into(),
            key: Some(key.into()),
            ..Default::default()
        }
    }
}

/// Flat provider map; no envelope or version marker, matching pi's auth shape.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthStore(pub BTreeMap<String, OAuthAccount>);

/// Home directory backing a store path. The real path is `<home>/config.redb`;
/// a legacy/test `*.json` path is interpreted as `<parent> = home` so the one
/// initial JSON import runs against the right home. The JSON file is read at
/// most once (by config_store migration) and never again.
fn store_home(path: &std::path::Path) -> &std::path::Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
}

impl AuthStore {
    pub fn path() -> std::path::PathBuf {
        crate::config::feather_home().join("config.redb")
    }

    /// Best-effort status display only. Runtime and writes use the strict reader.
    pub fn load() -> Self {
        Self::load_from(&Self::path()).unwrap_or_default()
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        let home = store_home(path);
        match crate::config_store::read(home, "auth") {
            Ok(Some(value)) => serde_json::from_value(value).map_err(|e| {
                // serde's detailed error may contain a token value: never display it.
                format!(
                    "auth store invalid credential format at line {}, column {}; file was not changed",
                    e.line(),
                    e.column()
                )
            }),
            Ok(None) => Ok(Self::default()),
            Err(e) => Err(format!("auth store read: {e}")),
        }
    }

    /// Cross-process read/modify/write lock. Refreshes and interactive writes share
    /// a dedicated `auth.lock` so token rotation cannot race login/logout/subagents.
    /// That lock is held across `f` (a refresh may perform blocking HTTP), while
    /// config_store takes `config.lock` only briefly for its own read/write, so the
    /// HTTP round-trip never blocks the shared config lock. Call from a worker thread.
    pub fn transact_at<T>(
        path: &std::path::Path,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let home = store_home(path);
        std::fs::create_dir_all(home).map_err(|e| format!("auth directory: {e}"))?;
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(home.join("auth.lock"))
            .map_err(|e| format!("auth lock: {e}"))?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|e| format!("auth lock: {e}"))?;
        // Held under auth.lock only; each config_store call locks config.lock on its own.
        let mut store = Self::load_from(path)?;
        let before = store.clone();
        let result = f(&mut store)?;
        if store != before {
            store.save_to(path)?;
        }
        // Closing the lock handle releases it even on an error or panic.
        Ok(result)
    }

    pub fn update(provider: &str, account: Option<OAuthAccount>) -> Result<(), String> {
        Self::transact_at(&Self::path(), |store| {
            match account {
                Some(account) => store.set(provider, account),
                None => store.remove(provider),
            }
            Ok(())
        })
    }

    fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let home = store_home(path);
        let value = serde_json::to_value(self).map_err(|_| "auth store encode failed")?;
        crate::config_store::write(home, "auth", &value)
            .map_err(|e| format!("auth store save: {e}"))
    }

    pub fn get(&self, provider: &str) -> Option<&OAuthAccount> {
        self.0.get(provider)
    }
    pub fn set(&mut self, provider: &str, account: OAuthAccount) {
        self.0.insert(provider.into(), account);
    }
    pub fn remove(&mut self, provider: &str) {
        self.0.remove(provider);
    }
    pub fn valid(&self, provider: &str) -> bool {
        self.get(provider).is_some_and(OAuthAccount::is_live)
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// PKCE (RFC 7636) — base64url(SHA256(verifier)), plain base64url verifier
// ---------------------------------------------------------------------------

pub fn random_verifier() -> String {
    // 64 random bytes -> 86 base64url chars (>= 43 required)
    let mut bytes = [0u8; 64];
    getrandom::getrandom(&mut bytes).expect("OS RNG");
    base64url(&bytes)
}

pub fn challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64url(&digest)
}

/// RFC 4648 base64url, no padding.
pub fn base64url(data: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TBL[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TBL[n as usize & 63] as char);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Callback URL parsing
// ---------------------------------------------------------------------------

/// Extract `code` and `state` from `http://localhost:PORT/path?code=..&state=..`.
pub fn parse_callback(request_line: &str, expected_path: &str) -> Result<(String, String), String> {
    // "GET /auth/callback?code=...&state=... HTTP/1.1"
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "malformed request line".to_string())?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != expected_path {
        return Err(format!("unexpected callback path {path:?}"));
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "code" => code = Some(url_decode(v)),
            "state" => state = Some(url_decode(v)),
            "error" => error = Some(url_decode(v)),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Err(format!("provider returned error: {err}"));
    }
    match (code, state) {
        (Some(c), Some(s)) => Ok((c, s)),
        (Some(c), None) if expected_path.starts_with("/oauth/callback/") => Ok((c, String::new())),
        _ => Err("callback missing code or state".to_string()),
    }
}

/// Minimal percent-decoding for query values (UTF-8 safe).
pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// Login flow
// ---------------------------------------------------------------------------

/// Build the authorize URL (PKCE S256).
pub fn authorize_url(p: &OAuthProvider, verifier: &str, state: &str) -> String {
    let redirect = format!("http://localhost:{}{}", p.redirect_port, p.redirect_path);
    if p.id == "anthropic" {
        return format!(
            "{}?code=true&response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            p.authorize_url,
            urlencode(p.client_id),
            urlencode(&redirect),
            urlencode(p.scopes),
            urlencode(verifier),
            urlencode(&challenge(verifier)),
        );
    }
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        p.authorize_url,
        urlencode(p.client_id),
        urlencode(&redirect),
        urlencode(p.scopes),
        urlencode(state),
        urlencode(&challenge(verifier)),
    );
    if p.id == "openai-codex" {
        url.push_str(
            "&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=pi",
        );
    }
    url
}

pub struct LoginOutcome {
    pub account: OAuthAccount,
}

/// What the UI should show while a device flow waits for the browser.
#[derive(Debug, Clone)]
pub struct LoginPrompt {
    pub user_code: String,
    pub url: String,
}

/// UI callback for device codes and browser authorization URLs.
pub type Notify<'a> = &'a dyn Fn(LoginPrompt);

pub fn run_login_with_notify(
    p: &'static OAuthProvider,
    notify: Notify<'_>,
) -> Result<LoginOutcome, String> {
    match p.kind {
        Flow::ApiKey => Err(String::from(
            "api-key providers are entered in the login window",
        )),
        Flow::Pkce => {
            let verifier = random_verifier();
            let state = if p.id == "anthropic" {
                verifier.clone()
            } else {
                random_verifier()
            };
            let listener = bind_callback_listener(p.redirect_port)?;
            let url = authorize_url(p, &verifier, &state);
            notify(LoginPrompt {
                user_code: String::new(),
                url: url.clone(),
            });
            open_browser(&url)?;
            let code = wait_for_callback_listener(listener, p.redirect_path, &state)?;
            exchange_code(p, &code, &verifier)
        }
        Flow::Device => device_login(p, notify),
        Flow::OpenRouter => openrouter_login(p, notify),
        Flow::Radius => radius_login(p, notify),
    }
}

// ---------------------------------------------------------------------------
// RFC 8628 device authorization grant (GitHub Copilot, xAI, Kimi, Meta)
// ---------------------------------------------------------------------------

struct DeviceStart {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval: u64,
    expires_in: u64,
}

const DEVICE_MIN_INTERVAL_MS: u64 = 1000;
const DEVICE_DEFAULT_INTERVAL_S: u64 = 5;
const DEVICE_DEFAULT_EXPIRES_S: u64 = 900;

fn slow_down_interval(current: u64, server_seconds: Option<u64>) -> u64 {
    current
        .saturating_add(5000)
        .max(server_seconds.unwrap_or(0).saturating_mul(1000))
}

fn post_form(
    url: &str,
    fields: &[(&str, &str)],
    extra_headers: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(url).form(fields);
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| format!("{url}: {e}"))?;
    if !status.is_success() {
        return Err(format!("{url}: {status}: {}", oauth_error_message(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url}: invalid JSON: {e}"))
}

fn oauth_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some((err, _)) = json_err(&v) {
            return err;
        }
        for key in ["message", "detail"] {
            if let Some(s) = json_str(&v, key) {
                return s.to_string();
            }
        }
    }
    if body.trim().is_empty() {
        "request failed".into()
    } else {
        "request failed".into()
    }
}

fn post_form_status(
    url: &str,
    fields: &[(&str, &str)],
    extra_headers: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, serde_json::Value), String> {
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.post(url).form(fields);
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| format!("{url}: {e}"))?;
    let value = serde_json::from_str(&body)
        .map_err(|e| format!("{url}: invalid JSON: {e}: {}", oauth_error_message(&body)))?;
    Ok((status, value))
}

fn get_json(url: &str, headers: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| format!("{url}: {e}"))?;
    if !status.is_success() {
        return Err(format!("{url}: {status}: {}", oauth_error_message(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url}: invalid JSON: {e}"))
}

fn get_json_bearer(
    url: &str,
    token: &str,
    headers: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client
        .get(url)
        .header("Authorization", format!("Bearer {token}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| format!("{url}: {e}"))?;
    if !status.is_success() {
        return Err(format!("{url}: {status}: {}", oauth_error_message(&body)));
    }
    serde_json::from_str(&body).map_err(|e| format!("{url}: invalid JSON: {e}"))
}

fn post_json_bearer(
    url: &str,
    token: &str,
    body: serde_json::Value,
    headers: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .json(&body);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req.send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("{url}: {e}"))?;
    if !status.is_success() {
        return Err(format!("{url}: {status}: {}", oauth_error_message(&text)));
    }
    serde_json::from_str(&text).map_err(|e| format!("{url}: invalid JSON: {e}"))
}

fn mint_meta(identity: &str) -> Result<OAuthAccount, String> {
    let minted = post_json_bearer(
        "https://api.meta.ai/muse-code/key",
        identity,
        serde_json::json!({}),
        &[("x-api-version", "1.0.0")],
    )?;
    let key = json_str(&minted, "api_key")
        .ok_or("meta did not issue an API key")?
        .to_string();
    Ok(OAuthAccount {
        kind: String::from("oauth"),
        access: key,
        refresh: identity.to_string(),
        expires: now_millis() + 24 * 3_600_000,
        key: None,
        account_id: None,
        ..Default::default()
    })
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str())
}

pub fn copilot_base_url(account: &OAuthAccount) -> Option<String> {
    account
        .extra
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| copilot_base_url_from_token(&account.access))
}

fn copilot_base_url_from_token(token: &str) -> Option<String> {
    let marker = "proxy-ep=";
    let host = token
        .split(';')
        .find_map(|part| part.strip_prefix(marker))?;
    Some(format!("https://{}", host.replacen("proxy.", "api.", 1)))
}
fn json_err(v: &serde_json::Value) -> Option<(String, Option<u64>)> {
    let err = json_str(v, "error")?;
    let interval = v.get("interval").and_then(|x| x.as_u64());
    let desc = json_str(v, "error_description").unwrap_or("");
    Some((
        if desc.is_empty() {
            err.to_string()
        } else {
            format!("{err}: {desc}")
        },
        interval,
    ))
}

fn device_start(p: &OAuthProvider) -> Result<DeviceStart, String> {
    let mut fields = vec![("client_id", p.client_id)];
    if !p.scopes.is_empty() {
        fields.push(("scope", p.scopes));
    }
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if p.id == "xai" {
        fields.push(("referrer", "pi"));
    }
    if p.id == "github-copilot" {
        headers.push(("Accept", "application/json"));
    }
    let v = post_form(p.authorize_url, &fields, &headers)?;
    let device_code = json_str(&v, "device_code")
        .ok_or("device_code missing")?
        .to_string();
    let user_code = json_str(&v, "user_code")
        .ok_or("user_code missing")?
        .to_string();
    let uri = json_str(&v, "verification_uri")
        .ok_or("verification_uri missing")?
        .to_string();
    if !uri.starts_with("https://") && !uri.starts_with("http://") {
        return Err(format!("untrusted verification_uri: {uri}"));
    }
    let complete = json_str(&v, "verification_uri_complete")
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_string);
    let interval = v
        .get("interval")
        .and_then(|x| x.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(DEVICE_DEFAULT_INTERVAL_S);
    let expires_in = v
        .get("expires_in")
        .and_then(|x| x.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(DEVICE_DEFAULT_EXPIRES_S);
    Ok(DeviceStart {
        device_code,
        user_code,
        verification_uri: uri,
        verification_uri_complete: complete,
        interval: interval * 1000,
        expires_in,
    })
}

/// Poll the token endpoint until the user finishes the browser step.
/// `grant` is the form payload used per poll (client_id/device_code filled here).
fn device_poll(
    p: &OAuthProvider,
    start: &DeviceStart,
    extra_headers: &[(&str, &str)],
    on_pending: &dyn Fn(),
) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
    let mut interval = start.interval.max(DEVICE_MIN_INTERVAL_MS);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("device code expired".into());
        }
        std::thread::sleep(
            std::time::Duration::from_millis(interval)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
        if std::time::Instant::now() >= deadline {
            return Err("device code expired".into());
        }
        on_pending();
        let (status, v) = post_form_status(
            p.token_url,
            &[
                ("client_id", p.client_id),
                ("device_code", start.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
            extra_headers,
        )?;
        if status.is_success() && json_str(&v, "access_token").is_some() {
            return Ok(v);
        }
        if let Some((err, srv_interval)) = json_err(&v) {
            match err.split(':').next().unwrap_or(err.as_str()) {
                "authorization_pending" => {}
                "slow_down" => {
                    interval = slow_down_interval(interval, srv_interval);
                }
                "access_denied" | "authorization_denied" => {
                    return Err("authorization denied".into());
                }
                "expired_token" => return Err("device code expired".into()),
                other => return Err(format!("device flow failed: {other}")),
            }
        } else if !status.is_success() {
            return Err(format!(
                "device token request {status}: {}",
                oauth_error_message(&v.to_string())
            ));
        } else {
            return Err("unexpected device token response".into());
        }
    }
}

fn device_login(p: &'static OAuthProvider, notify: Notify<'_>) -> Result<LoginOutcome, String> {
    let start = device_start(p)?;
    let url = start
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| start.verification_uri.clone());
    open_browser(&url)?;
    notify(LoginPrompt {
        user_code: start.user_code.clone(),
        url,
    });

    let copilot_headers: Vec<(&str, &str)> = if p.id == "github-copilot" {
        vec![
            ("Accept", "application/json"),
            ("User-Agent", "GitHubCopilotChat/0.35.0"),
        ]
    } else {
        Vec::new()
    };

    let v = device_poll(p, &start, &copilot_headers, &|| {})?;

    match p.id {
        // GitHub: device token is a long-lived GH token; swap it for a
        // short-lived Copilot API token that is what the models endpoint wants.
        "github-copilot" => {
            let gh_token = json_str(&v, "access_token")
                .ok_or("github device flow returned no access_token")?
                .to_string();
            let tok = get_json_bearer(
                "https://api.github.com/copilot_internal/v2/token",
                &gh_token,
                &[
                    ("Accept", "application/json"),
                    ("User-Agent", "GitHubCopilotChat/0.35.0"),
                    ("Editor-Version", "vscode/1.107.0"),
                    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
                    ("Copilot-Integration-Id", "vscode-chat"),
                ],
            )?;
            let token = json_str(&tok, "token")
                .ok_or("copilot token missing")?
                .to_string();
            let expires_at = tok
                .get("expires_at")
                .and_then(|x| x.as_f64())
                .ok_or("copilot expires_at missing")? as u64
                * 1000;
            let base_url = tok
                .get("endpoints")
                .and_then(|e| e.get("api"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .or_else(|| copilot_base_url_from_token(&token))
                .unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string());
            let mut extra = BTreeMap::new();
            extra.insert("baseUrl".to_string(), serde_json::Value::String(base_url));
            Ok(LoginOutcome {
                account: OAuthAccount {
                    kind: String::from("oauth"),
                    access: token,
                    refresh: gh_token, // the GH token is the renewable part
                    expires: expires_at,
                    key: None,
                    account_id: None,
                    extra,
                },
            })
        }
        "meta" => {
            // identity token is not renewable; mint a Model API key with it
            let identity = json_str(&v, "access_token")
                .ok_or("meta device flow returned no access_token")?
                .to_string();
            Ok(LoginOutcome {
                account: mint_meta(&identity)?,
            })
        }
        // xAI / Kimi: standard access+refresh pair
        _ => {
            let access = json_str(&v, "access_token")
                .ok_or("device flow returned no access_token")?
                .to_string();
            let refresh = json_str(&v, "refresh_token").unwrap_or("").to_string();
            let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
            Ok(LoginOutcome {
                account: OAuthAccount {
                    kind: String::from("oauth"),
                    access,
                    refresh,
                    expires: now_millis() + expires_in * 1000,
                    key: None,
                    account_id: None,
                    ..Default::default()
                },
            })
        }
    }
}

// ---------------------------------------------------------------------------
// OpenRouter: code -> permanent API key (no refresh)
// ---------------------------------------------------------------------------

fn openrouter_login(p: &'static OAuthProvider, notify: Notify<'_>) -> Result<LoginOutcome, String> {
    let verifier = random_verifier();
    let challenge = challenge(&verifier);
    let cb_path = format!("{}/{}", p.redirect_path, random_verifier());
    let listener = bind_callback_listener(0)?;
    let port = callback_listener_port(&listener)?;
    let state = String::new(); // OpenRouter sends no state; the random path is the guard
    let redirect = format!("http://localhost:{port}{cb_path}");
    let url = format!(
        "{}?callback_url={}&code_challenge={}&code_challenge_method=S256",
        p.authorize_url,
        urlencode(&redirect),
        urlencode(&challenge),
    );
    open_browser(&url)?;
    notify(LoginPrompt {
        user_code: String::new(),
        url,
    });
    let code = wait_for_callback_listener(listener, &cb_path, &state)?;
    let body = serde_json::json!({
        "code": code,
        "code_verifier": verifier,
        "code_challenge_method": "S256",
    });
    let client = transport::blocking_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(p.token_url)
        .json(&body)
        .send()
        .map_err(|e| format!("openrouter exchange: {e}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "openrouter exchange {status}: {}",
            oauth_error_message(&text)
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("openrouter response: {e}"))?;
    let key = json_str(&v, "key")
        .ok_or("openrouter response carries no key")?
        .to_string();
    Ok(LoginOutcome {
        account: OAuthAccount {
            kind: String::from("oauth"),
            access: key,
            refresh: String::new(), // permanent key; refresh is a no-op
            expires: now_millis() + 100 * 365 * 24 * 3_600_000,
            key: None,
            account_id: None,
            ..Default::default()
        },
    })
}

// ---------------------------------------------------------------------------
// Radius: PKCE with a fixed callback port; authorize endpoint discovered
// ---------------------------------------------------------------------------

fn radius_login(p: &OAuthProvider, notify: Notify<'_>) -> Result<LoginOutcome, String> {
    let discovery = get_json(p.authorize_url, &[("accept", "application/json")])?;
    let authorize_endpoint = json_str(&discovery, "authorizationEndpoint")
        .ok_or("radius discovery missing authorizationEndpoint")?
        .to_string();

    let listener = bind_callback_listener(p.redirect_port)?;
    let verifier = random_verifier();
    let state = random_verifier();
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&handoff=url",
        authorize_endpoint,
        urlencode(p.client_id),
        urlencode(&format!("http://127.0.0.1:{}{}", p.redirect_port, p.redirect_path)),
        urlencode(p.scopes),
        urlencode(&state),
        urlencode(&challenge(&verifier)),
    );
    notify(LoginPrompt {
        user_code: String::new(),
        url: url.clone(),
    });
    open_browser(&url)?;
    let code = wait_for_callback_listener(listener, p.redirect_path, &state)?;
    exchange_code(p, &code, &verifier)
}

/// Bind the callback port and wait for ONE request carrying the code,
/// enforcing the `state` anti-CSRF value. Replies with a small styled page.
fn bind_callback_listener(port: u16) -> Result<std::net::TcpListener, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot bind callback port {port}: {e}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("callback listener setup: {e}"))?;
    Ok(listener)
}

fn callback_listener_port(listener: &std::net::TcpListener) -> Result<u16, String> {
    listener
        .local_addr()
        .map(|a| a.port())
        .map_err(|e| e.to_string())
}

fn wait_for_callback_listener(
    listener: std::net::TcpListener,
    path: &str,
    state: &str,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("callback listener setup: {e}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err("callback timed out".into());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(format!("callback accept: {e}")),
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        // read until headers end (browsers send the request in one segment)
        loop {
            let n = stream
                .read(&mut tmp)
                .map_err(|e| format!("callback read: {e}"))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
                break;
            }
        }
        let first_line = String::from_utf8_lossy(&buf);
        let parsed = parse_callback(first_line.lines().next().unwrap_or(""), path);
        let (code, returned_state) = match parsed {
            Ok(v) => v,
            Err(_) => {
                reply(&mut stream, 400, "login failed");
                continue;
            }
        };

        // anti-CSRF: the state must round-trip
        if returned_state != state {
            reply(&mut stream, 400, "state mismatch — start over");
            continue;
        }

        reply(
        &mut stream,
        200,
        "<h2>Feather</h2><p>Callback received — return to Feather to finish sign-in. You can close this tab.</p>",
    );
        return Ok(code);
    }
}

fn reply(stream: &mut std::net::TcpStream, status: u16, body_html: &str) {
    let page = format!(
        "<html><body style=\"font-family:sans-serif;background:#171a21;color:#dfe5ee;\
display:grid;place-items:center;height:100vh\"><div>{}</div></body></html>",
        body_html
    );
    let resp = format!(
        "HTTP/1.1 {} OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        page.len(),
        page
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

/// Exchange the authorization code for tokens (PKCE verifier).
fn exchange_code(p: &OAuthProvider, code: &str, verifier: &str) -> Result<LoginOutcome, String> {
    let redirect_host = if p.id == "radius" {
        "127.0.0.1"
    } else {
        "localhost"
    };
    let redirect = format!(
        "http://{}:{}{}",
        redirect_host, p.redirect_port, p.redirect_path
    );
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", p.client_id),
        ("redirect_uri", redirect.as_str()),
        ("code_verifier", verifier),
    ];
    if p.id == "anthropic" {
        let body = serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": p.client_id,
            "code": code,
            "state": verifier,
            "redirect_uri": redirect,
            "code_verifier": verifier,
        });
        let client = transport::blocking_builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(p.token_url)
            .json(&body)
            .send()
            .map_err(|e| format!("token exchange failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "token exchange {}: {}",
                status,
                oauth_error_message(&text)
            ));
        }
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("token response: {e}"))?;
        return account_from_token_response(p, &v);
    }
    let client = transport::blocking_builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(p.token_url)
        .header("accept", "application/json")
        .form(&form)
        .send()
        .map_err(|e| format!("token exchange failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "token exchange {}: {}",
            status,
            oauth_error_message(&body)
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("token response: {e}"))?;
    account_from_token_response(p, &v)
}

fn account_from_token_response(
    p: &OAuthProvider,
    v: &serde_json::Value,
) -> Result<LoginOutcome, String> {
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("token response missing access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
    let mut extra = BTreeMap::new();
    if p.id == "radius" {
        if let Some(scope) = json_str(v, "scope") {
            extra.insert(
                "scope".to_string(),
                serde_json::Value::String(scope.to_string()),
            );
        }
    }
    let account_id = v
        .get("accountId")
        .or_else(|| v.get("account_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| {
            if p.id == "openai-codex" {
                account_id_from_jwt(&access).or_else(|| {
                    v.get("id_token")
                        .and_then(|x| x.as_str())
                        .and_then(account_id_from_jwt)
                })
            } else {
                None
            }
        });
    if p.id == "openai-codex" && account_id.is_none() {
        return Err("Codex OAuth token response did not contain a valid ChatGPT account id".into());
    }
    Ok(LoginOutcome {
        account: OAuthAccount {
            kind: "oauth".to_string(),
            access,
            refresh,
            expires: now_millis() + expires_in.saturating_mul(1000),
            key: None,
            account_id,
            extra,
            ..Default::default()
        },
    })
}

fn account_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("https://api.openai.com/auth")
        .and_then(|x| x.get("chatgpt_account_id"))
        .or_else(|| v.get("chatgpt_account_id"))
        .or_else(|| v.get("account_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut buf = 0u32;
    let mut bits = 0u8;
    let mut out = Vec::new();
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Renew an account on a worker thread immediately before a provider request.
pub fn refresh(p: &OAuthProvider, account: &OAuthAccount) -> Result<OAuthAccount, String> {
    if account.kind == "api_key" {
        return Ok(account.clone()); // keys never expire
    }
    match p.kind {
        Flow::Pkce => refresh_pkce(p, account),
        Flow::Device => refresh_device(p, account),
        Flow::OpenRouter => Ok(account.clone()), // permanent key
        Flow::Radius => refresh_pkce(p, account),
        Flow::ApiKey => Ok(account.clone()),
    }
}

fn refresh_device(p: &OAuthProvider, account: &OAuthAccount) -> Result<OAuthAccount, String> {
    if p.id == "meta" {
        return mint_meta(&account.refresh);
    }
    if p.id == "github-copilot" {
        // swap the stored GitHub token for a fresh Copilot API token
        let tok = get_json_bearer(
            "https://api.github.com/copilot_internal/v2/token",
            &account.refresh,
            &[
                ("Accept", "application/json"),
                ("User-Agent", "GitHubCopilotChat/0.35.0"),
                ("Editor-Version", "vscode/1.107.0"),
                ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
                ("Copilot-Integration-Id", "vscode-chat"),
            ],
        )?;
        let token = json_str(&tok, "token")
            .ok_or("copilot token missing")?
            .to_string();
        let expires_at = tok
            .get("expires_at")
            .and_then(|x| x.as_f64())
            .ok_or("copilot expires_at missing")? as u64
            * 1000;
        let base_url = tok
            .get("endpoints")
            .and_then(|e| e.get("api"))
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| copilot_base_url_from_token(&token));
        let mut out = account.clone();
        out.access = token;
        out.expires = expires_at;
        if let Some(base_url) = base_url {
            out.extra
                .insert("baseUrl".to_string(), serde_json::Value::String(base_url));
        }
        return Ok(out);
    }
    // xAI / Kimi: refresh_token grant against the token endpoint
    let v = post_form(
        p.token_url,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", account.refresh.as_str()),
            ("client_id", p.client_id),
        ],
        &[],
    )?;
    let access = json_str(&v, "access_token")
        .ok_or("refresh response missing access_token")?
        .to_string();
    let refresh = json_str(&v, "refresh_token")
        .unwrap_or(&account.refresh)
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
    let mut out = account.clone();
    out.access = access;
    out.refresh = refresh;
    out.expires = now_millis() + expires_in.saturating_mul(1000);
    Ok(out)
}

fn refresh_pkce(p: &OAuthProvider, account: &OAuthAccount) -> Result<OAuthAccount, String> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", account.refresh.as_str()),
        ("client_id", p.client_id),
    ];
    if p.id == "anthropic" {
        let body_json = serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": p.client_id,
            "refresh_token": account.refresh,
        });
        let client = transport::blocking_builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client
            .post(p.token_url)
            .json(&body_json)
            .send()
            .map_err(|e| format!("refresh failed: {e}"))?;
        let status = resp.status();
        let body = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "refresh {}: {}",
                status,
                oauth_error_message(&body)
            ));
        }
        let v: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("refresh response: {e}"))?;
        let mut out = account.clone();
        out.access = json_str(&v, "access_token")
            .ok_or("refresh response missing access_token")?
            .to_string();
        if let Some(refresh) = json_str(&v, "refresh_token") {
            out.refresh = refresh.to_string();
        }
        let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
        out.expires = now_millis() + expires_in.saturating_mul(1000);
        return Ok(out);
    }

    let client = transport::blocking_builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(p.token_url)
        .header("accept", "application/json")
        .form(&form)
        .send()
        .map_err(|e| format!("refresh failed: {e}"))?;
    let status = resp.status();
    let body = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "refresh {}: {}",
            status,
            oauth_error_message(&body)
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("refresh response: {e}"))?;
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("refresh response missing access_token")?
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(3600);
    let mut out = account.clone();
    out.access = access;
    if let Some(refresh) = json_str(&v, "refresh_token") {
        out.refresh = refresh.to_string();
    }
    if p.id == "openai-codex" {
        if let Some(new_id) = account_id_from_jwt(&out.access).or_else(|| {
            v.get("id_token")
                .and_then(|x| x.as_str())
                .and_then(account_id_from_jwt)
        }) {
            out.account_id = Some(new_id);
        }
    }
    out.expires = now_millis() + expires_in.saturating_mul(1000);
    Ok(out)
}

/// Launch the default browser without a command shell: OAuth query separators
/// and percent-encoded redirect URIs must reach the browser byte-for-byte.
pub fn open_browser(url: &str) -> Result<(), String> {
    let target = browser_target(url)?;
    #[cfg(windows)]
    {
        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                window: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                params: *const u16,
                dir: *const u16,
                show: i32,
            ) -> isize;
        }
        let action: Vec<u16> = "open\0".encode_utf16().collect();
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                action.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result <= 32 {
            return Err(format!(
                "default browser launch failed (Windows {result}); open the login URL manually"
            ));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        #[cfg(target_os = "macos")]
        let launcher = "open";
        #[cfg(not(target_os = "macos"))]
        let launcher = "xdg-open";
        feather_core::process::command(launcher)
            .arg(url)
            .spawn()
            .map_err(|e| format!("browser launch failed: {e}"))?;
    }
    Ok(())
}
fn browser_target(url: &str) -> Result<Vec<u16>, String> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return Err("browser target must be an HTTP(S) URL without control characters".into());
    }
    Ok(url.encode_utf16().chain(std::iter::once(0)).collect())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_vectors() {
        // RFC 7636 appendix B
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        // base64url has no padding and uses -_
        let v = random_verifier();
        assert!(v.len() >= 43);
        assert!(!v.contains('=') && !v.contains('+') && !v.contains('/'));
        // verifier -> challenge is deterministic
        assert_eq!(challenge(&v), challenge(&v));
    }

    #[test]
    fn base64url_known_values() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8"); // no '+'/'/'
    }

    #[test]
    fn callback_parsing() {
        let (code, state) = parse_callback(
            "GET /auth/callback?code=abc.def&state=xyz%2B1 HTTP/1.1",
            "/auth/callback",
        )
        .unwrap();
        assert_eq!(code, "abc.def");
        assert_eq!(state, "xyz+1");
        // error passthrough
        assert!(parse_callback(
            "GET /auth/callback?error=access_denied&state=s HTTP/1.1",
            "/auth/callback"
        )
        .is_err());
        // wrong path
        assert!(parse_callback("GET /nope?code=a&state=b HTTP/1.1", "/auth/callback").is_err());
        let (code, state) = parse_callback(
            "GET /oauth/callback/random?code=or-code HTTP/1.1",
            "/oauth/callback/random",
        )
        .unwrap();
        assert_eq!(code, "or-code");
        assert_eq!(state, "");
    }

    #[test]
    fn authorize_url_shape() {
        let url = authorize_url(&OPENAI_CODEX, "verifier-123", "state-456");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", challenge("verifier-123"))));
        assert!(url.contains("state=state-456"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("originator=pi"));
    }

    #[test]
    fn codex_authorize_url_matches_pi_required_params() {
        let url = authorize_url(&OPENAI_CODEX, "verifier-123", "state-456");
        for param in [
            "response_type=code",
            "client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "scope=openid%20profile%20email%20offline_access",
            "code_challenge_method=S256",
            "state=state-456",
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            "originator=pi",
        ] {
            assert!(url.contains(param), "missing {param} in {url}");
        }
        assert!(url.contains(&format!("code_challenge={}", challenge("verifier-123"))));
    }

    #[test]
    fn windows_browser_target_preserves_complete_oauth_query() {
        let url = authorize_url(&OPENAI_CODEX, "verifier-123", "state-456");
        let target = browser_target(&url).unwrap();
        assert_eq!(target.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&target[..target.len() - 1]).unwrap(),
            url
        );
        assert!(browser_target("https://example.com/\0oops").is_err());
        assert!(browser_target("file:///tmp/test").is_err());
    }

    #[test]
    fn anthropic_authorize_url_shape() {
        let url = authorize_url(&ANTHROPIC, "verifier-123", "ignored-state");
        assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(url.contains("code=true"));
        assert!(url.contains("state=verifier-123"));
        assert!(url.contains("user%3Asessions%3Aclaude_code"));
        assert!(url.contains("user%3Amcp_servers"));
        assert!(url.contains("user%3Afile_upload"));
    }

    #[test]
    fn codex_jwt_account_id_no_sub_fallback() {
        let sub_only = format!("x.{}.y", base64url(br#"{"sub":"user_123"}"#));
        assert_eq!(account_id_from_jwt(&sub_only), None);
        let with_account = format!("x.{}.y", base64url(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct_456"},"sub":"user_123"}"#));
        assert_eq!(
            account_id_from_jwt(&with_account).as_deref(),
            Some("acct_456")
        );
    }

    #[test]
    fn token_response_requires_codex_account_id() {
        let v =
            serde_json::json!({"access_token":"not.a.jwt","refresh_token":"r","expires_in":3600});
        let err = match account_from_token_response(&OPENAI_CODEX, &v) {
            Ok(_) => panic!("expected codex account id error"),
            Err(e) => e,
        };
        assert!(err.contains("ChatGPT account id"));
    }

    #[test]
    fn device_error_decisions_parse_pending_slow_down() {
        let pending = serde_json::json!({"error":"authorization_pending"});
        let slow = serde_json::json!({"error":"slow_down","interval":7});
        assert_eq!(json_err(&pending).unwrap().0, "authorization_pending");
        assert_eq!(json_err(&slow).unwrap().1, Some(7));
        assert_eq!(slow_down_interval(10_000, Some(7)), 15_000);
        assert_eq!(slow_down_interval(1000, Some(20)), 20_000);
        assert_eq!(slow_down_interval(1000, None), 6000);
    }

    #[test]
    fn copilot_base_url_falls_back_to_token_proxy_ep() {
        let account = OAuthAccount {
            access: "tid=1;proxy-ep=proxy.individual.githubcopilot.com;".into(),
            kind: "oauth".into(),
            ..Default::default()
        };
        assert_eq!(
            copilot_base_url(&account).as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
    }

    #[test]
    fn auth_store_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "feather-auth-{}-{}",
            std::process::id(),
            feather_core::message::now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("auth.json");
        let mut store = AuthStore::default();
        store.set(
            "openai-codex",
            OAuthAccount {
                kind: "oauth".into(),
                access: "eyJhbG.abc".into(),
                refresh: "rt.1.xyz".into(),
                expires: now_millis() + 3_600_000, // 1h ahead: must read as valid
                key: None,
                account_id: Some("8bb868a4dc".into()),
                ..Default::default()
            },
        );
        std::fs::write(&p, serde_json::to_string_pretty(&store).unwrap()).unwrap();
        // pi-shaped: flat map + camelCase accountId
        let txt = std::fs::read_to_string(&p).unwrap();
        assert!(txt.contains("\"type\": \"oauth\""));
        assert!(txt.contains("\"accountId\""));
        let back: AuthStore = serde_json::from_str(&txt).unwrap();
        assert_eq!(back.get("openai-codex").unwrap().access, "eyJhbG.abc");
        assert!(back.valid("openai-codex"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn form_refresh_rotates_tokens_and_preserves_account_metadata() {
        let jwt = format!(
            "x.{}.y",
            base64url(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"new-account"}}"#)
        );
        let body = serde_json::json!({"access_token":jwt,"refresh_token":"rotated-refresh","expires_in":3600}).to_string();
        let server = crate::http_test::serve(vec![(200, "application/json", body)]);
        let endpoint = format!("{}/token", server.url);
        let provider = OAuthProvider {
            token_url: Box::leak(endpoint.into_boxed_str()),
            ..OPENAI_CODEX
        };
        let old = OAuthAccount {
            kind: "oauth".into(),
            access: "old-access".into(),
            refresh: "old-refresh".into(),
            expires: 1,
            account_id: Some("old-account".into()),
            extra: [("custom".into(), serde_json::json!(true))].into(),
            ..Default::default()
        };
        let fresh = refresh(&provider, &old).unwrap();
        assert_eq!(fresh.refresh, "rotated-refresh");
        assert_eq!(fresh.account_id.as_deref(), Some("new-account"));
        assert_eq!(fresh.extra["custom"], true);
        assert!(fresh.expires > now_millis() + 3_000_000);
        let raw = server
            .requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(raw
            .to_ascii_lowercase()
            .contains("content-type: application/x-www-form-urlencoded"));
        assert!(raw.contains("grant_type=refresh_token"));
        assert!(raw.contains("refresh_token=old-refresh"));
        server.finish();
    }

    #[test]
    fn anthropic_refresh_is_json_and_keeps_old_refresh_if_omitted() {
        let server = crate::http_test::serve(vec![(
            200,
            "application/json",
            serde_json::json!({"access_token":"sk-ant-oat-test","expires_in":3600}).to_string(),
        )]);
        let provider = OAuthProvider {
            token_url: Box::leak(format!("{}/token", server.url).into_boxed_str()),
            ..ANTHROPIC
        };
        let old = OAuthAccount {
            kind: "oauth".into(),
            access: "old-access".into(),
            refresh: "old-refresh".into(),
            ..Default::default()
        };
        let fresh = refresh(&provider, &old).unwrap();
        assert_eq!(fresh.refresh, "old-refresh");
        assert_eq!(fresh.access, "sk-ant-oat-test");
        let raw = server
            .requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(headers
            .to_ascii_lowercase()
            .contains("content-type: application/json"));
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], "old-refresh");
        assert_eq!(body["client_id"], ANTHROPIC.client_id);
        server.finish();
    }

    #[test]
    fn device_pending_http_400_is_polled_until_success() {
        let server = crate::http_test::serve(vec![
            (
                400,
                "application/json",
                serde_json::json!({"error":"authorization_pending"}).to_string(),
            ),
            (
                200,
                "application/json",
                serde_json::json!({"access_token":"test-access"}).to_string(),
            ),
        ]);
        let provider = OAuthProvider {
            token_url: Box::leak(format!("{}/token", server.url).into_boxed_str()),
            ..XAI
        };
        let start = DeviceStart {
            device_code: "device".into(),
            user_code: "code".into(),
            verification_uri: "https://example.invalid".into(),
            verification_uri_complete: None,
            interval: 1,
            expires_in: 10,
        };
        let token = device_poll(&provider, &start, &[], &|| {}).unwrap();
        assert_eq!(token["access_token"], "test-access");
        for _ in 0..2 {
            let raw = server
                .requests
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(raw.contains("device_code=device"));
        }
        server.finish();
    }
}
