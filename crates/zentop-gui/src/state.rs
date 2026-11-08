//! Desktop state adds an icon cache to the shared monitoring state.
use crate::native_icons::IconCache;
use std::{ops::Deref, sync::Mutex};

#[derive(Debug)]
pub struct AppState {
    core: zentop_core::state::AppState,
    pub icons: Mutex<IconCache>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            core: zentop_core::state::AppState::new(),
            icons: Mutex::new(IconCache::default()),
        }
    }
}

impl Deref for AppState {
    type Target = zentop_core::state::AppState;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}
