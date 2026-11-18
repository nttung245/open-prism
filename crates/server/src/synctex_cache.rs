//! A few recently used, parsed SyncTeX files: navigation queries arrive in bursts for the PDF
//! on screen, and parsing a long document takes tens of milliseconds.

use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use openprism_core::synctex::SyncTex;

const CAPACITY: usize = 8;

#[derive(Debug, Default)]
pub struct SyncTexCache {
    entries: Mutex<VecDeque<(PathBuf, Arc<SyncTex>)>>,
}

impl SyncTexCache {
    pub async fn get_or_load<E, F, Fut>(&self, path: &Path, load: F) -> Result<Arc<SyncTex>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<SyncTex, E>>,
    {
        if let Some(found) = self.get(path) {
            return Ok(found);
        }
        let loaded = Arc::new(load().await?);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.retain(|(cached, _)| cached != path);
        entries.push_front((path.to_path_buf(), loaded.clone()));
        entries.truncate(CAPACITY);
        Ok(loaded)
    }

    fn get(&self, path: &Path) -> Option<Arc<SyncTex>> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = entries.iter().position(|(cached, _)| cached == path)?;
        let entry = entries.remove(index)?;
        let found = entry.1.clone();
        entries.push_front(entry);
        Some(found)
    }
}
