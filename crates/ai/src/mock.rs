//! A model with canned answers, for tests and for trying the app without any provider.
//!
//! Scripted turns are played in order; once they run out, the model echoes the last user
//! message.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::llm::{EventStream, LlmEvent, LlmRequest, Message, Usage};

pub const MOCK_MODEL: &str = "mock-echo";

#[derive(Debug, Clone, Default)]
pub struct MockModel {
    turns: Arc<Mutex<VecDeque<Vec<LlmEvent>>>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl MockModel {
    /// A model that answers each call with the next list of events.
    pub fn scripted(turns: Vec<Vec<LlmEvent>>) -> Self {
        Self {
            turns: Arc::new(Mutex::new(turns.into())),
            requests: Arc::default(),
        }
    }

    /// Requests received so far.
    pub fn requests(&self) -> Vec<LlmRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn stream(&self, request: LlmRequest) -> EventStream {
        let scripted = self
            .turns
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front();
        let events = scripted.unwrap_or_else(|| {
            let last_user = request
                .messages
                .iter()
                .rev()
                .find_map(|message| match message {
                    Message::User { text } => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            vec![
                LlmEvent::Text("This is the offline mock model. ".into()),
                LlmEvent::Text(format!("You said: {last_user}")),
                LlmEvent::Usage(Usage {
                    input_tokens: u32::try_from(request.system.len() / 4).unwrap_or(u32::MAX),
                    output_tokens: 12,
                }),
            ]
        });
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request);
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}
