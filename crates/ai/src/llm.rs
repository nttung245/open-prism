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
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error::{AiError, http_failure};
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
                let chat = chat_request(*kind, &request);
                let requested = request.reasoning.map(ReasoningLevel::to_genai);
                let options = chat_options(*kind, &request, requested.clone());
                let first = start(client, model, chat.clone(), &options).await;
                // A model that does not take the chosen reasoning level refuses the whole call;
                // ask once more with a level it does take.
                let Some(fallback) =
                    first
                        .error()
                        .and_then(http_failure)
                        .and_then(|(status, body)| {
                            reasoning_fallback(requested.as_ref(), status, &body)
                        })
                else {
                    return first.into_events();
                };
                tracing::warn!(
                    provider = kind.as_str(),
                    model = %model,
                    requested = ?requested,
                    fallback = ?fallback,
                    "the model refused the reasoning level, sending the request again"
                );
                let options = chat_options(*kind, &request, fallback);
                start(client, model, chat, &options).await.into_events()
            }
        }
    }
}

fn chat_request(kind: ProviderKind, request: &LlmRequest) -> ChatRequest {
    let chat =
        ChatRequest::new(to_genai_messages(&request.messages)).with_system(request.system.clone());
    if request.tools.is_empty() {
        return chat;
    }
    chat.with_tools(
        request
            .tools
            .iter()
            .map(|tool| {
                let built = Tool::new(tool.name)
                    .with_description(tool.description)
                    .with_schema(tool.schema.clone());
                // The Responses API makes tools strict unless told otherwise, and strict schemas
                // cannot have optional fields, which ours do.
                if kind == ProviderKind::Openai {
                    built.with_strict(false)
                } else {
                    built
                }
            })
            .collect::<Vec<_>>(),
    )
}

/// Options for one call; `effort` is the reasoning level to send (`None`: the provider's default).
fn chat_options(
    kind: ProviderKind,
    request: &LlmRequest,
    effort: Option<ReasoningEffort>,
) -> ChatOptions {
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
    match (kind, effort) {
        // `genai` keeps OpenAI's encrypted reasoning, which must be sent back with tool results,
        // only when capturing reasoning content. That also asks for a reasoning summary, which
        // OpenAI may refuse to unverified organisations; `extra_body` replaces the whole
        // `reasoning` object, so the summary is dropped.
        (ProviderKind::Openai, effort) => {
            let reasoning = match effort.as_ref().and_then(openai_effort) {
                Some(name) => json!({ "effort": name }),
                None => json!({}),
            };
            options = options
                .with_capture_reasoning_content(true)
                .with_extra_body(json!({ "reasoning": reasoning }));
        }
        // `genai` sends nothing for "no reasoning", so Gemini would think at its default level.
        // `extra_body` replaces whole top-level keys, so it carries all of `generationConfig`.
        (ProviderKind::Gemini, Some(ReasoningEffort::Zero)) => {
            let mut config = json!({ "thinkingConfig": { "thinkingBudget": 0 } });
            if let Some(max) = request.max_tokens {
                config["maxOutputTokens"] = json!(max);
            }
            options = options.with_extra_body(json!({ "generationConfig": config }));
        }
        (_, Some(effort)) => options = options.with_reasoning_effort(effort),
        (_, None) => {}
    }
    let mut headers = vec![("user-agent".to_owned(), USER_AGENT.to_owned())];
    if kind == ProviderKind::OpencodeGo
        && let Some(session) = &request.session_id
    {
        headers.push(("x-opencode-session".to_owned(), session.clone()));
    }
    options.with_extra_headers(headers)
}

/// The name OpenAI uses for an effort level.
fn openai_effort(effort: &ReasoningEffort) -> Option<&'static str> {
    match effort {
        ReasoningEffort::Zero => Some("none"),
        other => other.as_keyword(),
    }
}

/// Effort levels from least to most reasoning, for picking the nearest one a model takes.
fn effort_rank(effort: &ReasoningEffort) -> Option<u8> {
    Some(match effort {
        ReasoningEffort::Zero => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
        ReasoningEffort::Max => 6,
        ReasoningEffort::Budget(_) => return None,
    })
}

/// When a provider refused the reasoning level (`requested`) of a call, the level to ask again
/// with: the nearest one the error lists as supported, else none at all. `None` when the
/// failure is about something else, so the call is not repeated.
fn reasoning_fallback(
    requested: Option<&ReasoningEffort>,
    status: u16,
    body: &str,
) -> Option<Option<ReasoningEffort>> {
    let requested = requested?;
    let lower = body.to_ascii_lowercase();
    let about_reasoning = ["reasoning", "thinking", "budget"]
        .iter()
        .any(|word| lower.contains(word));
    if status != 400 || !about_reasoning {
        return None;
    }
    let nearest = effort_rank(requested).and_then(|wanted| {
        supported_efforts(&lower)
            .into_iter()
            .filter_map(|effort| effort_rank(&effort).map(|rank| (rank, effort)))
            .filter(|(rank, _)| *rank != wanted)
            .min_by_key(|(rank, _)| (rank.abs_diff(wanted), *rank))
            .map(|(_, effort)| effort)
    });
    Some(nearest)
}

/// `… Supported values are: 'none', 'low', and 'xhigh'.` (lowercased) → none, low, xhigh.
fn supported_efforts(lower_body: &str) -> Vec<ReasoningEffort> {
    let Some((_, rest)) = lower_body.split_once("supported values are:") else {
        return Vec::new();
    };
    let list = rest.split(['.', '"']).next().unwrap_or_default();
    list.split('\'')
        .skip(1)
        .step_by(2)
        .filter_map(ReasoningEffort::from_keyword)
        .collect()
}

type GenaiStream = Pin<Box<dyn Stream<Item = Result<ChatStreamEvent, genai::Error>> + Send>>;

/// A started streamed call. A refused streamed call shows up as an error right after the
/// stream's `Start` marker, so the stream is read up to its first other event.
enum Start {
    /// The stream, with the event read ahead put back in front.
    Ready(GenaiStream),
    /// The call could not be made at all.
    CallFailed(genai::Error),
    /// The stream's first event is an error.
    Refused(genai::Error),
}

impl Start {
    fn error(&self) -> Option<&genai::Error> {
        match self {
            Self::Ready(_) => None,
            Self::CallFailed(err) | Self::Refused(err) => Some(err),
        }
    }

    fn into_events(self) -> Result<EventStream, AiError> {
        match self {
            Self::Ready(stream) => {
                Ok(Box::pin(stream.flat_map(|event| {
                    futures_util::stream::iter(convert_event(event))
                })))
            }
            Self::CallFailed(err) => Err(AiError::from_genai(err)),
            Self::Refused(err) => Ok(Box::pin(futures_util::stream::iter([Err(
                AiError::from_genai(err),
            )]))),
        }
    }
}

async fn start(client: &Client, model: &str, chat: ChatRequest, options: &ChatOptions) -> Start {
    let response = match client.exec_chat_stream(model, chat, Some(options)).await {
        Ok(response) => response,
        Err(err) => return Start::CallFailed(err),
    };
    let mut stream = Box::pin(response.stream);
    loop {
        match stream.next().await {
            // Carries nothing (`convert_event` drops it).
            Some(Ok(ChatStreamEvent::Start)) => {}
            Some(Err(err)) => return Start::Refused(err),
            first => {
                return Start::Ready(Box::pin(futures_util::stream::iter(first).chain(stream)));
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

    fn tool_request(reasoning: Option<ReasoningLevel>, max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            tools: vec![ToolSpec {
                name: "read_file",
                description: "Read a file",
                schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            }],
            max_tokens,
            reasoning,
            ..LlmRequest::default()
        }
    }

    fn options(kind: ProviderKind, request: &LlmRequest) -> ChatOptions {
        chat_options(
            kind,
            request,
            request.reasoning.map(ReasoningLevel::to_genai),
        )
    }

    #[test]
    fn openai_options_keep_encrypted_reasoning_without_a_summary() {
        let off = options(
            ProviderKind::Openai,
            &tool_request(Some(ReasoningLevel::Off), None),
        );
        assert_eq!(off.capture_reasoning_content, Some(true));
        assert!(off.reasoning_effort.is_none());
        assert_eq!(
            off.extra_body,
            Some(json!({"reasoning": {"effort": "none"}}))
        );

        let max = options(
            ProviderKind::Openai,
            &tool_request(Some(ReasoningLevel::Max), None),
        );
        assert_eq!(
            max.extra_body,
            Some(json!({"reasoning": {"effort": "max"}}))
        );

        let unset = options(ProviderKind::Openai, &tool_request(None, None));
        assert_eq!(unset.extra_body, Some(json!({"reasoning": {}})));

        let chat = chat_request(ProviderKind::Openai, &tool_request(None, None));
        assert_eq!(chat.tools.unwrap()[0].strict, Some(false));
    }

    #[test]
    fn gemini_off_turns_thinking_off_and_keeps_the_token_limit() {
        let off = options(
            ProviderKind::Gemini,
            &tool_request(Some(ReasoningLevel::Off), Some(256)),
        );
        assert!(off.reasoning_effort.is_none());
        assert_eq!(
            off.extra_body,
            Some(json!({"generationConfig": {
                "thinkingConfig": {"thinkingBudget": 0},
                "maxOutputTokens": 256,
            }}))
        );

        let low = options(
            ProviderKind::Gemini,
            &tool_request(Some(ReasoningLevel::Low), Some(256)),
        );
        assert!(matches!(low.reasoning_effort, Some(ReasoningEffort::Low)));
        assert!(low.extra_body.is_none());
    }

    #[test]
    fn other_providers_are_unchanged() {
        for kind in [
            ProviderKind::Openrouter,
            ProviderKind::OpenaiCompatible,
            ProviderKind::OpencodeGo,
            ProviderKind::Anthropic,
        ] {
            let off = options(kind, &tool_request(Some(ReasoningLevel::Off), Some(64)));
            assert!(matches!(off.reasoning_effort, Some(ReasoningEffort::Zero)));
            assert!(off.extra_body.is_none());
            assert_ne!(off.capture_reasoning_content, Some(true));
            assert_eq!(off.max_tokens, Some(64));
            let chat = chat_request(kind, &tool_request(None, None));
            assert_eq!(chat.tools.unwrap()[0].strict, None);
        }
    }

    fn openai_error(message: &str) -> String {
        json!({"error": {
            "message": message,
            "type": "invalid_request_error",
            "param": "reasoning.effort",
            "code": "unsupported_value",
        }})
        .to_string()
    }

    #[test]
    fn a_refused_level_falls_back_to_the_nearest_supported_one() {
        // Answers from OpenAI on 2026-11-19.
        let max_on_gpt_5_5 = openai_error(
            "Unsupported value: 'max' is not supported with the 'gpt-5.5' model. Supported values are: 'none', 'low', 'medium', 'high', and 'xhigh'.",
        );
        assert!(matches!(
            reasoning_fallback(Some(&ReasoningEffort::Max), 400, &max_on_gpt_5_5),
            Some(Some(ReasoningEffort::XHigh))
        ));
        let none_on_gpt_5_mini = openai_error(
            "Unsupported value: 'none' is not supported with the 'gpt-5-mini' model. Supported values are: 'minimal', 'low', 'medium', and 'high'.",
        );
        assert!(matches!(
            reasoning_fallback(Some(&ReasoningEffort::Zero), 400, &none_on_gpt_5_mini),
            Some(Some(ReasoningEffort::Minimal))
        ));
        assert!(matches!(
            reasoning_fallback(Some(&ReasoningEffort::Max), 400, &none_on_gpt_5_mini),
            Some(Some(ReasoningEffort::High))
        ));
    }

    #[test]
    fn a_refused_level_without_a_list_is_dropped() {
        let gpt_4o = openai_error(
            "Unsupported parameter: 'reasoning.effort' is not supported with this model.",
        );
        assert!(matches!(
            reasoning_fallback(Some(&ReasoningEffort::Medium), 400, &gpt_4o),
            Some(None)
        ));
        let gemini_pro = json!({"error": {
            "code": 400,
            "message": "Budget 0 is invalid. This model only works in thinking mode.",
            "status": "INVALID_ARGUMENT",
        }})
        .to_string();
        assert!(matches!(
            reasoning_fallback(Some(&ReasoningEffort::Zero), 400, &gemini_pro),
            Some(None)
        ));
    }

    #[test]
    fn other_failures_are_not_sent_again() {
        let too_long = json!({"error": {
            "message": "Your input exceeds the context window of this model.",
            "param": "input",
            "code": "context_length_exceeded",
        }})
        .to_string();
        assert!(reasoning_fallback(Some(&ReasoningEffort::Medium), 400, &too_long).is_none());
        let reasoning_body = openai_error("Unsupported value: 'max'.");
        assert!(reasoning_fallback(Some(&ReasoningEffort::Max), 401, &reasoning_body).is_none());
        // Nothing was asked for, so there is nothing to drop.
        assert!(reasoning_fallback(None, 400, &reasoning_body).is_none());
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
