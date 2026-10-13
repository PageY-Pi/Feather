//! Shared request-time auth.json resolution and automatic token renewal.

use crate::oauth::{self, AuthStore, OAuthAccount};
use feather_core::provider::credentials::{
    CredentialFuture, CredentialResolver, ResolvedCredential,
};
use feather_core::provider::ProviderError;
use std::path::{Path, PathBuf};

pub struct StoredCredentialResolver {
    path: PathBuf,
    provider: String,
    fallback: Option<String>,
    headers: Vec<(String, String)>,
}

impl StoredCredentialResolver {
    pub fn new(provider: &str, fallback: Option<String>) -> Self {
        Self {
            path: AuthStore::path(),
            provider: provider.into(),
            fallback,
            headers: Vec::new(),
        }
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    fn resolve_blocking(&self) -> Result<ResolvedCredential, String> {
        let account = resolve_stored_at(&self.path, &self.provider, |pid, account| {
            let provider = oauth::provider(pid).ok_or_else(|| {
                format!("{pid}: expired OAuth credential cannot be refreshed; sign in again")
            })?;
            oauth::refresh(provider, account).map_err(|e| {
                format!(
                    "{pid}: token refresh failed: {e}; sign in again if the session has expired"
                )
            })
        })?;
        let key = match &account {
            Some(account) if account.kind == "api_key" => resolve_fallback(account.key.as_deref())?,
            Some(account) => account.credential().map(str::to_string),
            None => resolve_fallback(self.fallback.as_deref())?,
        }
        .ok_or_else(|| {
            format!(
                "{}: no credential available; sign in or configure an API key",
                self.provider
            )
        })?;
        Ok(ResolvedCredential {
            api_key: key,
            account_id: account.as_ref().and_then(|a| a.account_id.clone()),
            bearer_auth: self.provider == "github-copilot",
            base_url: account
                .as_ref()
                .filter(|a| a.kind == "oauth" && self.provider == "github-copilot")
                .and_then(oauth::copilot_base_url),
            headers: self.headers.clone(),
        })
    }
}

impl CredentialResolver for StoredCredentialResolver {
    fn resolve(&self) -> CredentialFuture<'_> {
        let resolver = Self {
            path: self.path.clone(),
            provider: self.provider.clone(),
            fallback: self.fallback.clone(),
            headers: self.headers.clone(),
        };
        Box::pin(async move {
            // OAuth uses blocking HTTP/file locks; never run those on egui or a
            // Tokio worker. All agents/subagents share the locked store.
            tokio::task::spawn_blocking(move || resolver.resolve_blocking())
                .await
                .map_err(|_| ProviderError::Auth("credential worker failed".into()))?
                .map_err(ProviderError::Auth)
        })
    }
}

/// Strict lookup. Stored auth wins over environment/catalog credentials even
/// when renewal fails: a failed session must not silently select another key.
fn resolve_stored_at(
    path: &Path,
    pid: &str,
    refresh: impl Fn(&str, &OAuthAccount) -> Result<OAuthAccount, String>,
) -> Result<Option<OAuthAccount>, String> {
    let account = AuthStore::load_from(path)?.get(pid).cloned();
    let Some(account) = account else {
        return Ok(None);
    };
    validate_account(pid, &account)?;
    if account.credential().is_some()
        && !account.needs_refresh_at(feather_core::message::now_millis())
    {
        return Ok(Some(account));
    }
    AuthStore::transact_at(path, |store| {
        // Another agent/process may already have rotated the refresh token.
        let Some(account) = store.get(pid).cloned() else {
            return Ok(None);
        };
        validate_account(pid, &account)?;
        if account.credential().is_some()
            && !account.needs_refresh_at(feather_core::message::now_millis())
        {
            return Ok(Some(account));
        }
        let fresh = refresh(pid, &account)?;
        validate_account(pid, &fresh)?;
        if fresh.kind != "oauth"
            || fresh.credential().is_none()
            || fresh.expires <= feather_core::message::now_millis()
        {
            return Err(format!(
                "{pid}: refresh returned an expired or invalid credential"
            ));
        }
        store.set(pid, fresh.clone());
        Ok(Some(fresh))
    })
}

fn validate_account(pid: &str, account: &OAuthAccount) -> Result<(), String> {
    match account.kind.as_str() {
        "api_key" if account.credential().is_some() => Ok(()),
        "oauth" if !account.access.is_empty() || !account.refresh.is_empty() => Ok(()),
        _ => Err(format!(
            "{pid}: stored credential is invalid; sign in again"
        )),
    }
}

/// Accept pi's $ENV form, legacy bare ENV names, or literal keys. An unset env
/// variable is never itself sent as a key. Command-backed secrets are not run.
pub(crate) fn resolve_fallback(spec: Option<&str>) -> Result<Option<String>, String> {
    let Some(spec) = spec.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if spec.starts_with('!') {
        return Err(
            "command-backed API keys are not supported; use an environment variable or saved key"
                .into(),
        );
    }
    let key = if let Some(name) = spec.strip_prefix('$') {
        std::env::var(name).ok()
    } else if is_env_name(spec) {
        std::env::var(spec).ok()
    } else {
        Some(spec.into())
    };
    Ok(key.filter(|s| !s.trim().is_empty()))
}

fn is_env_name(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Offline availability for model pickers. Expired OAuth access remains usable
/// when a refresh token exists; checking availability never refreshes or does HTTP.
pub fn credential_available(store: &AuthStore, pid: &str, spec: Option<&str>) -> bool {
    if let Some(account) = store.get(pid) {
        return match account.kind.as_str() {
            "api_key" => account.credential().is_some(),
            "oauth" => {
                (!account.access.is_empty() && account.is_live()) || !account.refresh.is_empty()
            }
            _ => false,
        };
    }
    resolve_fallback(spec).ok().flatten().is_some()
}

pub(crate) fn credential_configured(pid: &str, spec: Option<&str>) -> bool {
    // Invalid store errors are surfaced by the next provider request, not hidden
    // by dropping to the offline demo provider.
    match AuthStore::load_from(&AuthStore::path()) {
        Ok(store) if store.get(pid).is_some() => true,
        Err(_) => true,
        _ => resolve_fallback(spec).map(|k| k.is_some()).unwrap_or(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct TempStore(PathBuf);
    impl TempStore {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("feather-auth-{}", oauth::random_verifier())))
        }
        fn path(&self) -> PathBuf {
            self.0.join("auth.json")
        }
        fn put(&self, pid: &str, account: OAuthAccount) {
            AuthStore::transact_at(&self.path(), |s| {
                s.set(pid, account);
                Ok(())
            })
            .unwrap();
        }
    }
    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn expired() -> OAuthAccount {
        OAuthAccount {
            kind: "oauth".into(),
            access: "old-access".into(),
            refresh: "old-refresh".into(),
            expires: 1,
            account_id: Some("account".into()),
            extra: [("scope".into(), serde_json::json!("test"))].into(),
            ..Default::default()
        }
    }

    #[test]
    fn pi_api_key_roundtrip_and_precedence() {
        let tmp = TempStore::new();
        tmp.put("zai", OAuthAccount::api_key("stored-key"));
        let parsed: serde_json::Value = crate::config_store::read(&tmp.0, "auth").unwrap().unwrap();
        assert_eq!(
            parsed["zai"],
            serde_json::json!({"type":"api_key","key":"stored-key"})
        );
        let resolver = StoredCredentialResolver {
            path: tmp.path(),
            provider: "zai".into(),
            fallback: Some("catalog-key".into()),
            headers: vec![],
        };
        assert_eq!(resolver.resolve_blocking().unwrap().api_key, "stored-key");
        tmp.put("zai", OAuthAccount::api_key("replacement-key"));
        assert_eq!(
            resolver.resolve_blocking().unwrap().api_key,
            "replacement-key"
        );
    }

    #[test]
    fn refresh_rotation_persists_metadata_and_unrelated_provider() {
        let tmp = TempStore::new();
        tmp.put("openai-codex", expired());
        tmp.put("zai", OAuthAccount::api_key("zai-key"));
        let fresh = resolve_stored_at(&tmp.path(), "openai-codex", |_, a| {
            let mut fresh = a.clone();
            fresh.access = "new-access".into();
            fresh.refresh = "new-refresh".into();
            fresh.expires = feather_core::message::now_millis() + 3_600_000;
            Ok(fresh)
        })
        .unwrap()
        .unwrap();
        let stored = AuthStore::load_from(&tmp.path()).unwrap();
        assert_eq!(stored.get("openai-codex"), Some(&fresh));
        assert_eq!(fresh.extra["scope"], "test");
        assert_eq!(fresh.account_id.as_deref(), Some("account"));
        assert_eq!(stored.get("zai").unwrap().credential(), Some("zai-key"));
    }

    #[test]
    fn concurrent_agents_refresh_once() {
        let tmp = TempStore::new();
        tmp.put("openai-codex", expired());
        let calls = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = tmp.path();
                let calls = calls.clone();
                std::thread::spawn(move || {
                    resolve_stored_at(&path, "openai-codex", |_, a| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        Ok(OAuthAccount {
                            access: "new-access".into(),
                            refresh: "rotated".into(),
                            expires: feather_core::message::now_millis() + 3_600_000,
                            ..a.clone()
                        })
                    })
                    .unwrap()
                    .unwrap()
                    .access
                })
            })
            .collect();
        for w in workers {
            assert_eq!(w.join().unwrap(), "new-access");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refresh_error_does_not_change_store_or_fall_back() {
        let tmp = TempStore::new();
        tmp.put("openai-codex", expired());
        let before = crate::config_store::read(&tmp.0, "auth").unwrap();
        assert!(
            resolve_stored_at(&tmp.path(), "openai-codex", |_, _| Err("denied".into()))
                .unwrap_err()
                .contains("denied")
        );
        assert_eq!(crate::config_store::read(&tmp.0, "auth").unwrap(), before);
    }

    #[test]
    fn corrupt_store_is_not_overwritten() {
        let tmp = TempStore::new();
        std::fs::create_dir_all(&tmp.0).unwrap();
        std::fs::write(tmp.path(), "invalid-private-value").unwrap();
        let err = AuthStore::transact_at(&tmp.path(), |s| {
            s.set("zai", OAuthAccount::api_key("test"));
            Ok(())
        })
        .unwrap_err();
        assert!(!err.contains("invalid-private-value"));
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "invalid-private-value"
        );
    }

    #[test]
    fn refresh_threshold_and_unset_env() {
        let mut a = expired();
        a.expires = 1_000_000;
        assert!(!a.needs_refresh_at(699_999));
        assert!(a.needs_refresh_at(700_000));
        assert!(!OAuthAccount::api_key("key").needs_refresh_at(u64::MAX));
        assert_eq!(
            resolve_fallback(Some("$FEATHER_TEST_UNSET_AUTH_724FDBE9")).unwrap(),
            None
        );
        assert_eq!(
            resolve_fallback(Some("FEATHER_TEST_UNSET_AUTH_724FDBE9")).unwrap(),
            None
        );
        assert_eq!(
            resolve_fallback(Some("literal-key")).unwrap().as_deref(),
            Some("literal-key")
        );
    }

    fn request() -> feather_core::provider::ChatRequest {
        use feather_core::provider::{CacheHints, ChatRequest, ToolDef};
        ChatRequest {
            system: "Feather test prompt".into(),
            messages: vec![feather_core::message::Message::user_text("hello")].into(),
            tools: vec![ToolDef {
                name: "read".into(),
                description: "Read".into(),
                parameters: serde_json::json!({"type":"object"}),
            }]
            .into(),
            model: "test-model".into(),
            max_tokens: 1024,
            temperature: None,
            thinking: None,
            cache: CacheHints::default(),
            session_id: "test-session".into(),
        }
    }

    #[tokio::test]
    async fn anthropic_login_and_key_change_apply_without_rebuilding() {
        let tmp = TempStore::new();
        let resolver = Arc::new(StoredCredentialResolver {
            path: tmp.path(),
            provider: "anthropic".into(),
            fallback: Some("catalog-key".into()),
            headers: vec![],
        });
        let response = "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n";
        let server = crate::http_test::serve(vec![
            (200, "text/event-stream", response.into()),
            (200, "text/event-stream", response.into()),
        ]);
        let provider = feather_core::provider::anthropic::AnthropicProvider::new(
            "unused",
            Some(format!("{}/v1/", server.url)),
        )
        .with_credentials(resolver);
        // Login AFTER provider construction: the next HTTP call must see it.
        tmp.put(
            "anthropic",
            OAuthAccount {
                kind: "oauth".into(),
                access: "sk-ant-oat-test".into(),
                refresh: "test-refresh".into(),
                expires: feather_core::message::now_millis() + 3_600_000,
                ..Default::default()
            },
        );
        let req = request();
        let mut stream = provider.stream(&req).await.unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let raw = server
            .requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1/messages "));
        let headers = headers.to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer sk-ant-oat-test"));
        assert!(!headers.contains("x-api-key:"));
        assert!(headers.contains("oauth-2025-04-20"));
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("You are Claude Code"));
        assert_eq!(body["system"][1]["text"], "Feather test prompt");
        assert_eq!(body["tools"][0]["name"], "Read");
        tmp.put("anthropic", OAuthAccount::api_key("replacement-key"));
        let mut stream = provider.stream(&req).await.unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let raw = server
            .requests
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(headers
            .to_ascii_lowercase()
            .contains("x-api-key: replacement-key"));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert!(!headers.to_ascii_lowercase().contains("oauth-2025-04-20"));
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["tools"][0]["name"], "read");
        server.finish();
    }

    #[tokio::test]
    async fn openai_compatible_stored_key_and_logout_apply_each_request() {
        let tmp = TempStore::new();
        let resolver = Arc::new(StoredCredentialResolver {
            path: tmp.path(),
            provider: "zai".into(),
            fallback: Some("catalog-key".into()),
            headers: vec![],
        });
        let response =
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n";
        let server = crate::http_test::serve(vec![
            (200, "text/event-stream", response.into()),
            (200, "text/event-stream", response.into()),
        ]);
        let provider =
            feather_core::provider::openai::OpenAiProvider::new("unused", Some(server.url.clone()))
                .with_credentials(resolver);
        tmp.put("zai", OAuthAccount::api_key("stored-key"));
        let req = request();
        for key in ["stored-key", "catalog-key"] {
            let mut stream = provider.stream(&req).await.unwrap();
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            let raw = server
                .requests
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert!(raw.starts_with("POST /chat/completions "));
            assert!(raw
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {key}")));
            AuthStore::transact_at(&tmp.path(), |s| {
                s.remove("zai");
                Ok(())
            })
            .unwrap();
        }
        server.finish();
    }

    #[tokio::test]
    async fn stored_refresh_failure_is_auth_error_before_http() {
        let tmp = TempStore::new();
        tmp.put("unknown-oauth-provider", expired());
        let resolver = Arc::new(StoredCredentialResolver {
            path: tmp.path(),
            provider: "unknown-oauth-provider".into(),
            fallback: Some("catalog-key".into()),
            headers: vec![],
        });
        let provider = feather_core::provider::openai::OpenAiProvider::new(
            "",
            Some("http://127.0.0.1:1".into()),
        )
        .with_credentials(resolver);
        let error = match provider.stream(&request()).await {
            Ok(_) => panic!("unexpected fallback"),
            Err(e) => e,
        };
        assert!(matches!(error, ProviderError::Auth(_)));
    }
}
