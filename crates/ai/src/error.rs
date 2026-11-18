//! Provider errors, normalised so the UI can say something useful and know when a retry helps.

/// Longest provider message passed on to the user.
const MAX_DETAIL: usize = 600;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AiError {
    #[error("the provider rejected the API key: {0}")]
    Unauthorized(String),
    #[error("rate limited by the provider, try again shortly: {0}")]
    RateLimited(String),
    #[error("the conversation is too long for this model: {0}")]
    ContextTooLong(String),
    #[error("model or endpoint not found: {0}")]
    NotFound(String),
    #[error("cannot reach the provider: {0}")]
    Unreachable(String),
    #[error("this model cannot call tools: {0}")]
    ToolsUnsupported(String),
    #[error("configuration problem: {0}")]
    Config(String),
    #[error("provider error: {0}")]
    Provider(String),
}

impl AiError {
    /// Whether trying the same request again later may work.
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited(_) | Self::Unreachable(_) | Self::Provider(_)
        )
    }

    /// Classifies an HTTP error answer from a provider.
    pub fn from_status(status: u16, body: &str) -> Self {
        let detail = summarise(body);
        let lower = body.to_ascii_lowercase();
        if mentions_missing_tool_support(&lower) {
            return Self::ToolsUnsupported(detail);
        }
        match status {
            401 | 403 => Self::Unauthorized(detail),
            429 => Self::RateLimited(detail),
            404 => Self::NotFound(detail),
            400 | 413 | 422
                if lower.contains("context")
                    && (lower.contains("length")
                        || lower.contains("too long")
                        || lower.contains("maximum")
                        || lower.contains("exceed")) =>
            {
                Self::ContextTooLong(detail)
            }
            502..=504 => Self::Unreachable(detail),
            _ => Self::Provider(format!("HTTP {status}: {detail}")),
        }
    }

    pub fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_connect() || err.is_timeout() {
            Self::Unreachable(err.to_string())
        } else if let Some(status) = err.status() {
            Self::from_status(status.as_u16(), &err.to_string())
        } else {
            Self::Provider(err.to_string())
        }
    }

    pub fn from_genai(err: genai::Error) -> Self {
        use genai::Error as G;
        use genai::webc::Error as W;
        match err {
            G::WebModelCall { webc_error, .. } | G::WebAdapterCall { webc_error, .. } => {
                match webc_error {
                    W::ResponseFailedStatus { status, body, .. } => {
                        Self::from_status(status.as_u16(), &body)
                    }
                    W::Reqwest(err) => Self::from_reqwest(err),
                    other => Self::Provider(summarise(&other.to_string())),
                }
            }
            G::HttpError { status, body, .. } => Self::from_status(status.as_u16(), &body),
            // Streamed calls report HTTP failures inside a stream error.
            G::WebStream { error, cause, .. } => match error.downcast_ref::<G>() {
                Some(G::HttpError { status, body, .. }) => Self::from_status(status.as_u16(), body),
                _ => match parse_http_cause(&cause) {
                    Some((status, body)) => Self::from_status(status, body),
                    None if mentions_missing_tool_support(&cause.to_ascii_lowercase()) => {
                        Self::ToolsUnsupported(summarise(&cause))
                    }
                    None => Self::Unreachable(summarise(&cause)),
                },
            },
            G::RequiresApiKey { .. } | G::NoAuthData { .. } | G::NoAuthResolver { .. } => {
                Self::Unauthorized("no API key is configured".into())
            }
            other => {
                let text = other.to_string();
                if mentions_missing_tool_support(&text.to_ascii_lowercase()) {
                    Self::ToolsUnsupported(summarise(&text))
                } else {
                    Self::Provider(summarise(&text))
                }
            }
        }
    }
}

/// `HTTP error.\nStatus: 401 Unauthorized\nBody: {…}` → (401, "{…}").
fn parse_http_cause(cause: &str) -> Option<(u16, &str)> {
    let after = cause.split_once("Status: ")?.1;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let status = digits.parse().ok()?;
    let body = cause.split_once("Body: ").map_or("", |(_, body)| body);
    Some((status, body))
}

fn mentions_missing_tool_support(lower: &str) -> bool {
    lower.contains("does not support tools")
        || lower.contains("tools is not supported")
        || lower.contains("tool use is not supported")
        || lower.contains("tool calling is not supported")
        || lower.contains("function calling is not enabled")
        || lower.contains("does not support function calling")
}

/// The provider's own message when the body is JSON (`{"error": {"message": …}}`), else the
/// start of the body.
fn summarise(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|json| {
            let error = json.get("error")?;
            error
                .get("message")
                .and_then(|m| m.as_str())
                .or_else(|| error.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().to_owned());
    if message.chars().count() > MAX_DETAIL {
        let cut: String = message.chars().take(MAX_DETAIL).collect();
        format!("{cut}…")
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_http_errors() {
        assert!(matches!(
            AiError::from_status(401, r#"{"error":{"message":"Incorrect API key"}}"#),
            AiError::Unauthorized(m) if m == "Incorrect API key"
        ));
        assert!(matches!(
            AiError::from_status(429, "slow down"),
            AiError::RateLimited(_)
        ));
        assert!(matches!(
            AiError::from_status(
                400,
                r#"{"error":{"message":"This model's maximum context length is 8192 tokens"}}"#
            ),
            AiError::ContextTooLong(_)
        ));
        assert!(matches!(
            AiError::from_status(
                400,
                r#"{"error":"registry.ollama.ai/library/gemma3:1b does not support tools"}"#
            ),
            AiError::ToolsUnsupported(_)
        ));
        assert!(matches!(
            AiError::from_status(500, "boom"),
            AiError::Provider(_)
        ));
        assert!(AiError::RateLimited(String::new()).retryable());
        assert!(!AiError::Unauthorized(String::new()).retryable());
    }

    #[test]
    fn parses_http_failures_reported_inside_streams() {
        let cause =
            "HTTP error.\nStatus: 429 Too Many Requests\nBody: {\"error\":{\"message\":\"slow\"}}";
        assert_eq!(
            parse_http_cause(cause),
            Some((429, "{\"error\":{\"message\":\"slow\"}}"))
        );
        assert_eq!(parse_http_cause("connection reset"), None);
    }

    #[test]
    fn long_bodies_are_cut() {
        let long = "x".repeat(5000);
        let AiError::Provider(message) = AiError::from_status(500, &long) else {
            panic!("unexpected classification");
        };
        assert!(message.len() < 700);
    }
}
