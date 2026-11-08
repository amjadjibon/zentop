//! Shared, frontend-independent system monitoring for Zentop.
//!
//! Desktop icons, windows, themes and preferences belong to the GUI crate.
//! Use [`state::AppState`] with [`sampling::Hub`] to collect cached snapshots.

pub mod gpu;
pub mod monitoring;
pub mod sampling;
pub mod state;
