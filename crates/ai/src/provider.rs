//! LLM providers: what kinds exist, how to reach them, and how to list their models.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::AiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ProviderKind {
    Openai,
    Anthropic,
    Gemini,
    Openrouter,
    Deepseek,
    Mistral,
    Groq,
    Xai,
    /// A local or remote Ollama server.
    Ollama,
    /// OpenCode Go: a subscription gateway to open models (GLM, Kimi, Qwen, MiniMax, …).
    OpencodeGo,
    /// Any server speaking the OpenAI chat API (vLLM, LM Studio, llama.cpp, LiteLLM, …).
    OpenaiCompatible,
    /// Canned answers, for tests and offline demos.
    Mock,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 12] = [
        Self::Openai,
        Self::Anthropic,
        Self::Gemini,
        Self::Openrouter,
        Self::Deepseek,
        Self::Mistral,
        Self::Groq,
        Self::Xai,
        Self::Ollama,
        Self::OpencodeGo,
        Self::OpenaiCompatible,
        Self::Mock,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Openrouter => "openrouter",
            Self::Deepseek => "deepseek",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
            Self::Xai => "xai",
            Self::Ollama => "ollama",
            Self::OpencodeGo => "opencode-go",
            Self::OpenaiCompatible => "openai-compatible",
            Self::Mock => "mock",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// The `genai` adapter namespace used for this provider.
    pub(crate) fn genai_namespace(self) -> &'static str {
        match self {
            Self::Openai | Self::Openrouter | Self::OpenaiCompatible | Self::Mock => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Deepseek => "deepseek",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
            Self::Xai => "xai",
            Self::Ollama => "ollama",
            Self::OpencodeGo => "opencode_go",
        }
    }

    /// Base URL used when none is configured (always ends with `/`).
    pub fn default_base_url(self) -> Option<&'static str> {
        Some(match self {
            Self::Openai => "https://api.openai.com/v1/",
            Self::Anthropic => "https://api.anthropic.com/v1/",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta/",
            Self::Openrouter => "https://openrouter.ai/api/v1/",
            Self::Deepseek => "https://api.deepseek.com/v1/",
            Self::Mistral => "https://api.mistral.ai/v1/",
            Self::Groq => "https://api.groq.com/openai/v1/",
            Self::Xai => "https://api.x.ai/v1/",
            Self::Ollama => "http://localhost:11434/",
            Self::OpencodeGo => "https://opencode.ai/zen/go/v1/",
            Self::OpenaiCompatible | Self::Mock => return None,
        })
    }

    pub fn needs_api_key(self) -> bool {
        !matches!(self, Self::Ollama | Self::OpenaiCompatible | Self::Mock)
    }

    pub fn needs_base_url(self) -> bool {
        matches!(self, Self::OpenaiCompatible)
    }
}

/// Everything needed to call one provider. The key never leaves the server.
#[derive(Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ProviderConfig {
    /// Configured base URL, else the provider's default, normalised to end with `/`.
    pub fn base_url(&self) -> Option<String> {
        let url = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .or(self.kind.default_base_url())?;
        Some(if url.ends_with('/') {
            url.to_owned()
        } else {
            format!("{url}/")
        })
    }

    /// Checks the configuration before it is saved or used.
    pub fn validate(&self) -> Result<(), AiError> {
        if self.kind.needs_base_url() && self.base_url().is_none() {
            return Err(AiError::Config(
                "this provider needs a base URL, e.g. http://localhost:1234/v1".into(),
            ));
        }
        if let Some(url) = &self.base_url
            && !url.trim().is_empty()
            && !(url.starts_with("http://") || url.starts_with("https://"))
        {
            return Err(AiError::Config(
                "the base URL must start with http:// or https://".into(),
            ));
        }
        Ok(())
    }
}

const LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Asks the provider which models it offers. Also serves as the connection test.
pub async fn list_models(config: &ProviderConfig) -> Result<Vec<String>, AiError> {
    config.validate()?;
    if config.kind == ProviderKind::Mock {
        return Ok(vec![crate::mock::MOCK_MODEL.to_owned()]);
    }
    if config.kind.needs_api_key() && config.api_key.as_deref().is_none_or(str::is_empty) {
        return Err(AiError::Unauthorized("no API key is configured".into()));
    }
    let base = config
        .base_url()
        .ok_or_else(|| AiError::Config("no base URL".into()))?;
    let key = config.api_key.clone().unwrap_or_default();
    let client = reqwest::Client::builder()
        .timeout(LIST_TIMEOUT)
        .user_agent(crate::llm::USER_AGENT)
        .build()
        .map_err(|err| AiError::Provider(err.to_string()))?;

    let request = match config.kind {
        ProviderKind::Anthropic => client
            .get(format!("{base}models?limit=1000"))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01"),
        ProviderKind::Gemini => client
            .get(format!("{base}models?pageSize=1000"))
            .header("x-goog-api-key", &key),
        ProviderKind::Ollama => client.get(format!("{base}api/tags")),
        _ => {
            let request = client.get(format!("{base}models"));
            if key.is_empty() {
                request
            } else {
                request.bearer_auth(&key)
            }
        }
    };
    let response = request.send().await.map_err(AiError::from_reqwest)?;
    let status = response.status();
    let body = response.text().await.map_err(AiError::from_reqwest)?;
    if !status.is_success() {
        return Err(AiError::from_status(status.as_u16(), &body));
    }
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| {
        AiError::Provider("the provider answered with something that is not JSON".into())
    })?;
    let mut models = parse_model_list(config.kind, &json);
    models.sort();
    models.dedup();
    Ok(models)
}

fn parse_model_list(kind: ProviderKind, json: &serde_json::Value) -> Vec<String> {
    let ids = |array: Option<&serde_json::Value>, field: &str| -> Vec<String> {
        array
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get(field).and_then(|v| v.as_str()))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    match kind {
        ProviderKind::Ollama => ids(json.get("models"), "name"),
        ProviderKind::Gemini => json
            .get("models")
            .and_then(|models| models.as_array())
            .map(|models| {
                models
                    .iter()
                    .filter(|model| {
                        model
                            .get("supportedGenerationMethods")
                            .and_then(|m| m.as_array())
                            .is_none_or(|methods| methods.iter().any(|m| m == "generateContent"))
                    })
                    .filter_map(|model| model.get("name").and_then(|n| n.as_str()))
                    .map(|name| name.trim_start_matches("models/").to_owned())
                    .collect()
            })
            .unwrap_or_default(),
        _ => ids(json.get("data"), "id"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn kinds_round_trip_through_strings() {
        for kind in ProviderKind::ALL {
            assert_eq!(ProviderKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn base_urls_are_normalised_and_validated() {
        let mut config = ProviderConfig {
            id: "p".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: None,
            api_key: None,
        };
        assert!(config.validate().is_err());
        config.base_url = Some("http://localhost:1234/v1".into());
        assert_eq!(config.base_url().unwrap(), "http://localhost:1234/v1/");
        config.base_url = Some("localhost:1234".into());
        assert!(config.validate().is_err());

        let ollama = ProviderConfig {
            kind: ProviderKind::Ollama,
            base_url: Some("  ".into()),
            ..config
        };
        assert_eq!(ollama.base_url().unwrap(), "http://localhost:11434/");
    }

    #[test]
    fn debug_output_hides_the_key() {
        let config = ProviderConfig {
            id: "p".into(),
            kind: ProviderKind::Openai,
            base_url: None,
            api_key: Some("sk-secret".into()),
        };
        assert!(!format!("{config:?}").contains("sk-secret"));
    }

    #[test]
    fn parses_model_lists_of_each_api_shape() {
        assert_eq!(
            parse_model_list(
                ProviderKind::Openai,
                &json!({"data": [{"id": "gpt-x"}, {"id": "gpt-y"}]})
            ),
            ["gpt-x", "gpt-y"]
        );
        assert_eq!(
            parse_model_list(
                ProviderKind::Ollama,
                &json!({"models": [{"name": "qwen2.5:1.5b"}]})
            ),
            ["qwen2.5:1.5b"]
        );
        assert_eq!(
            parse_model_list(
                ProviderKind::Gemini,
                &json!({"models": [
                    {"name": "models/gemini-x", "supportedGenerationMethods": ["generateContent"]},
                    {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]}
                ]})
            ),
            ["gemini-x"]
        );
    }
}
