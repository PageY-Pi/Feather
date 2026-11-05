//! Request-time credentials, independent of the frontend and credential store.

use super::ProviderError;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Placeholder a configured header value can use to mean "this conversation".
/// Substituted per request from [`super::ChatRequest::session_id`].
pub const SESSION_PLACEHOLDER: &str = "$SESSION";

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
        if headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("copilot-integration-id"))
        {
            let initiator = if req
                .messages
                .last()
                .is_some_and(|m| m.role != crate::message::Role::User)
            {
                "agent"
            } else {
                "user"
            };
            headers.push(("X-Initiator".into(), initiator.into()));
            headers.push(("Openai-Intent".into(), "conversation-edits".into()));
            if req.messages.iter().any(|m| {
                m.content
                    .iter()
                    .any(|b| matches!(b, crate::message::ContentBlock::Image { .. }))
            }) {
                headers.push(("Copilot-Vision-Request".into(), "true".into()));
            }
        }
        // `$SESSION` in a configured header stands for the conversation id.
        // Gateways that cache prompts per conversation (OpenCode Zen's
        // `x-opencode-session`) reject a request without it, and a fixed literal
        // would collapse every conversation onto one cache bucket — so the value
        // has to come from the request, not the config.
        for (_, value) in headers.iter_mut() {
            if value.contains(SESSION_PLACEHOLDER) {
                *value = value.replace(SESSION_PLACEHOLDER, &req.session_id);
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
        Self {
            static_key: api_key.into(),
            resolver: None,
        }
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
            Ok(ResolvedCredential {
                api_key: self.static_key.clone(),
                ..Default::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    fn request(session_id: &str) -> super::super::ChatRequest {
        super::super::ChatRequest {
            system: String::new(),
            messages: std::sync::Arc::new(vec![Message::user_text("hi")]),
            tools: std::sync::Arc::new(Vec::new()),
            model: "m".into(),
            max_tokens: 16,
            temperature: None,
            cache: Default::default(),
            thinking: None,
            session_id: session_id.into(),
        }
    }

    fn credential(headers: &[(&str, &str)]) -> ResolvedCredential {
        ResolvedCredential {
            api_key: "k".into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }
    }

    /// OpenCode Zen refuses a request without `x-opencode-session` and uses the
    /// value to keep that conversation's prompt cache warm. A single literal
    /// would work once and then collapse every conversation onto one bucket.
    #[test]
    fn the_session_placeholder_becomes_this_request_session() {
        let cred = credential(&[
            ("x-opencode-session", SESSION_PLACEHOLDER),
            ("x-opencode-client", "feather"),
        ]);
        let headers = cred.request_headers(&request("s01abc"));
        assert_eq!(
            headers,
            [
                ("x-opencode-session".to_string(), "s01abc".to_string()),
                ("x-opencode-client".to_string(), "feather".to_string()),
            ]
        );

        // A different conversation gets a different value, which is the point.
        let other = cred.request_headers(&request("s02def"));
        assert_eq!(other[0].1, "s02def");
    }

    #[test]
    fn headers_without_the_placeholder_are_passed_through_untouched() {
        let cred = credential(&[("x-opencode-client", "feather")]);
        let headers = cred.request_headers(&request("s01abc"));
        assert_eq!(
            headers,
            [("x-opencode-client".to_string(), "feather".to_string())]
        );
    }
}
