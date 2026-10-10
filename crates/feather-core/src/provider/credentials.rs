//! Request-time credentials, independent of the frontend and credential store.

use super::ProviderError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct ResolvedCredential {
    pub api_key: String,
    pub account_id: Option<String>,
    pub bearer_auth: bool,
    /// Some OAuth providers (notably Copilot) select an account-specific API host.
    pub base_url: Option<String>,
    pub headers: Vec<(String, String)>,
}
impl ResolvedCredential {
    pub fn request_headers(&self, req: &super::ChatRequest) -> Vec<(String, String)> {
        let mut headers = self.headers.clone();
        if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("copilot-integration-id")) {
            let initiator = if req.messages.last().is_some_and(|m| m.role != crate::message::Role::User) {
                "agent"
            } else { "user" };
            headers.push(("X-Initiator".into(), initiator.into()));
            headers.push(("Openai-Intent".into(), "conversation-edits".into()));
            if req.messages.iter().any(|m| m.content.iter().any(|b| matches!(b, crate::message::ContentBlock::Image { .. }))) {
                headers.push(("Copilot-Vision-Request".into(), "true".into()));
            }
        }
        headers
    }
}

pub type CredentialFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ResolvedCredential, ProviderError>> + Send + 'a>>;

/// Resolve immediately before each HTTP request, including tool-loop iterations,
/// compaction requests, and subagent calls. Implementations must not block the
/// async executor while refreshing credentials.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self) -> CredentialFuture<'_>;
}

pub struct CredentialSource {
    static_key: String,
    resolver: Option<Arc<dyn CredentialResolver>>,
}

impl CredentialSource {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { static_key: api_key.into(), resolver: None }
    }

    pub fn with_resolver(mut self, resolver: Arc<dyn CredentialResolver>) -> Self {
        self.resolver = Some(resolver);
        self
    }

    pub async fn resolve(&self) -> Result<ResolvedCredential, ProviderError> {
        if let Some(resolver) = &self.resolver {
            // Do not turn refresh errors into a fallback to another account.
            resolver.resolve().await
        } else {
            Ok(ResolvedCredential { api_key: self.static_key.clone(), ..Default::default() })
        }
    }
}
