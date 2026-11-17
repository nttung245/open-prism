use std::sync::Arc;

use openprism_core::engines::EngineInfo;

use crate::auth::AccessToken;

/// Shared, read-mostly application state.
#[derive(Debug, Clone)]
pub struct AppState(Arc<Inner>);

#[derive(Debug)]
pub struct Inner {
    pub token: AccessToken,
    /// TeX toolchains found at startup.
    pub engines: Vec<EngineInfo>,
}

impl AppState {
    pub fn new(token: AccessToken, engines: Vec<EngineInfo>) -> Self {
        Self(Arc::new(Inner { token, engines }))
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}
