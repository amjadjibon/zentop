---
date: 2026-11-09
feature: alerts-and-saved-views
axes: [system, data-model, api, ui-ux]
---
# Design: Resource alerts and saved monitoring views

Opt-in resource alerts and named process views share their rules and persisted data
between GUI and TUI. Existing theme and window preferences remain compatible.

## System and data model
- Core owns a pure sustained-threshold/cooldown engine, bounded recent alert history,
  desktop notification delivery, and a typed `monitoring.json` store beside existing settings.
- CPU and memory thresholds use system-wide percentages. Low-space alerts use the
  least-free writable volume, keeping its mount identity through the sustained period.
- Pauses, errors, missing readings, gaps and rule changes reset pending durations.
  Repeated frames never advance a pending alert. CPU ignores the warm-up frame.
- Saved views contain a name, search, filters, sort, visible columns, list/tree mode
  and pinned commands. They exclude process samples, environment values and node IDs.
- Atomic writes under a file lock reload the latest data before mutation, preserving
  updates made by the other frontend. Invalid files are reported without overwriting them.

## Interface
`AlertEngine::observe(frame, valid, interval, settings, now)` returns newly fired events.
`MonitoringStore::{save_view, delete_view, set_alerts}` validate and persist operations.
Settings default to alerts disabled. Duration and cooldown apply to all three rules.
Save replaces an existing case-insensitive name; delete is explicit; names and counts
are bounded. Notification failures stay visible in the alert panel.

## UI / UX
GUI: Settings gains Alerts with enable switches and threshold/timing dropdowns, desktop
notification opt-in and recent events. The process toolbar gains Views with a name
field, Save/update, load and delete. Active resource alerts remain visible in the main window.
TUI: `a` opens alert rules/history; `b` opens saved views. Both sheets document their
keyboard controls. Terminal alerts also appear in the footer. Name entry captures keys
before global actions. Loading a view resets selection and expansion, preserving no PIDs.
Alert-enabled GUI monitoring continues while windows are hidden; pause still suspends it.

## Decisions
- Shared core engine over independent frontend rules: consistent cooldowns and missing-data behavior.
- Separate typed shared file over moving legacy GUI preferences: smaller compatible migration.
- Desktop notifications plus bounded in-app history over transient notifications alone: failures
  or denied OS delivery do not hide alerts.
- Existing themed controls over a new UI framework: matches Zentop.

## Assumptions
- Both frontends on the same account use the same OS configuration directory.
- OS notification delivery depends on permissions/service availability; error fallback is required.

## Open Questions
None. Notification API is available in the workspace's existing notify-rust dependency.
