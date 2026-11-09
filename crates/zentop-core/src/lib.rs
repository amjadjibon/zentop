//! Shared, frontend-independent system monitoring for Zentop.
//!
//! Desktop icons, windows and themes belong to the GUI crate. Alert rules and
//! named monitoring views share frontend-independent models and persistence.
//! Use [`state::AppState`] with [`sampling::Hub`] to collect cached snapshots.

pub mod alerts;
pub mod disktree;
pub mod gpu;
pub mod monitoring;
pub mod monitoring_preferences;
pub mod process_view;
pub mod sampling;
pub mod state;
