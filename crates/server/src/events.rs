//! Event channels streamed to the browser over SSE: one per project, and one for the whole app.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;
use ts_rs::TS;

use crate::compiler::CompileResult;
use crate::jobs::store::JobView;

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
    /// People were added to or removed from the project, or their roles changed: reload them.
    MembersChanged {
        /// Accounts that just lost access; their streams end after this event. Never sent.
        #[serde(skip)]
        #[ts(skip)]
        removed: Vec<String>,
    },
    /// Events were dropped (slow client): reload everything.
    Resync,
}

/// Something every open page may react to, whatever project it shows.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum AppEvent {
    /// A background job was created, changed state or finished a step.
    JobChanged {
        job: JobView,
        /// Who started the job; only their stream gets the event (server mode). Never sent.
        #[serde(skip)]
        #[ts(skip)]
        owner: Option<String>,
    },
    /// A finished background job was deleted.
    JobRemoved {
        id: String,
        /// As for `JobChanged`. Never sent.
        #[serde(skip)]
        #[ts(skip)]
        owner: Option<String>,
    },
    /// The projects of some accounts changed (added to or removed from a project): reload the
    /// project list.
    ProjectsChanged {
        /// The accounts concerned; only their streams get the event. Never sent.
        #[serde(skip)]
        #[ts(skip)]
        users: Vec<String>,
    },
    /// Events were dropped (slow client): reload the jobs.
    Resync,
}

impl ProjectEvent {
    /// The last event of the project stream of `user_id`: they were just removed from it.
    pub fn ends_stream_of(&self, user_id: &str) -> bool {
        matches!(self, Self::MembersChanged { removed } if removed.iter().any(|id| id == user_id))
    }
}

impl AppEvent {
    /// Whether the stream of `user_id` gets this event (`None`: single-user mode and the access
    /// token, which get everything). `Resync` carries nothing, so everyone gets it.
    pub fn is_for(&self, user_id: Option<&str>) -> bool {
        let Some(user_id) = user_id else {
            return true;
        };
        match self {
            Self::JobChanged { owner, .. } | Self::JobRemoved { owner, .. } => {
                owner.as_deref() == Some(user_id)
            }
            Self::ProjectsChanged { users } => users.iter().any(|id| id == user_id),
            Self::Resync => true,
        }
    }
}

const CHANNEL_CAPACITY: usize = 256;

#[derive(Debug)]
pub struct EventHub {
    channels: Mutex<HashMap<String, broadcast::Sender<ProjectEvent>>>,
    app: broadcast::Sender<AppEvent>,
}

impl Default for EventHub {
    fn default() -> Self {
        Self {
            channels: Mutex::default(),
            app: broadcast::channel(CHANNEL_CAPACITY).0,
        }
    }
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

    pub fn subscribe_app(&self) -> broadcast::Receiver<AppEvent> {
        self.app.subscribe()
    }

    pub fn publish_app(&self, event: AppEvent) {
        // An error only means nobody listens right now.
        let _ = self.app.send(event);
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
