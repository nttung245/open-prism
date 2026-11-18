//! Per-project event channels, streamed to the browser over SSE.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::compiler::CompileResult;

/// Something the open workspace should react to.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum ProjectEvent {
    /// Files changed on disk, by Open Prism or another program. `""` means the whole project.
    FilesChanged {
        paths: Vec<String>,
    },
    CompileStarted {
        run_id: String,
    },
    CompileFinished {
        result: CompileResult,
    },
    /// A snapshot was added or a label changed: reload the history.
    HistoryChanged,
    /// Events were dropped (slow client): reload everything.
    Resync,
}

const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug, Default)]
pub struct EventHub {
    channels: Mutex<HashMap<String, broadcast::Sender<ProjectEvent>>>,
}

impl EventHub {
    pub fn subscribe(&self, project_id: &str) -> broadcast::Receiver<ProjectEvent> {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        channels
            .entry(project_id.to_owned())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    pub fn publish(&self, project_id: &str, event: ProjectEvent) {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A send error means nobody listens any more.
        if let Some(sender) = channels.get(project_id)
            && sender.send(event).is_err()
        {
            channels.remove(project_id);
        }
    }
}
