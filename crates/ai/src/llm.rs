//! A provider-neutral chat model: our own message format (stored in the database and shown in
//! the UI) and a streaming call that yields text, reasoning, tool calls and usage.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use genai::chat::{
    Binary, ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart, MessageContent,
    ReasoningEffort, Tool, ToolCall as GenaiToolCall, ToolResponse,
};
use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::AiError;
use crate::mock::MockModel;
use crate::provider::{ProviderConfig, ProviderKind};

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "role",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, rename = "AiMessage")]
pub enum Message {
    User {
        text: String,
        /// Images sent with the text; absent from messages without any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<ImageRef>>", optional)]
        images: Vec<ImageRef>,
    },
    Assistant {
        parts: Vec<AssistantPart>,
    },
    /// Results of the tool calls of the preceding assistant message.
    Tool {
        results: Vec<ToolResult>,
    },
}

impl Message {
    /// A user message with text only.
    pub fn user(text: impl Into<String>) -> Self {
        Self::User {
            text: text.into(),
            images: Vec::new(),
        }
    }
}

/// An image attached to a user message. Its bytes are kept outside the message (the server
/// stores them as files) and are loaded into `data` only to send the message to a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImageRef {
    pub id: String,
    /// `image/png`, `image/jpeg` or `image/webp`.
    pub media_type: String,
    /// The PDF page it was captured from (1-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub page: Option<u32>,
    /// Where SyncTeX places the captured region in the project's sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<SourceRef>,
    /// The image, base64-encoded, while it is being sent to a model; never stored.
    #[serde(skip)]
    #[ts(skip)]
    pub data: Option<Arc<str>>,
}

/// A position in the project's sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceRef {
    /// Path relative to the project root.
    pub file: String,
    /// 1-based line.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum AssistantPart {
    Text {
        text: String,
    },
    /// Shown to the user, never sent back to the model.
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: Value,
        /// Opaque provider data that must be echoed back (Gemini thought signatures).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        signatures: Vec<String>,
    },
    /// Opaque provider data that must be echoed back before the tool calls.
    Signature {
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

#[derive(Debug, Clone, Default)]
pub struct LlmRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
    /// How hard a reasoning model should think; `None` keeps the provider's default.
    pub reasoning: Option<ReasoningLevel>,
    /// Identifies the conversation to gateways that route by session (OpenCode Go).
    pub session_id: Option<String>,
}

/// Reasoning ("thinking") effort requested from models that support it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ReasoningLevel {
    /// No reasoning: fastest and cheapest.
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl ReasoningLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "off" => Self::Off,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "max" => Self::Max,
            _ => return None,
        })
    }

    fn to_genai(self) -> ReasoningEffort {
        match self {
            Self::Off => ReasoningEffort::Zero,
            Self::Low => ReasoningEffort::Low,
            Self::Medium => ReasoningEffort::Medium,
            Self::High => ReasoningEffort::High,
            Self::Max => ReasoningEffort::Max,
        }
    }
}

/// Sent with every request so providers can tell Open Prism's traffic apart.
pub const USER_AGENT: &str = concat!("open-prism/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmEvent {
    Text(String),
    Reasoning(String),
    /// Emitted once the call is complete, with full arguments.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        signatures: Vec<String>,
    },
    Signature(String),
    Usage(Usage),
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<LlmEvent, AiError>> + Send>>;

/// How long to wait for the first byte, and between bytes, of a streamed answer.
const HEADER_TIMEOUT: Duration = Duration::from_secs(120);
const READ_TIMEOUT: Duration = Duration::from_secs(120);

/// A model of a configured provider.
#[derive(Debug, Clone)]
pub enum Model {
    Genai {
        client: Client,
        model: String,
        kind: ProviderKind,
    },
    Mock(MockModel),
}

impl Model {
    pub fn new(config: &ProviderConfig, model: &str) -> Result<Self, AiError> {
        config.validate()?;
        if config.kind == ProviderKind::Mock {
            return Ok(Self::Mock(MockModel::default()));
        }
        let base_url = config.base_url();
        let api_key = config.api_key.clone().filter(|key| !key.is_empty());
        if config.kind.needs_api_key() && api_key.is_none() {
            return Err(AiError::Unauthorized("no API key is configured".into()));
        }
        let resolver = ServiceTargetResolver::from_resolver_fn(
            move |mut target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                if let Some(url) = &base_url {
                    target.endpoint = Endpoint::from_owned(url.clone());
                }
                // Local servers ignore the key, but some adapters insist on having one.
                target.auth =
                    AuthData::from_single(api_key.clone().unwrap_or_else(|| "none".into()));
                Ok(target)
            },
        );
        let client = Client::builder()
            .with_service_target_resolver(resolver)
            .build()
            .map_err(AiError::from_genai)?;
        Ok(Self::Genai {
            client,
            model: format!("{}::{model}", config.kind.genai_namespace()),
            kind: config.kind,
        })
    }

    pub async fn stream(&self, request: LlmRequest) -> Result<EventStream, AiError> {
        match self {
            Self::Mock(mock) => Ok(mock.stream(request)),
            Self::Genai {
                client,
                model,
                kind,
            } => {
                let mut chat = ChatRequest::new(to_genai_messages(&request.messages))
                    .with_system(request.system.clone());
                if !request.tools.is_empty() {
                    chat = chat.with_tools(
                        request
                            .tools
                            .iter()
                            .map(|tool| {
                                Tool::new(tool.name)
                                    .with_description(tool.description)
                                    .with_schema(tool.schema.clone())
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                let mut options = ChatOptions::default()
                    .with_capture_usage(true)
                    .with_capture_content(true)
                    .with_capture_tool_calls(true)
                    .with_normalize_reasoning_content(true)
                    .with_stream_header_timeout(HEADER_TIMEOUT)
                    .with_stream_read_timeout(READ_TIMEOUT);
                if let Some(max) = request.max_tokens {
                    options = options.with_max_tokens(max);
                }
                if let Some(level) = request.reasoning {
                    options = options.with_reasoning_effort(level.to_genai());
                }
                let mut headers = vec![("user-agent".to_owned(), USER_AGENT.to_owned())];
                if *kind == ProviderKind::OpencodeGo
                    && let Some(session) = &request.session_id
                {
                    headers.push(("x-opencode-session".to_owned(), session.clone()));
                }
                options = options.with_extra_headers(headers);
                let response = client
                    .exec_chat_stream(model.as_str(), chat, Some(&options))
                    .await
                    .map_err(AiError::from_genai)?;
                Ok(Box::pin(response.stream.flat_map(|event| {
                    futures_util::stream::iter(convert_event(event))
                })))
            }
        }
    }
}

fn convert_event(event: Result<ChatStreamEvent, genai::Error>) -> Vec<Result<LlmEvent, AiError>> {
    let event = match event {
        Ok(event) => event,
        Err(err) => return vec![Err(AiError::from_genai(err))],
    };
    match event {
        ChatStreamEvent::Chunk(chunk) if !chunk.content.is_empty() => {
            vec![Ok(LlmEvent::Text(chunk.content))]
        }
        ChatStreamEvent::ReasoningChunk(chunk) if !chunk.content.is_empty() => {
            vec![Ok(LlmEvent::Reasoning(chunk.content))]
        }
        ChatStreamEvent::End(end) => {
            let mut events = Vec::new();
            if let Some(content) = end.captured_content {
                for part in content.into_parts() {
                    match part {
                        ContentPart::ToolCall(call) => events.push(Ok(LlmEvent::ToolCall {
                            id: call.call_id,
                            name: call.fn_name,
                            arguments: call.fn_arguments,
                            signatures: call.thought_signatures.unwrap_or_default(),
                        })),
                        ContentPart::ThoughtSignature(value) => {
                            events.push(Ok(LlmEvent::Signature(value)));
                        }
                        _ => {}
                    }
                }
            }
            if let Some(usage) = end.captured_usage {
                events.push(Ok(LlmEvent::Usage(Usage {
                    input_tokens: to_u32(usage.prompt_tokens),
                    output_tokens: to_u32(usage.completion_tokens),
                })));
            }
            events
        }
        _ => Vec::new(),
    }
}

fn to_u32(value: Option<i32>) -> u32 {
    value.and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}

/// The parts of a user message with images: its text, then for each image a line saying where
/// it comes from and the image itself. An image whose bytes were not loaded (an old one, past
/// the number sent per turn) keeps only its line.
fn user_parts(text: &str, images: &[ImageRef]) -> Vec<ContentPart> {
    let mut parts = Vec::with_capacity(1 + 2 * images.len());
    if !text.is_empty() {
        parts.push(ContentPart::Text(text.to_owned()));
    }
    for (index, image) in images.iter().enumerate() {
        let number = index + 1;
        let origin = describe_image(image);
        match &image.data {
            Some(data) => {
                parts.push(ContentPart::Text(format!("Image {number}: {origin}")));
                parts.push(ContentPart::Binary(Binary::from_base64(
                    image.media_type.clone(),
                    Arc::clone(data),
                    None,
                )));
            }
            None => parts.push(ContentPart::Text(format!(
                "Image {number} (no longer sent): {origin}"
            ))),
        }
    }
    parts
}

/// Where an image comes from, for the model.
fn describe_image(image: &ImageRef) -> String {
    match (image.page, &image.source) {
        (Some(page), Some(source)) => format!(
            "captured from the PDF, page {page}, near {} line {}.",
            source.file, source.line
        ),
        (Some(page), None) => format!("captured from the PDF, page {page}."),
        (None, Some(source)) => format!("near {} line {}.", source.file, source.line),
        (None, None) => "attached by the user.".to_owned(),
    }
}

/// Our messages in `genai` form. Reasoning is display-only and is not sent back.
fn to_genai_messages(messages: &[Message]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            Message::User { text, images } if images.is_empty() => {
                out.push(ChatMessage::user(text.clone()));
            }
            Message::User { text, images } => out.push(ChatMessage::user(
                MessageContent::from_parts(user_parts(text, images)),
            )),
            Message::Assistant { parts } => {
                let parts: Vec<ContentPart> = parts
                    .iter()
                    .filter_map(|part| match part {
                        AssistantPart::Text { text } if !text.is_empty() => {
                            Some(ContentPart::Text(text.clone()))
                        }
                        AssistantPart::ToolCall {
                            id,
                            name,
                            arguments,
                            signatures,
                        } => Some(ContentPart::ToolCall(GenaiToolCall {
                            call_id: id.clone(),
                            fn_name: name.clone(),
                            fn_arguments: arguments.clone(),
                            thought_signatures: (!signatures.is_empty())
                                .then(|| signatures.clone()),
                        })),
                        AssistantPart::Signature { value } => {
                            Some(ContentPart::ThoughtSignature(value.clone()))
                        }
                        _ => None,
                    })
                    .collect();
                if !parts.is_empty() {
                    out.push(ChatMessage::assistant(MessageContent::from_parts(parts)));
                }
            }
            Message::Tool { results } => {
                for result in results {
                    let content = if result.is_error {
                        format!("ERROR: {}", result.content)
                    } else {
                        result.content.clone()
                    };
                    out.push(ChatMessage::from(ToolResponse::new(
                        result.call_id.clone(),
                        content,
                    )));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn messages_serialise_with_role_tags() {
        let message = Message::Assistant {
            parts: vec![
                AssistantPart::Text { text: "Hi".into() },
                AssistantPart::ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: json!({"path": "main.tex"}),
                    signatures: Vec::new(),
                },
            ],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["parts"][1]["type"], "toolCall");
        assert!(json["parts"][1].get("signatures").is_none());
        let back: Message = serde_json::from_value(json).unwrap();
        assert_eq!(back, message);
    }

    fn image(id: &str, data: Option<&str>) -> ImageRef {
        ImageRef {
            id: id.into(),
            media_type: "image/png".into(),
            page: Some(2),
            source: Some(SourceRef {
                file: "chapters/intro.tex".into(),
                line: 12,
            }),
            data: data.map(Into::into),
        }
    }

    #[test]
    fn user_messages_keep_their_old_form_and_store_images_without_bytes() {
        // Stored before images existed.
        let old: Message = serde_json::from_value(json!({"role": "user", "text": "Hi"})).unwrap();
        assert_eq!(old, Message::user("Hi"));
        // Without images, written exactly as before.
        assert_eq!(
            serde_json::to_value(Message::user("Hi")).unwrap(),
            json!({"role": "user", "text": "Hi"})
        );
        // With images: the reference, never the bytes.
        let message = Message::User {
            text: "Why?".into(),
            images: vec![image("a1", Some("iVBORw0KGgo="))],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            json!({"role": "user", "text": "Why?", "images": [{
                "id": "a1", "mediaType": "image/png", "page": 2,
                "source": {"file": "chapters/intro.tex", "line": 12}
            }]})
        );
        let back: Message = serde_json::from_value(json).unwrap();
        let Message::User { images, .. } = back else {
            panic!("a user message")
        };
        assert_eq!(images[0].data, None);
        assert_eq!(images[0].source, image("a1", None).source);
    }

    #[test]
    fn user_images_are_sent_after_the_text_with_where_they_come_from() {
        let mut second = image("a2", None);
        second.source = None;
        let messages = vec![
            Message::User {
                text: "Why does it overflow?".into(),
                images: vec![image("a1", Some("iVBORw0KGgo=")), second],
            },
            Message::user("Plain"),
        ];
        let text = |part: &ContentPart| match part {
            ContentPart::Text(text) => Some(text.clone()),
            _ => None,
        };
        let out = to_genai_messages(&messages);
        let parts = out[0].content.parts();
        assert_eq!(parts.len(), 4, "{parts:?}");
        assert_eq!(text(&parts[0]).as_deref(), Some("Why does it overflow?"));
        assert_eq!(
            text(&parts[1]).as_deref(),
            Some("Image 1: captured from the PDF, page 2, near chapters/intro.tex line 12.")
        );
        let ContentPart::Binary(binary) = &parts[2] else {
            panic!("an image: {parts:?}")
        };
        assert_eq!(binary.content_type, "image/png");
        assert!(binary.is_image());
        // Not loaded: only the line remains.
        assert_eq!(
            text(&parts[3]).as_deref(),
            Some("Image 2 (no longer sent): captured from the PDF, page 2.")
        );
        // Messages without images are unchanged.
        assert_eq!(out[1].content.joined_texts().as_deref(), Some("Plain"));
        assert_eq!(out[1].content.parts().len(), 1);
        // Only images, no text.
        let only = to_genai_messages(&[Message::User {
            text: String::new(),
            images: vec![image("a3", Some("AAAA"))],
        }]);
        assert_eq!(only[0].content.parts().len(), 2);
    }

    #[test]
    fn conversion_drops_reasoning_and_splits_tool_results() {
        let messages = vec![
            Message::user("Fix it"),
            Message::Assistant {
                parts: vec![
                    AssistantPart::Reasoning {
                        text: "thinking".into(),
                    },
                    AssistantPart::ToolCall {
                        id: "c1".into(),
                        name: "read_file".into(),
                        arguments: json!({}),
                        signatures: Vec::new(),
                    },
                ],
            },
            Message::Tool {
                results: vec![
                    ToolResult {
                        call_id: "c1".into(),
                        name: "read_file".into(),
                        content: "x".into(),
                        is_error: false,
                    },
                    ToolResult {
                        call_id: "c2".into(),
                        name: "grep".into(),
                        content: "bad".into(),
                        is_error: true,
                    },
                ],
            },
        ];
        let converted = to_genai_messages(&messages);
        assert_eq!(converted.len(), 4);
        let json = serde_json::to_string(&converted).unwrap();
        assert!(!json.contains("thinking"));
        assert!(json.contains("ERROR: bad"));
    }

    #[test]
    fn mock_and_local_providers_need_no_key() {
        let config = ProviderConfig {
            id: "p".into(),
            kind: ProviderKind::Ollama,
            base_url: None,
            api_key: None,
        };
        assert!(matches!(
            Model::new(&config, "qwen2.5:1.5b").unwrap(),
            Model::Genai { ref model, .. } if model == "ollama::qwen2.5:1.5b"
        ));
        let cloud = ProviderConfig {
            kind: ProviderKind::Anthropic,
            ..config
        };
        assert!(matches!(
            Model::new(&cloud, "claude").unwrap_err(),
            AiError::Unauthorized(_)
        ));
    }
}
